// Whisper large-v3-turbo on mlx-rs, mirroring mlx_audio/stt/models/whisper
// (whisper.py, decoding.py, audio.py) as Kass calls it: greedy decode with
// the DecodingTask logit filters, the 30 s seek loop, fp16 weights, and the
// same hand-written attention (q·kᵀ, precise softmax) so outputs can match.
//
// Input is replay_py.py's whisper dump: <dir>/decodes.jsonl (one line per
// model.generate call), <dir>/<name>.safetensors (its audio) and
// <dir>/constants.safetensors (hanning window, mel filters).

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Result};
use mlx_rs::ops::indexing::{argmax_axis, IndexOp};
use mlx_rs::ops::{self, concatenate_axis};
use mlx_rs::{Array, Dtype};
use serde_json::{json, Value};
use tokenizers::Tokenizer;

const SAMPLE_RATE: usize = 16000;
const N_FFT: usize = 400;
const HOP: usize = 160;
const N_SAMPLES: usize = 480_000;
const N_FRAMES: i32 = 3000;
const N_CTX: usize = 448;
const SAMPLE_LEN: usize = N_CTX / 2;
const INPUT_STRIDE: i64 = 2;
const TIME_PRECISION: f64 = 0.02;
const MAX_INITIAL_TIMESTAMP_INDEX: u32 = 50;
const COMPRESSION_RATIO_THRESHOLD: f64 = 2.4;
const LOGPROB_THRESHOLD: f64 = -1.0;
const NO_SPEECH_THRESHOLD: f64 = 0.6;

fn f16(v: f32) -> Result<Array> {
    Ok(Array::from_f32(v).as_dtype(Dtype::Float16)?)
}

struct Linear {
    w: Array,
    b: Option<Array>,
}

impl Linear {
    fn load(t: &mut HashMap<String, Array>, name: &str) -> Result<Self> {
        let w = t.remove(&format!("{name}.weight")).ok_or_else(|| anyhow!("missing {name}.weight"))?;
        Ok(Self { w, b: t.remove(&format!("{name}.bias")) })
    }

    fn forward(&self, x: &Array) -> Result<Array> {
        let wt = self.w.t();
        Ok(match &self.b {
            Some(b) => ops::addmm(b, x, &wt, None, None)?,
            None => x.matmul(&wt)?,
        })
    }
}

struct LayerNorm {
    w: Array,
    b: Array,
}

impl LayerNorm {
    fn load(t: &mut HashMap<String, Array>, name: &str) -> Result<Self> {
        let mut take = |s: &str| t.remove(&format!("{name}.{s}")).ok_or_else(|| anyhow!("missing {name}.{s}"));
        Ok(Self { w: take("weight")?, b: take("bias")? })
    }

    fn forward(&self, x: &Array) -> Result<Array> {
        Ok(mlx_rs::fast::layer_norm(x, &self.w, &self.b, 1e-5)?)
    }
}

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: i32,
}

type Kv = (Array, Array);

impl Attention {
    fn load(t: &mut HashMap<String, Array>, name: &str, heads: i32) -> Result<Self> {
        Ok(Self {
            q: Linear::load(t, &format!("{name}.q_proj"))?,
            k: Linear::load(t, &format!("{name}.k_proj"))?,
            v: Linear::load(t, &format!("{name}.v_proj"))?,
            o: Linear::load(t, &format!("{name}.out_proj"))?,
            heads,
        })
    }

    // MultiHeadAttention.__call__: self-attention appends to its cache;
    // cross-attention computes k, v from the audio once, then reuses them.
    fn forward(&self, x: &Array, xa: Option<&Array>, mask: Option<&Array>, cache: Option<Kv>) -> Result<(Array, Kv, Array)> {
        let q = self.q.forward(x)?;
        let (k, v) = match (xa, cache) {
            (None, cache) => {
                let mut k = self.k.forward(x)?;
                let mut v = self.v.forward(x)?;
                if let Some((ck, cv)) = cache {
                    k = concatenate_axis(&[&ck, &k], 1)?;
                    v = concatenate_axis(&[&cv, &v], 1)?;
                }
                (k, v)
            }
            (Some(xa), None) => (self.k.forward(xa)?, self.v.forward(xa)?),
            (Some(_), Some(kv)) => kv,
        };
        let (out, qk) = self.qkv(&q, &k, &v, mask)?;
        Ok((self.o.forward(&out)?, (k, v), qk))
    }

    fn qkv(&self, q: &Array, k: &Array, v: &Array, mask: Option<&Array>) -> Result<(Array, Array)> {
        let (b, n, state) = (q.shape()[0], q.shape()[1], q.shape()[2]);
        let nk = k.shape()[1];
        let scale = f16(((state / self.heads) as f64).powf(-0.25) as f32)?;
        let q = ops::multiply(q.reshape(&[b, n, self.heads, -1])?.transpose_axes(&[0, 2, 1, 3])?, &scale)?;
        let k = ops::multiply(k.reshape(&[b, nk, self.heads, -1])?.transpose_axes(&[0, 2, 3, 1])?, &scale)?;
        let v = v.reshape(&[b, nk, self.heads, -1])?.transpose_axes(&[0, 2, 1, 3])?;
        let mut qk = q.matmul(&k)?;
        if let Some(m) = mask {
            qk = ops::add(&qk, m.index((..n, ..n)))?;
        }
        let w = ops::softmax_axis(&qk, -1, true)?;
        let out = w.matmul(&v)?.transpose_axes(&[0, 2, 1, 3])?.reshape(&[b, n, state])?;
        Ok((out, qk))
    }
}

struct Block {
    attn: Attention,
    attn_ln: LayerNorm,
    cross: Option<(Attention, LayerNorm)>,
    mlp1: Linear,
    mlp2: Linear,
    mlp_ln: LayerNorm,
}

struct BlockCache {
    kv: Option<Kv>,
    cross: Option<Kv>,
}

impl Block {
    fn load(t: &mut HashMap<String, Array>, name: &str, heads: i32, cross: bool) -> Result<Self> {
        Ok(Self {
            attn: Attention::load(t, &format!("{name}.self_attn"), heads)?,
            attn_ln: LayerNorm::load(t, &format!("{name}.self_attn_layer_norm"))?,
            cross: if cross {
                Some((
                    Attention::load(t, &format!("{name}.encoder_attn"), heads)?,
                    LayerNorm::load(t, &format!("{name}.encoder_attn_layer_norm"))?,
                ))
            } else {
                None
            },
            mlp1: Linear::load(t, &format!("{name}.fc1"))?,
            mlp2: Linear::load(t, &format!("{name}.fc2"))?,
            mlp_ln: LayerNorm::load(t, &format!("{name}.final_layer_norm"))?,
        })
    }

    fn forward(&self, x: &Array, xa: Option<&Array>, mask: Option<&Array>, cache: BlockCache) -> Result<(Array, BlockCache, Option<Array>)> {
        let (y, kv, _) = self.attn.forward(&self.attn_ln.forward(x)?, None, mask, cache.kv)?;
        let mut x = ops::add(x, &y)?;
        let mut cross_kv = None;
        let mut cross_qk = None;
        if let Some((attn, ln)) = &self.cross {
            let (y, ckv, qk) = attn.forward(&ln.forward(&x)?, xa, None, cache.cross)?;
            x = ops::add(&x, &y)?;
            cross_kv = Some(ckv);
            cross_qk = Some(qk);
        }
        let h = self.mlp2.forward(&mlx_rs::nn::gelu(self.mlp1.forward(&self.mlp_ln.forward(&x)?)?)?)?;
        Ok((ops::add(&x, &h)?, BlockCache { kv: Some(kv), cross: cross_kv }, cross_qk))
    }
}

pub struct Whisper {
    conv1_w: Array,
    conv1_b: Array,
    conv2_w: Array,
    conv2_b: Array,
    enc_pos: Array,
    enc_blocks: Vec<Block>,
    ln_post: LayerNorm,
    embed: Array,
    dec_pos: Array,
    dec_blocks: Vec<Block>,
    dec_ln: LayerNorm,
    mask: Array,
    alignment_heads: Vec<(usize, i32)>,
    n_mels: i32,
}

// whisper.sinusoids, in float32 then fp16 like AudioEncoder.
fn sinusoids(length: i32, channels: i32) -> Result<Array> {
    let inc = (10000f64).ln() / (channels / 2 - 1) as f64;
    let half = Array::arange::<_, i32>(None, channels / 2, None)?;
    let inv = ops::exp(ops::multiply(Array::from_f32(-inc as f32), &half)?)?;
    let t = Array::arange::<_, i32>(None, length, None)?.reshape(&[-1, 1])?;
    let scaled = ops::multiply(&t, inv.reshape(&[1, -1])?)?;
    Ok(concatenate_axis(&[ops::sin(&scaled)?, ops::cos(&scaled)?], 1)?.as_dtype(Dtype::Float16)?)
}

impl Whisper {
    pub fn load(dir: &Path) -> Result<Self> {
        let config: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let get = |k: &str| config[k].as_i64().map(|v| v as i32).ok_or_else(|| anyhow!("config {k}"));
        let generation: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("generation_config.json"))?)?;
        let alignment_heads = generation["alignment_heads"]
            .as_array()
            .ok_or_else(|| anyhow!("alignment_heads"))?
            .iter()
            .map(|p| (p[0].as_u64().unwrap() as usize, p[1].as_i64().unwrap() as i32))
            .collect();
        let raw = Array::load_safetensors(dir.join("model.safetensors"))?;
        let mut t: HashMap<String, Array> =
            raw.into_iter().map(|(k, v)| (k.strip_prefix("model.").unwrap_or(&k).to_string(), v)).collect();
        let mut take = |t: &mut HashMap<String, Array>, k: &str| t.remove(k).ok_or_else(|| anyhow!("missing {k}"));
        // Conv1d weights: (out, in, k) -> (out, k, in).
        let conv1_w = take(&mut t, "encoder.conv1.weight")?.transpose_axes(&[0, 2, 1])?;
        let conv2_w = take(&mut t, "encoder.conv2.weight")?.transpose_axes(&[0, 2, 1])?;
        let conv1_b = take(&mut t, "encoder.conv1.bias")?;
        let conv2_b = take(&mut t, "encoder.conv2.bias")?;
        t.remove("encoder.embed_positions.weight");
        let enc_heads = get("encoder_attention_heads")?;
        let dec_heads = get("decoder_attention_heads")?;
        let enc_blocks = (0..get("encoder_layers")?)
            .map(|i| Block::load(&mut t, &format!("encoder.layers.{i}"), enc_heads, false))
            .collect::<Result<Vec<_>>>()?;
        let dec_blocks = (0..get("decoder_layers")?)
            .map(|i| Block::load(&mut t, &format!("decoder.layers.{i}"), dec_heads, true))
            .collect::<Result<Vec<_>>>()?;
        let ln_post = LayerNorm::load(&mut t, "encoder.layer_norm")?;
        let dec_ln = LayerNorm::load(&mut t, "decoder.layer_norm")?;
        let embed = take(&mut t, "decoder.embed_tokens.weight")?;
        let dec_pos = take(&mut t, "decoder.embed_positions.weight")?;
        if !t.is_empty() {
            return Err(anyhow!("unused weights: {:?}", t.keys().take(5).collect::<Vec<_>>()));
        }
        let n_ctx = get("max_target_positions")?;
        // nn.MultiHeadAttention.create_additive_causal_mask(n_ctx).astype(fp16)
        let idx = Array::arange::<_, i32>(None, n_ctx, None)?;
        let upper = ops::lt(idx.reshape(&[-1, 1])?, idx.reshape(&[1, -1])?)?;
        let mask = ops::multiply(upper.as_dtype(Dtype::Float32)?, Array::from_f32(f32::MIN))?.as_dtype(Dtype::Float16)?;
        let enc_pos = sinusoids(get("max_source_positions")?, get("d_model")?)?;
        let model = Self {
            conv1_w,
            conv1_b,
            conv2_w,
            conv2_b,
            enc_pos,
            enc_blocks,
            ln_post,
            embed,
            dec_pos,
            dec_blocks,
            dec_ln,
            mask,
            alignment_heads,
            n_mels: get("num_mel_bins")?,
        };
        let mut all: Vec<&Array> = vec![
            &model.conv1_w, &model.conv1_b, &model.conv2_w, &model.conv2_b, &model.enc_pos, &model.embed,
            &model.dec_pos, &model.mask,
        ];
        for b in model.enc_blocks.iter().chain(&model.dec_blocks) {
            for l in [&b.attn.q, &b.attn.k, &b.attn.v, &b.attn.o, &b.mlp1, &b.mlp2] {
                all.push(&l.w);
                all.extend(l.b.as_ref());
            }
            if let Some((a, ln)) = &b.cross {
                for l in [&a.q, &a.k, &a.v, &a.o] {
                    all.push(&l.w);
                    all.extend(l.b.as_ref());
                }
                all.extend([&ln.w, &ln.b]);
            }
            all.extend([&b.attn_ln.w, &b.attn_ln.b, &b.mlp_ln.w, &b.mlp_ln.b]);
        }
        mlx_rs::transforms::eval(all)?;
        Ok(model)
    }

    fn encode(&self, mel: &Array) -> Result<Array> {
        let x = ops::add(ops::conv1d(mel, &self.conv1_w, 1, 1, 1, 1)?, &self.conv1_b)?;
        let x = mlx_rs::nn::gelu(&x)?;
        let x = ops::add(ops::conv1d(&x, &self.conv2_w, 2, 1, 1, 1)?, &self.conv2_b)?;
        let mut x = ops::add(mlx_rs::nn::gelu(&x)?, &self.enc_pos)?;
        for b in &self.enc_blocks {
            x = b.forward(&x, None, None, BlockCache { kv: None, cross: None })?.0;
        }
        self.ln_post.forward(&x)
    }

    // TextDecoder.__call__ plus Inference.logits' float32 cast and Kass's
    // word_timing harvest (alignment heads' row for the last position).
    fn decode(&self, tokens: &Array, audio: &Array, cache: &mut Vec<BlockCache>) -> Result<(Array, Array)> {
        let n = tokens.shape()[1];
        let offset = cache.first().and_then(|c| c.kv.as_ref()).map(|(k, _)| k.shape()[1]).unwrap_or(0);
        let mut x = ops::add(self.embed.take_axis(tokens, 0)?, self.dec_pos.index(offset..offset + n))?;
        let fresh = cache.is_empty();
        let mut qks = vec![];
        for (i, b) in self.dec_blocks.iter().enumerate() {
            let c = if fresh { BlockCache { kv: None, cross: None } } else { std::mem::replace(&mut cache[i], BlockCache { kv: None, cross: None }) };
            let (y, c, qk) = b.forward(&x, Some(audio), Some(&self.mask), c)?;
            x = y;
            if fresh {
                cache.push(c);
            } else {
                cache[i] = c;
            }
            qks.push(qk.unwrap());
        }
        let x = self.dec_ln.forward(&x)?;
        let logits = x.matmul(self.embed.t())?.as_dtype(Dtype::Float32)?;
        let rows: Vec<Array> = self
            .alignment_heads
            .iter()
            .map(|(l, h)| qks[*l].index((0, *h, -1)))
            .collect();
        Ok((logits, ops::stack_axis(&rows, 0)?))
    }
}

pub struct Mel {
    window: Array,
    filters: Array,
}

impl Mel {
    // audio.log_mel_spectrogram(audio, n_mels, padding=N_SAMPLES): (frames, n_mels) f32.
    fn compute(&self, audio: &[f32]) -> Result<Array> {
        let mut x: Vec<f32> = audio.to_vec();
        x.resize(audio.len() + N_SAMPLES, 0.0);
        let p = N_FFT / 2;
        let mut padded: Vec<f32> = x[1..p + 1].iter().rev().copied().collect();
        padded.extend(&x);
        padded.extend(x[x.len() - p - 1..x.len() - 1].iter().rev());
        let frames = 1 + (padded.len() - N_FFT) / HOP;
        let a = Array::from_slice(&padded, &[padded.len() as i32]);
        let f = ops::as_strided(&a, &[frames as i32, N_FFT as i32][..], &[HOP as i64, 1][..], None)?;
        let spec = mlx_rs::fft::rfft(ops::multiply(&f, &self.window)?, None, -1)?;
        let mags = ops::square(ops::abs(spec.index(..-1))?)?;
        let mel = mags.matmul(self.filters.t())?;
        let log = ops::log10(ops::maximum(&mel, Array::from_f32(1e-10))?)?;
        let log = ops::maximum(&log, ops::subtract(log.max(None)?, Array::from_f32(8.0))?)?;
        Ok(ops::divide(ops::add(&log, Array::from_f32(4.0))?, Array::from_f32(4.0))?)
    }
}

fn pad_frames(mel: &Array) -> Result<Array> {
    let n = mel.shape()[0];
    let m = if n > N_FRAMES { mel.index(..N_FRAMES) } else { mel.clone() };
    let m = if n < N_FRAMES {
        let z = Array::zeros::<f32>(&[N_FRAMES - n, mel.shape()[1]])?;
        concatenate_axis(&[&m, &z], 0)?
    } else {
        m
    };
    Ok(m.as_dtype(Dtype::Float16)?.reshape(&[1, N_FRAMES, -1])?)
}

#[derive(Clone)]
pub struct Spec {
    sot_sequence: Vec<u32>,
    suppress: Vec<u32>,
    blank: Vec<u32>,
    sot_prev: u32,
    eot: u32,
    no_speech: u32,
    no_timestamps: u32,
    timestamp_begin: u32,
    language_tokens: Vec<u32>,
}

struct DecodeResult {
    tokens: Vec<u32>,
    avg_logprob: f64,
    no_speech_prob: f64,
    compression_ratio: f64,
}

pub struct Transcriber<'a> {
    pub model: &'a Whisper,
    pub tokenizer: &'a Tokenizer,
    pub mel: &'a Mel,
    pub fallbacks: usize,
    pub vocab: i32,
}

// transformers' clean_up_tokenization (tokenizer_config clean_up_tokenization_spaces).
fn clean_up(s: &str) -> String {
    s.replace(" .", ".")
        .replace(" ?", "?")
        .replace(" !", "!")
        .replace(" ,", ",")
        .replace(" ' ", "'")
        .replace(" n't", "n't")
        .replace(" 'm", "'m")
        .replace(" 's", "'s")
        .replace(" 've", "'ve")
        .replace(" 're", "'re")
}

fn compression_ratio(text: &str) -> f64 {
    use flate2::write::ZlibEncoder;
    let mut e = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(text.as_bytes()).unwrap();
    text.len() as f64 / e.finish().unwrap().len() as f64
}

impl Transcriber<'_> {
    fn text(&self, tokens: &[u32], spec: &Spec) -> Result<String> {
        let kept: Vec<u32> = tokens.iter().copied().filter(|t| *t < spec.timestamp_begin).collect();
        Ok(clean_up(&self.tokenizer.decode(&kept, false).map_err(|e| anyhow!("{e}"))?))
    }

    fn mask(&self, ids: impl IntoIterator<Item = u32>) -> Array {
        let mut m = vec![0f32; self.vocab as usize];
        for i in ids {
            m[i as usize] = f32::NEG_INFINITY;
        }
        Array::from_slice(&m, &[self.vocab])
    }

    // SuppressBlank, SuppressTokens, ApplyTimestampRules, then GreedyDecoder.
    fn sample(&self, logits: &Array, seq: &[u32], sample_begin: usize, spec: &Spec, fixed: &Array, blank: &Array, text_only: &Array) -> Result<(Array, Array)> {
        let first = seq.len() == sample_begin;
        let mut l = ops::add(logits, fixed)?;
        if first {
            l = ops::add(&l, blank)?;
        }
        let tb = spec.timestamp_begin as usize;
        let mut m = vec![0f32; self.vocab as usize];
        m[spec.no_timestamps as usize] = f32::NEG_INFINITY;
        let s = &seq[sample_begin..];
        let last_ts = !s.is_empty() && s[s.len() - 1] >= spec.timestamp_begin;
        let pen_ts = s.len() < 2 || s[s.len() - 2] >= spec.timestamp_begin;
        if last_ts {
            if pen_ts {
                m[tb..].fill(f32::NEG_INFINITY);
            } else {
                m[..spec.eot as usize].fill(f32::NEG_INFINITY);
            }
        }
        // mlx-audio's "timestamps shouldn't decrease" rule slices by an index
        // below timestamp_begin, so it masks nothing; it is left out here too.
        if first {
            m[..tb].fill(f32::NEG_INFINITY);
            let last_allowed = tb + MAX_INITIAL_TIMESTAMP_INDEX as usize;
            m[last_allowed + 1..].fill(f32::NEG_INFINITY);
        }
        let lp = ops::subtract(&l, l.logsumexp_axis(-1, true)?)?;
        let ts = lp.index(tb as i32..).logsumexp(None)?;
        let text_max = lp.index(..tb as i32).max(None)?;
        let cond = ops::gt(&ts, &text_max)?;
        let extra = ops::r#where(&cond, text_only, Array::from_f32(0.0))?;
        let m = Array::from_slice(&m, &[self.vocab]);
        let l = ops::add(ops::add(&l, &m)?, &extra)?;
        let next = argmax_axis(&l, -1, None)?;
        let lps = ops::subtract(&l, l.logsumexp_axis(-1, true)?)?;
        let chosen = lps.take_axis(next.reshape(&[1])?, 0)?.reshape(&[])?;
        Ok((next, chosen))
    }

    fn decode_window(&self, mel: &Array, prompt: &[u32], spec: &Spec) -> Result<DecodeResult> {
        let mut initial: Vec<u32> = vec![];
        if !prompt.is_empty() {
            initial.push(spec.sot_prev);
            initial.extend(&prompt[prompt.len().saturating_sub(N_CTX / 2 - 1)..]);
        }
        initial.extend(&spec.sot_sequence);
        let sample_begin = initial.len();
        let sot_index = initial.iter().position(|t| *t == spec.sot_sequence[0]).unwrap() as i32;
        let fixed = self.mask(spec.suppress.iter().copied());
        let blank = self.mask(spec.blank.iter().copied());
        let text_only = self.mask(0..spec.timestamp_begin);

        let audio = self.model.encode(mel)?;
        let mut cache = vec![];
        let ids = Array::from_slice(&initial.iter().map(|t| *t as i32).collect::<Vec<_>>(), &[1, initial.len() as i32]);
        let (logits, row) = self.model.decode(&ids, &audio, &mut cache)?;
        let mut rows = vec![row];
        let no_speech = ops::softmax_axis(logits.index((0, sot_index)), -1, None)?.index(spec.no_speech as i32);
        let mut seq = initial.clone();
        let (mut next, lp) = self.sample(&logits.index((0, -1)), &seq, sample_begin, spec, &fixed, &blank, &text_only)?;
        mlx_rs::transforms::async_eval([&next, &lp, &no_speech])?;
        let mut lps = vec![lp];
        let mut pending = true;
        let mut samples = 1;
        for _ in 1..SAMPLE_LEN {
            if sample_begin + samples > N_CTX {
                break;
            }
            let (logits, row) = self.model.decode(&next.reshape(&[1, 1])?.as_dtype(Dtype::Int32)?, &audio, &mut cache)?;
            mlx_rs::transforms::async_eval([&logits, &row])?;
            rows.push(row);
            let token = next.item::<u32>();
            seq.push(token);
            pending = false;
            if token == spec.eot {
                break;
            }
            let (n, lp) = self.sample(&logits.index((0, -1)), &seq, sample_begin, spec, &fixed, &blank, &text_only)?;
            mlx_rs::transforms::async_eval([&n, &lp])?;
            next = n;
            lps.push(lp);
            pending = true;
            samples += 1;
        }
        if pending {
            seq.push(next.item::<u32>());
        }
        mlx_rs::transforms::eval(rows.iter())?;
        let mut sum = 0f32;
        for lp in &lps {
            sum += lp.item::<f32>();
        }
        let mut tokens = seq[sample_begin..].to_vec();
        if let Some(i) = tokens.iter().position(|t| *t == spec.eot) {
            tokens.truncate(i);
        }
        let text = self.text(&tokens, spec)?;
        Ok(DecodeResult {
            avg_logprob: sum as f64 / (tokens.len() + 1) as f64,
            no_speech_prob: no_speech.item::<f32>() as f64,
            compression_ratio: compression_ratio(text.trim()),
            tokens,
        })
    }

    fn detect_language(&self, mel: &Array, spec: &Spec) -> Result<u32> {
        let audio = self.model.encode(&pad_frames(mel)?)?;
        let sot = Array::from_slice(&[spec.sot_sequence[0] as i32], &[1, 1]);
        let (logits, _) = self.model.decode(&sot, &audio, &mut vec![])?;
        let mut keep = vec![f32::NEG_INFINITY; self.vocab as usize];
        for t in &spec.language_tokens {
            keep[*t as usize] = 0.0;
        }
        let l = ops::add(logits.index((0, 0)), Array::from_slice(&keep, &[self.vocab]))?;
        Ok(argmax_axis(&l, -1, None)?.item::<u32>())
    }

    // Model.generate with temperature 0 only; a window that would fall back
    // to sampling is counted in `fallbacks` and kept.
    pub fn generate(&mut self, audio: &[f32], initial_prompt: &[u32], detect: bool, spec: &Spec) -> Result<(String, Vec<u32>)> {
        let mel = self.mel.compute(audio)?;
        let content_frames = mel.shape()[0] - N_FRAMES;
        let mut spec = spec.clone();
        if detect {
            spec.sot_sequence[1] = self.detect_language(&mel, &spec)?;
        }
        let spec = &spec;
        let tb = spec.timestamp_begin;
        let mut seek: i32 = 0;
        let mut all_tokens: Vec<u32> = initial_prompt.to_vec();
        let mut segments_tokens: Vec<u32> = vec![];
        let mut prompt_reset_since = 0;
        while seek < content_frames {
            let time_offset = seek as f64 * HOP as f64 / SAMPLE_RATE as f64;
            let size = N_FRAMES.min(content_frames - seek);
            let segment = pad_frames(&mel.index(seek..seek + size))?;
            let duration = size as f64 * HOP as f64 / SAMPLE_RATE as f64;
            let r = self.decode_window(&segment, &all_tokens[prompt_reset_since..], spec)?;
            let mut fallback = r.compression_ratio > COMPRESSION_RATIO_THRESHOLD || r.avg_logprob < LOGPROB_THRESHOLD;
            if r.no_speech_prob > NO_SPEECH_THRESHOLD {
                fallback = false;
            }
            if fallback {
                self.fallbacks += 1;
            }
            let tokens = r.tokens;
            let skip = r.no_speech_prob > NO_SPEECH_THRESHOLD && r.avg_logprob <= LOGPROB_THRESHOLD;
            if skip {
                seek += size;
                continue;
            }
            let is_ts: Vec<bool> = tokens.iter().map(|t| *t >= tb).collect();
            let n = tokens.len();
            let single_ending = n >= 2 && !is_ts[n - 2] && is_ts[n - 1];
            let consecutive: Vec<usize> = (0..n.saturating_sub(1)).filter(|&i| is_ts[i] && is_ts[i + 1]).map(|i| i + 1).collect();
            let mut current: Vec<(f64, f64, Vec<u32>)> = vec![];
            if !consecutive.is_empty() {
                let mut slices = consecutive;
                if single_ending {
                    slices.push(n);
                }
                let mut last = 0;
                for cur in slices {
                    let s = &tokens[last..cur];
                    let start = time_offset + (s[0] - tb) as f64 * TIME_PRECISION;
                    let end = time_offset + (s[s.len() - 1] - tb) as f64 * TIME_PRECISION;
                    current.push((start, end, s.to_vec()));
                    last = cur;
                }
                if single_ending {
                    seek += size;
                } else {
                    let pos = (tokens[last - 1] - tb) as i64;
                    seek += (pos * INPUT_STRIDE) as i32;
                }
            } else {
                let mut d = duration;
                if let Some(last) = tokens.iter().rev().find(|t| **t >= tb) {
                    if *last != tb {
                        d = (last - tb) as f64 * TIME_PRECISION;
                    }
                }
                current.push((time_offset, time_offset + d, tokens.clone()));
                seek += size;
            }
            for (start, end, toks) in current {
                let text_tokens: Vec<u32> = toks.iter().copied().filter(|t| *t < spec.eot).collect();
                let text = self.text(&text_tokens, spec)?;
                if start == end || text.trim().is_empty() {
                    continue;
                }
                all_tokens.extend(&toks);
                segments_tokens.extend(&toks);
            }
            // Kass decodes with condition_on_previous_text=False.
            prompt_reset_since = all_tokens.len();
        }
        mlx_rs::memory::clear_cache()?;
        Ok((self.text(&all_tokens[initial_prompt.len()..], spec)?, segments_tokens))
    }
}

fn u32s(v: &Value) -> Vec<u32> {
    v.as_array().map(|a| a.iter().map(|x| x.as_u64().unwrap() as u32).collect()).unwrap_or_default()
}

pub fn run(dir: &str, out: &str, passes: usize) -> Result<()> {
    let dir = Path::new(dir);
    let started = Instant::now();
    let snapshot = crate::qwen::snapshot("openai/whisper-large-v3-turbo")?;
    let model = Whisper::load(&snapshot)?;
    let tokenizer = Tokenizer::from_file(snapshot.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
    let constants = Array::load_safetensors(dir.join("constants.safetensors"))?;
    let mel = Mel { window: constants["window"].clone(), filters: constants["filters"].clone() };
    if mel.filters.shape()[0] != model.n_mels {
        return Err(anyhow!("mel filters do not match the model"));
    }
    eprintln!("loaded Whisper in {:.2}s", started.elapsed().as_secs_f64());
    let vocab = model.embed.shape()[0];
    let mut tr = Transcriber { model: &model, tokenizer: &tokenizer, mel: &mel, fallbacks: 0, vocab };
    let specs: Vec<Value> = std::fs::read_to_string(dir.join("decodes.jsonl"))?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect::<Result<_>>()?;
    let mut audio: HashMap<String, Vec<f32>> = HashMap::new();
    for s in &specs {
        let name = s["name"].as_str().unwrap().to_string();
        let a = Array::load_safetensors(dir.join(format!("{name}.safetensors")))?;
        let a = a["audio"].as_dtype(Dtype::Float32)?;
        audio.insert(name, a.as_slice::<f32>().to_vec());
    }
    let mut file = std::fs::File::create(out)?;
    for pass in 0..passes {
        for s in &specs {
            let spec = Spec {
                sot_sequence: u32s(&s["sot_sequence"]),
                suppress: u32s(&s["suppress"]),
                blank: u32s(&s["blank"]),
                sot_prev: s["sot_prev"].as_u64().unwrap() as u32,
                eot: s["eot"].as_u64().unwrap() as u32,
                no_speech: s["no_speech"].as_u64().unwrap() as u32,
                no_timestamps: s["no_timestamps"].as_u64().unwrap() as u32,
                timestamp_begin: s["timestamp_begin"].as_u64().unwrap() as u32,
                language_tokens: u32s(&s["language_tokens"]),
            };
            let name = s["name"].as_str().unwrap();
            let before = tr.fallbacks;
            let t = Instant::now();
            let (text, tokens) = tr.generate(&audio[name], &u32s(&s["prompt"]), s["detect"].as_bool().unwrap_or(false), &spec)?;
            let seconds = t.elapsed().as_secs_f64();
            writeln!(
                file,
                "{}",
                json!({"name": name, "n": s["n"], "pass": pass, "seconds": seconds, "text": text, "tokens": tokens,
                       "fallback": tr.fallbacks > before})
            )?;
        }
    }
    Ok(())
}

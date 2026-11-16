// Qwen3 (mlx-community 4-bit) on mlx-rs, mirroring mlx_lm/models/qwen3.py and
// backend/backends/qwen_llm_backend.py: per-prompt KV caches trimmed to the
// shared prefix, and prompt-lookup decoding from the hint and the prompt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use mlx_rs::fast::{self, ScaledDotProductAttentionMask};
use mlx_rs::ops::indexing::{argmax_axis, IndexOp, TryIndexMutOp};
use mlx_rs::ops::{
    self, concatenate, dequantize, quantized_matmul, DequantizeOptions, QuantizedMatmulOptions,
};
use mlx_rs::{Array, Dtype};
use tokenizers::Tokenizer;

const GROUP: i32 = 64;
const BITS: i32 = 4;
const STEP: i32 = 256;
pub const SAME_PROMPT_TOKENS: usize = 64;
pub const KEPT_CACHE_TOKENS: usize = 4096;
pub const LOOKUP_DRAFT_TOKENS: usize = 24;
pub const MAX_PROMPT_CACHES: usize = 8;

fn qopts() -> QuantizedMatmulOptions {
    QuantizedMatmulOptions { group_size: Some(GROUP), bits: Some(BITS), transpose: true, ..Default::default() }
}

struct Lora {
    a: Array,
    b: Array,
    scale: f32,
}

struct QLinear {
    w: Array,
    scales: Array,
    biases: Array,
    lora: Option<Lora>,
}

impl QLinear {
    fn load(t: &mut HashMap<String, Array>, name: &str) -> Result<Self> {
        let mut take = |s: &str| t.remove(&format!("{name}.{s}")).ok_or_else(|| anyhow!("missing {name}.{s}"));
        Ok(Self { w: take("weight")?, scales: take("scales")?, biases: take("biases")?, lora: None })
    }

    fn forward(&self, x: &Array) -> Result<Array> {
        let y = quantized_matmul(x, &self.w, &self.scales, &self.biases, qopts())?;
        match &self.lora {
            None => Ok(y),
            Some(l) => {
                let z = x.matmul(&l.a)?.matmul(&l.b)?;
                let z = ops::multiply(&z, Array::from_f32(l.scale))?.as_dtype(x.dtype())?;
                Ok(ops::add(&y, &z)?)
            }
        }
    }
}

struct Layer {
    q: QLinear,
    k: QLinear,
    v: QLinear,
    o: QLinear,
    q_norm: Array,
    k_norm: Array,
    gate: QLinear,
    up: QLinear,
    down: QLinear,
    ln1: Array,
    ln2: Array,
}

#[derive(Clone)]
pub struct KvCache {
    keys: Option<Array>,
    values: Option<Array>,
    pub offset: i32,
}

impl KvCache {
    fn new() -> Self {
        Self { keys: None, values: None, offset: 0 }
    }

    fn update_and_fetch(&mut self, k: Array, v: Array) -> Result<(Array, Array)> {
        let prev = self.offset;
        let n = k.shape()[2];
        let cap = self.keys.as_ref().map(|a| a.shape()[2]).unwrap_or(0);
        if self.keys.is_none() || prev + n > cap {
            let s = k.shape();
            let steps = (STEP + n - 1) / STEP;
            let shape = [s[0], s[1], steps * STEP, s[3]];
            let nk = ops::zeros_dtype(&shape, k.dtype())?;
            let nv = ops::zeros_dtype(&shape, v.dtype())?;
            match (self.keys.take(), self.values.take()) {
                (Some(mut ok), Some(mut ov)) => {
                    if prev % STEP != 0 {
                        ok = ok.index((.., .., ..prev, ..));
                        ov = ov.index((.., .., ..prev, ..));
                    }
                    self.keys = Some(concatenate(&[ok, nk], 2)?);
                    self.values = Some(concatenate(&[ov, nv], 2)?);
                }
                _ => {
                    self.keys = Some(nk);
                    self.values = Some(nv);
                }
            }
        }
        self.offset += n;
        let off = self.offset;
        let keys = self.keys.as_mut().unwrap();
        keys.try_index_mut((.., .., prev..off, ..), &k)?;
        let values = self.values.as_mut().unwrap();
        values.try_index_mut((.., .., prev..off, ..), &v)?;
        Ok((keys.index((.., .., ..off, ..)), values.index((.., .., ..off, ..))))
    }

    fn trim(&mut self, n: i32) {
        self.offset -= n.min(self.offset);
    }

    fn state(&self) -> Vec<Array> {
        match (&self.keys, &self.values) {
            (Some(k), Some(v)) => {
                if self.offset == k.shape()[2] {
                    vec![k.clone(), v.clone()]
                } else {
                    vec![k.index((.., .., ..self.offset, ..)), v.index((.., .., ..self.offset, ..))]
                }
            }
            _ => vec![],
        }
    }

    fn prefix(&self, shared: i32) -> Self {
        let (k, v) = (self.keys.as_ref().unwrap(), self.values.as_ref().unwrap());
        Self {
            keys: Some(k.index((.., .., ..shared, ..))),
            values: Some(v.index((.., .., ..shared, ..))),
            offset: shared,
        }
    }
}

pub struct Qwen {
    embed_w: Array,
    embed_s: Array,
    embed_b: Array,
    layers: Vec<Layer>,
    norm: Array,
    n_heads: i32,
    n_kv: i32,
    head_dim: i32,
    eps: f32,
    theta: f32,
    pub tokenizer: Tokenizer,
    pub eos: Vec<u32>,
}

pub fn snapshot(repo: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(home)
        .join(".cache/huggingface/hub")
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots");
    let entry = std::fs::read_dir(&dir).with_context(|| format!("{dir:?}"))?.next().ok_or_else(|| anyhow!("no snapshot"))??;
    Ok(entry.path())
}

impl Qwen {
    pub fn load(dir: &Path, adapter: Option<&Path>) -> Result<Self> {
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let get = |k: &str| config[k].as_i64().ok_or_else(|| anyhow!("config {k}"));
        let n_layers = get("num_hidden_layers")? as usize;
        let mut t = Array::load_safetensors(dir.join("model.safetensors"))?;
        let mut layers = Vec::with_capacity(n_layers);
        for i in 0..n_layers {
            let p = format!("model.layers.{i}");
            let mut take = |s: &str| t.remove(&format!("{p}.{s}")).ok_or_else(|| anyhow!("missing {p}.{s}"));
            let (q_norm, k_norm, ln1, ln2) = (
                take("self_attn.q_norm.weight")?,
                take("self_attn.k_norm.weight")?,
                take("input_layernorm.weight")?,
                take("post_attention_layernorm.weight")?,
            );
            layers.push(Layer {
                q: QLinear::load(&mut t, &format!("{p}.self_attn.q_proj"))?,
                k: QLinear::load(&mut t, &format!("{p}.self_attn.k_proj"))?,
                v: QLinear::load(&mut t, &format!("{p}.self_attn.v_proj"))?,
                o: QLinear::load(&mut t, &format!("{p}.self_attn.o_proj"))?,
                gate: QLinear::load(&mut t, &format!("{p}.mlp.gate_proj"))?,
                up: QLinear::load(&mut t, &format!("{p}.mlp.up_proj"))?,
                down: QLinear::load(&mut t, &format!("{p}.mlp.down_proj"))?,
                q_norm,
                k_norm,
                ln1,
                ln2,
            });
        }
        if let Some(adapter) = adapter {
            let cfg: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(adapter.join("adapter_config.json"))?)?;
            let scale = cfg["lora_parameters"]["scale"].as_f64().unwrap_or(20.0) as f32;
            let mut a = Array::load_safetensors(adapter.join("adapters.safetensors"))?;
            let names: Vec<String> = a.keys().filter(|k| k.ends_with(".lora_a")).cloned().collect();
            for name in names {
                let base = name.trim_end_matches(".lora_a").to_string();
                let rest = base.strip_prefix("model.layers.").ok_or_else(|| anyhow!("adapter key {base}"))?;
                let (idx, module) = rest.split_once('.').unwrap();
                let layer = &mut layers[idx.parse::<usize>()?];
                let lin = match module {
                    "self_attn.q_proj" => &mut layer.q,
                    "self_attn.k_proj" => &mut layer.k,
                    "self_attn.v_proj" => &mut layer.v,
                    "self_attn.o_proj" => &mut layer.o,
                    "mlp.gate_proj" => &mut layer.gate,
                    "mlp.up_proj" => &mut layer.up,
                    "mlp.down_proj" => &mut layer.down,
                    other => return Err(anyhow!("adapter module {other}")),
                };
                lin.lora = Some(Lora {
                    a: a.remove(&name).unwrap(),
                    b: a.remove(&format!("{base}.lora_b")).ok_or_else(|| anyhow!("{base}.lora_b"))?,
                    scale,
                });
            }
        }
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
        let eos = match &config["eos_token_id"] {
            serde_json::Value::Array(v) => v.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect(),
            v => vec![v.as_u64().ok_or_else(|| anyhow!("eos"))? as u32],
        };
        let model = Self {
            embed_w: t.remove("model.embed_tokens.weight").unwrap(),
            embed_s: t.remove("model.embed_tokens.scales").unwrap(),
            embed_b: t.remove("model.embed_tokens.biases").unwrap(),
            norm: t.remove("model.norm.weight").unwrap(),
            layers,
            n_heads: get("num_attention_heads")? as i32,
            n_kv: get("num_key_value_heads")? as i32,
            head_dim: get("head_dim")? as i32,
            eps: config["rms_norm_eps"].as_f64().unwrap() as f32,
            theta: config["rope_theta"].as_f64().unwrap() as f32,
            tokenizer,
            eos,
        };
        let mut all = vec![&model.embed_w, &model.embed_s, &model.embed_b, &model.norm];
        for l in &model.layers {
            all.extend([&l.q_norm, &l.k_norm, &l.ln1, &l.ln2]);
            for lin in [&l.q, &l.k, &l.v, &l.o, &l.gate, &l.up, &l.down] {
                all.extend([&lin.w, &lin.scales, &lin.biases]);
                if let Some(lo) = &lin.lora {
                    all.extend([&lo.a, &lo.b]);
                }
            }
        }
        mlx_rs::transforms::eval(all)?;
        Ok(model)
    }

    pub fn new_cache(&self) -> Vec<KvCache> {
        (0..self.layers.len()).map(|_| KvCache::new()).collect()
    }

    fn attention(&self, l: &Layer, x: &Array, cache: &mut KvCache) -> Result<Array> {
        let (b, n) = (x.shape()[0], x.shape()[1]);
        let q = l.q.forward(x)?.reshape(&[b, n, self.n_heads, -1])?;
        let q = fast::rms_norm(&q, Some(&l.q_norm), self.eps)?.transpose_axes(&[0, 2, 1, 3])?;
        let k = l.k.forward(x)?.reshape(&[b, n, self.n_kv, -1])?;
        let k = fast::rms_norm(&k, Some(&l.k_norm), self.eps)?.transpose_axes(&[0, 2, 1, 3])?;
        let v = l.v.forward(x)?.reshape(&[b, n, self.n_kv, -1])?.transpose_axes(&[0, 2, 1, 3])?;
        let q = fast::rope(&q, self.head_dim, false, self.theta, 1.0, cache.offset, None)?;
        let k = fast::rope(&k, self.head_dim, false, self.theta, 1.0, cache.offset, None)?;
        let (k, v) = cache.update_and_fetch(k, v)?;
        let scale = (self.head_dim as f32).powf(-0.5);
        let o = if n > 1 {
            fast::scaled_dot_product_attention(&q, &k, &v, scale, ScaledDotProductAttentionMask::Causal, None)?
        } else {
            fast::scaled_dot_product_attention(&q, &k, &v, scale, None, None)?
        };
        let o = o.transpose_axes(&[0, 2, 1, 3])?.reshape(&[b, n, -1])?;
        l.o.forward(&o)
    }

    fn hidden(&self, tokens: &[u32], cache: &mut [KvCache]) -> Result<Array> {
        let ids: Vec<i32> = tokens.iter().map(|&t| t as i32).collect();
        let ids = Array::from_slice(&ids, &[1, ids.len() as i32]);
        let mut h = dequantize(
            self.embed_w.take_axis(&ids, 0)?,
            self.embed_s.take_axis(&ids, 0)?,
            &self.embed_b.take_axis(&ids, 0)?,
            DequantizeOptions { group_size: Some(GROUP), bits: Some(BITS), ..Default::default() },
        )?;
        for (l, c) in self.layers.iter().zip(cache.iter_mut()) {
            let r = self.attention(l, &fast::rms_norm(&h, Some(&l.ln1), self.eps)?, c)?;
            let h2 = ops::add(&h, &r)?;
            let x = fast::rms_norm(&h2, Some(&l.ln2), self.eps)?;
            let g = l.gate.forward(&x)?;
            let act = ops::multiply(ops::multiply(&g, ops::sigmoid(&g)?)?, l.up.forward(&x)?)?;
            h = ops::add(&h2, l.down.forward(&act)?)?;
        }
        Ok(fast::rms_norm(&h, Some(&self.norm), self.eps)?)
    }

    pub fn prefill(&self, tokens: &[u32], cache: &mut [KvCache]) -> Result<()> {
        self.hidden(tokens, cache)?;
        let states: Vec<Array> = cache.iter().flat_map(|c| c.state()).collect();
        mlx_rs::transforms::eval(&states)?;
        Ok(())
    }

    // Greedy samples (argmax of log-probabilities) for the last `last` positions.
    pub fn step(&self, tokens: &[u32], cache: &mut [KvCache], last: i32) -> Result<Array> {
        let h = self.hidden(tokens, cache)?;
        let logits = quantized_matmul(&h, &self.embed_w, &self.embed_s, &self.embed_b, qopts())?;
        let n = logits.shape()[1];
        let logits = logits.index((0, (n - last)..n, ..));
        let lp = ops::subtract(&logits, logits.logsumexp_axis(-1, true)?)?;
        Ok(argmax_axis(&lp, -1, None)?)
    }

    pub fn encode(&self, text: &str) -> Result<Vec<u32>> {
        Ok(self.tokenizer.encode(text, false).map_err(|e| anyhow!("{e}"))?.get_ids().to_vec())
    }

    pub fn decode(&self, ids: &[u32]) -> Result<String> {
        self.tokenizer.decode(ids, false).map_err(|e| anyhow!("{e}"))
    }
}

pub fn chat_prompt(system: Option<&str>, examples: &[(String, String)], prompt: &str) -> String {
    let mut s = String::new();
    if let Some(sys) = system.filter(|s| !s.is_empty()) {
        s += &format!("<|im_start|>system\n{sys}<|im_end|>\n");
    }
    for (u, a) in examples {
        s += &format!("<|im_start|>user\n{u}<|im_end|>\n<|im_start|>assistant\n{a}<|im_end|>\n");
    }
    s += &format!("<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n");
    s
}

pub fn propose_draft(
    generated: &[u32],
    sources: &[Vec<u32>],
    limit: usize,
    cursor: &mut HashMap<usize, usize>,
) -> Vec<u32> {
    if generated.is_empty() {
        return sources.first().map(|s| s.iter().take(limit).copied().collect()).unwrap_or_default();
    }
    for size in [4usize, 3, 2, 1] {
        if generated.len() < size {
            continue;
        }
        let tail = &generated[generated.len() - size..];
        for (index, source) in sources.iter().enumerate() {
            let last = source.len() as isize - size as isize;
            let start = *cursor.get(&index).unwrap_or(&0) as isize;
            let first: Vec<isize> = (start..=last).collect();
            let second: Vec<isize> = (0..start.min(last + 1)).collect();
            for position in first.into_iter().chain(second) {
                if position < 0 {
                    continue;
                }
                let p = position as usize;
                if p + size <= source.len() && &source[p..p + size] == tail && p + size < source.len() {
                    cursor.insert(index, p + size);
                    let end = (p + size + limit).min(source.len());
                    return source[p + size..end].to_vec();
                }
            }
        }
    }
    vec![]
}

pub struct PromptCache {
    pub cache: Vec<KvCache>,
    pub tokens: Vec<u32>,
    pub key: String,
}

pub struct Session {
    pub caches: Vec<PromptCache>,
}

pub struct Generated {
    pub text: String,
    pub reused: usize,
    pub prompt_tokens: usize,
    pub generated: usize,
    pub calls: usize,
    pub accepted: usize,
    pub prompt_ids: Vec<u32>,
}

impl Session {
    pub fn new() -> Self {
        Self { caches: vec![] }
    }

    fn reusable(&mut self, model: &Qwen, tokens: &[u32], cache_key: Option<&str>) -> PromptCache {
        let shared_with = |e: &PromptCache| {
            let limit = e.tokens.len().min(tokens.len() - 1);
            let mut s = 0;
            while s < limit && e.tokens[s] == tokens[s] {
                s += 1;
            }
            s
        };
        let key = match cache_key {
            Some(k) => format!("k:{k}"),
            None => format!("t:{:?}", &tokens[..tokens.len().min(SAME_PROMPT_TOKENS)]),
        };
        let entry = if let Some(pos) = self.caches.iter().position(|e| e.key == key) {
            let mut e = self.caches.remove(pos);
            let shared = shared_with(&e);
            let drop = (e.tokens.len() - shared) as i32;
            for c in e.cache.iter_mut() {
                c.trim(drop);
            }
            e.tokens.truncate(shared);
            e
        } else {
            let mut e = PromptCache { cache: model.new_cache(), tokens: vec![], key };
            // max(..., key=shared_with) keeps the first of equal ones.
            let mut best: Option<(usize, usize)> = None;
            for (i, d) in self.caches.iter().enumerate() {
                let s = shared_with(d);
                if best.map(|(_, b)| s > b).unwrap_or(true) {
                    best = Some((i, s));
                }
            }
            if let Some((i, shared)) = best {
                if shared > 0 {
                    let donor = &self.caches[i];
                    e.cache = donor.cache.iter().map(|c| c.prefix(shared as i32)).collect();
                    e.tokens = donor.tokens[..shared].to_vec();
                }
            }
            e
        };
        let kept: Vec<PromptCache> =
            self.caches.drain(..).filter(|e| e.tokens.len() <= KEPT_CACHE_TOKENS).collect();
        let skip = kept.len().saturating_sub(MAX_PROMPT_CACHES - 1);
        self.caches = kept.into_iter().skip(skip).collect();
        entry
    }

    #[allow(clippy::too_many_arguments)]
    pub fn generate(
        &mut self,
        model: &Qwen,
        prompt: &str,
        system: Option<&str>,
        examples: &[(String, String)],
        max_tokens: usize,
        hint: Option<&str>,
        cache_key: Option<&str>,
    ) -> Result<Generated> {
        let chat = chat_prompt(system, examples, prompt);
        let tokens = model.encode(&chat)?;
        let mut entry = self.reusable(model, &tokens, cache_key);
        let reused = entry.tokens.len();
        let result = match hint {
            Some(h) => self.lookup(model, &tokens, reused, &mut entry, max_tokens, h, prompt),
            None => self.plain(model, &tokens, reused, &mut entry, max_tokens),
        };
        self.caches.push(entry);
        let mut g = result?;
        g.prompt_ids = tokens;
        Ok(g)
    }

    fn plain(&self, model: &Qwen, tokens: &[u32], reused: usize, entry: &mut PromptCache, max_tokens: usize) -> Result<Generated> {
        let cache = &mut entry.cache;
        let mut y: Vec<u32> = tokens[reused..].to_vec();
        while y.len() > 1 {
            let n = 2048.min(y.len() - 1);
            model.prefill(&y[..n], cache)?;
            y.drain(..n);
            mlx_rs::memory::clear_cache()?;
        }
        let mut generated: Vec<u32> = vec![];
        let mut next = model.step(&y, cache, 1)?;
        mlx_rs::transforms::async_eval([&next])?;
        // generate_step feeds each token before yielding it, the last one and EOS
        // included, and stream_generate yields EOS too: the cache holds the
        // prompt plus every generated token.
        loop {
            let following = model.step_array(&next, cache)?;
            mlx_rs::transforms::async_eval([&following])?;
            next.eval()?;
            let token = next.as_slice::<u32>()[0];
            generated.push(token);
            if model.eos.contains(&token) || generated.len() >= max_tokens {
                break;
            }
            next = following;
        }
        let mut fed = tokens.to_vec();
        fed.extend(&generated);
        let text_tokens: Vec<u32> = generated.iter().copied().filter(|t| !model.eos.contains(t)).collect();
        let text = model.decode(&text_tokens)?.trim().to_string();
        entry.tokens = fed;
        Ok(Generated { text, reused, prompt_tokens: tokens.len(), generated: text_tokens.len(), calls: generated.len(), accepted: 0, prompt_ids: vec![] })
    }

    #[allow(clippy::too_many_arguments)]
    fn lookup(&self, model: &Qwen, tokens: &[u32], reused: usize, entry: &mut PromptCache, max_tokens: usize, hint: &str, prompt: &str) -> Result<Generated> {
        let sources = vec![if hint.is_empty() { vec![] } else { model.encode(hint)? }, model.encode(prompt)?];
        let cache = &mut entry.cache;
        let mut pending: Vec<u32> = tokens[reused..].to_vec();
        let mut fed: Vec<u32> = tokens[..reused].to_vec();
        let mut generated: Vec<u32> = vec![];
        let mut cursor = HashMap::new();
        let (mut calls, mut accepted) = (0, 0);
        while pending.len() > 1 {
            let n = 512.min(pending.len() - 1);
            model.prefill(&pending[..n], cache)?;
            fed.extend(&pending[..n]);
            pending.drain(..n);
        }
        let mut stopped = false;
        while !stopped && generated.len() < max_tokens {
            let limit = LOOKUP_DRAFT_TOKENS.min(max_tokens - generated.len() - 1);
            let draft = propose_draft(&generated, &sources, limit, &mut cursor);
            let mut input = pending.clone();
            input.extend(&draft);
            let samples = model.step(&input, cache, draft.len() as i32 + 1)?;
            samples.eval()?;
            let samples: Vec<u32> = samples.as_slice::<u32>().to_vec();
            calls += 1;
            let mut kept = 0;
            while kept < draft.len() && samples[kept] == draft[kept] {
                kept += 1;
            }
            for c in cache.iter_mut() {
                c.trim((draft.len() - kept) as i32);
            }
            fed.extend(&pending);
            fed.extend(&draft[..kept]);
            accepted += kept;
            for &token in draft[..kept].iter().chain(std::iter::once(&samples[kept])) {
                if model.eos.contains(&token) || generated.len() >= max_tokens {
                    stopped = true;
                    break;
                }
                generated.push(token);
            }
            if stopped {
                break;
            }
            pending = vec![samples[kept]];
        }
        let text = model.decode(&generated)?.trim().to_string();
        entry.tokens = fed;
        Ok(Generated { text, reused, prompt_tokens: tokens.len(), generated: generated.len(), calls, accepted, prompt_ids: vec![] })
    }
}

impl Qwen {
    // One decode step from an unevaluated token array, as mlx_lm's generate_step pipelines it.
    fn step_array(&self, token: &Array, cache: &mut [KvCache]) -> Result<Array> {
        let h = self.hidden_array(token, cache)?;
        let logits = quantized_matmul(&h, &self.embed_w, &self.embed_s, &self.embed_b, qopts())?;
        let logits = logits.index((0, -1, ..));
        let lp = ops::subtract(&logits, logits.logsumexp_axis(-1, true)?)?;
        Ok(argmax_axis(&lp, -1, None)?.reshape(&[1])?)
    }

    fn hidden_array(&self, ids: &Array, cache: &mut [KvCache]) -> Result<Array> {
        let ids = ids.as_dtype(Dtype::Int32)?.reshape(&[1, -1])?;
        let mut h = dequantize(
            self.embed_w.take_axis(&ids, 0)?,
            self.embed_s.take_axis(&ids, 0)?,
            &self.embed_b.take_axis(&ids, 0)?,
            DequantizeOptions { group_size: Some(GROUP), bits: Some(BITS), ..Default::default() },
        )?;
        for (l, c) in self.layers.iter().zip(cache.iter_mut()) {
            let r = self.attention(l, &fast::rms_norm(&h, Some(&l.ln1), self.eps)?, c)?;
            let h2 = ops::add(&h, &r)?;
            let x = fast::rms_norm(&h2, Some(&l.ln2), self.eps)?;
            let g = l.gate.forward(&x)?;
            let act = ops::multiply(ops::multiply(&g, ops::sigmoid(&g)?)?, l.up.forward(&x)?)?;
            h = ops::add(&h2, l.down.forward(&act)?)?;
        }
        Ok(fast::rms_norm(&h, Some(&self.norm), self.eps)?)
    }
}

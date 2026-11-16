// Replays recorded Kass model calls (record_calls.py) on mlx-rs.
//
//   mlxbench qwen <calls.jsonl> <out.jsonl> <model size|keep> <passes>
//   mlxbench whisper <calls dir> <out.jsonl> <passes>

mod qwen;
mod whisper;

use std::io::Write;
use std::time::Instant;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

const METALLIB: &str = "/Users/mrgnhnt96/Development/ts/kass/backend/venv/lib/python3.12/site-packages/mlx/lib/mlx.metallib";
// mx.device_info()["max_recommended_working_set_size"] on this Mac; mlx_lm's wired_limit sets it.
const WIRED: usize = 55_662_788_608;

fn calls(path: &str) -> Result<Vec<Value>> {
    std::fs::read_to_string(path)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    mlx_rs::metal::set_metallib_path(std::env::var("MLX_METALLIB").unwrap_or(METALLIB.into()))?;
    mlx_rs::memory::set_wired_limit(WIRED)?;
    match args.get(1).map(String::as_str) {
        Some("qwen") => run_qwen(&args[2], &args[3], &args[4], args[5].parse()?),
        Some("whisper") => whisper::run(&args[2], &args[3], args[4].parse()?),
        _ => Err(anyhow!("usage: mlxbench qwen|whisper ...")),
    }
}

fn run_qwen(calls_path: &str, out: &str, size: &str, passes: usize) -> Result<()> {
    let all: Vec<Value> = calls(calls_path)?.into_iter().filter(|c| c["kind"] == "generate").collect();
    let size = if size == "keep" { all[0]["model_size"].as_str().unwrap().to_string() } else { size.to_string() };
    let started = Instant::now();
    let dir = qwen::snapshot(&format!("mlx-community/Qwen3-{size}-4bit"))?;
    let model = qwen::Qwen::load(&dir, None)?;
    eprintln!("loaded Qwen3 {size} in {:.2}s", started.elapsed().as_secs_f64());
    let mut file = std::fs::File::create(out)?;
    let mut session = qwen::Session::new();
    for pass in 0..passes {
        for c in &all {
            let examples: Vec<(String, String)> = c["examples"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let t = Instant::now();
            let g = session.generate(
                &model,
                c["prompt"].as_str().unwrap(),
                c["system"].as_str(),
                &examples,
                c["max_tokens"].as_u64().unwrap() as usize,
                c["hint"].as_str(),
                c["cache_key"].as_str(),
            )?;
            let seconds = t.elapsed().as_secs_f64();
            writeln!(
                file,
                "{}",
                json!({"n": c["n"], "pass": pass, "seconds": seconds, "output": g.text, "reused": g.reused,
                       "prompt_tokens": g.prompt_tokens, "generated": g.generated, "calls": g.calls,
                       "accepted": g.accepted, "prompt_ids": g.prompt_ids})
            )?;
        }
    }
    Ok(())
}

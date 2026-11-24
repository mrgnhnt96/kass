#!/bin/bash
# Interleave the MLX cleanup baseline with llama.cpp variants, round by round.
# usage: run_llm_rounds.sh <scratch> <rounds> <passes>
#   <scratch>/prompts.json      cleanup prompts (see docs/plans/RUST_INFERENCE_COMPARISON.md, Reproduce)
#   <scratch>/models/*.gguf
#   <scratch>/data              copy of the Kass data dir
#   $LLBENCH                    built llbench binary (llama-cpp-2, in process)
#   $LLAMA_SERVER               built llama-server binary
# Output: <scratch>/llm.jsonl (appended)
set -u
S=$1; ROUNDS=$2; PASSES=$3
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
PY=${PY:-$REPO/backend/venv/bin/python}   # a worktree has no venv: set PY to the main checkout's
[ -x "$PY" ] || { echo "PY not executable: $PY" >&2; exit 1; }
[ -x "${LLBENCH:-}" ] && [ -x "${LLAMA_SERVER:-}" ] || { echo "set LLBENCH and LLAMA_SERVER" >&2; exit 1; }
OUT=$S/llm.jsonl
M=$S/models
tag() { sed "s/^{/{\"round\": $1, /"; }
for r in $(seq 1 "$ROUNDS"); do
  echo "round $r $(date +%T) load: $(sysctl -n vm.loadavg)" >&2
  "$PY" "$HERE/mlx_llm.py" "$REPO" "$S/data" "$S/prompts.json" "$PASSES" 2>/dev/null | tag "$r" >> "$OUT"
  "$LLBENCH" "$M/Qwen3-4B-Q4_K_M.gguf" "$S/prompts.json" "$PASSES" 2>/dev/null | tag "$r" >> "$OUT"
  "$PY" "$HERE/llama_llm.py" "$LLAMA_SERVER" "$M/Qwen3-4B-Q4_K_M.gguf" "$S/prompts.json" "$PASSES" 2>/dev/null | tag "$r" >> "$OUT"
  "$LLBENCH" "$M/Qwen3-4B-Q8_0.gguf" "$S/prompts.json" "$PASSES" 2>/dev/null | tag "$r" >> "$OUT"
done
echo "done $(date +%T)" >&2

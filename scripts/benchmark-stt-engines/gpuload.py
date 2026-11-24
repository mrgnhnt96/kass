"""Keep the GPU busy with MLX matmuls for N seconds (stand-in for the LLM cleanup running concurrently)."""

import sys
import time

import mlx.core as mx

secs = float(sys.argv[1])
a = mx.random.normal((4096, 4096))
b = mx.random.normal((4096, 4096))
end = time.time() + secs
n = 0
while time.time() < end:
    mx.eval(a @ b)
    n += 1
print("gpuload iterations", n, flush=True)

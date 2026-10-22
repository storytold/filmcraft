# filmcraft-gpu

FilmCraft's wgpu compositor implements the render plan with transformed layers,
linear-light blending and supported effects. Unsupported managed transforms
use the renderer's existing CPU fallback.

The raw YUV transfer stage handles explicit Apple Log with the same published
inverse curve as `filmcraft-color`; it does not enable the full managed camera
log transform on the GPU. The `apple_log_transfer_gpu_matches_cpu` test compares
a generated 10-bit luma ramp against CPU output, with a two-code-value tolerance.
Tests require an available GPU adapter and report a skip when none is available.

Run `cargo test --release -p filmcraft-gpu` and the full `cargo xtask ci` gates.

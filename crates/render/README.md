# filmcraft-render

Sequence evaluation and the CPU reference compositor. The same item renderer supplies monitors,
exports, and CPU-prepared layers in the GPU frame plan.

## Drop Shadow bounds

A clip's Drop Shadow needs transparent pixels beyond the decoded frame. `effect_bounds` expands
its working image immediately before each enabled shadow, after preceding effects have produced
its alpha. Padding covers the offset and the full support of the three-pass box approximation to
Gaussian blur. Shadow Only uses the same expanded bounds.

Padding is symmetric and its accumulated source origin is tracked separately. Final Motion placement
subtracts that origin, so expanding the canvas does not reposition or rescale the clip. Point controls
and effect/Opacity masks remain in original clip coordinates. Effects continue to run in stack order;
subsequent effects operate on the expanded image (including its shadow). Frame-relative operations,
such as percentage crops, therefore use those expanded working bounds. Temporal reads rebuild the
preceding stack and align it to the requesting frame rather than stretching the source into its padding.

Resource limits: expansion is capped at 64 megapixels per working image, with checked size arithmetic
and fallible reservation. If expansion cannot be allocated, rendering retains the previous clipped
bounds and logs a warning. Temporal samples are limited to the requesting frame's working bounds;
computing a union of bounds over Echo's sample times is not implemented here.

Regression coverage lives in `src/tests.rs` (`drop_shadow_*`) and `src/effect_bounds.rs`: all four
directions, normal and Shadow Only rendering, blur beyond the translated frame, full/half/quarter
resolution, source alignment, frame-plan fallback parity, masks, following effects, stacked shadows,
keyframed distance, temporal resampling, Motion rotation, disabled effects and hostile parameters.

Run:

```sh
cargo test -p filmcraft-render --lib
cargo clippy -p filmcraft-render --all-targets -- -D warnings
```

These are focused checks; the repository-wide gates remain `cargo xtask ci`.

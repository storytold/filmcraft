# ADR 0002: `unsafe` SIMD intrinsics in `crates/simd`, and nowhere else

- **Status:** proposed (2026-10-25), for maintainer review together with the
  [AGENTS.md](../../AGENTS.md) §0 item 3 change it needs
- **Supersedes:** nothing. [ADR 0001](0001-platform-ffi.md) stays as it is; it said anything else
  that wants `unsafe` needs a new decision, and this is that decision.

## Context

FilmCraft is pure Rust and the workspace sets `unsafe_code = "forbid"`. The clean-room decoders
(H.264, HEVC, VP8, VP9, AV1) spend most of their time in a few loops: motion compensation, inverse
transforms, deblocking, CDEF and sample adaptive offset. Making those loops competitive with the
reference decoders takes 128-bit vector instructions (NEON on aarch64, SSE2 on x86_64) for shuffles,
table lookups, saturating narrows and register transposes, which the compiler does not produce from
scalar or array code on its own.

In Rust those instructions are the `core::arch` intrinsics. Two things about them are `unsafe`:

1. **Calling a `#[target_feature]` function** from code compiled without that feature.
2. **Vector loads and stores**, which take raw pointers.

The crate `crates/simd` (`filmcraft-simd`, layer L0) already holds these kernels, with
`unsafe_code = "deny"` and per-module opt-ins. But AGENTS.md still names `crates/platform` as the
single exception, and no decision record covers this crate. This ADR closes that gap.

### What the kernels buy

Every number below is per frame on an M4 Pro, single thread, bit-exact before and after:

| Codec | Change | Before | After |
|---|---|---|---|
| AV1 1080p | fused motion-compensation kernel | 24.2 M cycles | 22.6 M cycles |
| AV1 1080p | 16-bit transform lanes plus register transposes | 19.5 M cycles | 19.2 M cycles |
| H.264 | CABAC and chroma kernels | | 80% of ffmpeg's speed |

The VP9, HEVC and VP8 decoders use kernels of the same kind for their hot loops.

### Alternatives considered

- **Safe code the compiler vectorises.** This works for lane-wise arithmetic, and the AV1 transforms
  use it (`[i16; 8]` lanes in `filmcraft-av1`, no `unsafe`). It does not give the kernels' data
  movement: in-register transposes, `vext` windows, table lookups and narrowing stores. Measured on
  the AV1 transforms, the gather and scatter around the safe lanes cost as much as the arithmetic.
- **`std::simd`.** Nightly only. The workspace builds on stable.
- **Third-party safe wrappers.** For example `safe_arch`. They cover x86 only, add a dependency,
  and still need `unsafe` underneath, so they move it rather than remove it.
- **Inline or external assembly.** More `unsafe`, not less, and harder to review and test.

## Decision

Allow `unsafe` in exactly one more crate, `crates/simd` (`filmcraft-simd`), for one job: running
hand-written SIMD kernels on the instruction sets every core of a target has. No other crate gains
`unsafe`. The codec crates call the safe API.

### Containment rules

1. **Lints.** The crate's `[lints]` table sets `unsafe_code = "deny"` (not `forbid`) and
   `clippy::undocumented_unsafe_blocks = "deny"`.
   - Only the per-architecture modules carry `#[allow(unsafe_code)]`: the `*_neon` modules and
     `neon` on aarch64, and `sse2` on x86_64.
   - The shared, scalar and test modules have no `unsafe`.
2. **Two kinds of `unsafe`, nothing else.**
   - **Entering a kernel.** A safe wrapper calls a `#[target_feature(enable = "neon")]` or
     `"sse2"` function. This is sound because NEON is part of the aarch64 baseline and SSE2 part of
     the x86_64 baseline: the code never runs on a core without them, so there is no runtime
     detection and no code path that only some machines run.
   - **Loads and stores at a computed offset.** The kernel's safe entry point checks the slice
     bounds once, up front, for every access the kernel will make, and returns `false` (writing
     nothing) when they don't fit. Each access then carries a `debug_assert!` of its bound.

   Nothing else: no FFI, no `transmute`, no uninitialised memory, no raw-pointer data structures,
   no unchecked indexing outside a vector access.
3. **Whole-array copies are safe code.** A load or store whose memory is a whole fixed-size array
   (a row the caller sized with `first_chunk`, a local table or a temporary) uses
   `bytemuck::must_cast`, so its size is checked at compile time. This removed 37 of the crate's
   119 `unsafe` blocks with no cycle cost (9-run measurements on every codec, all within ±0.3%),
   and review found two more. A constant table is cast once, into a `static`: casting the CDEF
   weights on every call measured 2% slower on AV1. What remains is 39 kernel entries and 41
   offset accesses.
4. **Safety comments.** Every `unsafe` block has a `// SAFETY:` comment naming the invariant that
   makes it sound: either the baseline feature, or the entry-point check that covers the access.
5. **The public API is safe.**
   - No `pub unsafe fn`, and no raw pointers or vector types in public signatures.
   - A kernel that cannot run (wrong level, unsupported size, inputs that don't fit) returns
     `false` and leaves its outputs untouched; the caller runs its scalar version.
   - Nothing panics on any input, under the never-crash rules every clean crate follows.
6. **Every kernel has a scalar twin and a level parameter.**
   - `kernel_at(level, …)` runs any level this build has, and `FILMCRAFT_SIMD=scalar` forces scalar
     at run time.
   - wasm32 and other targets run the scalar code.

### Verification required for a kernel to land or change

- **Bit-identical to its reference** on every level the build has: the scalar twin, or a direct
  model of the specification's formula. Tests iterate `levels()` and assert that the vector path
  actually ran on aarch64, so a kernel that silently declines everything fails.
- **Decline tests.** Inputs one sample short of what the kernel reads or writes return `false` and
  leave the outputs untouched.
- **Codec level.** Bit-exact against the reference decoders (dav1d / ffmpeg checksums) at 1 and 12
  threads, at 1080p and 4K, plus each codec's conformance suite. This runs with and without default
  features, under `FILMCRAFT_SIMD=scalar`, and for `x86_64-apple-darwin` so the SSE2 path is built
  and tested on Apple-silicon machines.
- **Mutation testing.** For a new or changed kernel, inject plausible faults and show a test fails
  for each: a wrong rounding constant, swapped lanes or halves, a bound off by one, a missing
  saturation or clamp. A surviving mutation is a test gap, closed before the change lands. For
  example, the AV1 transforms' saturating Hadamard needed a dedicated test because no stream
  reached the clamp.
- **Performance.** A kernel or a safety refactor stays only if it holds or lowers cycles in
  alternating runs against the commit before it. A regression of about 1% or more is reverted.

## Consequences

- **The decoders keep their vector speed**, and the crate's `unsafe` surface is small and countable:
  80 blocks of two kinds, each with its invariant written next to it.
- **A review burden.** Changes to the per-architecture modules of `crates/simd` need the scrutiny
  of `unsafe` code. A new kind of `unsafe` there (anything beyond the two above), or `unsafe`
  anywhere else, needs a new decision, not an extension of this one.
- **Every public kernel has a level-by-level test.** Writing this record found three that had
  none (`hevc_sao_band8`, `hevc_sao_edge8`, `vp8_predict`), covered only end to end by the codec
  suites. They now have spec-model and decline tests in `crates/simd/src/tests.rs`, and seven
  mutations of their vector code each fail them.
- **Follow-up:** six kernels (`hevc_deblock_luma8`, `hevc_idct_add8`, `hevc_dc_add8`,
  `vp8_idct_add`, `vp8_loop_filter`, `vp8_loop_filter_uv`) are tested level by level in their codec
  crates rather than in `filmcraft-simd`. That meets the rules above, but simd-level tests would put
  every kernel's guard in one place.

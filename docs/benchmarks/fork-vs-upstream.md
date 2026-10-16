# filmcraft: fork vs upstream benchmark

- Fork: yishengjiang99/filmcraft main @ 4d2efb40f2881e485d764a5d69e4dbb80d074062
- Parent: storytold/filmcraft. Fork point (merge-base): cee1feb185e675a108f78067f71ad86a640ace3d. Upstream main is now at cee1feb185e675a108f78067f71ad86a640ace3d.
- Machine: Intel Xeon (KVM), 8 vCPU, 15 GiB RAM, x86_64 Linux. Toolchain: Rust 1.95.0 via rustup. Node v20.19.2 runs WASI.

## Changes since the fork (5 commits, 8 files, +473/-14)
- **Perf:** wasm SIMD is now on for web builds (`+simd128` and `wasm-opt --enable-simd`), in xtask. A `wide` f32x4 version of the YUV→RGBA display row conversion was added; it is compiled only for wasm32 and for tests. The web frame-pump budget now adapts while playing: predicted_dt minus the last UI pass minus 2 ms, clamped to 4–24 ms.
- **Features/tooling:** smoke.mjs now records rAF frame times, long tasks and perf.stats during playback. Added docs/web-optimization-plan.md. Declared the `fc_wasm_threads` cfg.
- **Fixes:** none.

## Method
`main.rs` builds a 1920×1080 Rec.709 limited-range 8-bit YUV frame from a fixed LCG seed, in 4:2:0 and 4:4:4. It runs `VideoFrame::to_rgba8()` 3 times as warm-up, then times 20 iterations on native or 10 on wasm. The reported value is ms per frame. Each variant runs N times in sequence (`bench.sh` default N=7; the run below used N=9) and we report the median, min and max. A position-weighted checksum of the output checks that results are bit-identical.

Variants:
- **native-up / native-fork:** x86_64 release build. The native code path is unchanged in the fork, so this is a control.
- **wasm-up-nosimd:** upstream as its own xtask builds it, with no simd128.
- **wasm-up:** upstream built with `+simd128`. Same flags as the fork, so this isolates the code change.
- **wasm-fork:** fork built with `+simd128`.

wasm runs in Node's V8 through WASI, single-threaded. In the browser build rayon also runs single-threaded.

Commands:
```
./run.sh /workspace/fc fork; ./run.sh /workspace/fc-upstream up
WASM_RUSTFLAGS="" ./run.sh /workspace/fc-upstream up-nosimd
N=9 ./bench.sh
(cd <checkout> && cargo test -q -p filmcraft-frame)
```

## Results: to_rgba8, 1080p, ms per frame (lower is better)
| variant | 4:2:0 median [min–max] | 4:4:4 median [min–max] |
|---|---|---|
| native-up | 4.33 [4.21–5.22] | 4.25 [4.09–4.62] |
| native-fork | 4.45 [4.13–4.87] | 4.34 [4.06–4.73] |
| wasm-up-nosimd (upstream as shipped) | 71.15 [65.16–74.01] | 70.58 [68.03–76.20] |
| wasm-up (+simd128) | 66.05 [63.89–77.20] | 68.68 [66.38–72.87] |
| wasm-fork (+simd128) | 57.20 [55.51–61.36] | 62.09 [57.38–70.31] |

What the numbers mean:
- **Native:** no measurable difference. The ranges overlap, which is expected because the SIMD path is not compiled for native.
- **wasm, fork vs upstream as shipped:** about 20% faster for 4:2:0 (71.2→57.2 ms) and about 12% faster for 4:4:4 (70.6→62.1 ms).
- **Where that comes from:** turning on simd128 alone gives about 3–7%. The `wide` kernel adds about 13% (4:2:0) and about 10% (4:4:4) on top of that. The 4:4:4 ranges overlap somewhat.
- **Correctness:** checksums are identical across all 5 variants, so the output is bit-identical.
- **Tests:** filmcraft-frame passes 13/13 on the fork, including the new parity test, and 12/12 on upstream. No failures.

## Not benchmarked
- **Adaptive frame-pump budget:** needs a real browser and display (rAF, predicted_dt). The fork's smoke.mjs collects this, but no Chrome/CDP session was run here.
- **Web bundle size and startup:** a full `cargo xtask web` needs wasm-bindgen plus a long build, and was skipped.
- **Full workspace test suite:** only the changed crate (frame) was tested.
- **Native-only crates:** unchanged in the fork.

## Rerun 2026-10-13 21:21 PT: fork `2086529` vs upstream `cee1feb`

Same scripts (`run.sh` for up / up-nosimd / fork, then `bench.sh`) but **N=7** (the `bench.sh` default) instead of 9.
8 vCPU box, 1-min load 0.12 → 1.19 during the run. Checksums are identical across builds.

| variant | 4:2:0 median [min–max] ms | 4:4:4 median [min–max] ms |
|---|---|---|
| native-up | 4.58 [4.42–4.85] | 4.45 [4.17–4.77] |
| native-fork | 4.44 [4.21–4.64] | 4.17 [3.95–4.43] |
| wasm-up-nosimd | 72.30 [70.61–76.83] | 74.20 [71.80–80.83] |
| wasm-up | 70.81 [68.19–73.65] | 73.43 [71.67–76.53] |
| wasm-fork | 59.22 [57.76–63.07] | 62.46 [60.86–66.37] |

wasm fork vs upstream as shipped (SIMD off): **~18 % faster on 4:2:0** (72.3→59.2), **~16 % on 4:4:4** (74.2→62.5). Against upstream with SIMD on: ~16 % (70.8→59.2) and ~15 % (73.4→62.5).
The earlier run measured ~20 % and ~12 %. The ranges overlap with that run, so the SIMD gain still holds.
No conversion code changed between `53d7248` and `2086529`.

## Apple M4 Pro rerun (fork 53d0def vs upstream cee1feb)

Run by the maintainer on a MacBook Pro (Apple M4 Pro, macOS Darwin 27.0.0, Node v22.22.0, rustc 1.99.0), `N=9 ./bench.sh`, 2026-10-13. Checksums are identical across all builds.

| variant | 4:2:0 median [min–max] ms | 4:4:4 median [min–max] ms |
|---|---|---|
| native-up | 0.56 [0.54–0.66] | 0.57 [0.54–0.61] |
| native-fork | 0.53 [0.52–1.04] | 0.62 [0.50–0.69] |
| wasm-up-nosimd (as shipped) | 13.71 [13.68–13.77] | 13.75 [13.69–13.84] |
| wasm-up (simd128 flag) | 13.77 [13.71–14.05] | 13.80 [13.69–13.86] |
| wasm-fork | 12.25 [12.15–12.49] | 12.05 [11.95–12.14] |

On M4 Pro the fork's wasm conversion is ~11% faster for 4:2:0 and ~12% for 4:4:4 than upstream as shipped. The simd128 flag alone does nothing for upstream's code, so the gain comes from the fork's conversion code. Native is unchanged (control).

# Settled stereo reverb and mixer automation performance

This change is split from rotoscope PR #2122. It contains only audio DSP and mixer
optimizations plus correctness coverage; no model, editor, format or dependency changes.

- Reverb specializes a fully settled two-channel block. All smoothed parameters
  are checked before choosing that path. Automation and other channel counts use
  the general path. Sample arithmetic and the 32-sample gain-update grid are unchanged.
- Mixer lanes reuse their final keyframe value when the entire request lies after
  the last keyframe. Live overrides still evaluate sample by sample.

Correctness: the existing reverb reference test compares samples exactly across
parameter changes, variable block boundaries, tail decay and reset. Damping and
wet/dry automation are added to its size/decay/predelay cases. Mixer tests compare
whole versus partitioned requests, the final-keyframe boundary and live holds.

## Historical Colab comparison (October 16, 2026)

Eight-logical-thread Intel Xeon Broadwell 2.20 GHz, about 51 GiB RAM, Rust 1.95.
The unchanged benchmark uses 24 tracks, three inserts each and a compressed submix,
10 seconds of audio, on one core; it measures thread CPU time on Linux. Its release
floor remains greater than 4× realtime. No thresholds or workloads changed.

Both binaries used LTO disabled, 16 codegen units and `-C target-cpu=native`.
Shipping profile settings are unchanged. These three alternating comparisons
isolate **reverb specialization**; both versions already had mixer tail caching.
They must not be presented as the combined gain of both optimizations.

| Pair | Baseline CPU seconds | Candidate CPU seconds | Candidate realtime |
| --- | ---: | ---: | ---: |
| 1 | 2.426 | 2.224 | 4.50× |
| 2 (candidate first) | 2.426 | 2.201 | 4.54× |
| 3 | 2.426 | 2.225 | 4.49× |

Mean CPU time decreased 8.6%. Another Colab host measured the old baseline at
3.62–3.74×, so these results are host-specific, not a guarantee. This historical
receipt predates the split; the new PR records validation of its exact source.

## Split-branch Colab comparison (October 19, 2026)

The cleaned branch was compared with unmodified main `105c09bd` on another
8-logical-thread Xeon 2.20 GHz Colab CPU host, with Rust 1.95.0. Unlike the historical
comparison above, this measures both optimizations together against current main.

With LTO disabled, 16 codegen units and `-C target-cpu=native`:

| Pair | Baseline CPU seconds | Candidate CPU seconds |
| --- | ---: | ---: |
| 1 | 3.159 | 2.824 |
| 2 (candidate first) | 3.152 | 2.834 |
| 3 | 3.151 | 2.856 |

Mean CPU time decreased 10.0%. Baseline was 3.17× realtime and candidate was
3.50–3.54×: **both fail the unchanged greater-than-4× threshold on this host**.
The full workspace run also failed that floor (candidate 3.594 CPU seconds), so
this is evidence of an improvement, not a passing performance gate.

With the unmodified default release profile (thin LTO, one codegen unit and
portable CPU target), on the same host:

| Pair | Baseline CPU seconds | Candidate CPU seconds | Candidate realtime |
| --- | ---: | ---: | ---: |
| 1 | 3.052 | 2.390 | 4.18× |
| 2 (candidate first) | 3.021 | 2.357 | 4.24× |
| 3 | 3.072 | 2.919 | 3.43× |

Mean CPU time decreased 16.2%, with substantial candidate variability. Baseline
failed the unchanged floor in all three runs; candidate passed two and failed
one. These results do not establish a reliably passing gate on this shared host.
The sample-exact reverb reference test and all 24 mixer tests passed under this
profile too. The broader Colab workspace run had 3,554 passes, 45 existing ignored
tests, and two failures: the unchanged desktop MIME test and the throughput floor.
Strict workspace Clippy also stops on existing UI warnings. These unrelated
failures are documented in the draft; this change does not suppress them.

## Reproduce

Run baseline and candidate sequentially on the same idle host, alternating order:

```sh
cargo test --release -p filmcraft-render --test mixer_perf -- --nocapture
cargo test --release -p filmcraft-audio-dsp --lib
cargo test --release -p filmcraft-render --lib mixer_tests
```

Keep matching compiler/profile/CPU settings. Record host, flags, exact source and
both timings. Compare correctness independently of throughput.

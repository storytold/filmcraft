# Native rotoscope evidence

These observations were collected October 14–16, 2026, before the review cleanup.
They are not a new validation receipt for the cleaned branch.

## Numerical conformance

Conformance uses original PyTorch weights with the same raw RGBA/bilinear normalization,
without the former JPEG convenience adapter. The three-frame Fast comparison matched
all pixels on frames 0/2; frame 1 differed by three threshold pixels (IoU 0.999963).
Quality matched within one 16-bit step on frames 0/2; the differing threshold pixels on
frame 1 changed its trimap, with mean absolute alpha difference 0.709/65535 and maximum
4492/65535. A second case with multiple Include/Exclude points, a nonzero seed, backward
tracking, rectangular tiles and a brush matched within **one 16-bit alpha step** on all
three frames. These are numerical checks, not hair/occlusion ground-truth accuracy.

The native model package exported twice with identical graph hashes. The loader
pins exact graph lengths/hashes independently of its manifest. Local correction
checks preserved every soft-alpha pixel outside the brush; changing frame 1 of
three left frames 0 and 2 bit-identical. These compare implementations on a sample,
not hair or occlusion accuracy against ground truth.

## Editor acceptance and demo

The native editor exercised Include/Exclude previews, Option-click removal, undo
of removed/cleared points, a dragged brush as one undo step, Quality refinement,
Alpha/Composite views, generation, project persistence and cached playback.
The former Python worker was stopped during this native acceptance.

The 62.95-second 1920×1080 demo shows that workflow. The recorded 30-frame Quality
job took 118.96 seconds; the wait is trimmed and labeled. The movie is a PR
attachment, not a repository asset. Its source is Amada44's
[REM sleep of a dog](https://commons.wikimedia.org/wiki/File:REM_-_Rapid_eye_movement_sleep_of_a_dog.webm),
CC BY-SA 3.0. Changes include overlays, matting, compositing, trimming and captions;
the demo is distributed under CC BY-SA 3.0. FilmCraft UI is MIT OR Apache-2.0.

## Performance and limits

See [PERFORMANCE.md](PERFORMANCE.md) for the controlled Colab comparison of native
Quality scheduling: 93.613→77.648 seconds for 30 frames, byte-identical alpha,
875→1,400 MiB sampled peak RSS. Model loading and decoding are excluded from that
measurement. These are Linux CPU timings, not a new measurement of the Mac editor.

The T4 probe there used a separate research runtime on one tile; it is not a
shipped GPU backend or a full-clip GPU result. CPU-only inference, 720-pixel and
120-frame limits, frame-local strokes and difficult-occlusion quality remain open.

Detailed logs and generated examples remain outside Git in `target/rotoscope/`.
The old Python prototype is retained in commit `189c22d4` for historical reference,
not included in the current change. Audio measurements are now reviewed separately.

## Export cleanup check — October 19, Colab Linux

The reduced exporter and the pre-cleanup exporter at `189c22d4` produced identical
manifests and byte-identical files for all six graphs on the same Colab CPU with
the pinned Python dependencies. Five graphs also matched the committed package
pins. Both exporters failed verification of the tracking graph: positional
sine/cosine and folded positional constants differ from the existing package,
with maximum absolute difference 5.96e-8 in the inspected tensors. The committed
pins are unchanged. This is an inherited cross-platform export limitation, not a
successful reproduction of the pinned package; it remains open before merge.

## Review cleanup validation — October 19, Colab Linux

All builds, tests and inference checks ran remotely, with Rust 1.95.0. The
workspace run had 3,576 passes, four failures and 45 existing ignored tests.
Two catalog failures were fixed by validating the Russian/Ukrainian AI labels
against the full command registry; all 29 localization tests then passed.
Both upstream and rotoscope translations remain present in all 11 catalogs.

The inference-enabled engine run had 667 passes, one failure and three existing
ignored tests. Its unchanged hardware-decoder setting test failed while running
in parallel and passed three isolated reruns. All 10 native inference tests passed.
Three-frame Fast and Quality generation passed using the hash-verified original
model package; Quality contained 104,543 soft-alpha pixels across those frames,
and the cached Quality repeat was byte-identical. This is a smoke check, not
new ground-truth accuracy evidence.

Formatting, layering, all 69 asset attributions, WASM, offline Python lint/syntax
and launcher syntax passed. The full gate remains red: strict Clippy reports
four existing UI warnings also found on unmodified main; the workspace still
fails the unchanged desktop MIME test and audio throughput floor. Main also
fails that performance floor on this Colab host. The audio changes are reviewed
separately; unrelated lint/MIME fixes, threshold changes and new ignores are not
part of this draft. Source hashes are checked against the tested Colab trees.

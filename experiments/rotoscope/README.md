# Experimental native AI rotoscoping (#1733)

This opt-in editor workflow uses EdgeTAM for Fast selection/tracking and ViTMatte
Small for Quality alpha refinement. Inference runs in Rust through RTen 0.27.0 on
the CPU. The feature is off by default. The former Python browser app, editor
server and reference pipeline have been removed from this PR; Python here only
prepares the pinned model package and runs the native benchmark helper.

## Try it

Use a disposable copy of a project (see compatibility below). Install Rust 1.95,
Python 3.12 and `uv`, then run:

```sh
./experiments/rotoscope/run-editor.sh /absolute/path/to/test-copy.fcproj
```

Command access is disabled by default. For explicit local automation, pass
`--control 9876` to the launcher. This opens the editor's existing unauthenticated
loopback control endpoint, which accepts project commands from other local processes;
enable it only when needed for automation.

The first run downloads the Apache-2.0 checkpoints into ignored `target/rotoscope`,
verifies hashes, exports six ONNX graphs, and builds the feature-enabled editor.
Later runs verify and reuse that package. The initial export has a known platform
limitation: on Colab Linux, the tracking graph differs from the pinned package
in positional floating-point constants and verification refuses it. The original
and cleaned exporters produce identical output there; portability is unresolved.
The loader must continue rejecting unmatched graphs. Model weights, footage and generated
media are never committed. Model revisions, licensing, budgets and native oracle
instructions are in [the crate README](../../crates/rotoscope/README.md).

Select a video clip, open Opacity, and choose **Select object**. Include clicks
preview a green overlay; Exclude additionally shows red. Option/Alt-click a dot
removes it. Point edits, Clear points and brush gestures support undo/redo.
Choose Fast or Quality and Generate for up to the first 120 source frames.
Refine Edge paints unknown pixels for the matting model; it does not assign alpha
by brush strength. Release a stroke to infer soft alpha. Edge region, Alpha and
Composite reuse that result. Strokes affect their painted frame only.

## Review boundaries

- Source-timed raster masks: `crates/project`, `crates/format`, CPU/GPU rendering.
- Native inference: `crates/rotoscope`, optional RTen dependencies and pinned graphs.
- Editor integration: `crates/engine/src/ai_masks*`, history snapshots and Opacity UI.
- Offline preparation: the small exporter/downloader in this directory.

The audio optimizations and unrelated lint/desktop-entry fixes are excluded.
EdgeTAM remains the default. SAM 3 is a possible separately reviewed, optional
follow-up, pending a controlled runtime, memory and download-size comparison.

## Project compatibility — proposed, requires maintainer approval

This draft proposes schema 16 for stored raster alpha. Loading schemas 1–15 uses
the existing migration chain; the new 15→16 step is a no-op because absent raster
fields default to `None`. Loading alone does not rewrite the file. Saving writes
schema 16 **even when no AI mask is present**, including in a default-feature build.
This broad compatibility impact is a maintainer decision, not an approved policy.

Builds supporting only schema 15 reject the new envelope rather than silently lose
alpha. There is no supported downgrade converter. For rollback, keep the original
schema-15 project untouched and save the experiment to a separate copy; reopen the
original in the older editor. Undo removes a generated mask but does not downgrade
the save format. Do not manually lower the schema number: older builds would drop
the new fields. A capability-based writer or explicit loss-aware conversion would
need a separate design and tests before changing this policy.

Maintainers also need to approve the optional RTen runtime/dependency arrangement.
These design decisions remain pending review. This prototype does not claim production-ready
hair/occlusion quality, GPU inference, temporal alpha smoothing or 4K support.

## Evidence

[RESULTS.md](RESULTS.md) records native conformance and demo provenance.
[PERFORMANCE.md](PERFORMANCE.md) records the historical Colab CPU measurements,
including the increased RAM required by parallel Quality tiles. New validation
receipts belong to the exact cleaned PR head, separately from those older results.

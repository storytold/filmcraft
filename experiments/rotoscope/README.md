# AI rotoscoping experiment — issue #1733

Runnable local test of [the proposal](https://github.com/storytold/filmcraft/issues/1733):
EdgeTAM object selection and bidirectional video tracking, then optional ViTMatte Small
alpha refinement. The opt-in editor runs both models in process through the pure-Rust
`filmcraft-rotoscope` crate and RTen. The standalone Python reference remains an oracle.

## Test inside the native editor

```sh
./experiments/rotoscope/run-editor.sh
```

Requires Rust 1.95, Python 3.12 and uv for initial model preparation. The launcher verifies
pinned checkpoints and exports six ONNX graphs once, then builds with
`--features experimental-ai-rotoscope` and opens FilmCraft with isolated preferences and
recovery data under `target/rotoscope/editor-data`. It starts no model server. The editor
passes its decoded RGBA frames directly to Rust; inference needs neither Python nor
PyTorch at runtime. The default Rust toolchain is unchanged.

This native runtime currently uses the CPU. It does not retain the Python reference's
Metal refinement acceleration; a Rust port alone is not a speed improvement. Image
features, tracked masks and per-frame Quality alpha have bounded in-memory caches (two
feature states within 192 MiB, four tracking results within 40 MB, 120 alpha frames within
80 MB). A brush correction recomputes only frames whose trimap changed. Cancelled jobs
publish no new alpha frames. Four inference threads by default leave cores available to
decoding and UI; `FILMCRAFT_AI_ROTO_THREADS` can select a bounded thread count. See
`RESULTS.md` for measured timings and the numerical comparison.

1. Import a video, place it in a sequence, select its timeline clip and open **Effect Controls**.
2. Expand **Opacity**, choose **Select object**, and click the object in the Program monitor.
   **Include** and **Exclude** add foreground/background points and automatically show a
   translucent green overlay on included areas of the original clicked frame. Select
   **Exclude** to also show excluded areas in red; switching back to **Include** restores
   green only and leaves the background untinted. This display switch reuses cached alpha
   without running inference. Additional point clicks refresh the preview; rapid clicks cancel the previous
   request. The preview never alters the project or undo stack; point edits do have
   transient undo steps. Use a frame
   within the first **Frames** source frames of the clip; the default is 30.
   **Option-click** (Mac) or **Alt-click** (Windows/Linux) a dot to remove just that
   point; **Cmd-click/Ctrl-click** also works, following Premiere's control-point
   deletion convention. Option/Alt-click away from a dot temporarily adds an Exclude
   point, following After Effects' subtract convention. Removing a point refreshes the
   preview; removing the current frame's last Include point clears the overlay.
   **Clear points** removes the entire draft. **Cmd/Ctrl-Z** restores deleted/cleared
   points; **Cmd/Ctrl-Shift-Z** redoes the edit. Point edits share chronological history
   with timeline edits, without dirtying the project.
3. Select **Fast** or **Quality**, then **Generate**. Fast runs EdgeTAM; Quality also runs
   ViTMatte. **Cancel** discards the job. Progress appears in the editor's job list.
4. Scrub/play the timeline. The alpha is cached inside the project, so playback does not
   run a model. Regenerating replaces the previous AI mask as one undo step and preserves
   manual masks. Save/reopen retains the alpha without the inference worker.
5. For a correction, choose **Refine Edge**, paint on the displayed frame, then regenerate
   Quality. Refine Edge automatically switches to Quality and runs ViTMatte when a stroke
   ends. Paint across mixed foreground/background details such as hair: the stroke marks
   unknown pixels for the model, and does not fill opacity. **Edge region** shows inferred
   grayscale detail over the footage; **Alpha** shows the full black/white/gray matte;
   **Composite** shows the cutout. Each drag is one undo step. A generated frame can be
   refined without adding another Include point; its alpha outside the brush stays exact.
   Strokes currently affect only their painted frame.

Native inference is limited to 720 pixels on the longest side and 120 frames (with a
40-million-pixel aggregate budget). Unprocessed source frames remain visible. Opacity
and inversion use the existing mask controls; path/feather/expansion do not apply to
cached raster alpha. At most 16 processed frames can have Include/Exclude prompts.
Completed alpha remains in saved projects and plays through the existing GPU renderer.
Points are a session draft, so reopening requires new prompts to regenerate.

The feature is experimental and disabled in standard builds. With prepared models, run:

```sh
FILMCRAFT_AI_ROTO_MODELS="$PWD/target/rotoscope/models/native" \
  target/debug/filmcraft --control 9876 target/rotoscope/FilmCraft-AI-Test.fcproj
```

The directory is a verified local package; the Rust loader pins graph hashes as well as
original checkpoints. Missing, truncated or altered files return an error. New projects
use schema 16; older FilmCraft builds refuse them instead of losing their masks. Inference
is excluded from WebAssembly. For reference-only comparisons, explicitly set
`FILMCRAFT_AI_ROTO_BACKEND=python-reference` and `FILMCRAFT_AI_ROTO_ENDPOINT=127.0.0.1:7871`
and start `editor_worker.py`. There is no automatic Python fallback. Rust GPU inference,
temporal alpha smoothing, optical-flow brush propagation, dedicated GPU alpha kernels and
longer/higher-resolution jobs remain future work.

## Acceptance checks, defined before implementation

- Run the actual pinned model weights on Apple Silicon Metal, with an explicit CPU option.
- Add foreground/background points at any loaded frame; accept a bounding box in the CLI.
- Track forward and backward from the prompt, producing a mask for every loaded frame.
- In Quality mode, build a foreground/unknown/background trimap and infer continuous alpha
  in padded tiles. A brush expands the unknown region on its painted frame.
- Preserve definite foreground/background exactly and keep output alpha finite in [0, 1].
- Cache completed results by source content, prompts, frame limit, settings, and model identity.
- Export lossless alpha PNGs, transparent PNGs, browser-playable previews and timings.
- Reject invalid prompts, empty/unreadable media, missing models, and excessive allocations
  with an actionable error. Cancellation must leave no completed cache entry.
- Exercise the browser workflow and save inspectable inference results.

## Run

From the repository root:

```sh
./experiments/rotoscope/run.sh
```

The first run creates an isolated environment under `target/rotoscope/`, installs the
reference libraries and downloads the pinned models and an attributed sample. Subsequent
runs reuse them. The UI is served only at **http://127.0.0.1:7860**. No clips are uploaded
to a remote inference service. Internet is needed only for the initial dependencies,
weights and sample. Python 3.12, uv, git, ffmpeg and ffprobe are required; ffmpeg is an
external test tool, never linked into or bundled with FilmCraft.
Use `./experiments/rotoscope/run.sh --setup` to repair missing cached models or rerun
download verification explicitly.

1. Use the sample or upload a short clip, then select **Load clip**.
2. Choose a frame. Click the object with **Foreground** selected; use **Background** to
   exclude areas. Changing frame retains the prompts on their original frames.
3. Select **Fast** or **Quality** and choose **Track clip**.
4. Scrub the cached results, watch the previews, and download the PNG archive.
5. Optionally paint the Refine Edge editor on a frame, save the brush, and rerun Quality.

The demo initially limits clips to 30 frames and 720 pixels on the longest side. These
controls are explicit. Changing them invalidates the loaded clip. Up to 120 frames and
3840 pixels are accepted, subject to the memory cap. Preview videos have a checkerboard
or solid background; the PNG archive contains the actual alpha channel.

## Headless reproducible test

```sh
target/rotoscope/.venv/bin/python experiments/rotoscope/cli.py \
  --video target/rotoscope/samples/dog.webm \
  --point 640,360,1 --mode quality --max-frames 12 --max-side 720
target/rotoscope/.venv/bin/python -m pytest experiments/rotoscope/tests -q
```

CLI points are in the **original video pixel coordinates** (scaled with the frames).
Use `--seed-frame` for a frame other than zero, `--box x0,y0,x1,y1`, and `--device cpu`
to measure fallback. `--mode fast` does not load ViTMatte. The UI uses loaded-frame
coordinates directly. Benchmark JSON reports actual device, cold setup, per-frame
tracking/refinement times, memory snapshots, cache status and model identities.

## Boundaries of this test

- Both paths are experimental. Rust model inference is implemented, but standard builds
  do not include it and native GPU inference is not implemented.
- EdgeTAM internally encodes at 1024 square pixels even when the source is 1080p/4K.
  Resolution controls affect alpha output and ViTMatte, not encoder resolution.
- Refinement is per-frame. Brush regions apply only on their painted frames; optical-flow
  propagation and temporal alpha regularization are not implemented. Frame differences
  are diagnostics, not a ground-truth accuracy or flicker score.
- Tiled matting is experimental: context loss and seams must be inspected. Definite
  trimap pixels remain locked. Wide bands help hair but can expose tracking mistakes.
- Tests on this Mac do not prove NVIDIA performance, 4K quality, difficult occlusions,
  real-time inference or a dedicated GPU alpha implementation.

## FilmCraft integration evidence

At base commit `cee1feb185e675a108f78067f71ad86a640ace3d`, `MaskPath` holds vector
vertices (`crates/project`), `filmcraft_render::mask::coverage` rasterizes those paths,
and `filmcraft_gpu::mask::GpuMask` takes `FlatMask` polygon geometry. Generated raster
alpha needs a typed cached-mask input, not conversion into an approximate vector path.
The CPU opacity path multiplies premultiplied RGBA by coverage in
`crates/render/src/mask.rs::scale_by`. The native prototype adds a typed, bounded RLE
raster representation at exact source-frame times. The existing GPU plan falls back
to CPU layer preparation for opacity masks. Selection and corrections are engine
commands; inference results commit atomically through undo after validating the
result and checking the clip has not changed.

## Models and sample

- [EdgeTAM](https://github.com/facebookresearch/EdgeTAM), Meta, Apache-2.0 code/weights.
  Upstream code and checkpoint identities are pinned in `models.py`.
- [ViTMatte Small](https://huggingface.co/hustvl/vitmatte-small-distinctions-646), HUST,
  Apache-2.0 checkpoint; inference from the Apache-2.0 Transformers implementation.
- [Sleeping dog](https://commons.wikimedia.org/wiki/File:REM_-_Rapid_eye_movement_sleep_of_a_dog.webm),
  Amada44, CC BY-SA 3.0. The sample and generated derivatives remain local under
  `target/rotoscope/`; sidecars preserve author, license, source and modifications.

No model weights, videos or generated images are committed. See `RESULTS.md` for
the measured results and remaining work.

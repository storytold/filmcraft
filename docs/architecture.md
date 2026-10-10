# Architecture

FilmCraft is a Cargo workspace of small crates with strictly enforced layering. The engine is
headless: every feature can be reached without a window, and the egui UI is one client among the
CLI, the JSON control channel and the MCP server.

Design principles:

1. **Engine-first.** Project-changing actions go through `Session::execute(id, params)`.
2. **Everything is a command.** Stable id, label, menu path, shortcut, parameter doc, `enabled()`
   predicate with a human-readable reason, and `run()`.
3. **Exact time.** Integer ticks, rational frame rates. No `f64` seconds in edit math.
4. **Copy-on-write snapshots.** The project is an `Arc<Project>`. Undo is a stack of snapshots, and
   background readers (playback, export) hold a snapshot without locking.
5. **CPU reference, GPU fast path.** The CPU compositor is the oracle. The GPU path is tested
   against it.
6. **Pure Rust, clean-room.** Codecs and containers are written from public specifications
   (see [AGENTS.md](../AGENTS.md)).

## 1. Layers

```text
 L6  apps/filmcraft · apps/filmcraft-cli · apps/filmcraft-web
 L5  ui-egui · automation · platform
 L4  engine
 L3  render · gpu · export · golden (test-only)
 L2  edit · codecs · interchange · captions · speech
 L1  frame · media · project · audio-dsp · text
 L0  foundation: time · geom · color · bitstream · testkit (dev-dependency only)
     codecs/containers: isobmff · matroska · mxf · cfb · mpegts · ogg · h264 · h264enc · hevc · vp9 · av1 · mpeg2v · prores · dnx · apv · aac · ac3 · opus
```

Crates are named `filmcraft-<dir>` (`crates/time` is `filmcraft-time`). The apps are `filmcraft`
and `filmcraft-cli`.

| Crate | Layer | Purpose |
|---|---|---|
| `time` | L0 | `Tick`, `FrameRate`, `TimeRange`, timecode parse/format (NDF/DF, frames, feet+frames, samples) |
| `geom` | L0 | `Vec2`, `Rect`, `Affine`, Motion-transform composition |
| `color` | L0 | colour spaces, transfer functions, YUV↔RGB matrices, LUTs |
| `bitstream` | L0 | bit reader/writer, Exp-Golomb, emulation prevention |
| `isobmff` | L0 | MP4/MOV demux and mux |
| `matroska` | L0 | MKV/WebM demux |
| `mxf` | L0 | MXF demux and mux (SMPTE ST 377-1): OP1a / OP-Atom, index tables, AVC / VC-3 / ProRes / MPEG-2 identification, PCM / AES3 sound, timecode; writer for OP1a / OP-Atom with VC-3, ProRes, AVC and PCM ([README](../crates/mxf/README.md)) |
| `cfb` | L0 | Compound File Binary ([MS-CFB] structured storage) reader and writer, the container of AAF ([README](../crates/cfb/README.md)) |
| `mpegts` | L0 | MPEG-2 Systems demux (H.222.0): transport streams (188 / 192-byte BDAV / 204), program streams (MPEG-2 / MPEG-1, VOB, MOD), PSI, PES, PTS / PCR, access-unit index built on open ([README](../crates/mpegts/README.md)) |
| `ogg` | L0 | Ogg demux (RFC 3533), Ogg Opus timing (RFC 7845 granules, pre-skip, end trimming), Vorbis headers |
| `h264`, `h264enc` | L0 | H.264 decoder; H.264 encoder |
| `hevc` | L0 | H.265 Main/Main 10 decoder |
| `prores` | L0 | ProRes decoder and encoder |
| `dnx` | L0 | DNxHD / DNxHR (SMPTE ST 2019-1 VC-3) decoder and DNxHR encoder |
| `apv` | L0 | Advanced Professional Video (IETF RFC 9924) decoder and encoder ([README](../crates/apv/README.md)) |
| `av1` | L0 | AV1 decoder (Main profile; bit-exact with libdav1d; see its README for the stage table) |
| `mpeg2v` | L0 | MPEG-2 video (Main / 4:2:2 profile; frame and field pictures, dual prime) and MPEG-1 video decoder ([README](../crates/mpeg2v/README.md)) |
| `aac` | L0 | AAC-LC decoder and encoder |
| `ac3` | L0 | AC-3 (ATSC A/52) decoder ([README](../crates/ac3/README.md)) |
| `testkit` | L0 | test-only helpers, used only as a dev-dependency: ffmpeg/ffprobe discovery, fixture dirs, golden images ([testing.md](testing.md)) |
| `frame` | L1 | `VideoFrame` (planar YUV / RGBA8 / linear RGBA f32, colour metadata), `AudioBuffer` |
| `media` | L1 | `MediaSource` trait, probing/openers, frame cache, generators, stills, image sequences, WAV / Broadcast WAV |
| `project` | L1 | document model, effect definitions, keyframes |
| `audio-dsp` | L1 | loudness metering (BS.1770 / R128), audio effects, channel layouts (BS.775 up/downmix, 5.1 panner), music remix analysis; no dependencies |
| `text` | L1 | text engine: font database (bundled OFL fonts + system fonts), shaping (harfrust), bidi, line breaking, paragraph layout, glyph/path rasteriser, strokes ([crates/text/README.md](../crates/text/README.md)) |
| `edit` | L2 | pure edit algebra (insert, overwrite, razor, ripple, roll, slip, slide, rate stretch…; text-based editing: `edit::transcript`) |
| `speech` | L2 | speech-to-text: `Transcriber` trait, Whisper model catalogue + verified downloader (feature `download`), pure-Rust Whisper inference (faer matrix products + own CPU kernels) with word timestamps (feature `whisper`), safetensors / CTranslate2 weight readers, speaker labelling ([transcripts.md](transcripts.md)) |
| `codecs` | L2 | container + codec hub: MP4/MOV, MKV, MXF, Ogg and MPEG TS / PS / video elementary stream sources, GOP-aware seeking, decoder registry, audio decoding |
| `interchange` | L2 | EDL, FCP7 XML, FCPXML, OTIO, AAF (on `cfb`) and OMF 2.0 import/export (no file I/O; the engine supplies rendered audio essence) ([README](../crates/interchange/README.md)) |
| `render` | L3 | sequence evaluation, CPU compositor, video effects (`effects`, `vfx`; effects needing other frames or tracks read them through `vfx::FxEnv`), transitions, audio mix |
| `gpu` | L3 | wgpu compositor (WGSL) |
| `golden` | L3 | test-only: golden-image tests of the CPU renderer and GPU-vs-CPU parity; empty library, dev-dependencies only |
| `scopes` | L3 | video scope maths: waveform, parade, histogram, YUV / HLS vectorscopes, numeric summaries ([crates/scopes/README.md](../crates/scopes/README.md)) |
| `export` | L3 | render → encode → mux pipeline, progress/cancel |
| `engine` | L4 | `Session`, command registry, undo history, media pool, jobs, interchange glue |
| `ui-egui` | L5 | the egui frontend: docking, panels, timeline, monitors, playback, control-channel handlers |
| `automation` | L5 | MCP server (`rmcp`, stdio), headless or bridged to the running app |
| `platform` | L5 | OS media FFI only: hardware video decoding (VideoToolbox H.264 / HEVC on macOS; Media Foundation / Direct3D 11 H.264 / HEVC / VP9 / AV1 on Windows; VA-API H.264 / HEVC on Linux, with our own parsers and DPBs driving the stateless hardware; a no-op elsewhere) behind `codecs::VideoDecoder`, with transparent fallback to our decoders, and hardware H.264 encoding (VideoToolbox, opt-in) and H.265 encoding (VideoToolbox, the only H.265 encoder; the format exists only where a hardware encoder does) and NVIDIA NVENC H.264 (Windows and 64-bit Linux, opt-in) and H.265 (Windows, FilmCraft's only H.265 encoder there) encoding behind `export::VideoEncoder`; H.264 declines to the built-in encoder for what the hardware does not take. The one crate allowed `unsafe` ([ADR 0001](adr/0001-platform-ffi.md), [README](../crates/platform/README.md)) |
| `filmcraft` | L6 | desktop binary: eframe/wgpu window, cpal audio output, file dialogs, native macOS menu, TCP control server |
| `filmcraft-cli` | L6 | headless CLI: `exec`, `run`, `inspect`, `describe`, `commands`, `import`, `export`, `render`, `probe`, `mcp`; `--bridge` targets the running app |
| `filmcraft-web` | L6 | the browser app (wasm32): eframe web runner on WebGPU/WebGL2, Blob-backed services, OPFS recovery, WebAudio, WebCodecs, `window.filmcraft` API ([web.md](web.md)) |

### What `cargo xtask layers` enforces

The table of layers lives in `xtask/src/main.rs` (`LAYERS`). It also reserves names for planned
crates. The check reads `cargo metadata` and looks at normal and build dependencies (dev-dependencies
are exempt):

| Rule | Detail |
|---|---|
| Every crate has a layer | A new crate fails the check until it is added to `LAYERS`. |
| Only downward edges | A crate may not depend on a crate in a higher layer. |
| Same-layer edges are listed | From L1 up, a same-layer edge must be in `SAME_LAYER`: `media→frame`, `project→media`, `project→frame`, `gpu→render`, `export→render`, `cli→filmcraft`, plus a few reserved for planned crates. |
| L0 codecs stay standalone | L0 crates other than `time`, `geom`, `color`, `bitstream`, `testkit` may depend on no workspace crate except `filmcraft-bitstream`. External crates such as `thiserror` and `rayon` are allowed. |
| No UI/OS crates below L5 | `egui`, `eframe`, `egui-wgpu`, `winit`, `rfd`, `cpal`, `muda` are allowed only in L5 and L6. |

`cargo xtask wasm` runs `cargo check --target wasm32-unknown-unknown` on every L0–L4 crate, the
egui UI and the web app, so everything up to the engine stays web-portable and the web app builds
([web.md](web.md)). `unsafe_code = "forbid"` applies workspace-wide, except in `platform`
(`deny`, allowed only on its FFI modules; [ADR 0001](adr/0001-platform-ffi.md)).

## 2. Time base

All time is `filmcraft_time::Tick(i64)` at `TICKS_PER_SECOND = 254_016_000_000`.

That number divides evenly into the frame duration of every broadcast rate (23.976, 24, 25, 29.97,
30, 48, 50, 59.94, 60, 120…) and the sample duration of every common audio rate (8 kHz to 192 kHz,
including the 44.1 kHz family). So frame and sample positions are exact integers, edits never drift
at 29.97, and audio and video line up to the sample.

| Type | Use |
|---|---|
| `Tick` | timeline and media positions and durations |
| `FrameRate { num, den }` | `frame_duration()`, `tick_of(frame)`, `frame_at(tick)`, `snap(tick)` |
| `TimeRange` | half-open `start + duration` |
| Timecode | display only (SMPTE NDF/DF, frames, feet+frames, samples); `parse_timecode` and `format_time` |

Commands take time as `time` (ticks), `frame`, `seconds` or `timecode`; the engine converts once at
the boundary.

## 3. Data model (`filmcraft-project`)

```text
Project
├─ root: Bin                         tree of bins
├─ items: map ItemId → ProjectItem   flat
│    kind: Media(MediaClip) | Sequence(Sequence) | Subclip{..} | AdjustmentLayer{..} | Graphic{..}
└─ next_id

Sequence
├─ settings: frame rate, size, sample rate, …
├─ video_tracks / audio_tracks: Vec<Track>
├─ markers, mark_in / mark_out
Track
├─ locked, sync lock, targeting, mute/solo/visibility
├─ items: Vec<TrackItem>             sorted, never overlapping
├─ transitions: Vec<Transition>
└─ audio: volume_db, pan, effects (mixer inserts), mixer: MixerStrip
     (automation mode + lanes, sends, output, record arm, solo safe, input map)
Sequence (audio) ─ submix_tracks: Vec<Track>, master_volume_db / master_effects / master_mixer
TrackItem (a clip instance)
├─ item: ItemId, start (timeline ticks), source_in (media ticks), duration, speed
├─ link group, label, enabled
└─ effects: Vec<EffectInstance>      intrinsic Motion/Opacity/Volume… first, then standard effects
EffectInstance
├─ effect id, enabled, params: id → constant value or keyframe track
└─ masks: Vec<Mask>                  path (keyframable Bézier), feather, opacity, expansion, inverted, mode
```

- **Graphic clips** ([graphics.md](graphics.md)) reference a `Graphic` canvas item; their text and
  shape layers are hidden `graphic_text` / `graphic_shape` effect instances on the track item.
- Everything is plain serde data. `Sequence::check()` validates the invariants (no overlaps, unique
  ids), and the engine runs it after every sequence edit.
- Timeline positions are sequence ticks; `source_in` and keyframes are in media time, so trims and
  splits never move keyframes.
- **Effect definitions are data.** `project::effect::effect_defs()` lists every video effect,
  audio effect and transition with its parameter schema. The Effects panel tree, the Effect
  Controls rows and the parameter docs agents see are all generated from it.
- **Project files** (`.fcproj`) are the project serialised as JSON. Saves are atomic: write a
  temporary sibling file, then rename it over the target.

## 4. Command system (`filmcraft-engine`)

```rust
pub struct CommandSpec {
    pub id: &'static str,                 // "sequence.addEdit"
    pub label: &'static str,              // "Add Edit"
    pub menu: &'static [&'static str],    // ["Sequence"]; empty = not in menus
    pub shortcut: Option<&'static str>,   // "Cmd+K" (Cmd = ⌘ on macOS, Ctrl elsewhere)
    pub params: &'static str,             // r#"{"time":ticks?}"#, shown to agents
    pub enabled: fn(&Session) -> Result<(), String>,  // Err carries the reason
    pub run: fn(&mut Session, &Value) -> Result<Value>,
    pub journal: bool,                    // false for read-only queries
}
```

- All commands are in `crates/engine/src/commands.rs` (`cmd!` for actions, `query!` for read-only
  queries such as `project.inspect`, `sequence.inspect`, `effects.list`, `jobs.list`). Ids follow
  the menu structure: `file.*`, `edit.*`, `clip.*`, `sequence.*`, `markers.*`, `timeline.*`,
  `effects.*`…
- `Session::execute(id, params)` finds the spec, checks `enabled`, runs it and appends it to the
  journal.
- **Undo.** Edits go through `Session::edit(label, |project, state| …)` or
  `Session::edit_sequence(label, |seq, ctx, state| …)`. These clone the project, apply the closure,
  and on success push the old `Arc<Project>` with the label onto the undo stack (200 entries).
  On error nothing changes. `edit.undo` and `edit.redo` swap snapshots. Thanks to structural sharing
  a snapshot costs little.
- **Editor state** (`EditorState`: active sequence, playheads, selection, targeting, edit points…)
  is serde, so agents can read it with `state.inspect`.
- **Events** (`ProjectChanged`, `Toast`, `OpenSequence`, `OpenSource`) are drained by frontends
  each frame.
- **Event log** (`Session::log`, `engine::panels`): every failed top-level command (an error; a
  disabled one is a warning), messages and error toasts, auto-save errors, and background jobs
  starting, finishing, failing or being cancelled. Repeats of the newest entry bump its count. The
  Events panel shows it; `events.list {level?, since?}` and `events.clear` reach it headless.
- **UI-only commands** (tools, playback, zoom, panels, workspaces) live in
  `crates/ui-egui/src/menus.rs` (`UI_COMMANDS`). The menu bar is built from the engine registry plus
  this table, and `menus::invoke` is the single entry point for menus, shortcuts and the control
  channel.

## 5. Media, render and playback pipeline

```text
file ──► codecs (MP4/MOV, MKV, audio)        demux + decode, GOP-aware seek
          │   decoder registry: h264, hevc, vp9, av1, prores, dnx, mjpeg (+ any registered first:
          │   platform's VideoToolbox (macOS) or Media Foundation (Windows) H.264 / HEVC, falling back to ours)
          ▼
        media::MediaSource ──► frame cache (byte-budgeted LRU, shared)
          ▼
        render::render_sequence(project, seq, t, scale)        CPU reference
          per track bottom→top: map timeline t → media t (speed), fetch frame,
          standard effects → Motion → Opacity/blend, transitions, composite
          in linear-light premultiplied f32
          │
          └─ render::plan::plan_frame → gpu::GpuCompositor     GPU path
               layers = decoded YUV/RGBA frames + matrix + opacity + blend mode;
               anything the shaders don't cover is pre-rendered on the CPU
          ▼
        ui-egui frames.rs worker pool ──► monitors (program/source), thumbnails, prefetch
```

- **Sources.** `media::MediaSource` yields `video_frame(FrameRequest)` and
  `audio(start, frames, rate)` in media time. Sources are `Send + Sync` and shared by monitors,
  thumbnails, playback and export. The engine's `MediaPool` creates one per project item, lazily,
  through registered openers (`codecs::openers()`: MP4/MOV, MKV/WebM, audio files).
- **Pixel aspect ratio.** Demuxers report it in `VideoStreamInfo::par` (MP4/MOV `pasp`, Matroska
  display size, MXF aspect ratio, MPEG sequence header / VUI); Interpret Footage's
  `Interpretation::par` overrides it (`clip.interpretFootage {pixelAspect}`), and
  `SequenceSettings::par` is the sequence's. Both go through `project::checked_par` (a zero term
  or more than 8:1 either way = square). `render::motion_matrix` works in square display units,
  so a 1440 x 1080 clip with 4:3 pixels fills a 1920 x 1080 square-pixel sequence at 100 %
  (Scale to Frame Size, Set to Frame Size and Fit / Fill use `Project::conformed_source_size`);
  graphics and adjustment layers are in sequence pixels. New Sequence From Clip copies the clip's
  ratio, and the Source and Program monitors draw the picture at its display aspect. The
  renderer does not read decoded frames' own `VideoFrame::par`.
- **Offline media and proxies.** The pool caches each item's source per reference (path, offline
  flag), so relinking and undo take effect at once. Media that can't be opened renders the offline
  slate (`render::offline`) instead of failing. With proxies enabled, an item with a proxy reads
  it through `ProxySource`, which reports the original's size. Export always uses
  `MediaPool::full_res_provider`. See [project-files.md](project-files.md#media-offline-relinking-proxies-ingest).
- **Seeking.** `codecs::Mp4Source` seeks to the preceding sync sample and decodes forward, caching
  every frame of the GOP. Sequential playback reuses the decoder. Decoders implement
  `codecs::VideoDecoder`. `register_video_decoder` puts a factory in front of the built-in ones, so a
  hardware decoder can take precedence. The GOP cache never holds its lock while decoding.
- **Hardware decoding.** The apps (desktop, CLI / headless MCP, bench) call
  `filmcraft_platform::register()` at startup, which on macOS registers a VideoToolbox factory for
  `avcC` / `hvcC` streams (8 / 10-bit, 4:2:0 and 4:2:2; elsewhere it does nothing), and on Windows a
  Media Foundation one (below). The factory
  declines (our decoder is used) when Settings ▸ Playback ▸ Hardware decoding is Off
  (`codecs::hw::set_hardware_decoding`, applied by the engine whenever preferences change), for
  formats it does not take, and when the OS cannot create a *hardware* session for the stream.
  Its decoder is a `platform::HybridDecoder`: VideoToolbox decodes asynchronously (two access
  units in flight), a reorder buffer of the stream's own depth (`max_num_reorder_frames` /
  `sps_max_num_reorder_pics`, from `codecs::hw::NalStreamInfo`) restores presentation order, and
  the decoded biplanar `CVPixelBuffer` is copied into planar `Yuv8` / `Yuv16`. Colour, pixel
  aspect, random-access and disposable answers come from the same helpers as the software
  decoders (`video::vui_color`, `sar_par`, `h264_disposable`, `hevc_disposable`), so the two are
  interchangeable (bit-exact in the parity tests). If the hardware fails mid-stream (decode error,
  session lost to a GPU change or sleep, changed in-band parameter sets), the hybrid builds
  `codecs::software_video_decoder` (built-in factories only), replays the samples since the last
  restart point and continues in software for that instance; it is logged and counted. A source
  already open keeps its decoder when the setting changes, until it is reopened. `perf.stats`
  `decode.hardware` reports hardware vs software frames, sessions, declines and fallbacks, and the
  registered `backend`.
  On Windows the decoder (`platform::media_foundation`) is a Direct3D-aware decoder MFT driven at
  the level of single access units (no Source Reader: the demuxer above already delivers the
  samples): the `avcC` / `hvcC` sample becomes Annex B, the MFT is given the process's Direct3D 11
  video device through an `IMFDXGIDeviceManager` so it decodes with DXVA, and its NV12 (8-bit) /
  P010 (10-bit) texture is read back through a staging texture (the one GPU to CPU copy) into
  planar `Yuv8` / `Yuv16`. H.264 Baseline / Main / High, HEVC Main / Main 10, VP9 profiles 0 / 2 and AV1 main
  (8- and 10-bit), 4:2:0, progressive (VP9 / AV1 samples go in as they are, not as Annex B);
  everything else, and any stream the GPU's DXVA decoder does not list, is declined. A decoder that
  would hand back system-memory pictures (Microsoft's decoder MFTs do that when DXVA is not
  available) fails the stream, so the hybrid continues with our decoder and Windows' software
  decoding is never used in its place. Zero-copy (the texture straight to wgpu) is not done yet.
  Decoders run slices on rayon, so an export worker waiting inside a decode can pick up another
  frame of the same source. A request that finds the shared decoder busy decodes with a private
  decoder.
- **Hardware encoding.** On Windows and 64-bit Linux, `platform::nvenc::export::factory` is registered with
  `filmcraft_export::register_encoder` (`register()` does this once), in front of the software H.264
  encoder. It takes an export only when Export ▸ Hardware encoding (`ExportSettings.hardwareEncoding`,
  `HardwareEncoding::Auto`) is Auto, which is off by default: hardware streams differ from ours, and
  exports are otherwise byte-identical from run to run. NVENC (`platform::nvenc`, the driver's
  `nvEncodeAPI64.dll` / `libnvidia-encode.so.1` loaded at run time) then takes RGBA frames, converted to 4:2:0 by the software
  encoder's own conversion, and produces the H.264 samples and `avcC` of an MP4 / MOV. It declines
  (the software encoder runs, counted in `export.hardware.declined`) two-pass VBR, HDR, MXF,
  interlaced output, sizes outside NVENC's limits, and systems without an NVIDIA GPU or driver.
  A failure during an export ends it with an error: the software encoder cannot take over a hardware
  stream. The same NVENC session also encodes H.265 (HEVC Main 8-bit 4:2:0 SDR, or Main 10 HDR: PQ / HLG,
  BT.2020, limited range, from the float R'G'B' pictures through `rgbf_to_yuv420_10` into P010 input
  buffers, with the HDR10 static metadata as SEI on every IDR for PQ; the `hvcC` is built from the SPS
  the encoder wrote). An HDR sequence exports as HDR H.265 only where a registered probe says so
  (`filmcraft_export::register_hdr_probe`, `hdr_available`; NVENC with 10-bit support), otherwise it is
  tone-mapped to SDR as on macOS: `register()` hands `platform::nvenc::hevc_available` to
  `filmcraft_export::register_format_probe`, so the H.265 format exists where an HEVC encoder does,
  and choosing it is the opt-in (the Hardware encoding toggle governs H.264 only). With no software
  H.265 encoder, what NVENC declines is counted and ends the export with an error that says why.
- **Compositor.** `render` is the reference for monitors, thumbnails and export. `render::plan`
  turns a frame into GPU layers, each with its opacity and blend mode. All 27 blend modes run on the
  GPU (`filmcraft-gpu`): Normal and Dissolve with fixed-function "over" blending, the others by
  copying the accumulator under the layer into a backdrop texture and compositing in the fragment
  shader with the CPU reference's formulas. The common standard effects run on the GPU too
  (`render::gpufx`, `gpu::fx`): Brightness & Contrast, ProcAmp, Tint, Black & White, Color
  Balance, Leave Color, Change to Color, Color Pass, Color Replace, Channel Mix, ASC CDL, Gamma
  Correction, Levels, Extract, Invert, Posterize, Alpha Adjust, Gaussian Blur and Directional Blur
  (and their legacy aliases), Camera Blur, Sharpen, Unsharp Mask, Crop, Edge Feather, Transform,
  Horizontal / Vertical Flip, Mirror and Offset. When every enabled effect of a media clip is in
  that set (unmasked, with finite parameters, and no Transform shrinking the picture below half
  size, which the CPU pre-filters), the plan hands the GPU the clip's source with
  the effects' parameters evaluated at that time: the source is drawn into an `Rgba32Float`
  working image at the size the CPU decodes it, each effect runs as compute passes with the CPU
  reference's math (effects, then Motion, then Opacity / blend, as on the CPU), and the result is
  placed like any layer. Other standard effects, effect and opacity masks, adjustment layers,
  nested sequences (except a plain one, whose own layers go into the plan: same frame size and
  colour settings as its parent, nothing on the clip that changes the picture, every layer inside
  blended normally) and non-dissolve transitions are rendered on the CPU for that layer or frame
  and handed to the GPU as an image (a layer image keeps its clip's blend mode), so both paths
  give the same picture. Setting `FILMCRAFT_CPU_COMPOSITE=1`
  forces the CPU path in the desktop app.
- **CPU compositor shortcuts.** The CPU compositor, which is what export runs, works on
  premultiplied linear `f32` images, 33 MB at 1080p, so what it does not allocate, convert or mix
  is time saved. Three shortcuts, each of which gives the general path's bits (compared bit for
  bit, with every blend mode, in `render/src/region_tests.rs`): the canvas stays unallocated
  until a layer needs it, and an opaque bottom layer (an 8-bit or 16-bit Y'CbCr frame without an
  alpha plane, opacity 100 %, Normal) simply becomes the canvas; a plain media frame with an alpha
  plane (a ProRes 4444 banner) is converted and mixed only inside the rectangle where its alpha is
  not zero (`VideoFrame::alpha_region`, `to_linear_f32_region`, `blend::composite_at`), the rest
  being transparent anyway; and working images are recycled through `filmcraft_frame::pool`
  (`take_f32_overwritten` / `recycle_f32`: the buffer comes back with its old contents, so there is
  no zero-fill) by the export pipeline once a frame is converted to 8 bits; a standalone export's
  pipeline frees the idle float images (`pool::trim_f32`) when it goes away, so a finished export
  leaves none behind (parts of a batch, such as render-preview segments, keep them for the next part). A clip
  with effects, opacity masks, frame blending, colour management, or a picture that is not placed
  one to one on the output takes the general path.
- **Frame scheduling.** `crates/ui-egui/src/frames.rs` runs a small pool of worker threads with
  prioritised jobs: the frame on screen first, then playback prefetch, then thumbnails. The UI never
  decodes. It shows the exact frame when it is ready and holds the nearest cached frame meanwhile.
  Play waits for the first frames (`PREROLL_FRAMES`, at most 0.5 s) before starting the clock.
  Every refresh, `schedule_playback` asks for the next frames and drops (or cancels, through
  `filmcraft_media::cancel`) jobs for frames the playhead has passed; a new on-screen frame
  replaces the one asked for before (scrubbing); Stop cancels the prefetch. When frames cost more
  than the workers can render in real time (CPU effects), it starts only frames that can still be
  on time and spaces them evenly (`playback_plan`). When decoding is what falls behind, each job
  tells its sources which frames are already late (`filmcraft_media::cancel::with_catch_up`: the
  playhead's distance, two seconds for a scrubbed-to frame), and the GOP cache skips
  non-reference pictures of late frames on the way (`VideoDecoder::is_disposable`, H.264
  `nal_ref_idc` 0 and HEVC sub-layer non-reference pictures; the wanted frame decodes exactly as
  before). With Settings ▸ Playback ▸ Draft decoding (off by default), frames played at 1/2
  resolution or lower are requested as draft frames (`FrameKey::draft`,
  `filmcraft_media::cancel::with_draft`): the H.264 decoder skips deblocking of non-reference
  pictures (`VideoDecoder::set_draft`), the GOP cache serves those draft frames to draft
  requests only (a paused frame, render or export decodes them again exactly), and draft plans
  hand the GPU box-decimated Y'CbCr planes at the drawn size. `perf.stats` reports the counters.
  GPU plans carry their texels already converted for upload (`filmcraft_gpu::prepare`), so the UI
  thread only copies them.
- **Audio clock.** The desktop app passes a cpal output (`apps/filmcraft/src/audio.rs`) to the UI as
  `AudioOut`. While playing, the samples played by the sound card drive the playhead and video follows.
  Without an audio device, playback falls back to the wall clock. `PlaybackMeter` counts timeline
  frames: shown when the exact picture was on screen while due, dropped otherwise (including frames
  passed over without a refresh, not while the window is hidden). `cargo xtask bench-playback`
  measures the whole path headlessly ([testing.md](testing.md) §5).
  Sequence audio goes through the mixer graph (`render::mixer`, §5.1); clip audio effects run on
  `audio-dsp` via `render::audio_fx`.

### 5.1 Audio mixer

```text
clip: gain → clip effects → Volume / Channel Volume / Panner (clip keyframes, media time)
      → audio transitions → summed per track                           render::audio::track_input
track / submix strip:  input map, mono fold → pre-fader inserts → pre-fader sends → mute
      → fader (volume) → meter → post-fader inserts → post-fader sends → pan / balance → output
Mix:  bus sum → pre-fader inserts → fader → meter → post-fader inserts → out    render::mixer
```

- **Model** (`project::mixer`). Every audio track, submix and the Mix has a `MixerStrip`. Static
  values stay in `Track::volume_db`, `pan`, `muted` and the send/effect parameters; automation is
  keyframes in sequence ticks: lanes `volume`, `pan`, `mute` (hold), `send.<i>.level` in
  `MixerStrip::lanes`, and insert parameters (`fx.<slot>.<param>`) in the effect's own keyframes.
  Up to 5 inserts (`EffectInstance::post_fader` picks the side) and 5 sends per strip. Submixes feed
  the Mix or a submix after them (no feedback). All fields have serde defaults, so older projects
  load unchanged.
- **Graph** (`render::mixer::mix_graph`). Lanes are evaluated per sample; effect parameters update
  on an absolute 64-sample grid, so the output does not depend on how callers cut the timeline into
  requests (export batches and device callbacks give identical samples). Inserts that report
  latency delay their strip; each route into a bus gets a compensation delay and the graph is read
  ahead by its total latency. Graph state (DSP, delay lines) is cached per structure and continued by
  sequential readers; other requests start fresh with a pre-roll (effect tails, ≤ 3 s). Tracks run
  in parallel (rayon), buses in order. Mono tracks pan with the −3 dB constant-power law; stereo
  tracks and sends use balance. Solo keeps soloed and solo-safe strips plus everything feeding them
  or fed by them. 24 tracks × (EQ + Dynamics + Studio Reverb) + a compressed submix renders at
  ~6× realtime on one core (release).
- **Automation modes** (Premiere semantics). Off ignores lanes; Read plays them; Latch records from
  the first touch and holds the last value until playback stops; Touch records while held and ramps
  back to the existing automation over the **automatch time** (Preferences ▸ Audio, 1 s); Write
  records every control from playback start (then switches to Touch unless "Switch to Touch after
  Write" is off).
- **Recording** (`engine::mixer`). Playback start runs `mixer.recordStart`, stop runs
  `mixer.recordStop`. Fader and knob drags send `mixer.touch` (value, playhead) while held and
  `mixer.release` when let go. Held values go to `render::mixer::LiveMix`, which the playing mix
  reads, so moves are heard at once. At stop each gesture stream is thinned (linear keyframe
  thinning, optional minimum time interval) and written over its time range as one undo step,
  with boundary keyframes that keep the automation outside the range unchanged.
- **Live state.** `PreviewStore::live` (`LiveMix`) also carries per-strip meter peaks posted by the
  mix (one per channel; Track Mixer and Audio Meters read them), the voice-over input level (Meter
  Input(s) Only) and the newest project snapshot, which the audio callback uses, so edits made
  during playback are heard.
- **Audio Clip Mixer automation.** Each audio track has a Clip Mixer mode (`clipMixer.setMode`,
  session state `EditorState::clip_mixer_modes`, Read by default). During a pass the Clip Mixer's
  fader and pan knob are gestures on the track's `clip.volume` / `clip.pan` lanes
  (`clipMixer.touch` / `clipMixer.release`): the clip under the playhead plays the held value at
  once (`render::audio::track_input_live`), and Latch / Touch / Write follow the track rules. At
  `mixer.recordStop` each stream is thinned and written as **clip keyframes** (Volume `level`,
  Panner `balance`, in media time) into every clip it covers, with boundary keyframes that keep the
  clip's values outside the range; one undo step for the whole pass.
- **Track Mixer view.** The panel menu (`mixer.menu`) has Show/Hide Tracks (`UiState::mixer_hidden`)
  and Meter Input(s) Only (`UiState::mixer_meter_input_only`: record-armed tracks meter the
  voice-over input). The transport's record button runs `voiceover.recordToggle`.

### 5.1.1 Multichannel and 5.1

```text
5.1 track (6 ch: L R C LFE Ls Rs) ─┐                         ┌─► 5.1 Mix ─► 6-ch export / 5.1 device
stereo / mono track ──5.1 panner───┼─► 5.1 submix / 5.1 Mix ─┤
5.1 strip ──BS.775 downmix─────────┴─► stereo bus            └─► stereo device: 5.1 Mixdown Type fold
```

- **Model.** `Track::channels` (Standard = Stereo, Mono, 5.1, Adaptive) and
  `SequenceSettings::audio_master` (the Mix: Stereo, Mono, 5.1, Adaptive) existed already;
  `file.newSequence {mix, trackType}` and `sequence.settings {mix}` set them, `mixer.setStrip
  {channels}` / `mixer.addSubmix {channels}` per strip. The 5.1 panner is four lanes in
  `MixerStrip::lanes` (`pan51.x`, `pan51.y` −100…100, `pan51.center` 0…100 %, `pan51.lfe` dB): a
  lane without keyframes stores the static value, so no new project fields were needed and the
  puck is automatable like any lane.
- **Maths** (`audio_dsp::channels`). 5.1 order is L, R, C, LFE, Ls, Rs everywhere inside FilmCraft.
  Downmix is ITU-R BS.775 (`Lo = L + k·C + k·Ls`, `Ro = R + k·C + k·Rs`, mono
  `M = k·L + k·R + C + ½·Ls + ½·Rs`, `k = 1/√2`, LFE omitted); upmix puts mono on C and stereo on
  L/R. The panner splits a point source front/rear and left/right with the sine/cosine
  constant-power law and gives the centre speaker `center·(1 − |x|)` of the front power (Σg² = 1).
  Stereo sources sit at the puck ± 1 in x; 5.1 sources keep their layout moved by the puck's offset
  from front-centre (the default puck is the identity). LFE gets the source LFE (5.1) or the mean of
  the source channels (mono/stereo, default −∞) times the LFE level.
- **Graph.** Buses are 2 or 6 channels wide (`render::mixer::width_of`); inserts are built for the
  strip's width; routes convert between widths (5.1 panner into 5.1 buses, BS.775 into stereo
  buses); meters have one value per channel. `mix_graph` returns the Mix's width;
  `audio::mix_sequence` always returns stereo, `mix_sequence_layout` any layout and mixdown. Clips
  on 5.1 tracks play the source's first six channels (or the six picked with Modify ▸ Audio
  Channels), mono/stereo sources upmixed; clip effects run on all six channels.
- **Playback.** On a device with ≥ 6 output channels a 5.1 Mix plays as six channels; on a stereo
  device it is folded with Preferences ▸ Audio ▸ 5.1 Mixdown Type (Front Only, Front + Rear
  Surround = BS.775, Front + LFE, Front + Rear Surround + LFE; `PreviewStore::mix_layout`).
- **Export.** Audio Channels Mono / Stereo / 5.1: WAV writes `WAVE_FORMAT_EXTENSIBLE` with channel
  mask `0x3F`; QuickTime PCM (ProRes, DNxHR, MJPEG) adds a `chan` atom
  (`kAudioChannelLayoutTag_MPEG_5_1_A`); AAC uses channel configuration 6 (our encoder already
  supported 1–8 channels; the exporter reorders to AAC's C, L, R, Ls, Rs, LFE). A stereo export of a
  5.1 Mix is the BS.775 downmix.

### 5.1.2 Voice-over recording

**Voice-over recording** (`engine::voiceover`, `audio.voiceover.*`). The record point R is the playhead, or the In point when In/Out are set (punch-in; punch-out at Out). Playback starts the pre-roll before it (C = max(0, R − pre-roll)) and the input is captured from C. When the UI's audio clock really starts, `audio.voiceover.sync` restarts the capture there. `audio.voiceover.stop` keeps the audio from R to min(stop, Out), writes a mono 32-bit float WAV `<Name> <n>.wav` (Scratch Disks ▸ Captured, else next to the project, else the data or temporary directory), imports it and overwrites it onto the record track at R as one undo step. The record track is the given one, else the record-armed one, else the first targeted one. Input goes through the `AudioInput` trait: cpal in the desktop app (`apps/filmcraft/src/audio_in.rs`, on its own thread), and `SyntheticInput` headless, which produces exactly the samples the timeline asks for so recordings land sample-accurately. Voice-Over Record Settings (Source, Input channel, Name, Countdown Sound Cues, Pre-/Post-roll) are preferences (`voiceOver`). The track header's microphone records or stops, and a right-click opens the settings dialog. Countdown beeps (1 kHz, 100 ms, each whole second of pre-roll and at R) are mixed into playback, an overlay counts down, and playback stops at Out + post-roll.

### 5.1.3 Remix

```text
clip media (source In … +duration) ─► audio_dsp::remix::analyze: spectral-flux onsets → tempo (autocorrelation, 50–200 BPM)
  → DP beat tracker → per-beat chroma + mel cepstra → beat self-similarity
  ─► plan(target, Segments, Variations): intro … joints at beat boundaries … outro, length within one beat
  ─► hidden clip effect `remix` (target, sliders, original duration, plan in ticks) ─► render::remix::read (20 ms equal-power crossfades)
```

Clip ▸ Remix ▸ Enable Remix / Remix Properties… / Revert Remix (`clip.remix.*`; agents can use `clip.remix {duration|seconds…}`) and the Remix tool in the Rate Stretch group (drag a music clip's Out edge) retime a music clip to a target duration. The analysis finds the beat and compares every pair of beats by harmony (chroma) and timbre (cepstra). Each joint leaves the music at one beat boundary and continues at the beat boundary whose four beats on either side sound most alike. The intro and outro are kept, and the result lands within one beat of the target. Segments (0–100) sets the number of joints (1–3, more for long extensions). Variations (0–100) sets how far a joint may move from its evenly spaced position (±1–8 beats). The state is a hidden effect instance with no definition, so panels, the effect chain and Paste Attributes ignore it. The plan is stored in ticks, so playback and export read it without re-analysing. A remix never moves other clips: one that would overlap the next clip is refused, and Revert restores the original duration. Results are deterministic and the analysis is cached per media range.

### 5.2 Essential Sound

```text
clip.essential (type + settings)  ──apply()──►  clip effects marked `essential`, clip gain, Volume, Panner
essentialSound.autoMatch   BS.1770 integrated loudness of render::audio::clip_signal (mono clips as one channel) → match gain (clip gain)
essentialSound.generateDucking   trigger clips' summed level (10 ms hops) → activity regions → Volume keyframes
```

- **Model** (`project::essential`). A clip's `essential: Option<EssentialSound>` holds its audio type
  (Dialogue / Music / SFX / Ambience) and per-type sections: Loudness (match gain, measured and target
  LUFS), Repair (Reduce Noise, Reduce Rumble, DeHum 50/60 Hz, DeEss, Reduce Reverb), Clarity
  (Dynamics, EQ preset + amount, Enhance Speech), Creative (Reverb preset + amount, Stereo Width for
  Ambience), Ducking (against types, sensitivity, reduce by, fades), Pan, Clip Volume and Mute.
- **Effects under the hood.** `essential::apply(item, old, t)` turns the settings into ordinary clip
  effects (Highpass, DeNoise, DeHummer, DeEsser, DeReverb, Dynamics Processing, Parametric Equalizer,
  Enhance Speech, Stereo Width, Studio Reverb) flagged `EffectInstance::essential`, in that order, ahead
  of the user's own effects. Only parameters whose derived value changed are written (at the playhead),
  so keyframes added in Effect Controls survive. A section switch bypasses its effects, a slot switch
  removes its effect, clearing the type removes them all. Auto-match gain, Clip Volume (an offset on
  the Volume level or on all its keyframes) and Pan are applied as deltas, so clearing restores the
  clip. Because these are normal effects, playback, the mixer and export need nothing special.
- **Loudness.** The clip signal (clip gain + effects, before Volume) is measured with the BS.1770
  meter; the gain is linear after the effects, so one measurement hits the target exactly. Targets are
  preferences (`audio.dialogueTargetLufs` −23, `musicTargetLufs` −25, `sfxTargetLufs` −21,
  `ambienceTargetLufs` −30).
- **Ducking.** Sensitivity 0…10 maps to a threshold −20 − 4·s dBFS on the summed trigger signal (50 ms
  window); regions shorter than 100 ms are dropped and pauses under 250 ms bridged; each region gets a
  fade-down before it and a fade-up after it (`audio_dsp::ducking::duck_keyframes`), written as Volume
  level keyframes that replace earlier ones.
- **Presets** (our own names and values) per type; user presets are saved in preferences
  (`essentialSound.userPresets`). Music remixing to a duration is Clip ▸ Remix (§5.1.3).
- **Enhance Speech** is a DSP chain (high-pass, de-mud, presence and air EQ, expander, compressor), not a
  model: no speech-enhancement model with an open licence that we could ship and verify is bundled.
  DeepFilterNet (MIT/Apache-2.0, Rust inference via tract) is the candidate for a future optional
  integration behind a trait.

### 5.3 Masks

Every video effect and the intrinsic Opacity carry `EffectInstance::masks` (`project::mask`).
A mask is a closed cubic Bézier `MaskPath` in clip pixels (ellipse = four smooth vertices with
circular tangents, 4-point polygon = corner vertices, the pen draws arbitrary vertices), stored as a
`ParamValue::Path` so the ordinary keyframe engine animates it (vertex-wise interpolation; paths
with different vertex counts hold). Feather, Opacity and Expansion are ordinary float parameters.

- **Coverage** (`render::mask`): the path is flattened (≤ 0.05 working px chord error); the
  signed distance to the polygon (nonzero winding) plus Expansion goes through a falloff of width
  max(Feather, 1) centred on the edge (linear = exact box-filtered antialiasing at Feather 0,
  blending into smoothstep as Feather grows). Masks combine top to bottom with Add / Subtract /
  Intersect / Lighten / Darken / Difference; Inverted and Opacity apply per mask.
- **Semantics.** A masked effect is `lerp(original, effected, coverage)` per premultiplied channel
  (the effect only applies inside); Opacity masks scale the clip's layer before Motion, so they
  follow the clip's transform. Adjustment-layer masks are in sequence pixels.
- **GPU.** `filmcraft-gpu::GpuMask` evaluates the same coverage and mix in WGSL (compute), tested
  to agree with the CPU within 3·10⁻⁶. Layers with masks are CPU-rendered images in frame plans.
- **Editing.** `masks.*` commands (add / remove / set / moveVertex / translate / addVertex /
  removeVertex / toggleVertexSmooth / select / list); keyframe commands take `"mask": n`. The
  Program monitor overlay drags vertices, Bézier handles, the whole mask and the feather /
  expansion handles; drags merge into one undo step.
- **Tracking** (`render::track`, `masks.track`): Shi–Tomasi features inside the mask, pyramidal
  Lucas–Kanade (4 levels, 15×15 window) with a forward–backward check, then RANSAC + least squares
  for Position / Position & Rotation / Position, Scale & Rotation (2D Procrustes). Each frame's
  transform moves the path, written as Mask Path keyframes while the background job runs (one undo
  step per run; `jobs.cancel` stops and keeps what was tracked). Frames are tracked at ≤ 960 px
  wide. On synthetic footage with known similarity motion the path stays within 0.3 px over 20 frames.

### 5.4 Colour management

```text
frame (Y'CbCr/RGB + metadata) ─► source colour space: Interpret Footage override, else VUI/colr/MKV Colour
  ─► decode table (sRGB/BT.709, PQ, HLG scene light, camera log) ─► HLG OOTF ─► 3×3 to working gamut
  ─► BT.2390 tone map (HDR/log into an SDR sequence, Auto Tone Map Media) ─► gamut compression
  ─► effects + compositing in working linear (1.0 = reference white = SDR white = 203 cd/m²)
  ─► monitors: working → SDR BT.709 (tone map from HDR, gamut map from BT.2020)
  ─► HDR export: working → PQ/HLG BT.2020 R'G'B' → Y'CbCr (BT.2020 NCL) + VUI/colr/mdcv/clli/SEI
```

- **Model.** `SequenceSettings::color` (`ColorPipeline`: working space Rec. 709 / Rec. 2100 PQ /
  Rec. 2100 HLG, wide gamut, auto tone map) and `Interpretation::color_space` (per media item;
  `None` = from metadata). Commands: `sequence.colorSettings`, `clip.interpretFootage`,
  `color.spaces`, `media.colorInfo`.
- **Maths** in `filmcraft-color` (`transform`, `log`, `spaces`; formulas and sources in
  [crates/color/README.md](../crates/color/README.md)); the renderer side is `render::colorman`.
- **Fast path.** A Rec. 709 sequence without wide gamut and media whose metadata says Rec. 709 /
  sRGB decodes exactly as before, and its layers stay on the GPU. Log, HDR or wide-gamut media,
  and every layer of an HDR/wide-gamut sequence, are converted on the CPU (the GPU path draws them
  as pre-rendered images, like layers with effects).
- **Outputs.** `RenderOptions::working_output` returns working-space pixels (HDR exports,
  scopes); otherwise the top-level render is converted for an SDR monitor. The other colour
  effects work on display-encoded values clamped to 0..1 (they clip HDR highlights above
  reference white).
- **HDR grading (Lumetri).** `FxCtx::working` carries the sequence's working space. In a PQ or HLG
  sequence Lumetri grades the working space's own signal normalised to **HDR White** (cd/m²,
  Basic Correction; the curves, wheels, looks and HSL Secondary use the Curves section's **HDR
  Range**): `filmcraft_color::GradeSpace` (PQ: `PQ⁻¹(nits/10000) / PQ⁻¹(white/10000)`; HLG: the
  BT.2100 inverse OOTF of a display with that peak, then the OETF). So the sliders and curves span
  0 … HDR White like black … white in SDR, exposure is stops of light, and highlights above HDR
  White pass through (contrast leaves them alone; **HDR Specular** scales them). Rec. 709
  sequences grade exactly as before. HSL Secondary ▸ Refine: **Denoise** (median of the key,
  radius 1–3 px) and **Blur** (Gaussian of the key, σ ≤ 20 px), both scaled with the playback
  resolution.
- **HDR metadata.** `VideoStreamInfo::hdr` (`HdrMetadata`: ST 2086 mastering luminance, MaxCLL,
  MaxFALL) is read from `mdcv` / `clli` (MP4 / MOV) and Matroska `MasteringMetadata` / `MaxCLL` /
  `MaxFALL`. PQ media tone mapped into an SDR working space uses it as the BT.2390 source peak
  (MaxCLL, else the mastering peak; 1000 cd/m² without metadata), so a 4000-nit master keeps its
  highlight detail instead of clipping at 1000. `media.colorInfo` reports it (`hdrMetadata`,
  `toneMapPeakNits`). The field is optional and serde-defaulted (no schema change).
- **HDR export.** H.264 and ProRes exports of a PQ/HLG sequence encode BT.2020 PQ/HLG (ProRes
  10-bit; our H.264 encoder is 8-bit, so H.264 HDR is 8-bit) and signal it in the VUI / ProRes
  frame header, `colr`, and for PQ `mdcv` (BT.2020 / D65, 1000 / 0.0001 cd/m²) and `clli`
  (MaxCLL/MaxFALL 0 = unknown, they are not measured) plus the matching H.264 SEI. `sdr: true`
  exports the tone-mapped SDR picture instead; render previews always do.
- **Monitors / display colour management.** The monitors are SDR (sRGB-encoded RGBA8 textures):
  HDR sequences are shown tone mapped. macOS EDR (extended-range `CAMetalLayer` output) is not
  wired up: there is no `platform` crate yet and eframe/wgpu do not expose an EDR surface, so HDR
  values above SDR white are never sent to the display. The scopes of an HDR sequence show the
  working-space values (waveform in cd/m² on a PQ scale, BT.2020 vectorscope).
- **LUTs.** Lumetri Input LUT and Creative Look reference `lib:<id>` (the project's LUT library,
  `Project::luts`, embedded `.cube`/`.3dl` text) or `builtin:<id>` (code-generated camera
  conversions and looks). `filmcraft-gpu::GpuLut` is the WGSL tetrahedral counterpart, tested for
  parity.

### 5.4 Multi-camera and synchronisation

```text
clips ──sync (in | out | timecode[±hours] | marker | audio)──► anchors (media time ↔ common instant)
  ├─ clip.synchronize        moves selected timeline clips (link groups together) onto the reference
  ├─ clip.mergeClips         video + ≤16 audio clips → a merged-clip sequence
  └─ clip.createMulticam     cameras → a multi-camera source sequence (one video track per angle)
multi-camera clip = nested source + TrackItem::multicam {enabled, angle}
  render: only the angle's video track · audio: camera 1 | all | the angle (Switch Audio)
```

- **Model** (`project::multicam`). `Sequence::multicam` (`MulticamSource`: cameras with their video
  track, audio tracks, name, shown flag and source item; audio mode) marks a multi-camera source;
  `Sequence::merged` a merged clip. `TrackItem::multicam` (`MulticamSel`) makes a nested clip a
  multi-camera clip; any nest can be one (its video tracks are then the angles,
  `Sequence::cameras()`). Editing a multi-camera source into a sequence gives an enabled clip on the
  first angle (`Project::make_track_item`). Project schema v7.
- **Sync** (`engine::sync`). Each method reduces a clip to an *anchor* (the media time that lines up
  with the common instant). Audio uses `audio_dsp::sync::find_offset`: DC removal, windowed-sinc
  decimation to ≤ 8 kHz, GCC-PHAT-β (β = 0.75) via one packed complex FFT for the coarse lag, then
  the same at the full rate on the loudest common window (≤ 2.7 s) and parabolic interpolation.
  Recordings with different gains, microphones (filtered), 0 dB SNR noise or a strong echo are
  aligned to the sample; two 10-minute recordings take ~2.4 s (release). Clips are placed on frame
  boundaries with the sub-frame remainder taken from their source In, so video stays on frames
  while audio keeps sample accuracy.
- **Render.** `render::item_layer` renders a multi-camera clip's angle track only
  (`render_seq_tracks`); `render::audio` mixes the nested source with only the audible tracks
  (`Sequence::with_angle_audio`). Nested audio now plays at all (it was skipped when the nest had
  no media source) and is limited to the clip's range. `render::multicam::render_grid_page` renders
  one page of the shown angles at the cell scale in parallel (rayon) and tiles them: the
  Multi-Camera view is one frame job (`frames::Target::MulticamGrid(source, layout, page)`),
  prefetched while playing like the program. Only the page's angles are decoded.
- **Multi-Camera view settings** (`EditorState::multicam_view`, Program ▸ wrench menu): grid
  layout (automatic = smallest square up to 4×4, or fixed 2×2 / 3×3 / 4×4) with pages when the
  angles don't fit (`render::multicam::page_layout`; `multicam.gridLayout`, `multicam.page`,
  page arrows; keys 1–9 pick cameras on the shown page, `multicam.cut {camera}` stays absolute);
  Multi-Camera Selection Top Down (stacked multi-camera clips: topmost instead of lowest);
  Show Multi-Camera Preview Monitor (off: the grid fills the monitor); Auto-Adjust Multi-Camera
  Playback Quality (`grid_cell_scale`: ½ / ¼ of the cell scale while playing); Transmit
  Multi-Camera View (stored; there is no transmit device yet). `multicam.grid` returns the page.
  Edit Cameras… shows a thumbnail per angle (`frames::Target::MulticamAngle`).
- **Editing** (`edit::multicam`, `engine::multicam`). `multicam.switchAngle` (click an angle,
  Ctrl/⌘-click for video only), `multicam.selectCamera1…9` (keys 1–9) and `cutToCamera1…9`
  (Ctrl+1–9), Enable/Flatten, Edit Cameras, Audio Follows Video (`EditorState`). Live switching:
  playback in the Multi-Camera view runs `multicam.recordStart`; each key/click is a
  `multicam.cut` that is applied at once (the program shows it) by re-applying the whole pass to
  the project from before the pass (`edit_merged`), so a pass is one undo step; Stop runs
  `multicam.recordStop`, which ends the last angle at the stop point. Through edits inside the
  recorded range are healed, so pressing the angle already showing adds no edit. Flatten replaces
  a clip by the clip(s) its angle shows (outer effects carried over; linked pairs stay linked).

## 6. Export jobs (`filmcraft-export`)

```text
file.exportMedia {path, preset?, settings?, format?, range?, …}     export.quick {preset?, path?}
  → engine::export_tools: preset ⊕ settings JSON ⊕ flat overrides → ExportSettings; range resolved
  → a Job {id, label, progress, result} on a background thread (export.queue.* runs them in order)
  → export: per output frame render (output rate) → fit into the output frame → overlays → limiter
            → encode in order → mux; audio mixed at the output rate, folded to mono/stereo,
            loudness-normalized (measuring pass + gain + true-peak limiter) per batch
  → jobs.list shows progress; jobs.cancel sets the shared cancel flag
```

| Format | Encoder | Container |
|---|---|---|
| `h264` | `filmcraft-h264enc` (profile, level, CBR / VBR 1-pass / VBR 2-pass, keyframe distance) + `filmcraft-aac` | MP4 or QuickTime (`isobmff`, the Multiplexer setting) |
| `prores` | `filmcraft-prores` (Proxy / LT / 422 / HQ) | MOV |
| `dnxhr` | `filmcraft-dnx` (LB / SQ / HQ / HQX) | MOV (`AVdh`) |
| `mjpeg` | built in | MOV |
| `mxf-op1a` | DNxHR (default), ProRes or H.264 (Annex B, long GOP), `mxfVideoCodec`; PCM 16/24-bit | MXF OP1a (`filmcraft-mxf` writer: frame-wrapped, index with temporal offsets, start timecode) |
| `mxf-opatom` | as `mxf-op1a` | Avid-style MXF OP-Atom: clip-wrapped picture at `path`, one mono PCM file per channel (`<stem>_A1.mxf` …, `Report::extra_files`) |
| `png`, `tiff`, `bmp` | `image` | numbered stills `<name>000.<ext>`, `<name>001.<ext>` … |
| `gif`, `wav`, `aiff` | built in / `image` | GIF / RIFF WAVE (`WAVE_FORMAT_EXTENSIBLE` for 5.1) / AIFF (16- or 24-bit PCM) |

- **Reproducible.** The same project and settings give the same file on every machine. Frames render
  in batches of one per core, but the muxer gets them in fixed groups of 16 output frames
  (`INTERLEAVE` in `crates/export/src/job.rs`), each followed by its audio, and H.264 uses one slice
  per four macroblock rows instead of one per core. MXF files are the exception: their UMIDs and
  modification date come from the clock, as SMPTE ST 330 / ST 377 expect.
- **Settings.** `ExportSettings` (serde, camelCase, every field optional) holds Video (frame size or
  Match Source, frame rate, Scale to Fit / Fill / Stretch, pixel aspect, field order — progressive
  only, profile / level, bitrate encoding, target / maximum / adaptive bitrate, keyframe distance,
  maximum render quality), Audio (codec, sample rate, channels, AAC bitrate, PCM sample size),
  Multiplexer (audio channels: mono, stereo or 5.1, §5.1.1), Captions (burn-in or SRT / WebVTT sidecar), Effects (image / name / timecode
  overlays, video limiter, loudness normalization), Metadata (`udta` `©nam`, `©ART`, `©cpy`, `©des`,
  `©cmt`). `settings.summary()` / `estimate_bytes()` feed Export mode's Summary.
- **Presets.** `export::presets::builtin_presets()` are our own definitions (Match Source adaptive
  H.264 at 0.2 / 0.1 / 0.05 bits per pixel, 1080p / 2160p delivery, vertical 1080×1920, ProRes,
  DNxHR, MXF OP1a (DNxHR HQ / ProRes 422 HQ / H.264) and OP-Atom (DNxHR, Avid), image sequences, GIF, WAV / AIFF). User presets and favourites persist in
  `<data dir>/export-presets.json` (`export.presets.*`).
- **Queue.** `export.queue.add` snapshots the project and the resolved settings (several sequences
  or ranges add several items); `export.queue.start` encodes ready items one after another as
  ordinary jobs, advanced by `Session::poll_persistence` / `pump_jobs` (or synchronously with
  `wait`). Cancel, retry, reorder, remove and clear work per item. The queue is session state, not
  part of the project file.

Video encoders implement `export::VideoEncoder`. Codec crates plug in with `register_encoder` and
`register_audio_encoder`.

Timelines can be exchanged as EDL, FCP7 XML, FCPXML, OTIO, AAF or OMF. `file.import` detects these
formats and merges the result into the project as one undoable step (AAF / OMF embedded audio is
written next to the document first), and `file.exportInterchange`, `file.exportEdl`,
`file.exportFcpxml`, `file.exportOtio`, `file.exportAaf` and `file.exportOmf` write them.
`file.exportInterchange {format}` takes `edl`, `xml` (the default), `fcpxml`, `otio`, `aaf` or `omf`;
any other value is a parameter error and nothing is written. For AAF and
OMF the engine (`engine::aaf_omf`) first prepares the audio the document references: it lists the
used ranges (`interchange::essence::audio_needs`), decodes or renders them (clip effects through the
export audio pipeline), embeds them or writes WAV / AIFF files, and optionally renders a video
mixdown. A nested sequence is exported the way Premiere Pro does it: a composition of its own in
AAF (the media of the clips inside it is prepared with the rest), its sound mixed into the document
in OMF, a nested sequence in FCP7 XML, FCPXML and OTIO, and one `AX` event in an EDL (table in the
[interchange README](../crates/interchange/README.md)).

## 7. Automation surfaces

All of these dispatch the same command ids.

| Surface | Where | Scope |
|---|---|---|
| UI | `ui-egui` menus, shortcuts, panels | `menus::invoke` → engine or UI command |
| CLI | `filmcraft-cli` | `exec <id> key=value…`, `run script.jsonl` (one `{"id","params"}` per line), `inspect`, `import`, `export`, `render`, `probe`; `--save`, `--bridge` |
| Control channel | `filmcraft --control <port>` | JSON lines on loopback TCP: engine commands plus synthetic input, inspection and screenshots of the live UI |
| MCP | `filmcraft-cli mcp` | stdio MCP server: headless in-process session, or `--bridge` to the control channel |

- **Automation ids.** Every interactive widget calls `app.auto.add(id, rect, label)` each frame
  (`crates/ui-egui/src/automation.rs`). Agents click by id, e.g. `tools.Razor`,
  `timeline.clip.<id>`, `effects.item.gaussian_blur`, `panel.Timeline`.
- **UI state** that is not project data (tool, workspace, dock layout, zoom, scroll, monitor
  settings) is in `crates/ui-egui/src/state.rs` as serde structs, so `ui.inspect` and `ui.set` can
  read and write it.

- **Panels with engine data** (M8.9 / M12.6): Lumetri Scopes (`scopes.read` returns the same
  scopes as numbers), Metadata (`metadata.get` / `metadata.set`, one undo step per edit, stored in
  `ProjectItem::metadata`), Events (`events.list` / `events.clear`), Progress (`jobs.list` /
  `jobs.cancel`). Timecode and Reference Monitor are frontend-only views. Their settings are
  `UiState::panels` (`ui.set {"panels": {...}}`).
- **Project panel and Media Browser** (M12.7). View settings (List / Icon / Freeform, thumbnail
  and font size, Preview Area, Hover Scrub), the List view's columns, widths and sort, and ten view
  presets are preferences (`Preferences::project_panel`; `project.view.*`, `project.columns.*`,
  `project.sort`, `project.viewPreset.*`). Freeform positions, Clip Size, stacks and saved
  arrangements are item metadata (`Freeform Position`, `Freeform Stack`,
  `Freeform Arrangement: <name>`; `project.freeform.*`), so they are undoable and saved with the
  project without a schema change. The Media Browser lists directories through
  `Services::list_entries` / `volumes` / `home_dir` (native: `std::fs`; web: the virtual file
  table; tests: a fake filesystem): `engine::media_browser` keeps navigation history and the
  selection in `Session::browser`, Favorites / recent directories / file types / columns in
  `Preferences::media_browser` (`mediaBrowser.*`). Bins opened in a tab or a window, the inline
  rename, dialogs and the hover-scrubbed card are `UiState::project_panel`; the browser's tree
  state is `UiState::media_browser` (`ui.set {"projectPanel": …, "mediaBrowser": …}`).

Protocol reference: [control-protocol.md](control-protocol.md). Agent guide: [agents.md](agents.md).

## 8. Not built yet

The layer table reserves names for crates that don't exist yet: `riff`, `mjpeg`,
`keyframe`, `effects`, `audio` and `playback`. (`platform` exists but holds only OS media FFI;
other OS integration stays in `apps/filmcraft`.)
Until they exist, that work lives elsewhere: keyframes and effect definitions in `project`, effects
and the audio mix in `render`, playback in `ui-egui`, and OS integration (cpal, rfd,
native menus) in `apps/filmcraft`. [ROADMAP.md](../ROADMAP.md) has the milestone status.

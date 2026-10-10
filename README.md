<p align="center">
  <a href="https://getartcraft.com/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/brand/artcraft-logo-white.svg">
      <img alt="ArtCraft" src="docs/brand/artcraft-logo.svg" width="200">
    </picture>
  </a>
</p>


<h1 align="center">FilmCraft</h1>

<p align="center">
  <b>Video editing, color and sound; an open-source, clean-room reimplementation of Adobe Premiere Pro, rebuilt in pure Rust.</b>
</p>

<p align="center">
  An open-source, clean-room take on the Adobe Premiere Pro workflow: native on macOS, Windows and Linux, and in the browser via WebAssembly.<br>
  By the ArtCraft team.
</p>

<p align="center">
  <a href="#license-and-credits"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-8b5cf6"></a>
  <img alt="Written in pure Rust" src="https://img.shields.io/badge/pure-Rust-6a3fd6?logo=rust&logoColor=white">
  <img alt="Runs on macOS, Windows and Linux" src="https://img.shields.io/badge/runs%20on-macOS%20%7C%20Windows%20%7C%20Linux-8b5cf6">
  <a href="#status"><img alt="Status: young and moving fast" src="https://img.shields.io/badge/status-young%20and%20moving%20fast-6a3fd6"></a>
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Join the ArtCraft community on Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<p align="center">
  <a href="https://getartcraft.com/apps/filmcraft"><b>FilmCraft on getartcraft.com</b></a> ·
  <a href="https://getartcraft.com/">ArtCraft</a> ·
  <a href="https://getartcraft.com/apps">All Crafting Apps</a>
</p>

<br>

<p align="center">
  <img src="docs/images/filmcraft-hero.png" alt="FilmCraft in the Color workspace, mid-way through an Apollo 11 documentary cut from NASA footage: the Program monitor on the Saturn V clearing the launch tower with a Launch Complex 39A lower third and an air-to-ground subtitle, Effect Controls with Lumetri and Scale keyframes on the shot, the Lumetri Color panel, bins of NASA selects, and a timeline with 49 picture cuts, B-roll, titles, a caption track, mission audio, a ducked music bed, named markers, a rendered section and live loudness meters" width="100%">
</p>

<p align="center"><sub><i>Apollo 11 - Tranquility</i>: a three-minute documentary edit of NASA's 1969 launch, landing and moonwalk film, with subtitles from the mission transcript. Every frame in these screenshots comes from public-domain footage, decoded, composited and graded by FilmCraft's own code.</sub></p>

> [!NOTE]
> **ArtCraft is a community of artists from all walks of life.** Digital, generative, music,
> games &mdash; if you make things, you're one of us. **[Come say hi on Discord](https://discord.gg/artcraft).**

<p align="center">
  <a href="#edit">Edit</a> ·
  <a href="#color">Color</a> ·
  <a href="#effects-and-motion">Effects</a> ·
  <a href="#audio">Audio</a> ·
  <a href="#titles-and-captions">Titles</a> ·
  <a href="#formats-and-codecs">Formats</a> ·
  <a href="#export">Export</a> ·
  <a href="#interchange">Interchange</a> ·
  <a href="#built-for-agents">Agents</a> ·
  <a href="#get-started">Get started</a> ·
  <a href="#downloads">Downloads</a> ·
  <a href="#documentation">Docs</a> ·
  <a href="#status">Status</a>
</p>

<br>

FilmCraft is a non-linear editor for people who know Premiere: the same panels, workspaces, tools and shortcuts, so your hands already know where everything is. Underneath, it is new from the bitstream up. The H.264, HEVC, ProRes and AAC codecs are our own, written in Rust from the public specifications. A GPU compositor works in linear light. Frame math runs on exact integer time, so edits never drift. And every action in the app is a command that an AI agent can drive as precisely as you can.

<br>

## Edit

<p align="center">
  <img src="docs/images/filmcraft-assembly.png" alt="Assembly workspace: bins of film thumbnails, Carnival of Souls in the Source monitor, a Night of the Living Dead cemetery shot in the Program monitor, and the trailer on a three-track timeline with a Chopin score on A2 and loudness meters" width="100%">
</p>

<p align="center"><sub>The Assembly workspace, cutting a trailer for <i>Night of the Living Dead</i> (1968) with an insert from <i>Carnival of Souls</i> (1962).</sub></p>

**A timeline you already know.** Source and Program monitors, bins with thumbnails, a multi-track timeline with patch and target buttons, sync locks and linked selection. The Editing, Assembly, Color, Effects and Audio workspaces are all there, and every panel docks wherever you want it.

- **Three-point editing.** Mark In and Out in the Source monitor, then insert (`,`) or overwrite (`.`) onto the patched tracks. Lift (`;`) and extract (`'`) take ranges back out.
- **Every trim.** Ripple, roll, slip, slide, rate stretch and razor tools. Trim mode selects edit points as ripple, roll or trim, nudges them a frame at a time (`⌥←` `⌥→`, ×5 with `⇧`), toggles the trim type with `⌃T` and extends them to the playhead with `E`. `Q` and `W` ripple-trim to the playhead. The **Trim Monitor** shows both sides of the edit, and **dynamic trimming** trims live while it plays: `L` forward, `J` back, `K` to stop and commit as one undo step.
- **Exact time.** Every edit is computed on integer ticks: 254,016,000,000 per second, which divides evenly by every common frame rate and sample rate. 23.976, 29.97 drop-frame and 59.94 are exact, not approximate.
- **The details pros rely on.** Markers with colours, names and durations; export sequence review notes with **Markers ▸ Export Markers as CSV…**; add edit (`⌘K`) on one or all tracks; nesting; copy, paste and paste insert; ripple delete and close gap; snapping; unlimited undo with a History panel.
- **Your keys.** A Keyboard Shortcuts editor (`⌥⌘K`) with a drawn keyboard, panel-specific shortcuts, conflict warnings and presets for FilmCraft, Premiere Pro, Final Cut Pro and Avid key layouts.
- **Never lose work.** Saves are atomic, auto-save keeps a rolling set of versions, and a crash-recovery journal written about a second after each edit brings back unsaved changes after a crash or power cut.

<p align="center">
  <img src="docs/images/filmcraft-timeline.png" alt="Timeline close-up: Night of the Living Dead shots with dissolves on V1, a Carnival of Souls insert on V2, the Chopin nocturne as a green waveform on A2, coloured markers along the ruler, and the loudness meter reading momentary, short-term, integrated and true-peak values" width="100%">
</p>

<p align="center"><sub>Up close: dissolves, an insert on V2, the score on A2, coloured markers, and the loudness meters on the right.</sub></p>

<br>

## Color

<p align="center">
  <img src="docs/images/filmcraft-color.png" alt="Color workspace on Charade (1963): Cary Grant and Audrey Hepburn in the Program monitor, the waveform and vectorscope on the left, and the trailer bins and timeline below" width="100%">
</p>

<p align="center"><sub>The Color workspace on <i>Charade</i> (1963), with live scopes beside the Program monitor.</sub></p>

**A complete Lumetri-style grading panel**, in the order a colourist works:

- **Basic Correction:** temperature and tint, exposure, contrast, highlights, shadows, whites, blacks, saturation.
- **Creative:** eight built-in looks with an intensity control, plus faded film, sharpen and vibrance. The looks are colour transforms written in code, not LUT files.
- **Curves:** an RGB curve editor with monotone-cubic interpolation (no overshoot), and hue vs saturation, hue vs hue, hue vs luma, luma vs saturation and saturation vs saturation.
- **Color Wheels:** shadow, midtone and highlight wheels, each with its own lightness control.
- **HSL Secondary:** key a colour by hue, saturation and luma, view the key as a matte, and correct only what you selected.
- **Vignette:** amount, midpoint, roundness and feather.

<table>
  <tr>
    <td width="34%" valign="top" align="center">
      <img src="docs/images/filmcraft-lumetri.png" alt="The Lumetri Color panel on Charade (1963): Basic Correction sliders with coloured temperature and tint tracks, collapsed Creative and Curves sections, shadow, midtone and highlight colour wheels, and HSL Secondary and Vignette toggles"><br>
      <sub><b>The Lumetri Color panel.</b> Basic Correction, Color Wheels and every other section, each with its own on/off switch.</sub>
    </td>
    <td width="66%" valign="top" align="center">
      <img src="docs/images/filmcraft-scopes.png" alt="Lumetri Scopes: a green luma waveform and a vectorscope of a Charade (1963) frame"><br>
      <sub><b>Scopes that tell the truth.</b> The waveform and vectorscope are computed from the graded frame. All grading happens in linear light, in 32-bit float.</sub>
    </td>
  </tr>
</table>

<br>

## Effects and motion

<p align="center">
  <img src="docs/images/filmcraft-effects.png" alt="Effects workspace: Effect Controls with Motion, Opacity, Time Remapping and Lumetri Color, NASA's Earth Views from the ISS in the Program monitor, and the Effects browser open on the Dissolve transitions" width="100%">
</p>

<p align="center"><sub>NASA's <i>Earth Views from the ISS</i> with a look applied, and the Effects browser open on the dissolves.</sub></p>

- **Premiere 26's full Video Effects bin (93 effects in 16 folders), the Legacy bin and the obsolete effects old projects use, plus around 30 transitions.** Blurs (Bokeh, Focus, Compound), keys (Ultra Key, Track Matte), distortions (Corner Pin, Turbulent Displace, Warp Stabilizer), Lights & Glows, Immersive Video (VR) effects on equirectangular footage, Posterize Time, Echo and more. Cross dissolve, dip to black or white, film dissolve, wipes, irises, pushes, slides, zooms, page peel, cube spin and more.
- **Motion and opacity on every clip:** position, scale, rotation, anchor point and anti-flicker, plus 26 blend modes.
- **Keyframes like Premiere's:** linear, Bezier, auto and continuous Bezier, hold, ease in and ease out. Effect Controls shows a keyframe lane for every parameter under a time ruler with the playhead's handle, and each animated parameter opens into **value and velocity graphs** with draggable influence handles. Effect Controls and the Properties panel share the keyframe navigator (◀ ◆ ▶): add or remove the keyframe at the playhead, step to the previous or next one.
- **A GPU compositor** built on wgpu (Metal, Vulkan, DirectX 12, WebGPU). It samples YUV straight from the decoder with footprint supersampling and blends in linear light. A CPU path renders the same frames, and the two are tested against each other.

<p align="center">
  <img src="docs/images/filmcraft-keyframes.png" alt="Effect Controls with the value and velocity graphs of an eased Scale push-in on the Night of the Living Dead title card, the Program monitor showing the title over a country road, and the Properties panel with Transform and Crop" width="100%">
</p>

<p align="center"><sub>An eased push-in on the title card: the value graph, the velocity graph and the Bezier influence handle.</sub></p>

<br>

## Audio

- **Loudness meters to broadcast standards.** Momentary, short-term and integrated loudness and true peak to ITU-R BS.1770 and EBU R128, alongside the peak meters. Our integrated reading matches ffmpeg's `ebur128` filter to the tenth of a LU.
- **Clip effects on our own DSP library:** parametric EQ, high-pass, low-pass and band-pass filters, dynamics, a true-peak limiter, delay, reverb, DeNoise, DeHummer, invert and pitch shift. Parameters can be keyframed. Effects stay continuous across scrubbing, playback and export, with latency compensated.
- **Audio Track Mixer and Audio Clip Mixer:** faders, pan, mute, solo, five insert slots per track (pre- or post-fader), sends, submixes and a Mix track, all sample-accurate and latency-compensated. 24 tracks with three effects each mix about six times faster than real time on one core.
- **Automation like a console:** Read, Latch, Touch and Write modes recorded live from the mixer during playback, shown and edited as track keyframes on the timeline. Plus the Audio Gain dialog (set, adjust, normalize) and constant-power crossfades.
- **Audio-clock playback.** The sound card is the master clock, so picture follows sound and never the other way round.

<br>

## Titles and captions

- **A real text engine:** OpenType shaping with kerning and ligatures, bidirectional text, line breaking, tracking and leading, drawn in linear light and sharp at any scale or rotation.
- **Type, Shape and Pen tools** on the Program monitor: click to type, edit with a caret and selection, drag out rectangles, ellipses and paths. Graphic clips hold text and shape layers with fill, strokes, background and shadow, all keyframable, and edited in the Properties panel.
- **Captions:** caption tracks in Subtitle, CEA-608, CEA-708 and Teletext formats; import and export SRT, WebVTT and SCC with frame-exact timing; edit captions in the Text panel and burn them into exports.
<br>

## Formats and codecs

No FFmpeg inside. The video codecs, AAC, Opus and the containers are our own Rust code, implemented from the public ITU-T, ISO and IETF specifications and tested frame by frame against ffmpeg as an external oracle.

| | Decode | Encode | Notes |
|---|:---:|:---:|---|
| **H.264 / AVC** | ✓ | ✓ | Decoder bit-exact on 37+ streams, 500–600 fps at 1080p. Encoder: High, Main and Baseline profiles, CABAC, B-frames, CRF/CBR/VBR/2-pass |
| **HEVC / H.265** | ✓ | | Main and Main 10, bit-exact on 41 streams (tiles, WPP, PCM, long-term references), about 225 fps at 1080p |
| **Apple ProRes** | ✓ | ✓ | Decodes 422 Proxy to 4444 XQ; export writes 422 HQ |
| **AAC-LC** | ✓ | ✓ | |
| **VP9** | ✓ | | Profiles 0–3, 8/10/12-bit, 4:2:0 to 4:4:4, tiles and superframes; bit-exact on 50+ streams; in WebM/Matroska and MP4 |
| **Opus** | ✓ | | SILK, CELT, hybrid and multistream surround; range-exact on every RFC 8251 conformance vector; in WebM/Matroska and MP4 |
| **AV1** | ✓ | | Main profile, 8/10-bit 4:2:0 and monochrome, every coding tool incl. film grain, superres and spatial layers; bit-exact with libdav1d on the libaom vectors tested; in MP4 and WebM/Matroska |
| **Avid DNxHD / DNxHR** | ✓ | ✓ | All SMPTE ST 2019-1 CIDs (LB to 444, 8/10/12-bit); export writes DNxHR in MOV |
| **MJPEG, PCM** | ✓ | ✓ | |
| **MP3, FLAC, ALAC, Vorbis** | ✓ | | Via the [symphonia](https://github.com/pdeljanov/Symphonia) crate (MPL-2.0) for now, to be replaced by our own |
| **MP4 / MOV** | ✓ | ✓ | Fragmented MP4, edit lists, timecode tracks |
| **Matroska / WebM** | ✓ | | Lacing, Cues, header stripping, HDR colour metadata |
| **Stills** | ✓ | ✓ | Import PNG, JPEG, GIF, WebP, TIFF and BMP; export PNG sequences and animated GIF |

Also imported: MPEG-2 / MPEG-1 video, AC-3, MP2, MXF (OP1a / OP-Atom), MPEG transport and program streams (AVCHD, broadcast, DVD), Ogg and image sequences. Not yet: AV1 export, camera RAW, E-AC-3. Hardware decoding works on macOS (VideoToolbox), Windows (Media Foundation) and, for H.264 and HEVC, Linux (VA-API); H.264 export can use NVIDIA's encoder on Windows and Linux (opt-in), and H.265 (HEVC Main, 8-bit) export works through it (NVENC on Windows, VideoToolbox on macOS) ([#30](https://github.com/storytold/filmcraft/issues/30)).

<br>

## Export

<p align="center">
  <img src="docs/images/filmcraft-export.png" alt="Export mode: destinations for Media File, YouTube, Vimeo, TikTok, Instagram and FTP on the left, a Night of the Living Dead frame in the preview, and H.264 settings on the right (1440x1080 at 23.976 fps, 48000 Hz stereo, entire sequence)" width="100%">
</p>

<p align="center"><sub>Export mode, set to write the trailer as H.264 MP4.</sub></p>

- **H.264 MP4 with AAC, using our own encoders.** A 6-second 960×540 render takes 1.3 seconds and decodes in ffmpeg without a single warning, with strict error detection on.
- **Also:** Apple ProRes 422 HQ and Motion JPEG in QuickTime, MXF OP1a and Avid-style OP-Atom (DNxHR, ProRes or H.264 with PCM and start timecode), PNG sequences, animated GIF and WAV.
- **Background jobs** with progress and cancel, so you keep editing while it renders.
- **Render previews:** the render bar marks segments green, yellow or red; rendered previews are cached by content, so an edit only invalidates what it touches and undo brings the green back.

<br>

## Interchange

Move timelines between FilmCraft and every other editor:

- **Final Cut Pro 7 XML (xmeml):** the format Premiere and DaVinci Resolve exchange. It carries nested sequences, transitions, generators, Motion, Opacity, Time Remap and audio levels, with keyframes.
- **FCPXML 1.9–1.11:** the spine, connected clips as lanes, transitions, retiming and compound clips.
- **OpenTimelineIO:** the open interchange format of the film industry, round-tripping every FilmCraft detail through `metadata.filmcraft`.
- **CMX 3600 EDL:** the oldest format still in use, with drop-frame timecode, dissolves, wipes, speed changes (`M2`) and one EDL per track.
- **AAF (Edit Protocol):** for Avid Media Composer and Pro Tools. Video and audio tracks, dissolves and dips, clip volume with keyframes, markers and source timecode; audio embedded or as separate WAV / AIFF files, trimmed with handles, with clip effects rendered in and broken out to mono, plus an optional video mixdown. Import reads AAF back, extracting embedded audio.
- **OMF 2.0:** the audio-post handoff: the audio tracks with crossfades and gain, sample-accurate, with the audio encapsulated or alongside.

Import merges the document's bins, media and sequences into your project as one undoable step, and links each media file it finds on disk.

<br>

## Built for agents

Every menu item, button, slider and drag in FilmCraft is a **command** with an id, typed parameters and an enabled state. There are more than 650 engine commands (`filmcraft-cli commands` lists them), with the rest of Premiere's catalogue on the way. The UI, the CLI, a JSON control channel and an **MCP server** all dispatch the same commands, so Claude or any agent can cut, trim, grade, mix and export exactly the way a person does. The UI can also be driven at the level of mouse and keyboard: every widget has an automation id, and agents can click, drag, type and take screenshots.

```jsonc
// over the control channel (JSON lines on TCP) or as MCP tool calls
{"method": "engine.execute", "params": {"command": "timeline.place",
  "params": {"item": 1, "track": "V1", "seconds": 0, "sourceIn": 284298240000000, "duration": 1270080000000}}}
{"method": "engine.execute", "params": {"command": "effects.setParam",
  "params": {"clip": 87, "effect": "lumetri", "param": "curve_luma", "value": [[0,0],[0.25,0.21],[0.75,0.82],[1,1]]}}}
{"method": "ui.screenshot", "params": {"path": "frame.png"}}
```

The trailer and the grades in these screenshots were built exactly this way, by an agent driving the running app.

<br>

## Everywhere

- **Native** on macOS, Windows and Linux, with a native macOS menu bar.
- **The web:** the same engine and UI run in the browser via WebAssembly (`apps/filmcraft-web`, see [docs/web.md](docs/web.md)); every release ships it as `filmcraft-web-<version>.zip`.
- **Swappable UI.** The interface is one crate (`ui-egui`) over the engine, so a different front end can replace it without touching editing logic.

<br>

## Get started

```sh
cargo run --release -p filmcraft                           # the desktop app, with a demo project
cargo run --release -p filmcraft -- --control 9876         # plus the JSON-lines control server
cargo run --release -p filmcraft-cli -- commands           # list every engine command
cargo run --release -p filmcraft-cli -- mcp                # MCP server (headless)
```

Japanese text in the interface and in titles comes from [craft-fonts](https://github.com/storytold/craft-fonts), an optional build input (release builds always include it; without it FilmCraft uses its own and the system's fonts):

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR="$PWD/../craft-fonts" cargo run --release -p filmcraft
```

The control protocol is documented in [docs/control-protocol.md](docs/control-protocol.md). Stuck, or want to show what you made? Ask in [Discord](https://discord.gg/artcraft).

## Documentation

| | |
|---|---|
| [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/contributing.md](docs/contributing.md) | Setup, quality gates, commit conventions, how to add commands, effects, codecs, panels and assets |
| [AGENTS.md](AGENTS.md) | The rules every contributor must follow: assets, clean room, licences |
| [docs/architecture.md](docs/architecture.md) | Layers, data model, time base, command system, render and export pipeline |
| [docs/testing.md](docs/testing.md) | Unit, property and ffmpeg-oracle tests, accuracy criteria, benchmarks |
| [docs/agents.md](docs/agents.md) | Driving FilmCraft over MCP and the control channel; how agents develop it |
| [docs/control-protocol.md](docs/control-protocol.md) | Control-channel and MCP reference |
| [docs/project-files.md](docs/project-files.md) | `.fcproj` format, schema migrations, auto-save and crash recovery |
| [docs/graphics.md](docs/graphics.md) · [docs/captions.md](docs/captions.md) | Text engine, graphic clips and tools; caption tracks and formats |
| [ROADMAP.md](ROADMAP.md) | Honest assessment, what's missing, milestones and estimates |

## Status

FilmCraft is young and moving fast. Editing, trimming, multicam, colour, keyframes, effects, titles, captions, mixing, codecs and export work today.

We track two numbers ([ROADMAP.md](ROADMAP.md#honest-assessment-2026-10-05)):

- **Feature checklist: ~87%.** Premiere Pro's menu items, effects, transitions, panels and formats that exist in FilmCraft.
- **Ready for real work: ~50–60%.** Our honest estimate of how close FilmCraft is to replacing Premiere on real projects.

The biggest gaps today:

- **Speed on big footage.** Hardware decoding works on macOS and Windows, and for H.264 and HEVC on Linux (VA-API; VP9 and AV1 decode in software there). Blend modes and the most common effects run on the GPU, but Lumetri, keys and export rendering still run on the CPU; H.264 encoding can use the hardware encoder on macOS and on Windows and Linux with an NVIDIA GPU (opt-in, Export ▸ Hardware encoding), and H.265 export is hardware-only on both (HDR sequences export Main 10 PQ / HLG with NVENC; VideoToolbox writes 8-bit SDR) ([#30](https://github.com/storytold/filmcraft/issues/30)).
- **No plugins.** No VST3 / Audio Units or OpenFX hosting.
- **Delivery codecs.** H.264 is our only software delivery-codec export; H.265 exports only through a hardware encoder (NVIDIA on Windows: Main 8-bit SDR and Main 10 HDR PQ / HLG; VideoToolbox on macOS: 8-bit Main, SDR); no AV1 export yet.
- **Real-world media and platforms.** Our decoders are bit-exact on conformance streams, but camera and phone files in the wild are less tested. Windows and Linux get far less testing than macOS.
- **AI features.** Few so far; speech to text is optional and off by default.

Bug reports with real footage are the most useful thing you can send us: [open an issue](https://github.com/storytold/filmcraft/issues) or tell us in [Discord](https://discord.gg/artcraft).

## Architecture

A layered Cargo workspace:

| Layer | Crates |
|---|---|
| Codecs and containers | `h264`, `h264enc`, `hevc`, `vp9`, `prores`, `aac`, `opus`, `isobmff`, `matroska`, `bitstream` |
| Foundations | `time`, `geom`, `color`, `frame`, `media`, `text`, `project`, `audio-dsp` |
| Editing and interchange | `edit`, `codecs`, `captions`, `format` (project files), `interchange` |
| Rendering and output | `render`, `gpu`, `export` |
| Engine | `engine`: command registry, session, undo, jobs |
| Front ends | `ui-egui`, `automation` (MCP), `apps/filmcraft`, `apps/filmcraft-cli` |
| Test support | `testkit` (ffmpeg oracles, fixtures), `golden` (golden-image tests) |

Nothing below the front ends depends on a UI toolkit or OS API. `cargo xtask ci` checks formatting, lints, tests, the layering rules, asset attribution and the wasm build.

## Downloads

**New to FilmCraft?** Download it from the [FilmCraft page on getartcraft.com](https://getartcraft.com/apps/filmcraft). That's the easiest way to install it.

**Want a specific build or format?** On GitHub, the [latest release](https://github.com/storytold/filmcraft/releases/latest) has every build listed below, and [all releases](https://github.com/storytold/filmcraft/releases) has earlier versions and their notes. `<ver>` in the file names is the version number, and `SHA256SUMS.txt` lists a checksum for every file.

### Windows

| Build | Installer | Portable |
|---|---|---|
| x64 (64-bit Intel/AMD) | `filmcraft-<ver>-windows-x64.msi` | `filmcraft-<ver>-windows-x64-portable.zip` |
| arm64 (Snapdragon and other ARM PCs) | `filmcraft-<ver>-windows-arm64.msi` | `filmcraft-<ver>-windows-arm64-portable.zip` |
| x86 (32-bit) | `filmcraft-<ver>-windows-x86.msi` | `filmcraft-<ver>-windows-x86-portable.zip` |

Installers and executables are code-signed.

**If the app doesn't open on Windows:** the desktop app initializes only Vulkan and DirectX 12 by
default, not OpenGL. Letting wgpu also create an OpenGL instance can crash some graphics drivers (AMD's
`atio6axx.dll`) before the window appears, so the app would flash in Task Manager and quit.
`WGPU_BACKEND` overrides the default for troubleshooting (for example `dx12` or `vulkan`). In
PowerShell, from the folder containing the executable:

```powershell
$env:WGPU_BACKEND = "vulkan"
& .\filmcraft.exe
Remove-Item Env:WGPU_BACKEND                     # restore the default for later launches
```

An explicit `gl` override can bring the driver crash back on affected systems. The macOS, Linux
and web backend defaults are unchanged.

A PC whose GPUs were installed from different driver packages (a laptop with an integrated and a
discrete AMD GPU, each bringing its own Vulkan driver) lists one GPU twice, and drawing through the
stale entry crashes inside the driver (`amdvlk64.dll`, exit code `0xc0000005`) as the window opens.
FilmCraft draws with the entry that exposes the most features, the newer driver, and notes the
duplicate in `%APPDATA%\FilmCraft\Logs\filmcraft.log`; installing one driver package for all GPUs
removes it. Nothing is tied to one GPU or driver version: with one driver per GPU the choice is
wgpu's usual one. `WGPU_ADAPTER_NAME` (part of the adapter's name, backend or driver text as the log
writes them, any case) picks an adapter by hand:

```powershell
$env:WGPU_ADAPTER_NAME = "Radeon(TM) Graphics"   # draw with the integrated GPU
& .\filmcraft.exe
Remove-Item Env:WGPU_ADAPTER_NAME
```

### macOS

| Build | File | Notes |
|---|---|---|
| App, universal (Apple silicon + Intel) | `filmcraft-<ver>-macos-universal.dmg` | Signed and notarized |
| Command-line tool, universal | `filmcraft-cli-<ver>-macos-universal.zip` | Signed and notarized |

### Linux

| Format | x86_64 | aarch64 (ARM64) | Notes |
|---|---|---|---|
| AppImage | `filmcraft-<ver>-linux-x86_64.AppImage` | `filmcraft-<ver>-linux-aarch64.AppImage` | Runs anywhere; updates itself with [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) (`.zsync` files) |
| Flatpak | `filmcraft-<ver>-linux-x86_64.flatpak` | `filmcraft-<ver>-linux-aarch64.flatpak` | Sandboxed; `flatpak install --user <file>` |
| Debian/Ubuntu | `filmcraft-<ver>-linux-x86_64.deb` | `filmcraft-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `filmcraft-<ver>-linux-x86_64.rpm` | `filmcraft-<ver>-linux-aarch64.rpm` | |
| Tarball | `filmcraft-<ver>-linux-x86_64.tar.gz` | `filmcraft-<ver>-linux-aarch64.tar.gz` | Unpack and run `./install.sh` |

The tarball installer places FilmCraft, its command-line tool and desktop integration in
`~/.local`, without administrator permissions. For all users, run
`sudo ./install.sh --prefix /usr/local` instead. Running the installer again updates the installation.

**Gentoo (community-maintained):** the [::snakebyte overlay](https://github.com/switch87/snakebyte-overlay)
packages the Linux release as `media-video/filmcraft-bin`. It is maintained by the community, not by the
FilmCraft team, so report packaging problems to the overlay:

```sh
eselect repository add snakebyte git https://github.com/switch87/snakebyte-overlay.git
emaint sync -r snakebyte
echo 'media-video/filmcraft-bin ~amd64' >> /etc/portage/package.accept_keywords/filmcraft
emerge --ask media-video/filmcraft-bin
```

### FreeBSD

| Build | File |
|---|---|
| x86_64 | `filmcraft-<ver>-freebsd-x86_64.tar.gz` |

### Web (WebAssembly)

| Build | File | Notes |
|---|---|---|
| Static site | `filmcraft-web-<ver>.zip` | Runs in a modern browser; host it on any static server |

## The Crafting Apps

FilmCraft is one of the **Crafting Apps**: free, open-source creative tools from the
[ArtCraft](https://getartcraft.com/) team, each written from scratch in Rust and each able to
stand on its own.

| | App | What it's for | Code | Learn more |
|:-:|---|---|---|---|
| <img src="https://raw.githubusercontent.com/storytold/photocraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.photocraft.png" alt="" width="32" height="32"> | **PhotoCraft** | Image editing: layers, masks, type and real PSD files | [GitHub](https://github.com/storytold/photocraft) | [Website](https://getartcraft.com/apps/photocraft) |
| <img src="https://raw.githubusercontent.com/storytold/vectorcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.vectorcraft.png" alt="" width="32" height="32"> | **VectorCraft** | Vector illustration | [GitHub](https://github.com/storytold/vectorcraft) | [Website](https://getartcraft.com/apps/vectorcraft) |
| <img src="https://raw.githubusercontent.com/storytold/filmcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.filmcraft.png" alt="" width="32" height="32"> | **FilmCraft** | **Video editing, color and sound · you are here** | [GitHub](https://github.com/storytold/filmcraft) | [Website](https://getartcraft.com/apps/filmcraft) |
| <img src="https://raw.githubusercontent.com/storytold/lightcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" alt="" width="32" height="32"> | **LightCraft** | Photo library and raw development | [GitHub](https://github.com/storytold/lightcraft) | [Website](https://getartcraft.com/apps/lightcraft) |
| <img src="https://raw.githubusercontent.com/storytold/pdfcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.pdfcraft.png" alt="" width="32" height="32"> | **PdfCraft** | Reading, organizing and protecting PDFs | [GitHub](https://github.com/storytold/pdfcraft) | [Website](https://getartcraft.com/apps/pdfcraft) |
| <img src="https://raw.githubusercontent.com/storytold/effectcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.effectcraft.png" alt="" width="32" height="32"> | **EffectCraft** | Motion graphics and visual effects | [GitHub](https://github.com/storytold/effectcraft) | [Website](https://getartcraft.com/apps/effectcraft) |
| <img src="https://raw.githubusercontent.com/storytold/designcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.designcraft.png" alt="" width="32" height="32"> | **DesignCraft** | Page layout and publishing | [GitHub](https://github.com/storytold/designcraft) | [Website](https://getartcraft.com/apps/designcraft) |

And [**ArtCraft**](https://getartcraft.com/) itself, our AI image and video studio for artists who want real control.

<br>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Join the ArtCraft community on Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<h3 align="center">Come make things with us</h3>

<p align="center">
  Our Discord is where artists of every kind hang out: people who paint, shoot, draw, cut film,
  set type, and people still figuring out what they like to make. Share what you're working on,
  ask for help, tell us what's broken, or tell us what you wish these tools could do.
  Whatever your medium and however long you've been at it, you're welcome here.
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><b>discord.gg/artcraft</b></a> ·
  <a href="https://getartcraft.com/">getartcraft.com</a> ·
  <a href="https://getartcraft.com/apps">The Crafting Apps</a> ·
  <a href="https://getartcraft.com/apps/filmcraft">FilmCraft</a>
</p>

<br>

## License and credits

FilmCraft is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Copyright (c) 2026 ArtCraft Team and the FilmCraft contributors. Required notices are in [NOTICE](NOTICE).

Bundled fonts, icons, images and other assets keep their own open licenses; each one is listed
with its author, source and license in [ATTRIBUTION.md](ATTRIBUTION.md). Builds made with
[craft-fonts](https://github.com/storytold/craft-fonts) (all official releases) also embed its fonts
(OFL-1.1), listed in its [ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md).

**Footage and music in the screenshots:** NASA's Apollo 11 film and television footage from [images.nasa.gov](https://images.nasa.gov) (launch, Launch Control Center, lunar surface and recovery) and the Apollo 11 air-to-ground voice transcript, all US Government works in the public domain (NASA does not endorse this project); *Night of the Living Dead* (1968), *Carnival of Souls* (1962) and *Charade* (1963), all in the US public domain; *Earth Views from the ISS* by NASA; Chopin's Nocturne Op. 48 No. 1 and Ballade No. 1, performed for Musopen and released under CC0. The media itself is not in this repository. Sources and details for every asset are in [ATTRIBUTION.md](ATTRIBUTION.md).

FilmCraft is an independent implementation. It contains no Adobe code, icons, images, presets or LUTs, and no GPL or LGPL code; every icon is drawn in code and every asset is openly licensed and attributed ([AGENTS.md](AGENTS.md)), apart from the ArtCraft name and logos, which are trademarks of the ArtCraft Team used under [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt). ffmpeg is used only as an external test oracle.

The ArtCraft name, wordmark and logos in [`docs/brand/`](docs/brand/) are trademarks of the
ArtCraft Team and are not covered by this license. They may be used only unmodified, and only as
part of this repository and FilmCraft, under [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt).
Forks and modified versions must remove them.

<sub>Adobe, Photoshop, Illustrator, Premiere Pro, Lightroom, Acrobat, After Effects and InDesign are trademarks or registered trademarks of Adobe Inc. in the United States and/or other countries. FilmCraft is an independent, open-source project and is not affiliated with, sponsored by or endorsed by Adobe Inc.; these names are used only to describe the workflows it is compatible with.</sub>

<br>

<p align="center">
  <a href="https://getartcraft.com/"><img alt="ArtCraft" src="docs/brand/artcraft-mark.svg" width="28"></a><br>
  <sub>Made by the <a href="https://getartcraft.com/">ArtCraft</a> team and community.</sub>
</p>

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=storytold/filmcraft&type=Date&legend=top-left)](https://www.star-history.com/?repos=storytold%2Ffilmcraft&type=date&legend=top-left)

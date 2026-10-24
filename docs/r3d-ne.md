# Nikon ZR R3D NE (optional, through RED's SDK)

**Status:** optional, off by default, Windows x64 and Linux x64. Proposed under
[ADR 0002](adr/0002-optional-vendor-sdk-helpers.md) for #383; nothing here applies until the
project owners accept it.

R3D NE, the RAW format of Nikon's ZR camera, has no public specification. RED's R3D SDK is the
only decoder. FilmCraft can use it when you supply the SDK yourself and build with the `red-sdk`
feature: the SDK is compiled into a small helper program, `filmcraft-red-helper`, that runs as a
separate process; the editor itself stays pure, safe Rust apart from one module that maps the
helper's frame memory read-only. Default builds do not need, link, download or ship anything of
RED's, and a `.r3d` file opened in them reports which build feature it needs.

This is not a clean-room implementation of REDCODE. Read [Licensing](#licensing) before you pass
an SDK-enabled build to anyone.

Contents: [Setup](#setup) · [Behaviour](#behaviour) · [Source Settings](#source-settings-the-camera-raw-panel) ·
[Limits](#limits) · [The helper protocol](#the-helper-protocol-fcrd9) · [Tests](#tests) ·
[Measurements](#measurements) · [Licensing](#licensing)

## Setup

You need Rust 1.95 or newer, a C++17 compiler (the Visual Studio C++ build tools on Windows,
`g++` and `uuid-dev` on Linux), and RED R3D SDK 9.2.1, downloaded from RED under RED's own
agreement and extracted somewhere outside the source tree. `RED_R3D_SDK_DIR` names that folder;
it must contain `Include`, `Lib` and `Redistributable`.

```bash
export RED_R3D_SDK_DIR=<extracted R3D SDK 9.2.1>
cargo run -p filmcraft --features red-sdk
# inspect a clip without a window
cargo run -p filmcraft-cli --features red-sdk -- probe <clip>.R3D
```

(PowerShell: `$env:RED_R3D_SDK_DIR = '<extracted R3D SDK 9.2.1>'`.)

`crates/platform/build.rs` compiles `crates/platform/src/red/helper.cpp`, an original C++ program,
against the SDK's headers and static library, and copies the SDK's four runtime libraries
(`REDR3D`, `REDDecoder`, `REDCuda`, `REDOpenCL`) beside the helper in Cargo's build output.
Without `RED_R3D_SDK_DIR` the feature build stops with a message saying so; on any other target it
stops with "red-sdk currently supports Windows x64 (MSVC) and Linux x64 (GNU) only". Nothing of the
SDK is ever committed: no headers, libraries, documentation, sample code or footage.

| | Windows x64 (`x86_64-pc-windows-msvc`) | Linux x64 (`x86_64-unknown-linux-gnu`) |
| --- | --- | --- |
| Compiler | MSVC `/std:c++17 /EHsc /MD /O2` | the system C++ compiler, `-std=c++17 -O2 -fPIC -pthread` |
| SDK library | `Lib/win64/R3DSDK-2017MD.lib` | `Lib/linux64/libR3DSDKPIC-cpp11.a`, `-ldl -luuid` |
| Runtime libraries | `Redistributable/win/*-x64.dll` | `Redistributable/linux/*-x64.so` |
| Helper start | hidden (`CREATE_NO_WINDOW`) | runtime directory first on `LD_LIBRARY_PATH`; SIGPIPE ignored |
| Reply pipe | a named pipe the helper creates | the helper's stdout |
| Frame memory | a named, pagefile-backed section | a POSIX shared memory object (`/dev/shm`) |

On Linux, SDK 9.2.1's `FinalizeSdk()` faults inside `REDR3D-x64.so` once the CUDA decoder has
processed a frame (deterministic; CPU-only sessions are fine), so a GPU helper on Linux skips it at
exit. The process ends right after.

**Finding the helper.** The app looks for the helper beside its own executable, then in its Cargo
build directory. `FILMCRAFT_R3D_HELPER` names another helper and `RED_R3D_LIBRARY_PATH` another
directory for the runtime libraries; both select executable code, so point them only at files you
trust. Keep the SDK's header, static library and runtime libraries at the same version, and
rebuild the app and the helper together (see [the protocol](#the-helper-protocol-fcrd9)).

**GPU.** Image processing runs on the GPU: CUDA when available, else OpenCL. RED recommends 12 GB
of video memory; 10 GB works at half resolution with the default pools. `FILMCRAFT_R3D_PROCESSING=cpu`
forces the CPU for comparisons; it is a diagnostic, not a setting.

**Logs.** The app log records each helper's start (process id, GPU or CPU, path), its readiness
(SDK start-up time, processing device, slots, threads), every failure with its reason, idle
closes and each exit status. The helper's own stderr is forwarded line by line as warnings.
`FILMCRAFT_R3D_TIMING=1` adds per-frame timings (submit to callback, reply write, slot hold, the
app's round trip and conversion) at debug level (`RUST_LOG=filmcraft_platform::red=debug`). More
than two helper processes alive at once is a bug.

## Behaviour

**Import.** `.r3d` files (any case) import through every import path: File ▸ Import, the Media
Browser, drag and drop, the CLI and the control channel. Each Nikon file is one clip; consecutive
file numbers are never joined. Opening reads the clip through a local file path without reading the
file into memory; memory-only and browser readers answer "unsupported". Metadata (size, rational
frame rate, duration, start timecode, audio layout, the recorded lens squeeze, the as-shot
development) comes from the helper and is range-checked before the clip is accepted.

**Picture.** RED's IPP2 pipeline develops the frame on the GPU. The app receives linear light as
RGBA half floats (`PixelData::RgbaF16`), which the GPU compositor uploads as they are and the CPU
paths widen to f32 on demand. Seeking addresses frames directly; times before the first frame clamp
to it and times past the end to the last frame.

**Sound.** Native 32-bit float audio keeps samples outside ±1; integer audio arrives as the SDK's
big-endian, MSB-aligned samples. Both become planar f32, resampled linearly when the sequence rate
differs. Requests outside the clip are silence.

**Preview sizes.** A preview decodes at the smallest of full, half, quarter, eighth and sixteenth
resolution that still covers it, never larger than the clip's decode quality allows; full-size
requests (an export, a monitor at 100 %) always decode at full resolution. The SDK rounds a
dimension that does not divide down: a 6048×3402 clip decodes at 3024×1701, 1512×850, 756×425 and
378×212. That rule was measured (decoding into an oversized buffer filled with a sentinel and
scanning what changed, on the CPU and GPU paths), because the headers do not state it and the
helper's buffer and the app's pixel count must agree to the byte:

| Mode | Exact fraction | Written |
| --- | --- | --- |
| Half (Premium and Good) | 3024×1701 | 3024×1701 |
| Quarter Good | 1512×850.5 | 1512×850 |
| Eighth Good | 756×425.25 | 756×425 |
| Sixteenth Good | 378×212.625 | 378×212 |

Rows are packed at `floor(W / d) × 6` bytes (the GPU job's row pitch is ignored when larger), and
nothing is written past the frame. The helper sizes each buffer that way and writes a half-float
NaN into the last pixel before each decode, so a decode that wrote less is rejected by the app's
non-finite check instead of shown. Re-run this measurement when the SDK version changes.

**Two helpers serve every clip.** The SDK runs in at most two processes, whatever the number of
clips:

- The **GPU helper** decodes the frames monitors, playback and export ask for, into five shared
  frame slots. It starts on the first such frame, and a clip is opened in it (~6 ms inside the
  running SDK) the first time that clip is asked for a frame. A cut in a timeline of a hundred clips
  is a clip already open, not a process start.
- The **CPU helper** serves audio, thumbnails (Project panel, Media Browser, timeline), the metadata
  probe at import, and frames while the GPU helper is unavailable. It runs four threads fed from
  two queues, audio first, so a thumbnail never makes the sound wait.
- **Still frames** (playback stopped: a scrub, a click on the ruler, a paused monitor) at a quarter
  of the size or smaller are decoded by the CPU helper, which answers an eighth in ~35 ms where one
  frame through the GPU pipeline takes ~160–200 ms at any size. Such a frame also starts or keeps the
  GPU helper warm, so playback after a scrub starts without dropped frames. Playback, export and
  full or half still frames stay on the GPU. The hint comes from the frame server
  (`filmcraft_media::cancel::with_still`); other sources ignore it.
- A GPU helper that fails to become ready (no CUDA device, a full GPU, a missing runtime library)
  gets one warning naming the reason; frames then come from the CPU helper and the next GPU start
  waits for a back-off that doubles from two seconds to a minute and clears on success.
- A helper that fails or exits is started again by the next request, and the frames that were in
  flight are asked again of the new helper once. A helper idle for two minutes (GPU) or one minute
  (CPU) is closed and gives its memory back; an orderly close lets it exit by itself with status 0,
  so the log tells a failed helper from a closed one.
- An error answers its own request only (an unreadable clip, a refused setting); the helper serves
  the other clips on. A native crash, a closed pipe, an invalid reply or a request unanswered for
  120 seconds fails the helper and becomes a media error for the requests in flight, which the media
  pool shows as damaged media until frames arrive again. Quitting leaves no helper process and, on
  Linux, nothing under `/dev/shm`.

**Playback.** The source tells the frame server that its frames decode independently and what rate
the helper sustains (`MediaSource::frame_delivery`), and the server plans which frames to ask for
(`throughput_plan`, [architecture.md](architecture.md)): m of every n, the densest the measured
capacity covers, so a clip that decodes a little slower than the sequence rate plays an even subset
of its frames instead of freezing. That planner is generic and lives in its own change.

## Source Settings: the Camera Raw panel

Clip ▸ Source Settings… on a camera RAW clip is a Camera Raw panel in three columns
(`clip.sourceSettings`; every control has the automation id `sourceSettings.<id>`). File ▸ Project
Settings ▸ Camera RAW… holds the same controls for the project (`file.projectSettings.cameraRaw`),
without Decode Using and with a Standard button. The commands and their parameters are in
[control-protocol.md](control-protocol.md).

**Decode Using.** A clip develops with the project's Camera RAW settings (Decode Using ▸ Project; its
development controls are greyed out) or with settings of its own (Decode Using ▸ Clip, starting from
the project's). A new project's settings are the standard: IPP2 into Rec. 709 with the BT.1886 curve
and the SDK's default output tone map and highlight roll-off (Medium and Soft in SDK 9.2.1, which the
helper reports and a real-clip test checks), as shot. The project stores a clip's settings only once
it has its own (`interpret.develop`) and the project's in `settings.camera_raw` (project schema v16;
a project from before reads as scene-linear Rec. 709, what such clips decoded with, so nothing
changes picture on opening).

**Column 1: decode and colour pipeline.**

- *Decode Quality*: Use project setting, Full Res. Premium, Half Res. Premium, Half Res. Good, Quarter /
  Eighth / Sixteenth Res. Good. It caps the size a preview decodes at (a half-size preview at Quarter
  decodes a quarter and is scaled up); a full-size request always decodes at full resolution,
  premium, so the setting never lowers an export.
- *Color Science*: IPP2 (shown, fixed: the SDK develops R3D NE with IPP2 only).
- *Color Space* and *Gamma Curve*: Rec. 709, Rec. 2020, P3 D65 and REDWideGamutRGB; Linear, BT.1886,
  sRGB, Log3G10, ST 2084 (PQ) and Hybrid Log-Gamma: the IPP2 values the compositor can interpret.
  Display curves go with the display spaces, Log3G10 with REDWideGamutRGB, ST 2084 and HLG with
  Rec. 2020, Linear with any; choosing one that does not fit the other brings a fitting one along.
- *Blend Type* and *Blend Bias* (HDRx): greyed out with the reason. R3D NE records no HDRx track.
- *Apply Metadata Curves*, *Apply Creative LUT* (greyed out on a clip that carries none) and *Apply
  CDL*: what the clip or its RMD sidecar records. They act on a display or log curve only; the SDK's
  linear half-float output ignores curves and CDL.

**Column 2.** *ISO* (one of the SDK's 28 values, 50–25600), *Output Tone Map* and *Highlight Roll Off*
(for a display curve), *HDR Peak Nits* (ST 2084 only, 100–10000 within the SDK's limits), *Chroma
Noise Reduction* and *Flashing Pixel Adjust*.

**Column 3.** *Color Temp* (1700–100000 K), *Tint* (−100..100) and *Exposure Adjust* (−8..8 stops, two
decimals). With ISO these are the RAW adjustments IPP2 applies before development. Each shows the
clip's recorded value (as shot) until set, and its As Shot button puts it back. A value outside the
SDK's lists or limits is refused, not clamped. The colour effect's own temperature, tint and exposure
still act after development, in the working space.

**Buttons.** *Update RMD* writes the clip's settings into its RMD sidecar beside the clip, and only
when pressed (opening, previewing or saving never writes one); it is not an undo step, since it
changes a file. *Use Changes* applies this clip's settings to every selected clip. *Use Settings* puts
the clip back to what it records. When a clip has an RMD sidecar, its recorded values are the
sidecar's, at import too, and the panel names the sidecar in effect.

**Live preview.** Every change is applied while the dialog is open, so the monitors develop the
current frame as a control is dragged; OK keeps the values, Cancel or Escape puts back the ones the
dialog opened with, and one dialog is one undo step however many changes it made. During a drag at
most one change is outstanding: the next waits until the Program monitor shows the last (or a second
passes). The dialog does this through the command alone (`preview`, `commit`, `revert`), so the
control channel drives the same flow. A change reaches the open helper through the media pool; no
second decoder is started.

**Colour pipeline.** Every frame the source hands over is linear light in the developed primaries:
the Linear curve as the SDK's half floats; every other curve decodes as 16-bit code values (the SDK's
half-float output ignores the gamma curve and with it the IPP2 output transform), which the app
linearises with the compositor's own curve for that gamma into half floats. The compositor's
interpretation follows from the development and is derived, not stored
(`Project::color_space_override`): Rec. 709 frames are plain linear Rec. 709 (the fast path);
Rec. 2020 and P3 D65 are those spaces; ST 2084 and HLG are Rec. 2100 PQ and HLG; REDWideGamutRGB is
RED Log3G10 / REDWideGamutRGB (`filmcraft_color`, from RED's published white paper). An Interpret
Footage choice of your own wins. A test takes a mid-grey patch through every offered curve and space,
in a Rec. 709 and a Rec. 2100 PQ sequence, and gets 18 % back (and a BT.1886 grey reaches the monitor
as the code value the decoder wrote). Proxies keep the look they were made with.

## Limits

- Windows x64 and Linux x64 only; no macOS, no browser build. Single-file Nikon R3D NE only: classic
  RED spanned clips (`.RDC` folders), N-RAW (`.NEV`), HDRx blending and R3D encoding are not
  implemented.
- R3D NE decompresses on the CPU (SDK 9.2.1 has no GPU decompression for it; the GPU does the image
  processing), so throughput follows the CPU, and anything else using the CPU, the editor's own
  threads included, shows up in it. 6K is not realtime at full or half resolution; quarter
  resolution and proxies are (Clip ▸ Proxy ▸ Create Proxies, then View ▸ Toggle Proxies; exports still
  read the original).
- Export of 6K is decode-bound (about 14–15 frames per second rendering 6K to 1080p on a current
  desktop GPU): the export path asks for one frame at a time.
- Only the colour spaces and curves the compositor interprets are offered (no Log3G12, REDlogFilm,
  gamma 2.2 / 2.6 or the legacy curves). Nikon clips carry no creative LUT; import a `.cube` for a
  look.
- Bounds: 8192 pixels per side, 33,554,432 pixels per frame, 16 audio channels, sample rates up to
  384 kHz, 1,048,576 audio samples per request, 64 queued video requests in the helper; non-finite
  decoded values are rejected. Callers wait (cancellably) for a free slot; a wait of 120 seconds fails
  that request.

## The helper protocol (FCRD9)

The app and the helper speak one protocol version, FCRD9: the first field of the helper's start-up
reply and of every clip's metadata. Each side checks the other's; a helper from another build is
refused with an error naming the mismatch, and a request with the wrong field count is refused by the
helper. The rules below are the contract both sides test.

**Process.** The helper takes three arguments: the SDK runtime directory, the name of its reply pipe
(which also names its frame memory; letters, digits and dashes, at most 64, chosen by the app from its
process id, a serial and a random nonce) and the size of a frame slot in bytes (0 for the CPU helper;
the GPU helper starts with a 6K frame's size and the app starts a helper with larger slots for a
larger clip). Its start-up reply is `FCRD9 <slots> <slot-bytes> <cpu-threads>` and its processing
device.

**Requests** arrive on its stdin, one per line:

| Request | Meaning |
| --- | --- |
| `O <id> <clip> <path>` | open the file at `path` (the rest of the line, UTF-8) as handle `clip`; answers its metadata or an error |
| `C <clip>` | close it (no reply; a frame in flight keeps it until written) |
| `V <id> <clip> <frame> <divisor> <half> <space> <gamma> <tonemap> <rolloff> <peak> <options> <flashing> <raw> <iso> <kelvin> <tint> <exposure>` | decode a frame with those development settings |
| `W <id> <clip> <space> … <exposure>` | write those settings into the clip's RMD sidecar, read the clip's settings again and answer its metadata (CPU helper only, on Update RMD) |
| `A <id> <clip> <first-sample> <count>` | decode audio |
| `F <slot>` | the app has finished with a shared frame slot |

Request ids are nonzero, increasing `u64`s; clip handles are nonzero and unique across both helpers.
The development fields are small integers the helper range-checks against the SDK's limits (anything
else, or a field too many or too few, is refused and never applied): `half` 0 premium, 1 good; `space`
0 Rec. 709, 1 Rec. 2020, 2 P3 D65, 3 REDWideGamutRGB; `gamma` 0 linear (half floats), 1 BT.1886,
2 sRGB, 3 Log3G10, 4 ST 2084, 5 HLG (16-bit code values), and the two must go together as
`RawDevelop::supports` says; `tonemap`, `rolloff`, `peak` and `flashing` 0 for as recorded, else a
value; `options` a flag word (chroma noise reduction set and its value, metadata curves, creative LUT,
CDL); `raw` a flag word saying which of ISO, Kelvin, tint and exposure are set (a clear bit means the
recorded value and its field must be 0), `tint` and `exposure` in hundredths. Each request develops
its own copy of the clip's recorded settings, held with its slot until the frame is written, so
requests in flight with different settings or clips never share one.

**Replies** go down the reply pipe, each with a 24-byte little-endian header: status `u32`, request
id `u64`, payload length `u64`, slot `u32`.

- Status 0: exactly `length` payload bytes follow (metadata, audio, and video from a CPU-only helper).
- Status 1: an error message follows; it answers that request only.
- Status 2: nothing follows; the frame is the first `length` bytes of shared slot `slot`.
- Id 0 after start-up is a fatal error of the helper's own, after which it exits.

Replies may arrive out of order; the app routes them by id. A slot or length beyond the declared
geometry, a slot reply from a helper with no slots, or an unknown or duplicate id fails the helper. A
clip's metadata begins `FCRD9`, then its dimensions, timing and audio layout and the helper's slot
count and slot size; then a timecode line, a backend line, the as-shot line (`ISO Kelvin Tint
ExposureAdjust`, from the RMD sidecar when one is present), a line of 28 numbers of what else the clip
records (recorded colour space, gamma, tone map and roll-off; HDR peak; noise reduction; flashing
pixel; creative LUT; CDL; curves; video track count; whether an RMD is in effect; the SDK's HDR peak
range and default tone map and roll-off; the CDL values), the RMD path, and the recorded lens squeeze.
The app range-checks every number and refuses the clip otherwise.

**Shared frames.** A GPU helper creates its frame memory before its first reply: `slots × slot_bytes`,
where a slot holds a full-resolution frame (width × height × 6 bytes) rounded up to whole 4 KiB pages,
created exclusively and never resized. The SDK decodes straight into a slot. The app maps the memory
read-only, exactly as large as the metadata says (on Linux it checks the object's size first, since a
mapping past the end of a file faults when read), and repacks each frame straight out of its slot.
A slot is the SDK's from submission to its callback, the app's from the reply to its `F`, and free in
between. The app holds each slot reply as a lease whose drop sends `F`, wherever the reply ends up (a
cancelled caller's dropped channel included), so a cancelled frame is completed and discarded, never
mistaken for another. A slot the app has held for 120 seconds, or an `F` for a slot that is not the
app's, is a fatal helper error. On Linux the app removes the object's name once it holds the mapping,
and on every failed start-up, so nothing is left under `/dev/shm`.

**The app side.** The app keeps at most the helper's slot count plus one video request outstanding;
further callers wait for a slot, cancellably, so a frame playback has moved past is never sent. It
holds no source lock and not the helper's stdin while it waits for a reply or converts samples.
Every submitted request keeps a 120-second deadline after cancellation; a watchdog kills a stalled
helper and wakes the remaining callers. The frame memory is the backend's only `unsafe` code
(`red::shm`, under [ADR 0001](adr/0001-platform-ffi.md)'s rules): a safe API (open, bounds-checked
slices, unmap on drop) over memory another process can write, read only as bytes that are validated
before use, so no Rust invariant depends on the helper keeping to the protocol.

**Knobs** for measurement and small GPUs, not user settings: `FILMCRAFT_R3D_SLOTS` (1–8, default 5),
`FILMCRAFT_R3D_GPU_FRAMES` (the SDK's GPU streams, 1–8, default 3), `FILMCRAFT_R3D_HOST_POOL_MB` and
`FILMCRAFT_R3D_GPU_POOL_MB` (the SDK's pool limits, default 8192 and 6144; 4096 for a GPU that others
already fill), `FILMCRAFT_R3D_CPU_THREADS` (1–16, default 4) and `FILMCRAFT_R3D_STILL_CPU_DIVISOR`
(0, 1, 2, 4, 8 or 16, default 4: the smallest still frame the CPU helper decodes; 0 keeps them on the
GPU). The SDK's image-thread count stays at its default: one image thread serialises the whole SDK
pipeline however many frames are submitted.

## Tests

Default builds compile none of the backend; their tests cover what it builds on (half-float frames
against every bit pattern, Log3G10 against RED's published values, `.r3d` recognition without
reading the file, the Camera RAW model and commands with stand-in sources, the mid-grey round trip
through the compositor, the panel headless). With the feature, the backend's unit tests need no
footage (framing, metadata, the rounding rule, repack and linearisation, PCM, dispatch, deadlines,
back-off, the shared memory). The real-clip tests are `#[ignore]` unless `RED_R3D_TEST_CLIP` names a
clip of your own; they read it in place and write only into a temporary folder. Several count
processes, so run them alone; the ones that start the GPU helper need an NVIDIA GPU:

```bash
cargo test -p filmcraft-platform --features red-sdk --lib red::
RED_R3D_TEST_CLIP=<clip>.R3D cargo test -p filmcraft-platform --features red-sdk --lib red:: -- --include-ignored --test-threads=1
```

CI cannot build the feature, so a change to this backend carries its author's runs on both platforms.

## Measurements

Two hardware classes: **A** a 16-core desktop CPU with a 16 GB current-generation NVIDIA GPU
(Windows 11), **B** a 16-core desktop CPU with a 10 GB previous-generation NVIDIA GPU (Ubuntu 24.04);
Nikon ZR clips at 6048×3402 and 3984×2240, release builds.

| | A | B |
| --- | --- | --- |
| SDK, 6K at half resolution, 1 / 4 / 5 frames in flight | 4.2 / 24.3 / – fps | 4.8 / 24.4 / 25.1 fps |
| Playback at half: 6K 23.976p | every frame | every frame |
| Playback at half: 6K 59.94p | two of five frames (22.5–24 fps) | – |
| Playback at half: 4K 119.88p / 6K 59.94p at quarter | every frame | – |
| One frame (paused monitor, an eighth): CPU / GPU helper | 34 / ~200 ms | 28–36 / 160–178 ms |
| Scrub: playhead jump to picture (median) | 44–48 ms | 48–58 ms |
| Source Settings change to the monitor | 62–67 ms | 120–240 ms |
| Export, 6K timeline to 1080p | ~14–15 fps (decode-bound) | – |

R3D NE decompresses on the CPU, so these follow the CPU. GPU and CPU processing of a frame differ by
a mean linear error of 0.0005. B draws its window in software, so its UI figures are pessimistic and
its high-frame-rate playback (–) was not measured.

## Licensing

What follows is a contributor's reading of RED's *SDK License Agreement* shipped with R3D SDK 9.2.1,
not legal advice. The decision is the project owners' (#383).

- **The licence is personal.** It is a non-exclusive, non-transferable click-through between RED and
  whoever downloads the SDK. Its rights are that person's, not the project's: everyone who builds the
  feature downloads the SDK and accepts the agreement themselves.
- **The adapter source** (`helper.cpp`, `build.rs`, `red/*.rs`) is original code that calls the SDK's
  public API. The agreement forbids distributing the SDK's own parts (its source, sample code,
  headers, documentation, libraries and object code) and claims ownership of the SDK only; it does not
  plainly forbid publishing code of one's own that uses it. Two clauses make it a judgement rather than
  a clear yes: one forbids programs that expose or disclose "Header File Information", and one treats
  the whole SDK as confidential except what is public through no fault of the developer. The adapter
  necessarily names the API it calls (class names, enumerators, metadata keys), which also appear in
  RED's public sample code. To keep that exposure minimal, no header or documentation text is copied
  or paraphrased into the adapter, no enumerator values, structure layouts or sample code are
  reproduced, the adapter compiles only against an SDK the user supplies, and this document describes
  observed behaviour rather than quoting RED's documentation.
- **Shipping an SDK-enabled build** (the helper with RED's static library linked in, plus RED's runtime
  libraries) is allowed by the agreement only under conditions an MIT OR Apache-2.0 release does not
  meet on its own: an end-user licence with the agreement's terms for the helper and RED's libraries,
  the runtime libraries unmodified and installed in a private directory beside the helper, RED's
  notices intact, no headers, documentation or static libraries shipped, no use of RED's name or marks
  to market the build, and the distributor's own obligations (support, indemnity, export law). **This
  change proposes none of that.** Official FilmCraft builds and releases stay as they are; users build
  the feature locally against their own SDK.
- The app binary contains no RED code (the SDK lives in the helper process) and stays under the
  project's licence. The SDK's readme says it was developed with Kakadu software; a distributor's
  third-party notices would repeat that.

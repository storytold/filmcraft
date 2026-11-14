# ADR 0001: `unsafe` OS media FFI in `crates/platform`, and nowhere else

- **Status:** accepted (2026-10-05, approved by the project owner; recorded in
  [AGENTS.md](../../AGENTS.md) §0 item 3)
- **Issue:** #30 (hardware acceleration)

## Context

FilmCraft is pure Rust and the workspace sets `unsafe_code = "forbid"`. Every codec has a
clean-room software decoder, and those remain the reference. But decoding 4K H.264 / HEVC in
software costs 85–120 ms of CPU per frame on an M4 Pro, so 4K playback drops frames under load,
while every current computer has a hardware video decoder the operating system exposes
(VideoToolbox on macOS; Media Foundation / D3D11 on Windows and VA-API on Linux later).

Using it needs FFI into C / Objective-C APIs, which is `unsafe` Rust. The bindings we use (the
`objc2-*` crates, MIT / Apache-2.0 / Zlib) are generated declarations; nothing GPL/LGPL is linked.

## Decision

Allow `unsafe` in exactly one crate, `crates/platform` (`filmcraft-platform`, layer L5), for one
job: OS media FFI (hardware decoding, and hardware encoding through NVENC). Audio output, dialogs, menus
and other OS integration do not go in it.

Containment rules:

1. The crate does not use `lints.workspace = true`. Its own `[lints]` table copies the workspace
   lints except `unsafe_code = "deny"` (not `forbid`), and adds
   `clippy::undocumented_unsafe_blocks = "deny"`. Only FFI modules may opt in with
   `#[allow(unsafe_code)]`; the current allowances and deviations are recorded below. The fallback
   logic in `hybrid`, the decoder logic in `media_foundation`, `annexb`, `biplanar`, and the encoder
   logic in `nvenc` (H.264 and H.265 alike) keep `unsafe` denied. NVENC's H.265 support
   added no `unsafe` module: it reuses `nvenc::ffi` and `nvenc::session` (new data declarations and
   a second codec-configuration member of an existing union).
2. Every `unsafe` block has a `// SAFETY:` comment saying why it is sound.
3. The public API is safe: no `pub unsafe fn`, no raw pointers or FFI types in public signatures;
   failures are `Result`s. The crate keeps the never-crash `deny(clippy::unwrap_used, …)`
   attribute of every clean crate.
4. No panic crosses the FFI boundary. This applies to VideoToolbox's decoder and encoder output
   callbacks, NVDEC's parser callbacks, and VA-API's error-message callback; their current unwind
   guards are described below. Media Foundation drives a synchronous decoder MFT with
   `ProcessInput` / `ProcessOutput` and registers no Rust callbacks. NVENC loads the driver library
   at run time and registers no Rust callbacks either.
5. The crate compiles on every target. OS bindings are target-specific dependencies (the
   `objc2-*` crates on macOS, the official `windows` crate on Windows: MIT OR Apache-2.0, already
   in the lockfile through wgpu / winit, with only the Win32 features the backend uses); elsewhere
   `register()` reports `Unavailable` and does nothing. It is not part of the wasm check (L5).

## Current implementation (source audit, 2026-10-24)

Checked against `main` at
[`552f008d2757e307194256b3e672c9c1de4fa9b2`](https://github.com/storytold/filmcraft/commit/552f008d2757e307194256b3e672c9c1de4fa9b2)
for [#2814](https://github.com/storytold/filmcraft/issues/2814). This section records source
facts, not new architecture approvals or evidence that builds, hardware tests or a complete
soundness review passed. The accepted decision, its approval date and the containment rules above
remain in effect; implementation differences are not exceptions to those rules.

### Unsafe allowances and module boundaries

[`Cargo.toml`](../../crates/platform/Cargo.toml) still denies `unsafe_code` and
`clippy::undocumented_unsafe_blocks`. The following are the actual allowance scopes:

| Target | Scope carrying `#[allow(unsafe_code)]` | Work at the boundary |
|---|---|---|
| macOS | [`videotoolbox`](../../crates/platform/src/videotoolbox.rs) | VideoToolbox decoding, CoreMedia/CoreVideo buffers and its output callback. |
| macOS | [`videotoolbox_encode`](../../crates/platform/src/videotoolbox_encode.rs) | VideoToolbox H.264/HEVC encoding and its output callback; `dlsym` resolves an optional VideoToolbox property. |
| Windows | [`media_foundation::gpu`](../../crates/platform/src/media_foundation/gpu.rs), [`media_foundation::mft`](../../crates/platform/src/media_foundation/mft.rs) | COM, Media Foundation, D3D11 device setup and surface readback. |
| Windows / 64-bit Linux | [`nvenc::ffi`](../../crates/platform/src/nvenc/ffi.rs), [`nvenc::session`](../../crates/platform/src/nvenc/session.rs), [`nvenc::device`](../../crates/platform/src/nvenc/device.rs) | NVENC declarations and calls, driver loading, D3D11 or CUDA device/context operations. |
| 64-bit Linux | [`nvdec::cuvid`](../../crates/platform/src/nvdec/cuvid.rs) | NVDEC driver calls, parser callbacks and CUDA readback, using `nvenc::device`. |
| Linux | [`vaapi::va`](../../crates/platform/src/vaapi/va.rs) | libva/libva-drm loading, displays, decoding, surface readback and logging. |
| Windows | [`cursor::cursor_screen_position`](../../crates/platform/src/cursor.rs) (function only) | `GetCursorPos`; a non-media deviation, discussed below. |

The media allowances are on module declarations in
[`lib.rs`](../../crates/platform/src/lib.rs) and the backend module files
([Media Foundation](../../crates/platform/src/media_foundation.rs),
[NVENC](../../crates/platform/src/nvenc.rs),
[NVDEC](../../crates/platform/src/nvdec.rs), [VA-API](../../crates/platform/src/vaapi.rs)).
They apply to the module's contents, rather than to individual blocks; the cursor allowance is
on the Windows function itself. There is no crate-wide `allow(unsafe_code)`.
`vaapi::ffi` and `nvdec::ffi` contain public declarations but do not carry an unsafe allowance.

### Callbacks into Rust and panic handling

The registered production callbacks are:

| Backend and entry point | Guard and outcome visible in the source |
|---|---|
| `videotoolbox::output_callback` (`extern "C-unwind"`) | `catch_unwind(AssertUnwindSafe(..))` guards decoded-picture processing. A caught panic becomes `Output::Failed`, queued through `Shared::push`. |
| `videotoolbox_encode::output_callback` (`extern "C-unwind"`) | The same guard covers compressed-packet processing; a caught panic becomes `Output::Failed`. |
| `nvdec::cuvid::{sequence, decode, display}` (`extern "C"`) | All three call the shared `callback` helper, whose processing closure runs under `catch_unwind(AssertUnwindSafe(..))`. The first error or caught panic is stored in `State::error` and returns `0`; `Session::parse` checks that error after the driver call. |
| `vaapi::va::on_error` (`extern "C"`) | `catch_unwind` surrounds message conversion and logging; a caught panic is ignored. `Display::init` registers this callback and disables libva's info callback. |

NVDEC installs its three callbacks in `Session::create_parser`; they run synchronously within
`cuvidParseVideoData`. The VideoToolbox callbacks deliver output asynchronously through their
shared queues. NVENC's function-table entries are driver functions called by Rust, not callbacks
registered with the driver. Media Foundation uses `Mft::input` / `Mft::poll` and rejects
asynchronous MFTs in `Mft::configure`.

These guards do not validate foreign pointers, prevent driver faults or prove state lifetimes
and aliasing sound. Error recording and queueing outside a guarded closure must also obey rule 4;
in particular, the VideoToolbox callbacks call `Shared::push` after `catch_unwind` returns.
An `extern "C-unwind"` signature does not relax the requirement that no panic reach foreign code.

### Known deviations and follow-up

- **Non-media scope:** `cursor::cursor_screen_position()` returns `Option<(i32, i32)>`, calling
  `GetCursorPos` on Windows and returning `None` elsewhere. Its purpose is to locate file drops
  when the windowing layer supplies no pointer position. It is outside the media-only decision
  above, even though its Rust API is safe and read-only. Its presence is not approval to add other
  OS integration. Removing/replacing it or granting a separate scope exception needs maintainer
  resolution; [#2725](https://github.com/storytold/filmcraft/pull/2725) contains a proposed
  window-system ADR and is still open and unmerged at this audit revision.
- **Public FFI surface:** rule 3 is not fully satisfied. `vaapi` publicly exports `ffi`, Linux
  `va`, and the `Accel` trait. `vaapi::va::Session::new` takes `&[ffi::VAProfile]`, and
  `Accel::decode_h264` / `decode_hevc` expose VA picture, matrix and slice parameter structures.
  `nvdec` publicly exports `ffi` and `cuvid`; for example, `ffi::CUVIDPICPARAMS` exposes the raw
  `pBitstreamData` and `pSliceDataOffsets` fields, and `nvdec::ffi` callback types contain raw pointers.
  There is no production `pub unsafe fn`, but that alone does not meet the no-public-FFI-types
  rule. [#2800](https://github.com/storytold/filmcraft/issues/2800) tracks the VA-API visibility
  and driver-input validation problem: `Session::decode_into` forwards supplied parameter
  structures and slice data to libva. Restoring encapsulation and checking caller-controlled
  driver inputs are code follow-ups; this document does not waive rule 3 or fix #2800.
- **Separate FFI hardening:** [#2812](https://github.com/storytold/filmcraft/issues/2812) remains
  open. Callback guards are not a resolution of its lifetime, COM-apartment or cleanup concerns.
  For example, `media_foundation::mft::Mft` still has a `Send` implementation based on a
  free-threaded assumption, `gpu::init_com` accepts `RPC_E_CHANGED_MODE`, and
  `gpu::Readback::read` calls `Unmap` after its Rust closure rather than through an unwind guard.
  Conversely, NVDEC's callback state now uses `Box::into_raw`; `Session::drop` reclaims it with
  `Box::from_raw` after parser destruction, or leaks it if the parser cannot be destroyed.
  That part of the older issue description must not be treated as an unchanged snapshot.
  None of these observations changes the safety requirements.

## The fallback guarantee

Registering a hardware decoder never makes a file undecodable, and never changes a picture:

- The factory declines (the software decoder is used) when Settings ▸ Playback ▸ Hardware
  decoding is Off, for formats the hardware path does not take (field-coded H.264, bit depths
  other than 8 / 10, 4:4:4, mismatched luma / chroma depth), and when the OS cannot create a
  hardware session for the stream (hardware decoding is *required*, so the OS's own software
  decoder is never used instead of ours). On Windows that means the decoder MFT must be
  Direct3D-aware, the GPU's DXVA decoder must list the stream, and every picture must come back as
  a Direct3D 11 texture: Microsoft's decoder MFTs quietly decode in software when DXVA is not
  available, and a system-memory picture is treated as a failure.
- A hardware decoder that fails mid-stream (decode error, session invalidated by a GPU change or
  sleep, in-band parameter sets that differ from the sample entry) becomes the software decoder
  for that stream without the caller noticing: `HybridDecoder` replays the samples since the last
  restart point through it and drops pictures already returned. It is logged and counted
  (`perf.stats` `decode.hardware.fallbacks`).
- Pictures are interchangeable with the software decoder's: same planes (bit-exact on the H.264
  High, HEVC Main and Main 10 parity fixtures, including after seeks and flushes), colour, pixel
  aspect, pts and presentation order. Tests compare the two on every run on macOS, and the
  fallback logic is tested on every platform with a stand-in hardware decoder.

## Consequences

- 4K H.264 / HEVC decode costs ~4 ms of CPU per frame instead of 85–120 ms
  ([performance.md](../performance.md)).
- Better patent posture for the most encumbered formats: H.264 / HEVC decoding is done by the
  operating system, which carries the licences, wherever hardware decoding is used.
- A new review burden: changes to `crates/platform` need the same scrutiny as any FFI code.
  Anything else that wants `unsafe` needs a new decision, not an extension of this one.

## Addendum (2026-10-06): hardware H.264 encoding

The crate's second use of the FFI is the one this decision named, hardware encoding: a VideoToolbox
H.264 encoder behind `filmcraft_export::VideoEncoder` (`videotoolbox_encode`, with the same
containment rules; `hardware_encode` is safe code). It differs from decoding in two ways, both
deliberate:

- **It is opt-in per export** (`ExportSettings::hardware_encoding`, `Off` by default). Exports from
  the built-in encoder are byte-identical across machines; a hardware encoder's output depends on the
  machine, so it must be asked for.
- **There is no mid-stream fallback.** A decoder can replay samples through another decoder; an
  encoder cannot hand a half-written stream to another one. The factory declines up front (the
  built-in encoder is used) for everything the hardware path does not take, and for any machine
  where the OS cannot create a hardware session; a hardware encoder that fails during an export
  stops it with an error.

Everything else in this record applies unchanged: `unsafe` stays in `crates/platform`, every block
has a `// SAFETY:` comment, no panic crosses the FFI boundary, the public API is safe, and the crate
compiles everywhere (the VideoToolbox backend is macOS-only; `register()` also selects the other
backends described in this record).

## Addendum (2026-10-08): NVENC HEVC Main 10 (HDR)

HDR H.265 added no `unsafe` module either. `nvenc::ffi` gained data declarations (the HEVC picture
parameters, `NV_ENC_SEI_PAYLOAD`, the 10-bit buffer format) checked by the generated layout tests, and
`nvenc::session` gained the P010 pitch check and the SEI pointers it hands to the driver, which are
heap blocks owned by the session for its whole life (`// SAFETY:` comments say so). The conversion from
float pictures to 10-bit planes is safe code in the export crate.

## Addendum (2026-10-07): hardware H.265 (HEVC) encoding

The same VideoToolbox session wrapper also creates HEVC sessions (`VtProfile::HevcMain`). The rules
above hold; HEVC adds one difference. The crate has no software HEVC encoder to fall back to, and
there will not be one (pure Rust, clean-room: x265 is GPL), so `Format::Hevc` exists only where the
OS has a hardware encoder. Choosing the format is the opt-in, `filmcraft_export::available` asks a
probe the platform crate registers, and what the hardware path does not take (two-pass, odd sizes,
a machine without the encoder) is an error naming the reason instead of a different encoder.

## Addendum (2026-10-08): VA-API hardware decoding on Linux

The Linux backend this decision named: H.264 and HEVC decoding through VA-API (`vaapi/`). The rules above
hold; VA-API differs from the other two backends in three ways.

- **libva is loaded at run time** (`libva.so.2`, `libva-drm.so.2`, through `libloading`: ISC, already
  in the lockfile through wgpu). Building needs no libva headers, and a system without libva, a DRM
  render node or a working driver starts as before and decodes in software (`register()` reports
  `Unavailable`). The declarations (`vaapi/ffi.rs`) are transcribed from libva's MIT-licensed
  `va.h`, and `vaapi/abi_tests.rs` checks sizes, offsets, constants and bit-field positions against a
  C compiler's view of it, as for NVENC. Only `vaapi::va` carries `#[allow(unsafe_code)]`; the
  declarations, the H.264 front end (`vaapi::h264`) and the decoder (`VaDecoder`) are safe code.
- **Decoding is stateless:** the host parses the stream and keeps the decoded picture buffer; the GPU
  only decodes slice data. FilmCraft does that host side with the software decoders' own parsers and
  DPBs (`filmcraft_h264::dpb` and `filmcraft_hevc::dpb` are generic over what a picture is), so the two decoders decide alike by
  construction, and the front end is tested on every OS against a stand-in for the hardware. The
  one place they cannot agree is concealment: pictures predicted from frames that were never decoded
  (the leading pictures of an open H.264 GOP after a seek) are concealed by both but can differ;
  HEVC pictures with references that were never decoded go to the software decoder instead.
- **One callback into Rust:** libva's error messages go to our log through a callback that runs under
  `catch_unwind`; its info messages are turned off.

The fallback guarantee is unchanged: what the hardware path does not take is declined up front or,
mid-stream, handed to the software decoder by `HybridDecoder`.

## Addendum: Linux NVENC H.264 encoding

The existing NVENC session is shared with 64-bit Linux. Only device acquisition and driver loading
are platform-specific: Windows retains the Direct3D device; Linux retains the first CUDA device's
primary context through `libcuda.so.1`. `cuDevicePrimaryCtxRetain` does not push a context onto the
calling thread's stack. The retained reference is released after the NVENC session and its buffers;
other users' references are not reset. `libnvidia-encode.so.1` stays loaded with its function table.
Both libraries come from the installed NVIDIA driver, not this project. Export registration on
Linux does not claim that a hardware decoder is available. The same opt-in and fallback rules apply.

## Addendum (2026-10-09): NVDEC hardware decoding on Linux

NVIDIA's proprietary driver has no VA-API of its own (only through the separate
`libva-nvidia-driver`), so Linux gets a second decoder backend, `nvdec/`, registered in front of
VA-API's. The rules above hold: `libnvcuvid.so.1` and `libcuda.so.1` are loaded at run time,
`nvdec/ffi.rs` is transcribed from NVIDIA's MIT-licensed headers and checked by
`nvdec/abi_tests.rs`, only `nvdec::cuvid` carries `#[allow(unsafe_code)]`, and the CUDA context is
the one `nvenc::device` already retains for encoding. Unlike VA-API, NVDEC parses the stream
itself; its callbacks run inside our parse call and never unwind into C (they record the first
error and return 0, and the decoder then fails over to software like any other backend).

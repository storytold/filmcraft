# ADR 0001: `unsafe` OS media FFI in `crates/platform`, and nowhere else

- **Status:** accepted (2026-10-05, approved by the project owner; recorded in
  [AGENTS.md](../../AGENTS.md) §0 item 3 and the craftrules never-crash standard)
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
   `clippy::undocumented_unsafe_blocks = "deny"`. Only the FFI modules (`videotoolbox`, `media_foundation::gpu` / `media_foundation::mft`,
   `nvenc::ffi` / `nvenc::session` / `nvenc::device` on Windows and 64-bit Linux, and `vaapi::va` on Linux) carry `#[allow(unsafe_code)]`; the rest of the crate
   (the fallback logic in `hybrid`, the decoder logic in `media_foundation`, `annexb`, `biplanar`,
   the encoder logic in `nvenc`, H.264 and H.265 alike) has no `unsafe`. NVENC's H.265 support
   added no `unsafe` module: it reuses `nvenc::ffi` and `nvenc::session` (new data declarations and
   a second codec-configuration member of an existing union).
2. Every `unsafe` block has a `// SAFETY:` comment saying why it is sound.
3. The public API is safe: no `pub unsafe fn`, no raw pointers or FFI types in public signatures;
   failures are `Result`s. The crate keeps the never-crash `deny(clippy::unwrap_used, …)`
   attribute of every clean crate.
4. No panic crosses the FFI boundary: the VideoToolbox output callback runs under `catch_unwind`
   and reports a panic as a failed frame. The Windows backend has no callbacks into Rust (it drives
   a synchronous decoder MFT with `ProcessInput` / `ProcessOutput`), so nothing can unwind into
   COM. NVENC loads the driver library at run time and has no callbacks into Rust either.
5. The crate compiles on every target. OS bindings are target-specific dependencies (the
   `objc2-*` crates on macOS, the official `windows` crate on Windows: MIT OR Apache-2.0, already
   in the lockfile through wgpu / winit, with only the Win32 features the backend uses); elsewhere
   `register()` reports `Unavailable` and does nothing. It is not part of the wasm check (L5).

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
compiles everywhere (`register()` is a no-op off macOS).

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

## Addendum (2026-10-14): Blackmagic RAW through Blackmagic's runtime (proposed)

Blackmagic RAW has no public specification, so FilmCraft cannot decode it itself; Blackmagic's
free SDK is the only decoder. `braw/` opens `.braw` files through that SDK's runtime
(`libBlackmagicRawAPI.so` on Linux, `BlackmagicRawAPI.dll` on Windows and
`BlackmagicRawAPI.framework` on macOS, from Blackmagic RAW 6.0, or
DaVinci Resolve 21's identical Linux copy) when the user has installed it. This widens the decision
above from OS media APIs to a vendor's library, as NVENC already did for NVIDIA's driver, and is
the owner's call (#383). The rules above hold:

- The runtime is loaded at run time, only when the first `.braw` file opens, and is never bundled.
  Without it a `.braw` file fails to open with a message saying what to install.
- `braw/ffi.rs` is transcribed from the SDK's headers (Linux `BlackmagicRawAPI.h` and `LinuxCOM.h`,
  Windows `BlackmagicRawAPI.idl`, macOS `BlackmagicRawAPI.h` and Apple's `CFPlugInCOM.h`), which
  carry the Boost Software License 1.0 (its notice is
  kept). `braw/abi_tests.rs` checks every vtable slot FilmCraft uses against the headers'
  declaration order. Only `braw::sdk` carries `#[allow(unsafe_code)]`.
- The API is COM-style, with vtables that change between SDK versions. 6.0 inserted codec slots
  before `SetCallback` and a callback slot without changing the callback's id. So every interface
  is used only after `QueryInterface` confirms its id. A runtime with other interfaces is refused,
  not called at the wrong slots.
- The decoder completes jobs on its own threads through a callback object FilmCraft implements.
  The callbacks run under `catch_unwind` and never take FilmCraft's lock. Frame waits time out. A
  clip whose job is still in flight is leaked rather than released under the API.
- Verified on Linux with the SDK's sample clips and with 6K camera clips. It also ran on Windows 11
  (in a virtual machine) and on macOS 15.8 (in a virtual machine) with the same clips: all three
  decode to the same numbers. Each platform's build was checked with clippy for its target
  (`x86_64-pc-windows-msvc`, `x86_64-apple-darwin`); the Windows runtime also ran under Wine before
  that.


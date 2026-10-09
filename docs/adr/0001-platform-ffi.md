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
job: OS media FFI (hardware decoding, and hardware encoding through NVENC and VA-API). Audio output, dialogs, menus
and other OS integration do not go in it.

Containment rules:

1. The crate does not use `lints.workspace = true`. Its own `[lints]` table copies the workspace
   lints except `unsafe_code = "deny"` (not `forbid`), and adds
   `clippy::undocumented_unsafe_blocks = "deny"`. Only the FFI modules (`videotoolbox`, `media_foundation::gpu` / `media_foundation::mft`, `vaapi::ffi` / `vaapi::device` on Linux, and
   `nvenc::ffi` / `nvenc::session` on Windows) carry `#[allow(unsafe_code)]`; the rest of the crate
   (the fallback logic in `hybrid`, the decoder logic in `media_foundation`, `annexb`, `biplanar`,
   the encoder logic in `nvenc` and `vaapi::h264enc`) has no `unsafe`.
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

## Addendum (2026-10-07): hardware H.265 (HEVC) encoding

The same VideoToolbox session wrapper also creates HEVC sessions (`VtProfile::HevcMain`). The rules
above hold; HEVC adds one difference. The crate has no software HEVC encoder to fall back to, and
there will not be one (pure Rust, clean-room: x265 is GPL), so `Format::Hevc` exists only where the
OS has a hardware encoder. Choosing the format is the opt-in, `filmcraft_export::available` asks a
probe the platform crate registers, and what the hardware path does not take (two-pass, odd sizes,
a machine without the encoder) is an error naming the reason instead of a different encoder.

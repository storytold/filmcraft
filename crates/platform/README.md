# filmcraft-platform

OS media integration for FilmCraft (layer L5): hardware video decoding through the operating
system's codecs, behind `filmcraft_codecs::VideoDecoder`. It holds OS media FFI and nothing else.
It is the one crate of the workspace allowed to contain `unsafe`, under the rules of
[ADR 0001](../../docs/adr/0001-platform-ffi.md) and [AGENTS.md](../../AGENTS.md) §0.3.

```rust
// at startup (the desktop app, filmcraft-cli, the bench)
let availability = filmcraft_platform::register(); // Available("VideoToolbox") on macOS, Available("Media Foundation") on Windows
```

## What it does

- **macOS: VideoToolbox H.264 (`avcC`) and HEVC (`hvcC`)**, 8- and 10-bit, 4:2:0 and 4:2:2
  (`videotoolbox.rs`). The session is created from the sample entry's parameter sets with a
  hardware decoder *required*; samples go in as `CMSampleBuffer`s with asynchronous decompression
  (two access units in flight); the output callback copies each NV12 / P010-style biplanar
  `CVPixelBuffer` into planar `Yuv8` / `Yuv16` (chroma deinterleaved, 10-bit samples shifted down
  from the high bits, cropped to the conformance window when the buffer is the coded size). A
  reorder buffer of the stream's own depth (`max_num_reorder_frames` /
  `sps_max_num_reorder_pics`) restores presentation order; a run starting at an HEVC CRA leaves
  out its RASL pictures, as our decoder does. Each seek (`reset`) starts a fresh session.
- **Windows: Media Foundation + Direct3D 11 / DXVA, H.264 (`avcC`) and HEVC (`hvcC`)**, 8-bit 4:2:0
  (H.264 Baseline / Main / High, HEVC Main) and 10-bit 4:2:0 (HEVC Main 10)
  (`media_foundation/`). A Direct3D-aware decoder MFT (Microsoft's H.264 decoder, the HEVC Video
  Extensions' decoder, or a vendor's synchronous hardware MFT) is driven at the level of single
  access units, so there is no Source Reader and no second demuxer: the container samples FilmCraft
  already read go in as Annex B (`annexb.rs`; parameter sets are put in front of the first sample of
  a run). The process-wide Direct3D 11 device (`D3D11_CREATE_DEVICE_VIDEO_SUPPORT`, multithread
  protected) is handed to the MFT through an `IMFDXGIDeviceManager`, which makes it decode with DXVA
  on the GPU's video engine and return NV12 / P010 Direct3D 11 textures. Each picture is then read
  back through a staging texture (**the one GPU to CPU copy**, `gpu.rs` `Readback`) and turned into
  planar `Yuv8` / `Yuv16` (`biplanar.rs`: chroma deinterleaved, P010 shifted down, cropped to the
  conformance window). The MFT returns pictures in presentation order; a run starting at an HEVC CRA
  leaves out its RASL pictures, as our decoder does; `reset` (a seek) flushes the MFT.
  - *Hardware is verified, not assumed.* An MFT that is not Direct3D-aware, asynchronous, or does not
    provide its own output samples is not used; the GPU's DXVA decoder must list the profile, format
    and size (`ID3D11VideoDevice::CheckVideoDecoderFormat` / `GetVideoDecoderConfigCount`); and a
    picture that is not a Direct3D 11 texture (what Microsoft's decoders return when they fall back
    to software inside the MFT) fails the stream, so `HybridDecoder` continues with our decoder.
    Windows' own software decoding is never used in place of ours.
  - *Declined up front* (our decoder is used): field-coded H.264, H.264 profiles other than
    Baseline / Main / High, 10-bit H.264, 4:2:2 / 4:4:4 / monochrome, HEVC profiles other than
    Main / Main Still Picture / Main 10, larger than 8192×8192, no Direct3D 11 video device, no
    DXVA decoder for the stream on this GPU, no decoder MFT (HEVC needs the *HEVC Video Extensions*
    from the Microsoft Store, which the Windows "N" editions and some installs lack).
  - `mfplat.dll` is loaded at run time (`mft.rs`), not linked: Windows "N" editions without the Media
    Feature Pack still start FilmCraft, which then decodes in software.
  - After a `flush` (which drains the MFT) the MFT only restarts at an IDR picture; the GOP cache
    always seeks after a flush, and a caller that continues from the middle of a GOP gets an error
    that `HybridDecoder` answers by replaying the run in software.
- **Windows zero-copy** (`media_foundation::{interop,surface}`, [ADR 0002](../../docs/adr/0002-zero-copy-decode-interop.md)):
  when the renderer's wgpu device is a DX12 device on the decoder's adapter
  (`media_foundation::enable_zero_copy(&device)`), decoded pictures are not read back: each is
  copied GPU to GPU into a shareable NV12 / P010 Direct3D 11 texture and handed out as
  `PixelData::Gpu` (`MfSurface`, a `filmcraft_frame::GpuSurface`). `filmcraft_gpu` opens it through
  the importer this registers: `OpenSharedHandle` as a Direct3D 12 resource, its two planes wrapped as
  wgpu textures (`wgpu::hal::dx12`), sampled by the compositor's interleaved-chroma kind. CPU
  consumers call `VideoFrame::cpu()` (a download, once per picture, bit-exact with the software
  decoder). Off unless the renderer qualifies; `perf.stats` `decode.hardware.zeroCopyFrames` counts the
  pictures.
- **Other systems:** `register()` does nothing and returns `Availability::Unavailable`.
- **`HybridDecoder`** (`hybrid.rs`, safe code): the hardware decoder plus the means to build our
  software decoder for the same `SampleEntry` (`filmcraft_codecs::software_video_decoder`). On a
  mid-stream failure (decode error, invalidated session, changed in-band parameter sets) it replays
  the samples since the last restart point (IDR / IRAP; for an HEVC CRA the one before, so its RASL
  pictures decode) through the software decoder, drops pictures already returned, keeps the ones
  the hardware had decoded but not returned, and stays in software for that instance. The replay
  log is bounded (600 samples / 256 MB); beyond it one error is returned and the next seek restarts
  in software. Streams our decoders cannot decode (HEVC 4:2:2) have no fallback: the error stands.

## Guarantees

- **Never undecodable:** the factory declines (returns `None`, so the software decoder is used)
  when Settings ▸ Playback ▸ Hardware decoding is Off, for formats it does not take (field-coded
  H.264, bit depths other than 8 / 10, 4:4:4 or monochrome, luma / chroma depth mismatch, larger
  than 8192×8192; on Windows also 4:2:2 and the profiles listed above) and when the OS cannot
  create a hardware session (VideoToolbox) or a GPU-backed decoder (Media Foundation).
- **Interchangeable:** colour, pixel aspect, pts, presentation order, `is_random_access` and
  `is_disposable` come from the software decoders' own helpers (`filmcraft_codecs::hw`,
  `video::vui_color`, `sar_par`).
- **Never crash:** no `unwrap` / `expect` / `panic!` outside tests; the output callback runs under
  `catch_unwind`; every `unsafe` block has a `// SAFETY:` comment; the public API is safe.
- **Counted:** `perf.stats` `decode.hardware` (frames, software frames, sessions, declined,
  fallbacks; `filmcraft_codecs::hw::hw_stats`) and `backend` (the registered backend's name,
  `filmcraft_codecs::hw::hw_backend`).

## Tests

| test | what |
|---|---|
| `tests/videotoolbox.rs` (macOS) | H.264 High, HEVC Main (open GOP: CRA + RASL) and HEVC Main 10, 640×360 (coded 368: cropping) with B-frames: every picture **bit-exact** with our software decoder, same pts order, count, colour and aspect, also after `reset` + reseek to every later sync sample, mid-stream `flush`, and a full pass after resets; forced mid-stream failures (`VtDecoder::fail_after`) at five points continue with the software decoder's exact output; seeded mutation of samples and parameter sets (bit flips, truncation, corrupt length prefixes) never panics or hangs; HEVC 4:2:2 10-bit is bit-exact with ffmpeg's decode |
| `tests/fallback.rs` (every OS) | `HybridDecoder` with a stand-in hardware decoder failing after N samples (every sync sample ± a few, first / last sample, after a seek): output identical to the software decoder; in-band parameter sets identical to the sample entry's stay in hardware, different ones switch to software |
| `tests/setting.rs` | Hardware decoding Off gives the software decoder through `make_video_decoder` and the media stack (no hardware frames); Auto gives VideoToolbox where available |

Fixtures are made with ffmpeg into `target/fixtures/platform/` (generator only, never linked);
tests skip without ffmpeg or without a hardware decoder.

## Performance

M4 Pro, load 150–190 (`cargo xtask bench --hw off|auto`): CPU per decoded frame H.264 2160p
119 → 4.2 ms, HEVC 2160p 86 → 3.7 ms; decode 35 → 107 fps and 49 → 217 fps; 4K H.264 and HEVC
playback with no dropped frames at Full, 1/2 and 1/4. Details in
[docs/performance.md](../../docs/performance.md).

## Not yet

Zero-copy upload of `CVPixelBuffer`s into wgpu textures (macOS); a pool of shareable surfaces;
zero-copy on the Vulkan backend; hardware encoding; VA-API (Linux) decoders; field-coded H.264;
VP9 / AV1 on Windows.

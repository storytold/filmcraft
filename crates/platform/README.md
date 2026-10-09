# filmcraft-platform

OS media integration for FilmCraft (layer L5): hardware video decoding through the operating
system's codecs, behind `filmcraft_codecs::VideoDecoder`, and hardware H.264 and H.265 (HEVC)
encoding, behind `filmcraft_export::VideoEncoder`. It holds OS media FFI and nothing else.
It is the one crate of the workspace allowed to contain `unsafe`, under the rules of
[ADR 0001](../../docs/adr/0001-platform-ffi.md) and [AGENTS.md](../../AGENTS.md) §0.3.

```rust
// at startup (the desktop app, filmcraft-cli, the bench)
let availability = filmcraft_platform::register(); // Available("VideoToolbox") on macOS, Available("Media Foundation") on Windows
```

## What it does

- **Linux: VA-API H.264 (`avcC`, 8-bit), HEVC (`hvcC`, Main and Main 10) VP9 (`vpcC`, profiles 0 and 2) and AV1 (`av1C`, Main, 8- and 10-bit)**, 4:2:0 progressive
  (`vaapi/`). libva and libva-drm are
  loaded at run time (`libloading`), so builds and systems without them are unaffected;
  `register()` reports `Unavailable` when no render node's driver decodes H.264. VA-API is
  stateless: `filmcraft_h264`'s / `filmcraft_hevc`'s own decoder runs with an `accel::Accelerator` that receives each
  picture (slice headers, raw slice NALs, reference lists after modification, POCs, the DPB's
  references) instead of reconstructing it, so parameter sets, reference marking and output
  order are the software decoder's. `vaapi::h264` turns that into VA picture / IQ-matrix / slice
  parameter buffers, maps picture ids to surfaces and reads finished pictures back with
  `vaGetImage` (a GPU copy into system memory, avoiding slow reads of uncached VRAM) into the
  same biplanar → planar copy as the other backends. Field pictures, frame_num gaps and driver
  errors fall back to software in `HybridDecoder`. Layouts are checked against libva's headers
  (`vaapi/abi_tests.rs`, 113 H.264 + 91 HEVC + 37 VP9 + 94 AV1 checks). Bit-exact against the software decoder
  (`tests/vaapi.rs`: H.264 High, HEVC Main and Main 10, VP9 profile 0 and 2, AV1 8- and 10-bit (libaom and SVT-AV1) fixtures, whole stream, every reseek, resets; damaged samples never crash).
  HEVC: reference frames carry their RPS subset flags, scaling lists in the spec's
  `ScalingList[sizeId][matrixId]` order (32×32 lists at matrixId 0 and 3), weights as deltas.
  Missing reference pictures fall back to software.
  VP9: `filmcraft_vp9::accel::AccelDecoder` (the software decoder's superframe split, uncompressed
  header parser with its carried loop filter / segmentation state, reference slots and
  per-segment dequantizer / loop filter derivations) hands each frame to `vaapi::vp9`, which
  fills the VA picture and slice / segment buffers; the GPU parses the compressed header and
  keeps the probability contexts and segmentation map. Colour comes from
  `FrameStreamInfo::picture_params` at each key frame, as on Windows. A size change at an inter
  frame (scaled references), another bit depth or a missing reference falls back to software.
  AV1: `filmcraft_av1::accel::AccelDecoder` (the software decoder's OBU parsing, sequence / frame
  headers with their carried state, tile groups, reference slots and `show_existing_frame`)
  hands each complete frame with its tiles to `vaapi::av1`; the GPU does entropy decoding with its
  own CDFs. Film grain (the software decoder applies it), scalable streams, an inter-frame size
  change, a missing reference or a sequence header of another picture format fall back to
  software. (`FrameStreamInfo::parameters_changed` compares AV1 sequence headers by picture format,
  not bytes: SVT-AV1 writes a provisional `av1C` header whose tool flags differ from the in-band
  one, which sent every such MP4 to software on every backend.)
- **macOS: VideoToolbox H.264 (`avcC`) and HEVC (`hvcC`)**, 8- and 10-bit, 4:2:0 and 4:2:2
  (`videotoolbox.rs`). The session is created from the sample entry's parameter sets with a
  hardware decoder *required*; samples go in as `CMSampleBuffer`s with asynchronous decompression
  (two access units in flight); the output callback copies each NV12 / P010-style biplanar
  `CVPixelBuffer` into planar `Yuv8` / `Yuv16` (chroma deinterleaved, 10-bit samples shifted down
  from the high bits, cropped to the conformance window when the buffer is the coded size). A
  reorder buffer of the stream's own depth (`max_num_reorder_frames` /
  `sps_max_num_reorder_pics`) restores presentation order; a run starting at an HEVC CRA leaves
  out its RASL pictures, as our decoder does. Each seek (`reset`) starts a fresh session.
- **Windows: Media Foundation + Direct3D 11 / DXVA, H.264 (`avcC`), HEVC (`hvcC`), VP9 (`vpcC`) and
  AV1 (`av1C`)**, 8-bit 4:2:0 (H.264 Baseline / Main / High, HEVC Main, VP9 profile 0, AV1 main) and
  10-bit 4:2:0 (HEVC Main 10, VP9 profile 2, AV1 main 10)
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
    Main / Main Still Picture / Main 10, VP9 profiles 1 / 3 (4:2:2 / 4:4:4 / RGB) and 12-bit, AV1
    profiles 1 / 2 and 12-bit, larger than 8192×8192, no Direct3D 11 video device, no DXVA decoder
    for the stream on this GPU, no decoder MFT (HEVC, VP9 and AV1 need the *HEVC Video Extensions*,
    *VP9 Video Extensions* and *AV1 Video Extension* from the Microsoft Store, which the Windows "N"
    editions and some installs lack). Several GPUs' DXVA lacks AV1 profiles 1 / 2 as well.
  - *VP9 and AV1* (`codecs::hw::FrameStreamInfo`, `media_foundation/stream.rs`): the container
    sample goes in as it is (a VP9 frame or superframe, an AV1 temporal unit; the `av1C` sequence
    header goes first after a seek). The MFT outputs only shown pictures, in presentation order, so
    hidden alt-ref frames and `show_existing_frame` need nothing special. Picture size and colour
    are read from the bitstream the way the software decoders do (`hw_frame::vp9_color` /
    `av1_color` are shared with them). A VP9 key frame of another size or format, or an AV1 sequence
    header unlike `av1C`'s, hands the stream to the software decoder (`HybridDecoder`).
  - `mfplat.dll` is loaded at run time (`mft.rs`), not linked: Windows "N" editions without the Media
    Feature Pack still start FilmCraft, which then decodes in software.
  - After a `flush` (which drains the MFT) the MFT only restarts at an IDR picture; the GOP cache
    always seeks after a flush, and a caller that continues from the middle of a GOP gets an error
    that `HybridDecoder` answers by replaying the run in software.
- **Windows: NVIDIA NVENC H.264 encoding** (`nvenc/`), 8-bit SDR 4:2:0 for Export. The driver's
  `nvEncodeAPI64.dll` (API 12.1, no CUDA or SDK) is loaded at run time, so machines without NVIDIA
  still start. RGBA is converted with the software encoder's own BT.709 limited conversion into NV12
  input buffers (a ring of eight); the encoder runs preset P5 with high-quality tuning, CABAC (CAVLC
  for Baseline), one B-frame when the profile and GPU allow it, and an IDR at every keyframe
  distance; the parameter sets go into `avcC`. Export ▸ Hardware encoding (off by default) selects it.
  It declines two-pass VBR, HDR, MXF, interlaced output, sizes outside NVENC's limits and systems
  without an NVIDIA GPU or driver, and the software encoder runs instead. A failure during an export
  ends it with an error, since a hardware stream cannot be finished in software.
- **Linux: VA-API H.264 encoding** (`vaapi/h264enc.rs`), 8-bit SDR 4:2:0 for Export, through the
  same libva as the decoders (EncSlice entrypoint). IDR + P pictures, one slice each, CBR or VBR
  rate control in the driver, CABAC for Main / High, no 8×8 transform (AMD's encoder never uses
  it, so the PPS does not ask for it). The SPS and PPS are `filmcraft_h264enc`'s own writers given
  the values the driver gets, and go into `avcC`. Mesa's radeonsi writes slice NAL units with a
  zero header byte when the application supplies no packed headers; `sample()` sets that byte to
  what the picture is (IDR `0x65`, reference `0x41`). Export ▸ Hardware encoding (off by default)
  selects it; it declines two-pass VBR, HDR, MXF, interlaced output, sizes above the driver's
  4096×4096 and systems without a VA-API H.264 encoder, and the software encoder runs instead. A
  failure during an export ends it with an error.
- **Linux: VA-API H.265 encoding** (`vaapi/hevcenc.rs`), Main (8-bit SDR 4:2:0). H.265 has no
  software encoder: `register()` adds a format probe (`hevcenc::available`, asked once) so Export
  offers H.265 where the GPU encodes it, and choosing the format is the opt-in, as with
  VideoToolbox. IDR + P, one slice, the driver's CBR / VBR. The driver reports none of the coding
  tools it uses and writes slice headers our SPS cannot describe (a 4-bit picture order count and
  an empty reference picture set), so: the VPS / SPS / PPS describe what decoding its output
  showed (64×64 CTBs, coded width a multiple of 64 and height of 8, transform depth 3, AMP, SAO,
  strong intra smoothing, CU QP deltas, no sign hiding or transform skip), and each slice header
  is read (strictly: anything unexpected is an error) and rewritten with our POC and reference
  picture set, keeping the driver's QP and SAO choices; the slice data is untouched.
- **Other systems:** `register()` does nothing and returns `Availability::Unavailable`.
- **`HybridDecoder`** (`hybrid.rs`, safe code): the hardware decoder plus the means to build our
  software decoder for the same `SampleEntry` (`filmcraft_codecs::software_video_decoder`). On a
  mid-stream failure (decode error, invalidated session, changed in-band parameter sets) it replays
  the samples since the last restart point (IDR / IRAP; for an HEVC CRA the one before, so its RASL
  pictures decode) through the software decoder, drops pictures already returned, keeps the ones
  the hardware had decoded but not returned, and stays in software for that instance. The replay
  log is bounded (600 samples / 256 MB); beyond it one error is returned and the next seek restarts
  in software. Streams our decoders cannot decode (HEVC 4:2:2) have no fallback: the error stands.

## Hardware H.264 encoding (macOS)

`videotoolbox_encode.rs` (FFI) wraps a VideoToolbox compression session with a hardware encoder
*required*; `hardware_encode.rs` (safe code) is the `VideoEncoder` adapter and the factory that
`register()` puts in front of the built-in encoders (`filmcraft_export::register_encoder`).

- **Opt-in per export:** `ExportSettings::hardware_encoding` (`Off` | `Auto`; Export ▸ Video ▸
  Hardware Encoding, `"hardwareEncoding": "off|auto"` in `file.exportMedia`), off by default. The built-in encoder's
  output is byte-identical on every machine; a hardware encoder's depends on the machine.
- **What it takes:** H.264 in MP4 / MOV, 8-bit SDR, even picture sizes up to 8192, Baseline / Main /
  High (the level is chosen by the OS), constant or one-pass variable bitrate with the settings'
  target and ceiling, the keyframe distance (closed GOPs: every keyframe is an IDR picture).
  Pictures are converted exactly like the built-in encoder's (BT.709, limited range), 4:2:0 NV12.
- **No B-frames.** On real 1080p footage the quality is the same with and without them (±0.3 dB at
  equal bitrate) and the bitrate lands closer to the target without them, so frame reordering is
  off: compressed frames come out in presentation order, with no composition offsets or edit list.
- **What it declines** (the built-in encoder is used, nothing fails): the setting off, other formats,
  MXF (Annex B), two-pass VBR, HDR, odd sizes (4:2:0 cannot crop an odd number of samples),
  non-square pixels, and any configuration VideoToolbox cannot create a hardware session for
  (logged at `info`).
- **A hardware encoder that fails in the middle of an export is an error**, unlike the decoder:
  an encoder cannot hand a half-written stream to another one, so the export stops with the reason.
- The first frame is completed straight away: the SPS / PPS the container needs come with the first
  compressed frame, and the muxer asks for them after the first group of pictures.

## Hardware H.265 (HEVC) encoding (macOS)

The same session wrapper, created for `kCMVideoCodecType_HEVC` with the HEVC Main profile
(`VtProfile::HevcMain`: the profile also says which codec the session is). `Format::Hevc` has no
built-in encoder, which changes three things from H.264:

- **Choosing the format is the opt-in.** No `hardware_encoding` setting: Export ▸ Format ▸ H.265
  (HEVC), or `"format": "hevc"` in `file.exportMedia` (`h265` is accepted too). The format is
  listed as available only on a machine with a hardware HEVC encoder: `register()` hands
  `hardware_encode::hevc_available` (one small hardware session, created on the first question) to
  `filmcraft_export::register_format_probe`.
- **There is no fallback encoder.** What the hardware path does not take is an error, not a
  different encoder: two-pass VBR is refused up front (`ExportSettings::validate`), HDR sequences
  are exported as SDR (the H.265 path is 8-bit), and odd sizes, non-square pixels or a machine
  without the encoder end in "H.265 (HEVC) encoder not available yet".

The sessions are created with a hardware encoder *required*, and `VtEncoder::uses_hardware()` asks
VideoToolbox whether it agrees (`UsingHardwareAcceleratedVideoEncoder`; a test checks it for H.264 and
HEVC), so a requirement dropped by accident could never turn into a silent software encode. Export
mode says "Encoder: Hardware" for H.265, and the summary line "HEVC Main (hardware encoder)".

What it takes is H.264's list: MP4 (`hvc1`, the tag QuickTime reads) or QuickTime, AAC
audio, 8-bit 4:2:0 BT.709 limited range, even sizes up to 8192, constant or one-pass variable
bitrate, the keyframe distance (closed GOPs, IDR pictures), and no B-frames. Keyframes are the
random access points of types 16–21 (BLA, IDR, CRA).

The sample entry's `hvcC` record is the one VideoToolbox wrote for the stream (read from the
format description's sample description extension atoms, with the VPS / SPS / PPS), so profile,
level and flags are the encoder's own. If it is missing the export stops with that reason.

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
  `filmcraft_codecs::hw::hw_backend`). `export.hardware` counts the NVENC and VA-API encoders' frames, sessions and declines.

## Tests

| test | what |
|---|---|
| `tests/videotoolbox.rs` (macOS) | H.264 High, HEVC Main (open GOP: CRA + RASL) and HEVC Main 10, 640×360 (coded 368: cropping) with B-frames: every picture **bit-exact** with our software decoder, same pts order, count, colour and aspect, also after `reset` + reseek to every later sync sample, mid-stream `flush`, and a full pass after resets; forced mid-stream failures (`VtDecoder::fail_after`) at five points continue with the software decoder's exact output; seeded mutation of samples and parameter sets (bit flips, truncation, corrupt length prefixes) never panics or hangs; HEVC 4:2:2 10-bit is bit-exact with ffmpeg's decode |
| `tests/fallback.rs` (every OS) | `HybridDecoder` with a stand-in hardware decoder failing after N samples (every sync sample ± a few, first / last sample, after a seek): output identical to the software decoder; in-band parameter sets identical to the sample entry's stay in hardware, different ones switch to software |
| `tests/setting.rs` | Hardware decoding Off gives the software decoder through `make_video_decoder` and the media stack (no hardware frames); Auto gives VideoToolbox where available |
| `tests/hardware_encode.rs` (macOS) | what the hardware path takes and declines; round trip through our software decoder (every picture, in order, luma PSNR above 30 dB, keyframes no further apart than asked, no composition offsets); an export through `filmcraft_export` that decodes in our decoder and in ffmpeg / ffprobe (profile, size, frame count, BT.709); the built-in encoder still exporting everything hardware declines; exact output size at sizes that are not multiples of 16; hostile configurations (zero, huge, odd sizes, frame rates, bitrates, keyframe intervals, wrong planes) give errors and never panic; encoders dropped at any point do not crash or hang. The same for **HEVC**: Main profile, 8-bit 4:2:0, `hvc1` entry with VPS / SPS / PPS and 4-byte lengths, MP4 and QuickTime, AAC audio, two-pass refused, the format list agreeing with the probe, ffprobe reading `codec_name=hevc`, `profile=Main`, `codec_tag_string=hvc1`, `pix_fmt=yuv420p`, BT.709 |
| `tests/nvenc.rs` (Windows, NVIDIA) | H.264 from NVENC (1280×720, 6 Mbps, 72 frames) decodes with our decoder at worst 46.9 dB luma PSNR; IDR at 0, 24 and 48; dts / pts right |
| `tests/nvenc_export.rs` (Windows, NVIDIA) | Export with hardware encoding against the software encoder through the export pipeline: the two decoded files at worst 54.8 dB luma PSNR; ffmpeg decodes the file without errors; declined cases go to the software encoder; the counters. H.265: offered when the GPU encodes it, 52.9 dB from the software H.264 export of the same frames, ffprobe `hevc,Main,hvc1`, BT.709, sizes 642×362 / 640×360 / 1000×562 cropped in the SPS and clean in ffmpeg |
| `src/vaapi/abi_tests.rs` H.265 encode | 87 layout checks of the HEVC encode structures against libva 2.24 |
| `tests/vaapi_export.rs` (Linux, VA-API) | Export with hardware encoding against the software encoder through the export pipeline (640×360, 72 frames): the two decoded files at worst 52.1 dB luma PSNR on a Radeon RX 7900; ffmpeg decodes the file without errors; declined cases go to the software encoder; the counters |
| `src/vaapi/abi_tests.rs` (Linux) | FFI structs' sizes, alignments and field offsets against gcc's view of libva's headers (2.24): 113 H.264 decode, 91 HEVC decode and 128 H.264 encode checks |
| `src/nvenc/abi_tests.rs` (Windows) | FFI structs' sizes, alignments, field offsets, constants and GUIDs against a C compiler's view of NVIDIA's `nvEncodeAPI.h` (12.1) |

Fixtures are made with ffmpeg into `target/fixtures/platform/` (generator only, never linked);
tests skip without ffmpeg or without a hardware decoder.

## Performance

Hardware H.264 encoding, Apple M1 (8 cores), single runs: 1080p25 camera footage through the
encoder alone 200 fps at 4, 8 and 16 Mb/s (8× real time); a 9:29 timeline (camera clip, ProRes 4444
overlays, AAC, loudness) exported in 311 s against 793 s with the built-in encoder (84 s against 395 s
for its densest 134 s), the outputs at SSIM 0.990 / PSNR 46.5 dB. Details in
[docs/performance.md](../../docs/performance.md).

Hardware H.265 encoding, same machine: no faster than H.264 (the 9:29 timeline in 195 s against
204 s, both bound by the CPU compositor); same picture quality at 10 Mb/s and above, and 22–31 % less
bitrate for the same PSNR at 3–6 Mb/s ([docs/performance.md](../../docs/performance.md), HW3).

Hardware decoding, M4 Pro:

M4 Pro, load 150–190 (`cargo xtask bench --hw off|auto`): CPU per decoded frame H.264 2160p
119 → 4.2 ms, HEVC 2160p 86 → 3.7 ms; decode 35 → 107 fps and 49 → 217 fps; 4K H.264 and HEVC
playback with no dropped frames at Full, 1/2 and 1/4. Details in
[docs/performance.md](../../docs/performance.md).

## Not yet

Zero-copy upload of decoded pictures into wgpu textures (`CVPixelBuffer`s on macOS, Direct3D 11
textures on Windows); B-frames and 10-bit / HDR HEVC (Main 10) in hardware encoding; VA-API (Linux)
VP9 / AV1 decoders and HEVC / AV1 encoders; packed slice headers for VA-API encoding; field-coded H.264; HEVC, 10-bit and HDR encoding with NVENC, and encoders from other
vendors on Windows (through Media Foundation); VP9 / AV1 4:4:4 and 12-bit on Windows.

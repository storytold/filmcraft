# filmcraft-platform

OS media integration for FilmCraft (layer L5): hardware video decoding through the operating
system's codecs, behind `filmcraft_codecs::VideoDecoder`, and hardware H.264 and H.265 (HEVC)
encoding, behind `filmcraft_export::VideoEncoder`. It holds OS media FFI and nothing else.
It is the one crate of the workspace allowed to contain `unsafe`, under the rules of
[ADR 0001](../../docs/adr/0001-platform-ffi.md) and [AGENTS.md](../../AGENTS.md) §0.3.

```rust
// at startup (the desktop app, filmcraft-cli, the bench)
let availability = filmcraft_platform::register(); // Available("VideoToolbox") on macOS, Available("Media Foundation") on Windows, Available("VA-API") on Linux with a driver
```

## What it does

- **macOS: VideoToolbox H.264 (`avcC`) and HEVC (`hvcC`)**, 8- and 10-bit, 4:2:0 and 4:2:2
  (`videotoolbox.rs`). The session is created from the sample entry's parameter sets with a
  hardware decoder *required*; samples go in as `CMSampleBuffer`s with asynchronous decompression
  (two access units in flight); the output callback copies each NV12 / P010-style biplanar
  `CVPixelBuffer` into planar `Yuv8` / `Yuv16` (chroma deinterleaved, 10-bit samples shifted down
  from the high bits, cropped to the conformance window when the buffer is the coded size).
  Chroma copies use exact-size iterators in `chroma.rs`, allowing LLVM to vectorise the strided
  reads on Apple Silicon (including the A18 Pro in MacBook Neo), without intrinsics, additional
  threads or temporary planes. The same routines handle 4:2:0 and 4:2:2 rows. A
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
- **Windows and 64-bit Linux: NVIDIA NVENC H.264 encoding, and H.265 (HEVC) on Windows** (`nvenc/`), 8-bit SDR 4:2:0 for Export. The driver's
  `nvEncodeAPI64.dll` (Windows) or `libnvidia-encode.so.1` (Linux), API 12.1, is loaded at run time,
  so machines without NVIDIA still start. Windows uses a Direct3D 11 device; Linux retains a primary
  CUDA context through the runtime-loaded `libcuda.so.1`, released after the encoder is destroyed.
  No SDK, CUDA toolkit or NVIDIA binaries are bundled or required at build time. 8-bit pictures go
  in as the export's RGBA (ABGR input buffers, a ring of eight) and NVENC converts them to BT.709
  limited 4:2:0 on the GPU, with the same codes as the software encoder's own conversion; a driver
  that refuses RGB input gets that CPU conversion into NV12 instead. The conversion used to run on
  the encode thread, where it competed with the render of the next frames for the thread pool and
  held NVENC back. The encoder runs preset P5 with high-quality tuning, CABAC (CAVLC
  for Baseline), one B-frame when the profile and GPU allow it, and an IDR at every keyframe
  distance; the parameter sets go into `avcC`. Export ▸ Hardware encoding (off by default) selects it.
  It declines two-pass VBR, HDR, MXF, interlaced output, sizes outside NVENC's limits and systems
  without an NVIDIA GPU or driver, and the software encoder runs instead. A failure during an export
  ends it with an error, since a hardware stream cannot be finished in software. The same session,
  ring and input path encode H.265, see [Hardware H.265 (HEVC) encoding (Windows,
  NVENC)](#hardware-h265-hevc-encoding-windows-nvenc).
- **Linux: NVDEC, H.264 (`avcC`) and HEVC (`hvcC`)** on NVIDIA's proprietary driver (`nvdec/`): H.264
  8-bit and HEVC 8- / 10-bit, 4:2:0 progressive. `libnvcuvid.so.1` and `libcuda.so.1` are loaded at
  run time; without them (or without a device) nothing is registered. NVIDIA's own parser takes the
  stream as Annex B access units, so there is no host-side DPB: `cuvid::Session` creates the decoder
  in the parser's sequence callback, decodes in its picture callback, and reads each displayed
  picture back (NV12 / P016) through the CUDA driver into the same planes as the other backends.
  The factory goes in front of VA-API's: what NVDEC declines still reaches VA-API (another GPU),
  then the software decoders. With both registered, `perf.stats` names NVDEC as the hardware
  backend, also for the streams VA-API decodes. The callback state is a `Box::into_raw` allocation
  the parser and `Session` reach through the same pointer, freed in `Drop`. After a start at a CRA picture its RASL pictures are not fed (no
  decoder outputs them, and the parser would otherwise show the CRA picture with the last one's
  timestamp). A seek recreates the parser and keeps the decoder.
- **Linux: VA-API, H.264 (`avcC`) and HEVC (`hvcC`)**: H.264 8-bit 4:2:0 progressive, Constrained
  Baseline / Main / High; HEVC Main, Main 10 and Main Still Picture, 4:2:0 8- and 10-bit (`vaapi/`), on Intel (iHD, i965), AMD and other Mesa drivers. `libva.so.2` and `libva-drm.so.2`
  are loaded at run time (`vaapi/va.rs`; no libva headers needed to build), and each decoder opens
  its own display on the first DRM render node (`/dev/dri/renderD128`…) with a working driver.
  VA-API decoding is *stateless*: the host parses the stream and keeps the decoded picture buffer,
  and the GPU decodes one picture's slice data at a time into a surface. That host side
  (`vaapi/h264.rs`, `vaapi/hevc.rs`, safe code) is the software decoders' own: their parameter-set
  and slice-header parsers, POC computation and DPBs (`filmcraft_h264::dpb`, `filmcraft_hevc::dpb`,
  generic over what a picture is: H.264 reference marking including MMCOs and reference lists with
  modifications; HEVC reference picture sets and lists, RASL pictures of a CRA the run starts at
  left out; output order). The two decoders therefore make
  the same decisions and output the same pictures in the same order, and the parity tests are
  bit-exact. Each picture is sent as one picture-parameter buffer, the scaling matrices, and a slice
  parameter + slice data buffer per slice (the NAL unit as stored, emulation prevention included).
  HEVC pictures are sent the same way per slice segment (scaling lists only when enabled, converted
  to raster order). Pictures the DPB outputs are read back right away (`vaGetImage` into an NV12 or
  P010 image, then `biplanar.rs`), **the one GPU to CPU copy**. Surfaces: the stream's DPB size plus two.
  - *frame_num gaps* (and every start at a non-IDR picture: an open-GOP seek) get "non-existing"
    frames like the software decoder's: copies of the latest decoded frame, mid-gray when there is
    none (`vaPutImage`). Pictures predicted from them (the leading pictures of an open GOP after a
    seek) are concealment in both decoders and can differ: the hardware also reads motion data the
    stand-ins do not have. Everything from the seek point on is bit-exact.
  - *Declined up front* (our decoder is used): VP9 and AV1 (not through VA-API yet), H.264 profiles
    other than Baseline / Main / High, 10-bit H.264, HEVC range extensions and profiles other than
    Main / Main 10 / Main Still Picture, 4:2:2 / 4:4:4, field / MBAFF coding, no libva, no render
    node or driver, a profile or size the driver does not decode. *Mid-stream errors*
    (`HybridDecoder` continues in software): FMO, SP / SI slices, data partitioning, a size or DPB
    change, a missing reference picture (HEVC: one the RPS names that was never decoded), more than
    15 HEVC reference frames, any driver error.
  - An HEVC `flush` ends the stream as in software (references are dropped and the next CRA
    leaves out its RASL pictures); the GOP cache always seeks after a flush.
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
  `hardware_encode::hevc_available` (one small hardware session, about 0.1 s) to
  `filmcraft_export::register_format_probe`, and asks it right away on a thread of its own
  (`warm_hevc_probe`), so the first draw of the format list does not create the session on the UI
  thread.
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

## Hardware H.265 (HEVC) encoding (Windows, NVENC)

One codec enum (`nvenc::Codec`, chosen by `Profile::HevcMain` the way `VtProfile::HevcMain` does it
for VideoToolbox) parameterises the NVENC session: the codec and profile GUIDs, the capability
query, the `hevcConfig` member of the codec-configuration union, how NAL units are told apart
(`(b0 >> 1) & 0x3f`) and the level numbering (`level × 30`, so 4.1 is 123). The level is the lowest
whose **Main tier** limits hold the picture size, sample rate and peak bitrate (H.265 Table A.8,
`nvenc::hevc::main_tier_level`): left to choose, NVENC answered 1080p30 at an 18 Mbit/s peak with
level 4 High tier, which many hardware decoders refuse; it is now level 4.1 Main tier. The H.264 path is unchanged: the same export is the same file, byte for byte.

- **Choosing the format is the opt-in,** as on macOS: Export ▸ Format ▸ H.265 (HEVC) or
  `"format": "hevc"`. `register()` hands `nvenc::hevc_available` to
  `filmcraft_export::register_format_probe`: one small HEVC session (a 640×360 encoder, the answer
  kept; about half a second in a fresh process, nearly all of it opening the Direct3D 11 device and
  the NVENC session, which every NVENC export pays too). `register()` also asks it on a thread of its
  own (`nvenc::warm_hevc_probe`), so the first draw of the format list does not wait for it; a
  caller that asks while it runs waits for the same answer. The
  Hardware encoding toggle governs H.264 only: with H.265 selected NVENC is used whether it is Auto
  or Off, because nothing else can encode it.
- **What it writes:** HEVC Main, 8-bit 4:2:0, SDR BT.709 limited range (Main 10 HDR: see below) (the VUI says so, with the
  frame rate as `time_scale / num_units_in_tick`), progressive, preset P5 with high-quality tuning,
  constant or one-pass variable bitrate, an IDR at every keyframe distance, one B-frame when the GPU
  reports support and the GOP is longer than two pictures (shorter ones are written without), in
  MP4 (`hvc1`) or QuickTime. Pictures are converted exactly like the H.264 path's. The coded size is
  a whole number of 32-pixel coding tree blocks and the SPS carries a conformance window (1080 is
  coded as 1088 and cropped back); the `hvcC` is only accepted when the cropped size is the export's.
- **Samples are length-prefixed (4 bytes) with the parameter sets only in the `hvcC`:** VPS, SPS,
  PPS, access unit delimiters and end-of-sequence / bitstream markers are dropped; slices and SEI
  stay. `nvEncGetSequenceParams` returns all three parameter sets.
- **The `hvcC` is built from the SPS the encoder wrote** (`nvenc/hevc.rs`, parsed with
  `filmcraft_hevc`): profile space, tier, profile, compatibility flags and level from the SPS's
  profile / tier / level; the 48 general constraint flags and `sps_temporal_id_nesting_flag` read from
  the SPS bits that carry them; chroma format, bit depths and temporal layers from the SPS; the
  arrays complete (`array_completeness` 1). Nothing is a constant of ours except what the format
  leaves unspecified (`avg_frame_rate`, `constant_frame_rate`, `min_spatial_segmentation_idc` and
  `parallelism_type` are 0). It is built, and validated as Main 8-bit 4:2:0 of the right size, when
  the encoder is created, so a stream we cannot describe is a declined export and never a default
  sample entry.
- **Timestamps are the H.264 path's:** dts is the decode index minus the B-frame delay, composition
  offsets are `pts - dts` in frame durations, and the media starts `delay` frames early (edit list).
- **There is no fallback encoder.** A request NVENC cannot take is counted in
  `export.hardware.declined` and ends the export with "H.265 export with NVENC: …" and the reason:
  10-bit on a GPU without it (HDR), interlaced output, two-pass (`ExportSettings::validate` refuses it first), non-square pixels,
  MXF, sizes above 65535, odd sizes, sizes outside the GPU's limits (129×33 to 8192×8192 on the RTX
  5060). On a machine without an HEVC encoder the format is not available and an export fails with
  "encoder not available yet". Frames, sessions and declines are the same `export.hardware` counters
  as for H.264.
- **Main 10 HDR (PQ / HLG).** An HDR sequence (Rec. 2100 PQ or HLG working space) exports as HEVC
  Main 10 when `settings.sdr` is off and this GPU has a 10-bit HEVC encoder. `register()` hands
  `nvenc::hevc_hdr_available` (a Main 10 session, cached, asked on the same background thread as the
  8-bit probe and short-circuiting when HEVC itself is missing) to `filmcraft_export::register_hdr_probe`,
  and the export job makes the export HDR for `Format::Hevc` only when
  `filmcraft_export::hdr_available(Format::Hevc)` says so: on macOS (VideoToolbox is 8-bit) and on GPUs
  without 10-bit an HDR sequence is tone-mapped to 8-bit SDR HEVC exactly as before, and `settings.sdr`
  still forces that. The pixels are real: `EncoderFrame::hdr` (encoded BT.2020 R'G'B', 3 floats per
  pixel) goes through `filmcraft_export::rgbf_to_yuv420_10` (BT.2020 non-constant-luminance matrix,
  Y = 64 + 876·Y', C = 512 + 896·C, 2x2 chroma average, NaN counts as 0, values clamp to 0..1 and codes
  to 4..1019, wrong-length input is an error) and into a **P010** input buffer (`code << 6`,
  little-endian, chroma interleaved after the luma rows, pitch in bytes; a driver pitch under 2 bytes
  per pixel is refused). One buffer format (`NV_ENC_BUFFER_FORMAT_YUV420_10BIT`) is used for the
  initialisation, the input buffers and every picture; `pixelBitDepthMinus8` is 2, the profile GUID
  is Main 10, `NV_ENC_CAPS_SUPPORT_10BIT_ENCODE` is queried first and a GPU without it declines
  with "no 10-bit (HEVC Main 10) support". The VUI says BT.2020 primaries, transfer 16 (PQ) or 18
  (HLG), matrix 9, limited range. The `hvcC` is checked against what was asked (`hevc_config(…, 10)`:
  profile 2, 10-bit 4:2:0). NVENC chose the High tier for level 4 at 12 Mb/s; the record just says so.
  Pictures of the wrong depth for the encoder (an `hdr` picture to a Main encoder, an RGBA one to
  Main 10, or a float slice of the wrong length) are errors, never panics.
- **HDR10 static metadata is SEI, not an API.** NVENC 12.1 has no mastering-display field, so PQ
  streams get the mastering display colour volume (payload 137) and content light level (144)
  messages through `NV_ENC_PIC_PARAMS_HEVC::seiPayloadArray` on every IDR picture, with the values
  of `ColorSignal::static_metadata` (BT.2020/D65, 1000 / 0.0001 cd/m², MaxCLL/MaxFALL 0), the same
  that the software H.264 encoder's SEI and the `mdcv` / `clli` boxes carry. NVENC writes them as
  two prefix SEI NAL units (type 39) in front of the IDR slice, adding the emulation-prevention
  byte its `00 00 00 01` needs; nothing is attached to other pictures; HLG has no static metadata and
  gets none. The payload bytes and the descriptor array are heap blocks owned by the `Session` for
  its whole life (it is moved by value, and the driver may queue a picture), and IDR pictures are
  known from a submission counter (`n % gop == 0`). The sample entry carries `colr` (nclx BT.2020,
  PQ / HLG, limited), and for PQ `mdcv` and `clli`; 8-bit SDR HEVC is unchanged and has no `colr`
  (the VUI carries its description, as in the software H.264 path).

## Guarantees

- **Never undecodable:** the factory declines (returns `None`, so the software decoder is used)
  when Settings ▸ Playback ▸ Hardware decoding is Off, for formats it does not take (field-coded
  H.264, bit depths other than 8 / 10, 4:4:4 or monochrome, luma / chroma depth mismatch, larger
  than 8192×8192; on Windows also 4:2:2 and the profiles listed above; on Linux everything but
  8-bit 4:2:0 progressive H.264 and 4:2:0 HEVC Main / Main 10) and when the OS cannot create a hardware session (VideoToolbox), a
  GPU-backed decoder (Media Foundation) or a VA-API configuration and context (Linux).
- **Interchangeable:** colour, pixel aspect, pts, presentation order, `is_random_access` and
  `is_disposable` come from the software decoders' own helpers (`filmcraft_codecs::hw`,
  `video::vui_color`, `sar_par`).
- **Never crash:** no `unwrap` / `expect` / `panic!` outside tests; the output callback (and
  libva's error-message callback) runs under `catch_unwind`; every `unsafe` block has a `// SAFETY:` comment; the public API is safe.
- **Counted:** `perf.stats` `decode.hardware` (frames, software frames, sessions, declined,
  fallbacks; `filmcraft_codecs::hw::hw_stats`) and `backend` (the registered backend's name,
  `filmcraft_codecs::hw::hw_backend`). `export.hardware` counts the NVENC encoder's frames, sessions and declines.

## Tests

| test | what |
|---|---|
| `tests/videotoolbox.rs` (macOS) | H.264 High, HEVC Main (open GOP: CRA + RASL) and HEVC Main 10, 640×360 (coded 368: cropping) with B-frames: every picture **bit-exact** with our software decoder, same pts order, count, colour and aspect, also after `reset` + reseek to every later sync sample, mid-stream `flush`, and a full pass after resets; forced mid-stream failures (`VtDecoder::fail_after`) at five points continue with the software decoder's exact output; seeded mutation of samples and parameter sets (bit flips, truncation, corrupt length prefixes) never panics or hangs; HEVC 4:2:2 10-bit is bit-exact with ffmpeg's decode |
| `tests/fallback.rs` (every OS) | `HybridDecoder` with a stand-in hardware decoder failing after N samples (every sync sample ± a few, first / last sample, after a seek): output identical to the software decoder; in-band parameter sets identical to the sample entry's stay in hardware, different ones switch to software |
| `tests/setting.rs` | Hardware decoding Off gives the software decoder through `make_video_decoder` and the media stack (no hardware frames); Auto gives VideoToolbox where available |
| `tests/hardware_encode.rs` (macOS) | what the hardware path takes and declines; round trip through our software decoder (every picture, in order, luma PSNR above 30 dB, keyframes no further apart than asked, no composition offsets); an export through `filmcraft_export` that decodes in our decoder and in ffmpeg / ffprobe (profile, size, frame count, BT.709); the built-in encoder still exporting everything hardware declines; exact output size at sizes that are not multiples of 16; hostile configurations (zero, huge, odd sizes, frame rates, bitrates, keyframe intervals, wrong planes) give errors and never panic; encoders dropped at any point do not crash or hang. The same for **HEVC**: Main profile, 8-bit 4:2:0, `hvc1` entry with VPS / SPS / PPS and 4-byte lengths, MP4 and QuickTime, AAC audio, two-pass refused, the format list agreeing with the probe, ffprobe reading `codec_name=hevc`, `profile=Main`, `codec_tag_string=hvc1`, `pix_fmt=yuv420p`, BT.709 |
| `tests/nvenc.rs` (Windows / Linux, NVIDIA) | H.264 from NVENC (1280×720, 6 Mbps, 72 frames) decodes with our decoder at worst 46.9 dB luma PSNR; IDR at 0, 24 and 48; dts / pts right |
| `tests/nvenc_export.rs` (Windows / Linux; fallback also tested without NVIDIA) | Export with hardware encoding against the software encoder through the export pipeline: the two decoded files at worst 54.8 dB luma PSNR; ffmpeg decodes the file without errors; declined cases go to the software encoder; the counters |
| `tests/nvenc_hevc.rs` (Windows, NVIDIA with HEVC) | HEVC from NVENC (1280×720, 6 Mbps, 72 frames, keyframe every 24) decodes with our HEVC decoder at worst 48.9 dB luma / 51.0 dB chroma PSNR; IDR at 0, 24, 48; dts strictly increasing, pts a permutation, `sps_max_num_reorder_pics` within the dts shift; no parameter sets in the samples; the `hvcC`'s profile / tier / level bytes are the SPS's own; VUI BT.709 limited and timing `(1, 24)`; hostile configurations, sizes, planes and encoders dropped mid-stream give errors, never panics; keyframes every 1–2 pictures without B-frames |
| `tests/nvenc_hevc_export.rs` (Windows, NVIDIA with HEVC) | H.265 export through the real pipeline, MP4 and QuickTime, toggle Auto and Off: counters (72 frames, 1 session, 0 declined), our decoder against the software H.264 export at worst 54.8 dB luma PSNR, ffprobe `codec_name=hevc`, `profile=Main`, `codec_tag_string=hvc1`, `pix_fmt=yuv420p`, BT.709 limited, 72 frames, 24/1, keyframes at 0 and 48, start 0, `ffmpeg -xerror` clean; with AAC audio; 1920×1080 and 642×362 cropped back from the coded size; every decline (HDR, analysis pass, two-pass, interlaced, non-square pixels, MXF, sizes over 65535, odd sizes, sizes outside the GPU's limits) an error naming NVENC, counted once; H.264 with hardware encoding Off never touches NVENC |
| `tests/nvenc_rgba_input.rs` (Windows, NVIDIA) | RGBA input, HEVC Main and H.264 High, 1280×720: solid red, green, blue, white, black and grey decode (our decoder) to the exact BT.709 limited codes of `rgba_to_yuv420_8`, within 2; a moving picture at worst 47.7 dB luma / 50.7 dB chroma PSNR against that conversion; Main 10 has no RGBA input; short pictures and planar / RGBA pictures for the other kind of encoder are errors |
| `tests/nvenc_hevc_probe.rs` (Windows) | the HEVC probe in a fresh process: its cost, the cached answer, `available(Hevc)` following it after `register()` (twice) |
| `tests/nvenc_hevc_warm.rs` (Windows) | `register()` answers the HEVC and the Main 10 questions on a thread of its own, before anyone asks; the answers are kept and `hdr_available(Hevc)` follows the Main 10 probe |
| `tests/nvenc_hevc_main10.rs` (Windows, NVIDIA with 10-bit HEVC) | HEVC Main 10 in process, PQ and HLG: a float picture (grey ramp to peak white, a moving box, saturated BT.2020 bars) is converted with `rgbf_to_yuv420_10`, encoded and decoded with our HEVC decoder (`Yuv16`, 10 bits, codes below 1024): worst 71.1 dB luma / 70.6 dB chroma PSNR on the 10-bit scale, 803 distinct luma levels on a ramp row, peak code 940 survives; IDR at 0 and 24; the VUI is BT.2020 / 16 or 18 / BT.2020 NCL / limited; the IDR samples carry two prefix SEI NAL units whose messages are byte-for-byte the expected 137 / 144 payloads (HLG: none; other pictures: none); Main and Main 10 refuse each other's pictures; hostile sizes, bitrates, signals, huge or empty SEI payloads, encoders dropped with SEI in flight |
| `tests/nvenc_hevc_hdr_export.rs` (Windows, NVIDIA with 10-bit HEVC) | HDR sequences through the real export job (PQ MP4, HLG MP4, PQ QuickTime): 24 frames, 1 session, 0 declined; ffprobe `hevc` / `Main 10` / `hvc1` / `yuv420p10le`, `bt2020` / `smpte2084` or `arib-std-b67` / `bt2020nc` / `tv`; PQ first-frame side data Mastering display (max 10000000/10000, min 1/10000) and Content light level, HLG none; `ffmpeg -xerror` clean; ffmpeg's yuv420p10le picture equals our decoder's, code for code; our importer reads PQ / HLG, BT.2020, 10-bit, and the `colr` / `mdcv` / `clli` boxes; luma PSNR against a ProRes HDR export of the same sequence 62.6 dB (PQ) / 61.8 dB (HLG); `settings.sdr` and SDR sequences stay Main 8-bit BT.709 with no HDR boxes; the factory rejects mismatched or wrong-length pictures and clamps NaN / infinity |
| `src/nvenc/abi_tests.rs` (Windows / Linux) | FFI structs' sizes, alignments, field offsets, constants, GUIDs and the bit-field masks of the `flags` words (H.264, HEVC, and the HEVC picture parameters with the SEI payload array) against a C compiler's view of NVIDIA's `nvEncodeAPI.h` (12.1) |
| `src/nvenc/{mod,hevc,export}.rs` unit tests (Windows) | HEVC NAL types, stripping and parameter-set splitting (including `IDR_N_LP`, whose header byte reads as a PPS in H.264, and one-byte or truncated NAL units); the `hvcC` built from real VPS / SPS / PPS (Main and Main 10: accepted as what was asked for, refused as the other, other depths refused), every truncation and bit flip of the SPS, wrong profile, size and NAL types; HEVC levels; P010 layout (shift, little-endian, interleaving, pitch in bytes, narrow pitches and short buffers); the 10-bit capability decline; the SEI messages |
| `crates/export/src/hdr_tests.rs` | `rgbf_to_yuv420_10`: exact codes for black, white, grey and the BT.2020 primaries and secondaries, NaN / infinity / out-of-range, odd sizes, wrong lengths and overflowing sizes; the HDR probe registry; which exports are HDR (HEVC SDR without a probe, HDR with one, `settings.sdr` forcing SDR); the SEI of the software H.264 encoder equals `ColorSignal::static_metadata` |
| `tests/nvdec.rs` (Linux, NVIDIA driver) | H.264 High, HEVC Main (open GOP: CRA + RASL) and Main 10 at 640x360, H.264 and HEVC Main 10 at 1080p: bit-exact with the software decoders over the whole stream, after `reset` + reseek to every sync sample and again after the resets; a forced mid-stream failure continues in software with identical output; interlaced and 10-bit H.264 and 4:2:2 HEVC are declined, as is everything while Hardware decoding is Off; damaged samples (bit flips, truncation, corrupt lengths) never crash or hang |
| `src/nvdec/abi_tests.rs` | FFI structs' sizes, alignments, field offsets and constants against a C compiler's view of NVIDIA's `cuviddec.h` / `nvcuvid.h` (nv-codec-headers) |
| `tests/vaapi.rs` (Linux, VA-API) | HEVC Main (open GOP: CRA + RASL), Main 10, scaling lists, 3 slices with WPP (10-bit), weighted prediction with 4 references, transform skip + AMP + B-pyramid (10-bit, open GOP), Main / Main 10 at 1080p and 2160p (compared picture by picture, little memory); H.264 High (B-pyramid), Constrained Baseline with 3 slices per picture, Main with explicit weighted prediction (P and B) and 4 references, temporal direct, custom scaling matrices (JVT) with the 8x8 transform and 2 slices, an open-GOP stream, and 1080p / 2160p, 640×360 (coded 368: cropping): every picture **bit-exact** with our software decoder, same pts order, count, colour and aspect, also after `reset` + reseek to every later sync sample (after an open-GOP I picture, from the seek point on), a mid-stream `flush` with decoding carrying on, and a full pass after resets; forced mid-stream failures (`VaDecoder::fail_after`) at five points continue with the software decoder's exact output; interlaced and 10-bit H.264, 4:2:2 HEVC and the Off setting are declined; an HEVC mid-GOP flush is not checked (it ends the stream, as in software); seeded mutation of samples (through the hybrid and straight into the hardware decoder) and of `avcC` records never panics or hangs; 40 decoders created and dropped; concurrent decoders; a decoder moving between threads |
| `src/vaapi/tests.rs` (every OS) | The H.264 and HEVC front ends with a recording stand-in for the hardware, on x264 streams (B-pyramid, weighted prediction, 4 slices, open GOP with temporal direct) and x265 streams (open GOP with RASL, B-pyramid, Main 10 with 2 slices and weighted prediction): output order and pts are the software decoder's, also after seeks and a mid-stream flush; every buffer is consistent (current surface free, references decoded and listed once, reference lists of the active length, slice data offsets inside the slice); damaged samples are errors |
| `src/vaapi/abi_tests.rs` | FFI structs' sizes, alignments, field offsets, constants and bit-field positions against a C compiler's view of libva's `va.h` and `va_dec_hevc.h` (VA-API 1.23) |

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

Hardware decoding, Linux (Intel Iris Xe, iHD driver; `cargo xtask bench --hw off|auto`): CPU per
decoded frame H.264 1080p 104 → 2.3 ms, 2160p 417 → 11.4 ms; HEVC 1080p 46 → 2.3 ms, 2160p
203 → 10.1 ms, Main 10 2160p 212 → 18.4 ms.
Details in [docs/performance.md](../../docs/performance.md).

## Not yet

Zero-copy upload of decoded pictures into wgpu textures (`CVPixelBuffer`s on macOS, Direct3D 11
textures on Windows); B-frames in the VideoToolbox encoders and 10-bit / HDR HEVC (Main 10) in
VideoToolbox; VP9 and AV1 through VA-API (Linux), and VA-API encoding; field-coded H.264; 4:4:4 and 4:2:2 encoding with NVENC, HDR
H.264 with NVENC (HDR H.264 stays with the software encoder), HDR10 MaxCLL / MaxFALL measured from
the pictures (they are written as 0, unknown), AV1 with NVENC, H.265 with the other Windows vendors and on Linux, and encoders
from other vendors on Windows (through Media Foundation); VP9 / AV1 4:4:4 and 12-bit on Windows.

### Linux NVENC sources and limits

The device binding uses NVIDIA's [CUDA Driver API 12.1 primary-context reference](https://docs.nvidia.com/cuda/archive/12.1.0/cuda-driver-api/group__CUDA__PRIMARY__CTX.html)
and the existing [Video Codec SDK 12.1 encode API](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.1/nvenc-video-encoder-api-prog-guide/index.html).
It uses the first CUDA device. On Linux this backend handles H.264 only (H.265 through NVENC is
Windows-only for now); other formats and unsupported H.264 settings use the existing encoder selection.
Linux registers the export factory even without a driver; `register()`'s return value and
`registered()` still describe hardware **decoding**, which this backend does not provide.
Absent / old drivers decline once per export, with an informational log and
`perf.stats` → `export.hardware.declined`; hardware encoding Off never opens the driver.


The macOS native export bridge (`gpu_export`) consumes a completed wgpu Metal accumulator and
writes IOSurface-backed NV12 on the GPU for VideoToolbox H.264/HEVC. Compatible SDR exports with
GPU rendering Auto and a hardware encoder avoid CPU image readback and RGB→YUV conversion.
Export overlays, output resizing/cropping, HDR and unsupported frame plans retain the portable
path. The `gpu_decode` bridge additionally retains VideoToolbox NV12/P010-style decoder surfaces
and imports their Metal views directly, without CPU planar copying or GPU plane uploads.
CPU fallback materializes exact planes once; native cache charges reserve that possible copy.
A read-only CoreVideo mapping remains held for safe lazy CPU access. Texture lifetime guards
retain the buffer and CVMetalTexture through in-flight GPU work. All backend/CoreVideo access
remains in the OS media FFI modules. Platform startup also configures media-cache budgets from physical RAM; macOS
and Linux have a bounded best-effort available-memory monitor.

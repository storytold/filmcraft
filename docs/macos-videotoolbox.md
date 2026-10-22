# macOS VideoToolbox formats

The macOS backend uses hardware-required VideoToolbox sessions. A codec name or chip generation
is not sufficient: session creation checks the actual profile, dimensions and output layout.
Ordinary unsupported streams keep the software decoder. A failed hardware decoder replays from
the latest random access sample in software; ProRes and JPEG restart on every sample.
HEVC alpha has no equivalent software decoder. Without a VideoToolbox session (other platforms,
hardware decoding Off, Main 10 alpha) it opens in software showing its opaque base layer, with a
logged warning; a running hardware alpha session that fails mid-stream reports an error rather
than silently turning opaque.

## Formats

| Format | Decode | Encode |
|---|---|---|
| H.264 Baseline / Main / High, 8-bit 4:2:0 | hardware where supported | hardware, opt-in |
| H.264 10-bit / 4:2:2 | hardware where supported | not offered by Apple's enumerated H.264 encoder on the validated Mac |
| HEVC Main, 8-bit 4:2:0 | hardware where supported | hardware |
| HEVC Main 10, 10-bit 4:2:0 | hardware where supported | hardware, SDR / PQ / HLG |
| HEVC Main 4:2:2 10, 10-bit 4:2:2 | hardware where supported | hardware, SDR / PQ / HLG |
| H.264 / HEVC 8/10-bit 4:4:4 | requested without reducing chroma; session may decline | not offered by the validated encoders |
| HEVC with alpha | hardware, 8-bit SDR, straight RGBA | hardware, Main 8-bit SDR |
| ProRes 422 Proxy / LT / Standard / HQ | hardware, 10-bit 4:2:2 | hardware, opt-in, SDR / PQ / HLG |
| ProRes 4444 / 4444 XQ | hardware, planar 12-bit 4:4:4 plus alpha when present | hardware, opt-in, SDR / PQ / HLG, optional alpha |
| VP9 profile 0 / 2 | supplemental hardware decoder, 8/10-bit 4:2:0 | no VideoToolbox encoder |
| AV1 main | supplemental hardware decoder, 8/10-bit 4:2:0 | no VideoToolbox encoder |
| Motion JPEG (`jpeg`) | VideoToolbox native, straight RGBA8 (HW requested, status unconfirmed) | hardware, opt-in |

ProRes RAW is out of scope. VideoToolbox's depth/disparity and immersive/spatial-video APIs are not
ordinary FilmCraft video formats. This table does not promise a hardware implementation for every
profile on every Mac. 12-bit HEVC, AV1 high/professional profiles, 12-bit VP9, and field-coded H.264
are declined by these paths; they are never silently converted to 8-bit or 4:2:0.

On the tested Apple M5/macOS system, the OS can create an encoder for HEVC Main 10 with alpha but
refuses the corresponding decoder even without output-pixel-format restrictions. This combination
is rejected explicitly in the export adapter and low-level encoder; use ProRes 4444/XQ for HDR
with alpha. No 8-bit substitution is made. With hardware decoding Off, HEVC alpha opens opaque in
software.

JPEG needs special care: Apple's default JPEG component ignores the hardware-required flag and
selects software. We enumerate a component marked `IsHardwareAccelerated`, select its exact
`EncoderID`, and check that the active encoder ID matches when the component does not implement
`UsingHardwareAcceleratedVideoEncoder`. Other encoders require that property to confirm hardware. The JPEG decoder also omits the hardware
status property; its native path requests hardware but is not counted as confirmed hardware in
`perf.stats`. Actual JPEG hardware decoding was not independently confirmed.

## Export settings

- `hardwareEncoding: "auto"` opts H.264, ProRes and Motion JPEG into hardware. Off keeps the existing
  reproducible software encoders. Choosing HEVC is itself the hardware opt-in.
- `hevcProfile`: `auto` (the default, Main for SDR and Main 10 for HDR), `main`, `main10`, or
  `main42210`. Explicit choices are available in Export and `file.exportMedia`; invalid values and
  an 8-bit choice for HDR are errors. Explicit profile selection currently requires macOS.
- `proresProfile`: `proxy`, `lt`, `standard`, `hq`, `4444`, `4444xq`.
- `alpha: true` keeps straight alpha for ProRes 4444/XQ and macOS HEVC Main. HDR rendering preserves
  a separate alpha channel and unpremultiplies colour before transfer encoding.

Decoded biplanar frames retain their CoreVideo surface for direct Metal texture import; CPU consumers
materialise their planes on demand. Retention is bounded, and GPU or format incompatibility uses
the CPU upload path. Packed alpha formats continue through CPU conversion.

Eviction releases a CoreVideo buffer only after its CPU planes have been copied successfully.
A failed copy keeps the original buffer and its tracking entry, and rejects the new decoded
picture through the existing decoder-error/software-replay path rather than exceeding the
retention limit. Direct CPU plane requests log their readback error and leave the buffer available
for another request. Synthetic CoreVideo tests cover repeated failures and recovery for NV12 and
P010, and successful eviction with the eight-surface limit.

The current shared frame conversion API is still infallible: a failed direct CPU materialisation
can still return blank pixels after logging. This local retention fix does not resolve that API
limitation. The fallible CPU-access boundary in [#1019](https://github.com/storytold/filmcraft/pull/1019)
and shared surface/import contract in [#1021](https://github.com/storytold/filmcraft/pull/1021) need
to be coordinated with this macOS adapter before the implementations land together.

10-bit input uses P010/P210, with significant bits shifted into 16-bit CoreVideo storage. ProRes
4444 uses packed little-endian AYUV16, preserving alpha separately from video-range YUV. The current
export pipeline converts YUV to 10-bit values (including ProRes 4444); its 12-bit decoded output
must not be mistaken for an end-to-end 12-bit source/export pipeline. PQ/HLG colour is written to
the stream and sample entry; PQ also carries mastering-display/content-light metadata. Export
verifies the HEVC header's bit depth, chroma and alpha against the requested configuration.

Source references: Apple's Xcode SDK headers `VTCompressionProperties.h`, `VTDecompressionProperties.h`,
`VTVideoEncoderList.h`, `CMFormatDescription.h` and `CVPixelBuffer.h`. The VP9 and AV1 integration
and its parity tests adapt the existing contributions in [#2180](https://github.com/storytold/filmcraft/pull/2180)
and [#2608](https://github.com/storytold/filmcraft/pull/2608).

## Reproducing validation

```sh
# An absent device implementation skips only that hardware case.
cargo test -p filmcraft-platform --test vt_formats -- --nocapture
# Require all supported profile cases on a suitable Mac; no silent skips.
FILMCRAFT_REQUIRE_VT=1 cargo test -p filmcraft-platform --test vt_formats profile_round_trips -- --nocapture
cargo test -p filmcraft-platform --test videotoolbox --test hardware_encode
# Optional private/local media. Never commit this media.
FILMCRAFT_TEST_PRORES=/absolute/path/to/a-short-prores.mov \
  cargo test -p filmcraft-platform --test vt_formats local_prores_decode_and_seek -- --ignored --nocapture
```

`vt_formats` generates its own pictures and movies under `target/fixtures/platform/`. It checks six
ProRes profiles, HEVC Main/Main 10/Main 4:2:2 10 in SDR/PQ/HLG, supported alpha, JPEG, real export
jobs, seeks, forced software fallback, malformed samples and invalid inputs. FFmpeg/ffprobe are
external oracles only: independent decoding, frame counts, pixel formats, colour and HDR metadata.
The VP9/AV1 tests compare hardware output with FilmCraft's software decoder at 360p, 1080p and 4K.

Local evidence (Apple M5, 2026-10-21, development profile): a local 4K60 ProRes LT
capture, 120 frames copied without transcoding from 60 seconds into the movie, decoded and sought
successfully. Against FilmCraft software decode, worst luma/chroma PSNR was 79.28 dB, maximum
10-bit code difference 1. Hardware decoding, including compressed sample copies and CPU readback,
ran at 82.9 fps in one integrated run with workspace tests active concurrently. This is a short
segment under concurrent load, not a dedicated benchmark or full-movie/editor playback performance.
No private video, still, title, path or extracted frame is included in the repository.

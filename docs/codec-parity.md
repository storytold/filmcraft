# Codec parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (new file: every codec Premiere Pro 26.5.2 decodes or encodes, against ours) · **Target:** Adobe Premiere Pro 2026 (26.5.2) and Media Encoder 2026

Video and audio codecs, decode and encode, with fidelity and how each is tested. Containers,
project and interchange files are in [file-format-parity.md](file-format-parity.md); hardware
paths per platform in [hardware-parity.md](hardware-parity.md). All our codecs are clean-room
pure Rust from public specifications ([AGENTS.md](../AGENTS.md) §2); ffmpeg is a test oracle only.

Premiere's list comes from Adobe's published supported-formats pages and the names of the
installed bundle's `MediaIO/systempresets` and `Frameworks` entries (listings only, never their
contents). Percent is ready-for-real-work for that codec: depth, profiles, speed.

## Video

| Codec | Premiere | Our decode | Our encode | Fidelity and tests | % | Est. |
|---|---|---|---|---|---|---|
| H.264 / AVC | decode all common profiles incl. High 10, 4:2:2 (XAVC S-I, AVC-Intra); encode 8/10-bit | own decoder, **8-bit 4:2:0 only** (Baseline / Main / High, CABAC / CAVLC); no High 10 / 4:2:2 / 4:4:4, interlaced field / MBAFF, lossless | own encoder (High / Main / Baseline, B-frames, CBR / VBR / 2-pass); 8-bit 4:2:0 only; hardware VideoToolbox / NVENC | decode bit-exact on 37+ conformance streams, 500–600 fps 1080p; encode ffprobe-checked, byte-identical across machines | 65% | 20–35 h |
| HEVC / H.265 | decode Main, Main 10, 4:2:2 10 (XAVC HS); encode 8/10-bit, HDR | own decoder Main / Main 10 / Main Still Picture, bit-exact on 41 fixtures, ~225 fps 1080p; **no 4:2:2 / RExt** | **hardware only** (VideoToolbox 8-bit; NVENC 8-bit and Main 10 HDR on Windows and Linux); no software encoder | NVENC round trip 48.9 dB (8-bit), 71.1 dB (10-bit); ffprobe | 60% | 30–60 h |
| ProRes 422 family / 4444 / XQ | decode + encode all | own decode + encode, all six profiles, alpha | yes (export UI exposes 422 flavours; 4444 / XQ request #342) | ±1 LSB, 318 fps | 85% | 3–6 h |
| ProRes RAW | decode | none (#345) | — | — | 0% | 30–50 h (spec availability uncertain) |
| DNxHD / DNxHR | decode + encode, MXF / MOV | own decode (all SMPTE ST 2019-1 CIDs, 8/10/12-bit, 4:2:2 / 4:4:4, interlaced, alpha) and encode LB / SQ / HQ / HQX / 444 | yes, MOV + MXF | within IDCT precision of ffmpeg on 22 fixtures | 90% | 2–4 h |
| AV1 | decode (hardware on Windows); export through Media Encoder plug-ins / recent versions | own decoder Main 8/10-bit, all tools, film grain, bit-exact vs libdav1d on 41 vectors; hardware on Windows | none | slow in software (~4 fps 1080p single-thread) | 55% | 30–50 h (encoder + speed) |
| VP9 | not a Premiere import format on all versions | own decoder profiles 0–3, 8/10/12-bit, bit-exact on 50+ fixtures, frame threading | none | conformance | ahead | — |
| APV (RFC 9924) | — (Samsung's pro codec; not in Premiere) | own decode | own encode | spec tests | ahead | — |
| MPEG-2 / MPEG-1 | decode incl. 4:2:2 (IMX, XDCAM); encode DVD / Blu-ray / MXF | own decoder (4:2:2, interlaced, field pictures, IEEE 1180 IDCT) | **none** | conformance fixtures | 60% | 20–35 h (encoder) |
| Motion JPEG | decode + encode | yes | yes (MOV) | — | 90% | — |
| JPEG 2000 (MXF, DCP) | decode + encode (Kakadu) | none | none | — | 0% | 40–70 h |
| JPEG XS | decode (`libjpegxs`) | none | none | — | 0% | 20–40 h |
| DV / DVCPRO / DV100 | decode + encode | none | none | — | 0% | 15–25 h |
| MPEG-4 Part 2 | decode | none | none | — | 0% | 10–20 h |
| WMV / VC-1 | decode, encode on Windows | none | none | — | 0% | 20–35 h |
| Camera RAW: R3D (incl. R3D NE), ARRIRAW, Sony RAW / X-OCN, Canon RAW, BRAW (plug-in), Cinema DNG | decode (vendor SDKs) | none (#383) | — | — | 0% | 60–100 h for the ones with public specs; vendor-SDK-only formats need an owner decision |
| GIF (animated) | encode | decode as still | encode | — | 80% | — |
| Uncompressed / v210 / RGB | decode + encode (AVI, MOV) | partial (MOV PCM-style video not verified) | none | — | 30% | 8–15 h |

## Audio

| Codec | Premiere | Our decode | Our encode | Fidelity and tests | % | Est. |
|---|---|---|---|---|---|---|
| PCM (WAV, BWF, AIFF, MOV, MXF, Blu-ray / DVD LPCM) | yes | yes, BWF timecode | WAV, AIFF, MOV, MXF | sample-exact | 95% | — |
| AAC (LC, HE, LATM) | decode + encode | own decoder (LC, HE core), LATM | own LC encoder | loudness oracle vs ffmpeg | 90% | 3–5 h |
| AC-3 | decode (licensed) | own decoder (ATSC A/52) | none | conformance | 80% | — |
| **E-AC-3 (Dolby Digital Plus)** | decode | **refused** (bsid 11–16, #647) | none | — | 0% | 15–25 h |
| MP3 / MP2 | decode + MP3 encode | decode (symphonia, MPL-2.0; own MP2) | **none** | — | 60% | 10–20 h (MP3 encoder) |
| Opus | — (not a Premiere import format) | own decoder, RFC 8251 range-exact | none | conformance vectors | ahead | — |
| FLAC, ALAC, Vorbis | partial | decode (symphonia); FLAC in MP4 silent (#603) | none | — | 70% | 3–6 h |
| WMA | decode on Windows | none | none | — | 0% | 10–20 h |

## Summary

- **Ahead of Premiere:** VP9, Opus, APV decode / encode; bit-exact software decoders everywhere,
  including Linux and FreeBSD, where Premiere does not run.
- **Behind, by user impact:** 10-bit / 4:2:2 H.264 and HEVC in software ([G3](gaps.md#g3-10-bit-and-422-camera-media-needs-hardware)),
  E-AC-3, camera RAW, HEVC and AV1 software encoding, MPEG-2 / JPEG 2000 encoding for broadcast
  and DCP, DV, WMV.
- **Codec dimension: ~60%, 150–250 h** ([target-app-parity.md](target-app-parity.md#by-dimension)).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created: codec-by-codec comparison with Premiere 26.5.2 from the codec crates' READMEs, the export format registry and the installed bundle's listings |

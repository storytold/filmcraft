# File format parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (new file: every file type Premiere Pro 26.5.2 reads or writes, against ours) · **Target:** Adobe Premiere Pro 2026 (26.5.2) and Media Encoder 2026

Containers, stills, audio files, project and interchange files, captions and presets. Codecs inside
the containers are in [codec-parity.md](codec-parity.md). Our import list is
`filmcraft_media::{VIDEO,AUDIO,STILL}_EXTENSIONS` plus the openers in `filmcraft_codecs::openers()`;
our export list is `filmcraft_export::Format`. Premiere's list is from Adobe's supported-formats
documentation and the exporter folders in the installed bundle's `MediaIO/systempresets` (names
only). "Tested" names the evidence.

## Project and interchange

| Format | Premiere | Read | Write | Tested | % |
|---|---|---|---|---|---|
| **Premiere project `.prproj`** | native | **no** | **no** | — | **0%** ([G2](gaps.md#g2-premieres-prproj-cannot-be-opened)) |
| Our project `.fcproj` | — | yes, schema migrations | yes, atomic, auto-save, recovery journal | migration tests | — |
| FCP7 XML (`xmeml`) | read + write | yes | yes | round-trip tests; Premiere-exported XML slow (#461) | 75% |
| FCPXML | read (via XtoCC) | yes | yes | round-trip tests | 80% |
| AAF (Edit Protocol) | read + write | yes | yes (embedded / linked / consolidated) | own CFB; **never opened in Media Composer / Pro Tools** | 55% |
| OMF 2.0 | write | no | yes | never opened in Pro Tools | 50% |
| EDL (CMX 3600) | read + write | yes | yes | tests | 85% |
| OpenTimelineIO | read + write (26.0) | yes | yes | tests | 80% |
| Avid Log Exchange (ALE) | write | no | yes | — | 70% |
| Marker CSV / text | write | — | yes (#454) | — | 80% |
| Selection as project | Premiere project | — | `.fcproj` only | — | 50% |
| Templates (`.prproj` templates) | yes | — | our templates | — | 70% |
| Motion Graphics templates `.mogrt` | read + write | **refused by design** (clean-room, AGENTS.md §1); our `.fcgt` instead | `.fcgt` | — | n/a |
| Effect presets `.prfpset` | read + write | no | our JSON presets | — | 40% |
| Keyboard shortcuts `.kys` | read + write | no | our presets incl. a Premiere layout | — | 60% |
| Export presets `.epr` | read + write | no | our presets (import / export) | — | 50% |
| LUTs `.cube`, `.3dl` | read | yes (1D / 3D / shaper) | — | tetrahedral CPU = WGSL | 95% |

## Video containers

| Container | Premiere | Read | Write | Tested | % |
|---|---|---|---|---|---|
| MP4 / M4V / 3GP | read + write | yes | yes (H.264, HEVC + AAC) | ffprobe, round trips | 90% |
| QuickTime MOV | read + write | yes | yes (H.264, HEVC, ProRes, DNxHR, APV, MJPEG, PCM) | ffprobe | 90% |
| MXF OP1a / OP-Atom | read + write (XDCAM, AVC-Intra, P2, AS-10 / AS-11, JPEG 2000, DNx) | yes (AVC, DNx, ProRes, MPEG-2, PCM / AES3, timecode) | OP1a (DNxHR, ProRes, H.264), OP-Atom | index-table seeking tests | 70% |
| MPEG TS / PS (AVCHD `.mts`, `.m2ts`, `.ts`, `.mpg`, `.vob`, `.mod`) | read; write MPEG-2 / Blu-ray / DVD | yes | **no** | fixtures | 65% |
| Matroska / WebM | read (limited) | yes (H.264, HEVC, VP9, AV1, ProRes, MJPEG + AAC, Opus, FLAC, MP3, Vorbis, PCM); WebM black report (#432), MKV multi-audio (#601) | no | fixtures | 75% |
| **AVI** | read + write | **no** (extension listed, no demuxer, #598) | no | — | 0% |
| WMV / ASF | read; write on Windows | no | no | — | 0% |
| FLV / F4V | read + write | no | no | — | 0% |
| DCP | write (Wraptor) | no | no | — | 0% |
| APV raw / Y4M | — | yes | APV in MOV | — | ahead |
| Ogg (Opus / Vorbis) | — | yes | no | granule-seek tests | ahead |
| Camera folder structures (XDCAM, P2, AVCHD, Canon XF, RED) | Media Browser reads them | AVCHD files yes; spanned clips and metadata no | — | — | 30% |

## Stills and image sequences

| Format | Premiere | Read | Write (frame / sequence) | % |
|---|---|---|---|---|
| PNG, JPEG, TIFF, BMP, GIF | read + write | yes | PNG / TIFF / BMP / JPEG sequences, GIF | 90% |
| WebP | — | yes | no | ahead |
| **PSD** (layers as sequence) | read (layered) | no | — | 0% |
| **OpenEXR** | read + write | no | no | 0% |
| **DPX** | read + write | no | sequences (10-bit RGB, BT.709; #779) | 40% |
| Targa | read + write | no | sequences (24-bit, 32-bit with alpha; #779) | 40% |
| HEIF / HEIC | read | no | — | 0% |
| Radiance HDR, AI / EPS, ICO, PTL | read | no | — | 0% |

## Audio files

| Format | Premiere | Read | Write | % |
|---|---|---|---|---|
| WAV / BWF (timecode, iXML partial) | read + write | yes | yes | 90% |
| AIFF / AIFC | read + write | yes | yes | 95% |
| MP3 | read + write | read | **no** | 60% |
| AAC / M4A | read + write | yes | inside MP4 / MOV only, **no AAC-only file** | 70% |
| FLAC, Ogg, Opus | partial | yes | no | ahead on read |
| WMA | read (Windows) | no | no | 0% |

## Captions and subtitles

| Format | Premiere | Read | Write | Tested | % |
|---|---|---|---|---|---|
| SRT, WebVTT, SCC | read + write | yes | yes | frame-exact, property-tested | 95% |
| MCC (608 + 708), EBU STL, TTML / IMSC1, DFXP | read + write | yes | yes | frame-exact | 90% |
| 608 / 708 embedded in the video stream | write (MXF, H.264 SEI) | no | **no** | — | 0% |
| Burn-in | yes | — | yes | golden tests | 95% |

## Summary

**File formats dimension: ~60%, 140–240 h** (codecs counted separately). The beta blocker is
`.prproj`. After it, by user impact: AVI and WMV import, EXR / DPX / PSD stills, AAF / OMF
validation in Avid and Pro Tools, DVD / Blu-ray / DCP / broadcast MXF delivery.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created: format-by-format comparison with Premiere 26.5.2 from the import / export registries, crate READMEs, the installed bundle's exporter listings and open issues |

---
title: What file formats does FilmCraft support?
description: The video, audio, still image and project formats FilmCraft can open and export, and what it does not support yet.
order: 20
verified: 0.7.2
---

FilmCraft opens H.264, HEVC, ProRes, VP8, VP9, AV1 and DNxHD or DNxHR video, in MP4, MOV, Matroska, WebM and AVI files. It exports H.264 MP4, ProRes 422 HQ, DNxHR, Motion JPEG, image sequences, animated GIF, WAV and FLAC.

## What it can open and write

| Format                  | Open | Write | Notes                                                                           |
|-------------------------|:----:|:-----:|---------------------------------------------------------------------------------|
| H.264 / AVC             | yes  | yes   | The encoder has High, Main and Baseline profiles                                |
| HEVC / H.265            | yes  |       | Main and Main 10. Export only through a hardware encoder                        |
| Apple ProRes            | yes  | yes   | Opens 422 Proxy to 4444 XQ. Export writes 422 HQ                                |
| AAC-LC                  | yes  | yes   |                                                                                 |
| VP8, VP9                | yes  |       | In WebM, Matroska and MP4                                                       |
| Opus                    | yes  |       | In WebM, Matroska and MP4                                                       |
| AV1                     | yes  |       | Main profile. No AV1 export yet                                                 |
| Avid DNxHD / DNxHR      | yes  | yes   | Export writes DNxHR in MOV                                                      |
| MJPEG, PCM              | yes  | yes   |                                                                                 |
| FLAC                    | yes  | yes   | As `.flac` files, or as the audio in MP4 and MOV                                |
| MP3, ALAC, Vorbis       | yes  |       |                                                                                 |
| MP4 / MOV               | yes  | yes   |                                                                                 |
| Matroska / WebM         | yes  |       |                                                                                 |
| AVI                     | yes  |       | Motion JPEG, H.264, HEVC and uncompressed RGB or YUV. DivX, Xvid and DV not yet |
| Stills                  | yes  | yes   | See [Stills and image sequences](#stills-and-image-sequences)                   |

FilmCraft also opens MPEG-2 and MPEG-1 video, AC-3, MP2, MXF, MPEG transport and program streams, Ogg and image sequences.

## Stills and image sequences

- Open: PNG, JPEG, GIF, WebP, TIFF, BMP, Targa, DPX, OpenEXR and DNG. Animated GIF and APNG files open as clips, with transparency.
- Camera RAW: DNG stills, and CinemaDNG folders of numbered DNG frames as one clip. Lossy and JPEG XL DNG files are not supported.
- Write: PNG, TIFF, BMP, JPEG, Targa, DPX and OpenEXR sequences, and animated GIF.

## Not supported yet

- Camera RAW other than DNG, such as Canon, Nikon or Sony raw files
- E-AC-3
- AV1 export
- DivX, Xvid and DV in AVI
- Premiere Pro `.prproj` files. See [Can FilmCraft open Premiere Pro projects?](premiere-projects.md)

## Captions and timelines

FilmCraft imports and exports SRT, WebVTT and SCC captions. For timeline files, see [Move a timeline to or from another editor](../guides/exchange-timelines.md).

## Hardware decoding

Hardware decoding works on macOS (VideoToolbox), on Windows (Media Foundation) and, for H.264 and HEVC, on Linux (VA-API).

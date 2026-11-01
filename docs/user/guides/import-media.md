---
title: Import media
description: Bring video, audio and still files into a FilmCraft project from disk, by drag and drop, or from the demo footage.
order: 10
verified: 0.7.2
---

Choose **File ▸ Import…**, pick your files, and they appear in the Project panel. You can also drop files anywhere in the FilmCraft window.

## Steps

1. Create a project with **File ▸ New ▸ Project…**, or open one with **File ▸ Open Project…**.
2. Choose **File ▸ Import…**. Its shortcut is shown next to it in the menu.
3. Select your movies, audio or stills and confirm.
4. Find the new items in the Project panel.

The header also has an **Import** tab. It shows cards for **Browse files…** (movies, audio and stills from disk), **Demo footage** (six procedural 1080p clips with sound), **Demo project** (a cut sequence with transitions, effects and music) and **Bars and Tone** (SMPTE HD bars with a 1 kHz tone). Switch back with the **Edit** tab.

## Image sequences

To import numbered stills as one clip, use **File ▸ Import Image Sequence…**. A CinemaDNG shot, a folder of numbered DNG frames, comes in the same way as one clip.

## Open a clip in the Source monitor

Right-click an item in the Project panel and choose **Open in Source Monitor**. To make a sequence from the clip, choose **New Sequence From Clip**.

## Which files work

FilmCraft reads H.264, HEVC, ProRes, VP8, VP9, AV1, DNxHD and DNxHR, MJPEG and PCM video and audio, AAC, Opus, MP3, MP2, FLAC, ALAC and Vorbis audio, and PNG, JPEG, GIF, WebP, TIFF, BMP, Targa, DPX, OpenEXR and DNG stills. Animated GIF and APNG files come in as clips. See [What file formats does FilmCraft support?](../faq/file-formats.md) for the full list and what is not supported yet.

## If a file does not import

A file FilmCraft cannot read is reported as an error. The other files in the same import still go in. Camera and phone files from the wild are less tested than the test streams. If one fails, [report it](../troubleshooting/send-a-bug-report.md) with the file's codec and container.

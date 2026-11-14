---
title: Export your video
description: Render a sequence to a video file from Export mode, or use Quick Export, and watch the export queue.
order: 40
verified: 0.7.2
---

Open a sequence, click the **Export** tab in the header, set the file name, location and preset, then click **Export**.

## Steps

1. Open the sequence you want to export. With no sequence open, Export mode shows "Open a sequence to export".
2. Click **Export** in the header (next to **Import** and **Edit**), or choose **File ▸ Export ▸ Export**.
3. Under **Destinations**, keep **Media File**.
4. Set **File Name** and **Location**. The file name starts as the sequence name.
5. Pick a **Preset** and a **Format**. The **Video**, **Audio**, **Multiplexer**, **Captions**, **Effects** and **Metadata** sections hold the detailed settings. Changing any setting turns the preset into **Custom**.
6. Set **Range** to **Entire Source**, **Source In/Out**, **Work Area** or **Custom**. **Work Area** is available only when the sequence has one.
7. Check the **Summary** for the estimated file size.
8. Click **Export** to render now, or **Send to Queue** to render later.

Exports run as background jobs with progress and cancel, so you can keep editing. The export queue is on the left. Use **Start Queue** to run queued items, **Stop** to stop, and **Clear Finished** to tidy the list. Each item can be moved **Up** or **Down**, **Cancel**led, **Retry**ed or **Remove**d.

## Quick Export

The header also has a **Quick Export** button. It opens a small popup with a **File Name** field, the folder, a preset list and **Export**. Click the folder to pick another one.

## What FilmCraft can write

- H.264 MP4 with AAC, using FilmCraft's own encoders.
- Apple ProRes 422 HQ and Motion JPEG in QuickTime.
- MXF OP1a and Avid-style OP-Atom (DNxHR, ProRes or H.264 with PCM and start timecode).
- Image sequences (PNG, TIFF, BMP, JPEG, Targa, DPX, OpenEXR) and animated GIF.
- WAV, AIFF and FLAC audio. FLAC is lossless. To put it in an MP4 or MOV, set **Audio Format** in the **Audio** section to **FLAC (lossless)**. Its **Compression Level** goes from 0 (fastest) to 8 (smallest).
- H.265 only through a hardware encoder: NVIDIA on Windows, VideoToolbox on macOS. There is no H.265 export on Linux, and no AV1 export yet.

H.264 can also use a hardware encoder on macOS, and on Windows and Linux with an NVIDIA GPU. On Windows and Linux it is opt-in. In the **Video** section, the **Hardware Encoding** row has a **Use the hardware encoder when available** check box.

To burn captions into the video, open the **Captions** section and set **Export Options** to **Burn Captions Into Video**. The other choices are **None** and **Create Sidecar File**.

## Check the render bar

On the timeline, the render bar marks segments green, yellow or red. Rendered previews are cached, so an edit only invalidates what it touches.

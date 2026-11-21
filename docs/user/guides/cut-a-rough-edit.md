---
title: Cut a rough edit with three-point editing
description: Mark In and Out in the Source monitor, then insert or overwrite the clip into the timeline, and take ranges back out with lift and extract.
order: 20
verified: 0.7.2
---

Open a clip in the Source monitor, press `I` and `O` to mark the part you want, then press `,` to insert it or `.` to overwrite onto the timeline.

## Steps

1. [Import your media](import-media.md).
2. Right-click a clip in the Project panel and choose **Open in Source Monitor**.
3. Move the Source playhead to where the shot should start and press `I` (**Markers ▸ Mark In**).
4. Move to where it should end and press `O` (**Markers ▸ Mark Out**).
5. Move the timeline playhead to where the shot should go. The timeline has patch and target buttons for tracks. The edit goes onto the patched tracks.
6. Press `,` for **Clip ▸ Insert**. It adds the shot and pushes what follows later. Press `.` for **Clip ▸ Overwrite** instead to replace what is under it.

Play with `Space` (Play/Stop). `L` plays forward, `K` stops and `J` plays backward. Pressing `L` or `J` again in the same direction doubles the speed, up to 8 times.

## Take a range out

With In and Out set on the sequence:

- `;` is **Sequence ▸ Lift**. It takes the range out of the targeted tracks.
- `'` is **Sequence ▸ Extract**. It takes the range out of the targeted tracks and closes the gap.

## Cut and trim

- Press `C` for the **Razor Tool**. Press `V` to go back to the **Selection Tool**.
- **Sequence ▸ Add Edit** cuts at the playhead. Its shortcut is shown in the menu.
- The other tools are **Ripple Edit Tool** (`B`), **Rolling Edit Tool** (`N`), **Rate Stretch Tool** (`R`), **Slip Tool** (`Y`) and **Slide Tool** (`U`).

## Change the keys

Open **Edit ▸ Keyboard Shortcuts…** to see and change shortcuts. There is a **Premiere Pro Compatible** preset, and a **Final Cut Pro Compatible** one.

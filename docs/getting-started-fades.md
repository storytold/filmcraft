# Getting started: video fades and audio volume keyframes

Use a **Cross Dissolve** transition to fade a picture in or out. Use **Volume** keyframes
to make a clip's sound gradually louder or quieter.

These instructions follow FilmCraft's current `main` branch, checked on 24 October 2026
([source revision](https://github.com/storytold/filmcraft/commit/552f008d2757e307194256b3e672c9c1de4fa9b2)).
An older installed release may differ. The [planned fade handles](#whats-coming-timeline-fade-handles)
are separate from the operations available now.

## What you'll learn

- Fade a video clip in and out, and dissolve between two shots.
- Draw an audio fade using the Volume line on the clip.
- Change fade length, adjust levels, remove keyframes and undo a mistake.

A video transition changes the picture; it does not fade the clip's sound. An audio Volume
keyframe changes sound; it does not change the picture. A crossfade joins two clips, while a
fade at a free edge brings one clip in or takes it out.

## Before you start

1. Open FilmCraft and choose **File ▸ Import…**. Pick a video with sound, at least four
   seconds long, and confirm.
2. In the **Project** panel, right-click the imported clip and choose **New Sequence From Clip**.
   Its picture appears on a video track such as **V1**, and its sound on an audio track such as **A1**.
3. Open **Window ▸ Effects** and **Window ▸ Effect Controls**. These are different panels:
   Effects is the library; Effect Controls edits the selected clip or transition.
4. Keep the track unlocked. For the first exercise, use a video clip with empty space at both
   ends on its track and no other picture underneath it. This makes the fades appear against black.
5. Choose **File ▸ Save As…** to save your practice project.

Use the **Selection Tool** for selecting clips and transitions. Its default shortcut is **V**;
the **Pen Tool** is **P**. Shortcuts below assume FilmCraft Default or Premiere Pro Compatible.
If you have changed presets, check **Edit ▸ Keyboard Shortcuts…** or use the Tools panel.

To undo an edit, choose **Edit ▸ Undo** (**Ctrl+Z** on Windows/Linux, **Cmd+Z** on macOS).
Finish typing in a value field and click outside it before undoing a timeline edit.

## Fade a video clip in

1. In **Effects**, open **Video Transitions ▸ Dissolve** and find **Cross Dissolve**.
   You can also type its name in **Search effects**.
2. Drag **Cross Dissolve** onto the video clip near its **left edge**, inside the clip.
   Wait for the highlighted preview labelled **In**, then release.
3. With the **Selection Tool**, double-click the new transition block in the Timeline.
   In **Set Transition Duration**, edit **Duration:** and click **OK**.
   A whole number such as `12` means 12 frames; a larger number makes a slower fade.
4. Click the Timeline ruler at the clip's start and press **Space** to play. Press **Space** again
   to stop. The picture should gradually appear.

You can also click the transition once and change **Duration** in **Effect Controls**.
Undo the duration change or the transition application with **Edit ▸ Undo**, one edit at a time.

If a clip touches another clip at that edge, FilmCraft makes a transition between the two
instead of a fade from empty space. On an upper video track, a fade can reveal the picture
underneath rather than black.

## Fade a video clip out

1. Drag **Cross Dissolve** from **Effects ▸ Video Transitions ▸ Dissolve** onto the video clip
   near its **right edge**, inside the clip. Release when the preview says **Out**.
2. Double-click the transition block, set its duration in frames and click **OK**.
   Start with `12` frames, then lengthen it if the ending feels abrupt.
3. Click the Timeline ruler before the transition and press **Space** to preview the ending.
   With no picture underneath, the clip fades to black.

To resize an existing fade by eye, use the **Selection Tool** and drag an end of the
transition block, rather than the clip's trim edge. The fade stays anchored to the clip's
start or end. Use **Edit ▸ Undo** if you resize the wrong item.

## Crossfade between two video clips

1. Import a second video with **File ▸ Import…**. Drag it from the **Project** panel onto the
   **same video track** as the first clip, directly after it with no gap.
   If you already added a fade at their shared edge, select that transition block and choose
   **Edit ▸ Clear** before continuing.
2. Drag **Cross Dissolve** onto the shared cut, just inside the end of the first clip or the
   beginning of the second. Check that the highlighted preview spans the cut, then release.
3. Click the transition block and open **Effect Controls**. Set **Alignment** to
   **Center at Cut** for a dissolve balanced around the edit.
4. Double-click the block to set its duration, or edit **Duration** in Effect Controls.
   Click the Timeline ruler just before the cut and press **Space** to preview it.
   Shorten or lengthen the transition to taste.

This blends the outgoing picture into the incoming picture. It leaves their audio unchanged.
To remove it, select only the transition block and choose **Edit ▸ Clear**. Use **Edit ▸ Undo**
to restore it or undo its last adjustment.

**Shortcut option:** **Sequence ▸ Apply Video Transition** is **Ctrl+D** on Windows/Linux or
**Cmd+D** on macOS. With a video clip selected, it applies the default video transition to
both available edges, including shared cuts, wherever the playhead is. With only edit points
selected, it applies to those points. It uses your default transition, which may have been
changed from Cross Dissolve. Use the drag steps above when you want one specific edge and effect.

## Fade audio in and out with Volume keyframes

Each audio clip already has a **Volume** effect; you do not need to add another effect.
Its **Level** is measured in decibels (dB). **0 dB** keeps the original level; negative values
reduce it. **0 dB does not mean silence.**

These are **clip volume keyframes**: they belong to this clip. **Track automation** belongs
to the audio track and can affect other clips on it. You do not need to record a mixer
automation pass or change the Audio Track Mixer's automation mode for this exercise.

1. Find the audio clip on **A1**. Enlarge the track by dragging the bottom edge of its header
   downward, so you can see the white Volume line below the clip's name.
2. Click the diamond button in the audio track header (**Show Keyframes**) and choose
   **Clip Keyframes**. This shows the clip's Volume line instead of a track automation lane.
3. Choose the **Pen Tool** (**P**). Click directly on the Volume line at four positions, in order:
   just after the clip starts, about one second later, about one second before it ends, and
   just before it ends. Keep the outer clicks a little inside the clip, away from its trim edges.
   Four keyframe diamonds appear. Add all four before changing their levels.
4. Choose the **Selection Tool** (**V**). Drag the **first diamond** to the clip's start and
   down to the bottom of the Volume band. Watch the tooltip: **-∞ dB** means silence.
5. Drag the **last diamond** to the clip's last frame and down to **-∞ dB** too. Leave the two
   middle diamonds at the original level. The line now rises from silence, stays at the listening
   level, and falls back to silence.
6. Click the Timeline ruler at the clip's start and press **Space** to listen. Move the second
   diamond later for a longer fade-in, or the third earlier for a longer fade-out.
7. Choose **File ▸ Save** when you are happy with the result. Use **Edit ▸ Undo** to reverse
   a keyframe addition or a completed drag.

With the Selection Tool, you can also **Ctrl-click** the line on Windows/Linux or **Cmd-click**
it on macOS to add a keyframe. Click an empty part of the line: adding at an existing
keyframe's time can remove that keyframe. A plain click selects the clip; dragging the line
changes its level. Dragging the line between two keyframes adjusts both surrounding points,
so grab an individual diamond when shaping one end of a fade.

## Adjust and remove keyframes

### Change a fade's timing or level

1. With the **Selection Tool**, drag a Volume diamond **left or right** to change its time,
   or **up or down** to change its level. Keyframes stay inside the clip and cannot pass their
   neighbours. Use the Timeline's bottom zoom bar to spread short fades out on screen.
2. For a precise level, select the audio clip, open **Window ▸ Effect Controls** and expand
   **Volume** using the small arrow beside its name. Scroll down if linked video effects are
   shown above the audio effects.
3. In the keyframe area to the right of **Level**, click the diamond you want to edit.
   This moves the playhead to that keyframe. Click the blue **Level** number, type a value
   such as `0` or `-96`, and click the **Level** label to commit it. FilmCraft treats
   `-96 dB` or lower as silence.
4. Preview again with **Space**. Use **Edit ▸ Undo** to restore the previous value or position.

The arrows beside the Level keyframe button move to the previous or next keyframe. When
animation is enabled, editing Level at a different playhead position adds a keyframe there.
The **Toggle animation** stopwatch turns animation on or off; do not click it for every new
point. Turning it off removes the parameter's keyframes and keeps its value at the playhead.

### Remove an unwanted point

1. In the Timeline, right-click the **Volume keyframe diamond**, then choose **Delete**.
   Right-clicking the line or clip body opens a different menu.
2. In Effect Controls, the equivalent operation is to right-click the Level diamond in the
   keyframe area and choose **Clear**.
3. Listen again: the remaining points determine the fade. Choose **Edit ▸ Undo** to restore
   the deleted point.

## What's coming: timeline fade handles

**Planned / pending, not available on current main:**
[PR #1635](https://github.com/storytold/filmcraft/pull/1635) is open and **has not been merged**
as of 24 October 2026. It proposes small handles in eligible clips' top corners: click to apply
the default video or audio transition, or drag to preview and set its length. The result would
be an ordinary transition that can be resized or removed. These handles would not replace
Volume keyframes for detailed audio shaping. The proposal may change before release.

For now, use the transition library or the Volume-keyframe steps above. Do not look for the
proposed corner handles in your current installation.

## Common problems and solutions

| Problem | What to check |
|---|---|
| The video fades, but its sound cuts abruptly. | A video transition affects the picture only. Add Volume keyframes to the audio clip separately. |
| I get a dissolve to another shot instead of black. | That edge touches another video clip, or another video track is visible underneath. Practise at a free edge on V1. |
| I cannot see the audio Volume line. | Enlarge the audio track, then choose **Show Keyframes ▸ Clip Keyframes** from its header's diamond button. |
| I am changing every clip on A1. | You may be editing **Track Keyframes ▸ Volume** or the Audio Track Mixer. Switch to **Clip Keyframes** and edit the line inside the intended clip. |
| The whole audio line moves. | Without keyframes, a line drag changes the whole clip's level. Add the points first, then drag individual diamonds. |
| Clicking near an end trims the clip instead of adding a point. | Click farther inside the clip on its Volume line, then drag the new diamond to the desired end. |
| My fade jumps instead of changing gradually. | In Effect Controls, right-click the earlier Level keyframe and choose **Linear** rather than **Hold**. |
| Volume keyframes move but I hear no change. | In Effect Controls, make sure **Volume** is enabled with its **fx** switch and **Bypass** is unchecked. Also check that the track is not muted. |
| The edit will not move or resize. | Unlock its track using the lock button in the track header. Try a shorter transition duration if it would extend outside the clips. |
| Ctrl/Cmd+D affects both ends. | That is the selected-clip workflow. Drag Cross Dissolve onto one edge to apply just that fade. |

## Quick reference

| To do this | Use this |
|---|---|
| Fade a picture in / out | Drag **Cross Dissolve** to a free left / right video edge. |
| Dissolve between shots | Drag **Cross Dissolve** to their shared cut on one video track. |
| Set a transition's length | Double-click its block → **Set Transition Duration**, or edit **Duration** in Effect Controls. |
| Add a clip Volume keyframe | **Pen Tool** click, or **Ctrl-click** (Windows/Linux) / **Cmd-click** (macOS), on the Volume line. |
| Change an audio fade's length / level | Drag its Volume diamond horizontally / vertically. |
| Remove a Volume keyframe | Right-click its Timeline diamond → **Delete**. |
| Undo the last edit | **Edit ▸ Undo**: **Ctrl+Z** (Windows/Linux) / **Cmd+Z** (macOS). |

For customised keys, see [Keyboard shortcuts](keyboard.md).

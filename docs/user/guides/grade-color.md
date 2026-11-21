---
title: Grade color with Lumetri Color
description: Open the Color workspace, add Lumetri Color to a clip, and use its sections and the scopes to correct and grade the picture.
order: 30
verified: 0.7.2
---

Choose **Window ▸ Workspaces ▸ Color**, select a video clip in the timeline, and use the **Lumetri Color** panel on the right.

## Steps

1. Choose **Window ▸ Workspaces ▸ Color** (`Alt+Shift+3`, or `Option+Shift+3` on macOS). The workspace has **Lumetri Scopes** beside the Program monitor and **Lumetri Color** on the right.
2. Click a video clip in the timeline. While nothing is selected, the panel shows **(no clip selected)**.
3. If the panel shows **Add Lumetri Color to clip**, click it.
4. Open a section and move its sliders. Each section has its own on/off switch, and the **fx** toggle at the top turns the whole effect on and off.
5. Watch the scopes while you work. They are computed from the graded frame.

## The sections

| Section              | What is in it                                                                                            |
|----------------------|----------------------------------------------------------------------------------------------------------|
| Basic Correction     | Input LUT, Temperature, Tint, Saturation, Exposure, Contrast, Highlights, Shadows, Whites, Blacks        |
| Creative             | Look, Look LUT, Intensity, Faded Film, Sharpen, Vibrance, Saturation                                     |
| Curves               | RGB Curves, and Hue vs Sat, Hue vs Hue, Hue vs Luma, Luma vs Sat, Sat vs Sat                             |
| Color Wheels & Match | Shadows, Midtones and Highlights wheels, and Color Match with Comparison View and **Apply Match**        |
| HSL Secondary        | Key by Hue, Hue Range, Saturation Min, Luma Min and Max, Soften; Show Mask; Denoise and Blur; correction |
| Vignette             | Amount, Midpoint, Roundness, Feather                                                                     |

The built-in looks are color transforms written in code, not LUT files. To use your own, pick a `.cube` or `.3dl` file with **Input LUT** or **Look LUT**, using **Browse…**.

## Work in HDR

When the sequence's working space is HDR (Rec. 2100 PQ or HLG), the Basic Correction section also shows **HDR White** and **HDR Specular**.

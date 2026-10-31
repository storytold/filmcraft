# Quick Start — FilmCraft

> Five minutes from cold start to a finished edit.

## 1. Open FilmCraft

```
cargo run -p filmcraft
```

Or if you have the installed binary: launch **FilmCraft** from your Applications folder / desktop.

## 2. Import media

**File ▸ Import…** (`Cmd+I`) — pick your video, audio or still-image files.
Alternatively drag files directly onto the **Project** panel.

## 3. Create a sequence

Double-click a clip in the Project panel to open it in the Source monitor.
Drag from the Source monitor (or the Project panel) onto the **Timeline** to create a sequence automatically sized to match your first clip.

## 4. Edit on the timeline

| Action | How |
|---|---|
| Blade (cut) | `C` to switch to the Razor tool, click the clip. `V` to return to Selection. |
| Trim | Hover over a clip edge until the trim cursor appears, then drag. |
| Ripple delete a gap | Click the gap, press `Backspace`. |
| Add a transition | Click a cut point with the Selection tool; press `Cmd+D` for the default cross-dissolve. |
| Undo | `Cmd+Z` |

## 5. Colour grade

Switch to the **Color** workspace (click **Color** in the header bar).
Open the **Lumetri Color** panel and adjust Exposure, Contrast and White Balance in the Basic Correction section.

## 6. Export

Press `Cmd+M` (or **File ▸ Export ▸ Media…**) to open the Export panel.
Choose a preset (e.g. *H.264 – Match Source*), set a file name, and click **Export**.

---

## Next steps

- [Architecture overview](architecture.md) — how the crates fit together
- [Control protocol](control-protocol.md) — drive FilmCraft from scripts or AI agents
- [Keyboard shortcuts](keyboard.md) — full shortcut reference
- [Contributing](contributing.md) — how to build and add features

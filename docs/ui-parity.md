# UI and interaction parity

> **Last reviewed:** 2026-10-21 (source audit) · **Last updated:** 2026-10-21 · **Change:** minor (correct implemented controls/docking; separate implementation from visual validation) · **Target:** Adobe Premiere Pro 2026 (26.5.2, macOS)

The [2026-10-21 progress review](progress-review.md#ui-evidence-that-changes-the-work-list)
links implementations and test sources for the corrections below. Tests and the app were not run
in this audit. All percentages/hours remain the 2026-10-10 estimates; issue numbers are historical
reproduction references, not freshly checked open bugs. Exact Premiere fidelity needs a new visual
comparison.

How FilmCraft looks, feels and responds compared with Premiere: tools, on-screen handles, snapping,
nudging, modifier keys, shortcuts, drag behaviour, numeric entry, panels, context menus. Keyboard
detail is in [keyboard.md](keyboard.md), monitor view options in [monitors.md](monitors.md).
Reference screenshots and behaviour notes are maintainer-local (`plan/premiere/`, never committed).

**UI / UX dimension: ~60% ready, breadth ~88%, 120–200 h** ([target-app-parity.md](target-app-parity.md#by-dimension)).
UI fidelity was compared with Premiere reference screenshots by the agents that built it; that
comparison has not been repeated independently. The ready figure is pulled down by the volume of
open interaction reports (about 40 of the 188 open issues).

## Areas

| Area | Premiere | Ours | % | Open issues | Est. |
|---|---|---|---|---|---|
| Menus | 366 in-scope menu items | ~338 present (label diff, 92%) | 92% | spelling, Render and Replace, 5 workspaces | 10–20 h |
| Keyboard | Premiere default keyboard + editor | Premiere-compatible preset (143 keyboard-only commands; 39 cloud / absent-panel keys skipped with reasons), FilmCraft / FCP / Avid presets, editor | 90% | Ctrl chords (#256 fixed), modifier hints on Windows (#270 fixed) | 5–10 h |
| Timeline editing tools | Selection, Track Select, Ripple / Rolling / Rate Stretch, Razor, Slip / Slide, Pen, Hand, Zoom, Type | all present, trim modes, Trim Monitor, dynamic trimming; empty-space deselect and gap Delete/Backspace have tests | 80% | recheck clip stretch (#653), new tracks by dropping (#483) and remaining delete reports (#680, #404); #683/#648/#668 implemented | 30–50 h |
| Program monitor on-screen controls | Motion / Transform handles, mask handles, safe margins, guides | Motion/Transform move, scale, rotate and anchor handles with tests; guides, masks and snapping; Button Editor visibility/reset UI and per-monitor state implemented in `panels/monitor.rs` | 55% | independent visual/platform validation; historical #639/#431 do not establish absent controls | 15–25 h |
| Effect Controls | property rows, keyframe area with zoom, divider, graphs, eyedroppers | draggable divider, row lines, Position X/Y value and velocity graphs, keyframe selection and eyedropper implemented | 60% | keyframe zoom (#641, #645) and remaining interaction fidelity; #640/#643/#448 implemented | 15–25 h |
| Panels and docking | 17 workspaces, dock / undock / float, drop zones | shared merge/split/float/redock, tab reorder, resize and workspace persistence have tests; floating groups are in-app | 60% | separate native multi-monitor windows (#466) and visual/platform validation; rearrangement (#493) implemented | 20–35 h |
| Project panel | List / Icon / Freeform, hover scrub, marquee | all three views, hover scrub, view presets; no marquee (#578), Freeform bins and pan (#579), bins can't be left on Windows (#587), duplicate imports allowed (#356) | 75% | #578, #579, #587, #456 | 10–20 h |
| Timeline display | filmstrip thumbnails, waveforms, track heights, renaming | Head, Head and Tail and Continuous thumbnail modes implemented; waveforms and heights | 70% | recheck track rename (#654) and audio-area scrolling (#400); filmstrip (#614) implemented (`tests/timeline_scroll_ui.rs`) | 8–15 h |
| Transitions | drag, resize, alignment, edit in Effect Controls | drag / select / resize (#503); settings and the transition's own timeline in Effect Controls, drag to slide or resize (#577); no A/B previews with Start / End sliders | 70% | #577 | 5–10 h |
| Numeric entry and scrubbing | scrubbable hot text, typed values | scrubbable values, timecode scrub-edit (#576); audio clip mixer values cannot be typed (#481) | 80% | #481 | 3–6 h |
| Drag and drop | OS files into Project / Timeline, clipboard paste | OS drops into the timeline (#517); no clipboard paste of images / files (#611) | 70% | #519, #611 | 5–10 h |
| Theme and appearance | dark / light, brightness | dark / light / system (#505, #555) | 85% | macOS window controls (#637), Wayland title bar (#433), X11 scale (#457) | 5–10 h |
| Context menus | per panel and per item | most panels; check against Premiere not done | 70% | — | 5–10 h |
| Accessibility | screen reader partial, keyboard navigation | keyboard navigation; every widget has an automation id; no screen-reader support | 30% | — | 15–30 h |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-21 | minor | Source-confirmed handles, Button Editor, divider/row lines, Position graphs, docking, deselect/gap deletion and filmstrip; no new visual run or numeric estimate |
| 2026-10-10 | major | Created: UI areas against Premiere 26.5.2, menu label diff, keyboard parity from keyboard.md, open UI issues itemized |

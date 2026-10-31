# UI and interaction parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-30 · **Change:** minor (Timeline rows checked in the app; track rename is still missing, #654) · **Target:** Adobe Premiere Pro 2026 (26.5.2, macOS)

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
| Timeline editing tools | Selection, Track Select, Ripple / Rolling / Rate Stretch, Razor, Slip / Slide, Pen, Hand, Zoom, Type | all present, trim modes, Trim Monitor, dynamic trimming; a click on empty space deselects (#683 fixed); Delete goes by the panel in focus (#404 fixed); gap delete (#648, #668), a dragged edge stops at the end of the media (#653, with no mark at the limit), new tracks by dropping past the last one (#483), Escape cancels drag (#580), Shift+Delete and Alt-click delete (#680) | 80% | — | 30–50 h |
| Program monitor on-screen controls | Motion / Transform handles, mask handles, safe margins, guides, transparency grid | guides, rulers, safe margins, transparency grid, snapping, mask drawing; **no transform handles** (#639); Button Editor (+) does nothing (#431) | 55% | #639, #431 | 15–25 h |
| Effect Controls | property rows, keyframe area with zoom, divider, graphs, eyedroppers | rows, keyframes, value / velocity graphs, eyedropper (#518); missing keyframe zoom (#641, #645), draggable divider (#643), row lines (#640), Position curves (#448) | 60% | #412, #640–#645, #448 | 15–25 h |
| Panels and docking | 17 workspaces, dock / undock / float, drop zones | 11 workspaces, docking; rearranging windows reported missing (#493); multi-monitor floating windows (#466) | 60% | #493, #466 | 20–35 h |
| Project panel | List / Icon / Freeform, hover scrub, marquee | all three views, hover scrub, view presets; no marquee (#578), Freeform bins and pan (#579), bins can't be left on Windows (#587), duplicate imports allowed (#356) | 75% | #578, #579, #587, #456 | 10–20 h |
| Timeline display | filmstrip thumbnails, waveforms, track heights, renaming, duplicate frame markers | clip thumbnails with an optional continuous filmstrip (#614), waveforms, heights, duplicate frame markers; the wheel goes sideways by default on Windows and Linux, up and down on macOS (#400 fixed); no track rename in the UI, only `timeline.setTrack` (#654 is closed, but its change was reverted the same day; PR #2309 is open) | 70% | #654 | 8–15 h |
| Transitions | drag, resize, alignment, edit in Effect Controls | drag / select / resize (#503); settings and the transition's own timeline in Effect Controls, drag to slide or resize (#577), drag the cut to move it with the transition; no A/B previews with Start / End sliders | 70% | #577 | 5–10 h |
| Numeric entry and scrubbing | scrubbable hot text, typed values | scrubbable values, timecode scrub-edit (#576); audio clip mixer values cannot be typed (#481) | 80% | #481 | 3–6 h |
| Drag and drop | OS files into Project / Timeline, clipboard paste | OS drops into the timeline (#517); no clipboard paste of images / files (#611) | 70% | #519, #611 | 5–10 h |
| Theme and appearance | dark / light, brightness | dark / light / system (#505, #555) | 85% | macOS window controls (#637), Wayland title bar (#433), X11 scale (#457) | 5–10 h |
| Context menus | per panel and per item | most panels; check against Premiere not done | 70% | — | 5–10 h |
| Accessibility | screen reader partial, keyboard navigation | keyboard navigation; every widget has an automation id; no screen-reader support | 30% | — | 15–30 h |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created: UI areas against Premiere 26.5.2, menu label diff, keyboard parity from keyboard.md, open UI issues itemized |
| 2026-10-30 | minor | Clicking empty space deselects, on the Program monitor too (#683) |
| 2026-10-30 | minor | Delete and Backspace go by the panel in focus (#404) |
| 2026-10-30 | minor | The wheel over the Timeline goes sideways by default on Windows and Linux (#400) |
| 2026-10-30 | minor | Timeline rows checked in the app: #614, #648, #653, #668, #680 and #483 work and leave the open list; #654 stays, its change was reverted |

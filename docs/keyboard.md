# Keyboard parity with Premiere Pro

FilmCraft's **Premiere Pro Compatible** shortcut preset (Edit ▸ Keyboard Shortcuts…) follows
Premiere's default keyboard (macOS). The table lives in `crates/engine/src/shortcut_presets.rs`:
`PREMIERE` (application-wide keys and a few panel keys) and `PREMIERE_PANEL` (panel-scoped keys
that FilmCraft Default uses as well). `presets::premiere()` is the whole preset.

Every entry names a FilmCraft command id. Engine commands run through `Session::execute`; UI
commands (frames, panels, monitor zoom, Project and Text panel navigation) run through
`menus::invoke`. Both are reachable by id from the CLI (`filmcraft-cli exec <id>`), the control
channel (`engine.execute` / `ui.menu.invoke`) and MCP (`command_run`).

Tests: `crates/engine/src/keyboard_tests.rs` (no key used twice in one scope; behaviour of the
keyboard commands, with undo) and `crates/ui-egui/tests/it/keyboard_ui.rs` (no dangling ids in the
tables, no conflicts in FilmCraft Default or the Premiere preset, keys pressed in the headless app).
The Evolve preset is described [below](#evolve-preset).

## Keyboard-only commands (M3.12)

Premiere has these as keys only (most are not in a menu). Modules: `filmcraft_engine::keyboard`
(engine) and `filmcraft_ui_egui::panels::keyboard` (UI); their module docs list each id.

| Group | Commands | Keys |
|---|---|---|
| Navigation | Go to Next/Previous Edit Point (targeted tracks), …on Any Track, Go to Selected Clip Start/End, Reveal Nested Sequence | Down/Up, Shift+Down/Up, Shift+Home/End, Cmd+Alt+F |
| Selection | Select Clip at Playhead, Select Next/Previous Clip, Select Find Box, Open Search | D, Cmd+Down/Up, Shift+F, Cmd+Shift+F |
| Trimming | Extend Previous/Next Edit To Playhead; Nudge Clip Selection Left/Right One/Five Frames, Up/Down; Slip and Slide Clip Selection Left/Right One/Five Frames | Shift+Q/W; Cmd+(Shift+)Left/Right, Alt+Up/Down; Cmd+Alt+(Shift+)Left/Right; Alt+(Shift+), / . (Timeline) |
| Targeting | Toggle All Video/Audio Targets, Toggle All Source Video/Audio; Toggle Target Video/Audio 1–8, Move All Video/Audio Targets Up/Down, Toggle Mute/Solo for targeted audio, Toggle Track Output for targeted video (unbound, as in Premiere) | Cmd+0/9, Cmd+Alt+0/9 |
| Tracks | Expand/Minimize All Tracks (again restores), Increase/Decrease Video/Audio Tracks Height, Show Next/Previous Screen | Shift+= / Shift+-, Cmd+= / Cmd+- / Alt+= / Alt+-, PageDown/PageUp (Timeline) |
| Work area | Set Work Area Bar In/Out Point (`sequence.setWorkAreaIn` / `setWorkAreaOut` in `filmcraft_engine::commands`; the Timeline panel menu's Work Area Bar shows and hides the bar) | Alt+[ / Alt+] (Timeline) |
| Monitors | Zoom Program/Source Monitor to 100%/Fit, Toggle Full Screen, Play In to Out with Preroll/Postroll, Play from Playhead to Out Point, Export Frame, Set/Clear Poster Frame | Cmd+Shift+1/0, Cmd+Alt+Shift+1/0, Ctrl+`, Shift+Space, Ctrl+Space, Shift+E, Cmd+P / Alt+P |
| Panels | Maximize or Restore Active Frame / Frame Under Cursor, Select Next/Previous Panel, Toggle Source/Program Monitor Focus, Workspaces 7–9 | Shift+`, `, Ctrl+Shift+. / ,, Alt+Shift+7/8/9 |
| Audio | Increase/Decrease Clip Volume (1 dB) and …Many (Settings ▸ Audio ▸ Large Volume Adjustment), Nudge Volume ±1/±3 dB, Toggle Audio During Scrubbing | ] / [, Shift+] / Shift+[, Shift+S |
| Graphics | Increase/Decrease Font Size and Leading by One/Five Units, Left/Center/Right align text, Begin Text Editing, Nudge Selected Object by one/five (graphic layers, else Motion position) | Cmd+Alt+(Shift+)Left/Right, Alt+(Shift+)Up/Down, Cmd+Shift+L/C/R, Cmd+Alt+', Cmd+(Shift+)arrows (Program, Properties) |
| History panel | Delete (Delete Redoable Actions, which asks for OK first) | Backspace |
| Project panel | List/Icon/Toggle View, Hover Scrub, Thumbnail Size Next/Previous (and = / -), Move/Extend Selection, Mark In / Out of the hover-scrubbed clip | Cmd+PageUp/PageDown, Shift+\, Shift+H, Shift+] / Shift+[, arrows, Home/End, PageUp/PageDown, I / O |
| Text panel | Navigate/Select to Previous/Next Word and Line, Start/End of Segment, Delete (lift), Ripple Delete (extract), Show Program Transcript, Merge/Split Segments | arrows, Shift+arrows, Home/End, Shift+Home, Cmd+Shift+Down, Alt+Backspace, Backspace, Shift+X, Alt+M / Alt+S |
| Other | Help (F1), Quit (Cmd+Q), Media Browser ▸ Open In Source Monitor (imports the file first), Send to Media Encoder (= Send to Export Queue, Alt+Shift+M), Metadata panel Play/Loop | |

Existing commands that only lacked their Premiere key were added to the table too (Close, Close
Project, Import from Media Browser, Get Media File Properties, Paste Attributes, Find, Edit
Original, Make Subclip, Audio Channels, the Graphics New Layer / Arrange / Select Layer commands,
Program monitor Show Rulers / Show Guides / Snap / Lock Guides, New Bin From Selection, Paste and
Paste Insert in the Timeline, the Audio Track Mixer Loop). The Audio Track Mixer menu's
Show/Hide Tracks… and Meter Input(s) Only are the commands `mixer.showHideTracks` and
`mixer.meterInputOnly`, on Cmd+Alt+T and Ctrl+Shift+I with the Audio Track Mixer focused.

FilmCraft Default keeps its own keys where they differ (Shift+E is Clip ▸ Enable there, so Export
Frame has no default key; the Premiere preset moves Shift+E to Export Frame and Enable to ⇧⌘E).

## Evolve preset

The **Evolve** preset (`shortcut_presets::EVOLVE`) maps the default keys of page-based grading
suites onto FilmCraft's commands for editors coming from them ([appearance.md](appearance.md) has
the matching theme and page workspaces). Key assignments are functional facts about how such
suites are operated; nothing from any other product is used. Like the
other presets it starts from FilmCraft Default and gives each listed command exactly the listed
keys, taking a key away from whatever else had it (so Find loses ⌘F to the full-screen viewer,
and the Track Select tools lose A / ⇧A to the Selection tool and Mark Selection).
A custom preset saved as "Evolve" before this preset existed is kept: its file is untouched and
it is listed, loaded and restored at launch as **Evolve (custom)** (**Evolve (custom 2)** when a preset
of that name exists, so every listed name is one file).

| Group | Key → FilmCraft command |
|---|---|
| Modes | A Selection tool, T Trim Edit, B Razor, N Snapping, ⇧⌘L Linked Selection |
| Playback | Space, J / K / L, ← / → one frame, ⇧← / ⇧→ (one second in such suites; FilmCraft: five frames), ↑ / ↓ previous / next edit, Home / End, / Play Around, ⌥/ Play In to Out, ⌘/ Loop, F Match Frame, Q Toggle Source/Program Monitor Focus, ⌘F Toggle Full Screen, Z Zoom Program Monitor to Fit, ⇧S Toggle Audio During Scrubbing |
| Marks and markers | I / O, X Mark Clip, ⇧A Mark Selection, ⌥I / ⌥O / ⌥X clear, ⇧I / ⇧O go to, M Add Marker, ⌥M Clear Current Marker, ⇧↑ / ⇧↓ previous / next marker |
| Edits | F9 Insert, F10 Overwrite, F11 Replace With Clip From Source Monitor, ⌘B Add Edit, ⌥\ Join Through Edits, ⌘Z / ⇧⌘Z, ⌘X / ⌘C / ⌘V, ⇧⌘V Paste Insert, ⌥V Paste Attributes, ⌘A / ⇧⌘A, Backspace and Delete Clear, ⇧Backspace Ripple Delete, D Enable, ⌥⌘L Link, ⌘R Speed/Duration, ⌥⌘= / ⌥⌘- clip volume |
| Transitions | ⌘T Apply Default Transitions to Selection, ⌥T video only, ⇧T audio only |
| Trimming | V Select Nearest Edit Point, U Toggle Trim Type, E Extend Edit to Playhead, ⇧[ / ⇧] Trim Previous / Next Edit to Playhead, ⇧⌘[ / ⇧⌘] Ripple Trim Previous / Next Edit to Playhead, , / . one frame, ⇧, / ⇧. many frames, ⌥↑ / ⌥↓ nudge the selection up / down (Timeline) |
| Zoom | ⌘= / ⌘- (application-wide and in the Timeline, where the Standard preset's track-height keys would otherwise shadow them), ⇧Z Zoom to Sequence |
| Multicam | 1–9 cut to that camera |
| Pages | ⇧1 Project Manager, ⇧2 Media, ⇧3 Cut, ⇧4 Edit, ⇧6 Color, ⇧7 Audio, ⇧8 Deliver (`window.page.*`, which switch to the Evolve workspace set), ⇧9 Project Settings |
| Project and application | ⌘N New Sequence, ⇧⌘N New Bin, ⌘I Import, ⌘S / ⇧⌘S, ⌘, Settings, ⌥⌘K Keyboard Shortcuts, ⌘Q |

Tests: `keyboard_tests::shortcut_tables_have_no_key_used_twice_in_one_scope` (the table),
`shortcuts_tests` (the preset loads with no conflicts on macOS, every entry bound as listed, ⇧J / ⇧L
free; a custom "Evolve" stays reachable) and
`crates/ui-egui/tests/keyboard_ui.rs` (every id is a command; no conflicts on macOS or Windows
with the frontend's commands registered).

### Evolve keys with no FilmCraft counterpart

| Command (key) | Reason |
|---|---|
| Place On Top (F12), Ripple Overwrite (⇧F10), Fit to Fill (⇧F11), Append at End (⇧F12) | FilmCraft has Insert and Overwrite (and Replace); the other three-point edit types are not commands yet. |
| Split Clip (⌘\) | Add Edit (⌘B) cuts at the playhead on the targeted tracks; a split of the selected clips only is not a separate command. |
| Dynamic Trim Mode (W) | FilmCraft's dynamic J/K/L trimming is entered from Trim Edit (T); there is no separate mode toggle. |
| Modify Marker (⇧M) | Markers are edited from their dialog (double-click) or `markers.edit {marker}`; there is no "edit the marker at the playhead" key. |
| Flag Clip (G) and the flag colours | FilmCraft has label colours (Edit ▸ Label), not per-clip flags. |
| Select Clips Forward / Backward on This Track (Y / ⌥Y) and on All Tracks (⌘Y / ⌥⌘Y) | FilmCraft's Track Select tools select by clicking; there is no keyboard selection from the playhead. |
| Compositing page (⇧5) | No node-compositing page; the key keeps its FilmCraft Default meaning (the Effect Controls panel). |
| Enhanced Viewer (⌥F), Full Page Viewer (⇧F) | Only Toggle Full Screen (⌘F) and Maximize Frame (⇧`) exist. |
| Smart Insert, Smart Overwrite and edits at a smart indicator | Cut-page features without a FilmCraft counterpart. |
| Fast forward / reverse (⇧L / ⇧J) | FilmCraft has slow forward / reverse on those keys in FilmCraft Default; the preset leaves them unbound rather than map them wrongly (`shortcut_presets::EVOLVE_UNBOUND`). |
| Render cache and compositing output cache (⌥R and the cache menu) | Render previews work from In/Out (Sequence menu), not from a cache toggle. |

## Skipped Premiere default shortcuts

| Premiere command (key, scope) | Reason |
|---|---|
| Generative Media Tool (⇧V) | Adobe Firefly cloud generation; not part of FilmCraft. |
| Add to object / Subtract from object (⌘= / ⌘-) | Refine an AI object mask (Object Mask tool); FilmCraft has no AI object masks. |
| Change Draw Mode (⌥⌘L) | Premiere's shape draw modes for the Pen/shape tools; FilmCraft's shape tools have no draw modes. |
| Production panel: New Project, New Folder, Close Project, Make a Copy, Move Selection Home/End/Page Up/Page Down, Move To Trash, Open Project, Zoom In/Out (12 keys) | Productions (shared multi-project folders) are not implemented; there is no Production panel. |
| Search panel: Open in Source Monitor (⇧O) | No separate Search panel; Open Search (⇧⌘F) focuses the Project panel search. |
| Effect Controls: Remove Selected Effect (Delete), Loop During Audio-Only Playback (⌘L) | Effect Controls has no effect selection state and no audio-only playback mode; effects are removed from their context menu or `effects.remove`. |
| Effects panel: New Custom Bin (⌘/), Delete Custom Item (Delete) | The Effects panel has no custom bins (effect presets live in the Presets bin). |
| Media Browser: Select Directory List / Select Media List (⇧← / ⇧→) | The directory tree and media list take no separate keyboard focus. |
| Project panel: Delete Selection with Options (⌘Delete) | Needs the "delete instances in sequences" dialog; plain Clear (Backspace) deletes. |
| Project panel: Next/Previous Column Field, Next/Previous Row Field (Tab, ⇧Tab, Return, ⇧Return) | The list view has no inline-editable metadata cells. |
| Text panel: Edit Segment (Return) | No inline transcript text editing (corrections go through `transcript.set`). |
| Text panel: Follow Active Monitor (⇧C), Show Source Transcript (⇧Z) | Only sequence transcripts exist; source clips have no transcript view. |

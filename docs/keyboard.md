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
keyboard commands, with undo) and `crates/ui-egui/tests/keyboard_ui.rs` (no dangling ids in the
tables, no conflicts in FilmCraft Default or the Premiere preset, keys pressed in the headless app).

## Keyboard layouts

Shortcuts name letters and digits by what they type and every other key by its position on a US
keyboard (`crates/engine/src/shortcuts.rs`, `KEYS`), so one preset works on every layout: the key
a German keyboard labels Ö is `;`, + is `]`, # is `\`, ß is `-`, - is `/`, and its extra key `<`
is `IntlBackslash`. The UI reads key presses the same way (`menus::key_name`: letters by label, so
Z and Y follow QWERTZ; digits and punctuation by position, so Shift+0, which types `=` on a German
keyboard, is still Shift+0, and Ctrl+Alt+9 is not AltGr's `]`). Modifiers must match exactly, as in
Premiere: Alt+J does not run J. The numeric keypad has no keys of its own (egui does not tell them
apart): keypad keys are their main-keyboard twins.

Settings ▸ General ▸ Keyboard Layout (also in the Keyboard Shortcuts dialog) picks the labels for
menus and the shortcut editor: US, or Deutsch (QWERTZ), which also draws the German ISO keyboard.

## Importing Premiere Pro shortcut files (.kys)

Edit ▸ Keyboard Shortcuts… ▸ Import… (or `shortcuts.import {path}`) reads the `.kys` file Premiere
saves for a custom keyboard set (Windows: `Documents\Adobe\Premiere Pro\<version>\Profile-<name>\Win\`,
macOS: `…/Mac/`). `crates/engine/src/premiere_kys.rs` maps Premiere's command names onto FilmCraft
ids (`COMMANDS`, `PANEL_COMMANDS`) and its contexts onto panels; the keys go through the keyboard
layout the file was made with (detected: a German file has keys typing Ö, Ä, Ü or ß; `layout`
overrides), which also becomes the label layout. The result is a custom preset named after the file,
made active: FilmCraft Default, where every command the file lists gets exactly the file's keys
(listed without a key = unbound) and keys the file gives to commands FilmCraft lacks stay free.
What could not come over is returned in `skipped` with the reason (shown in the dialog): commands
FilmCraft does not have (table below), panels it does not have (Productions), keys no US-position
name covers (`=` is Shift+0 on a German keyboard), and keypad keys whose main-keyboard twin the
file already uses for something else.

Commands that exist so imported keys have something to run: Move Playhead to Cursor
(`timeline.playheadToCursor`), Audio Track Mixer ▸ Show/Hide Tracks… (`mixer.showHideTracks`) and
Meter Input(s) Only (`mixer.meterInputOnly`), the Rectangle and Ellipse tools (`tool.rectangle`,
`tool.ellipse`). Every menu item can have a key now, as in Premiere (Nest…, Rename…, Export ▸ AAF…):
the menus run them without parameters, opening their dialog where one is needed.

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
| Monitors | Zoom Program/Source Monitor to 100%/Fit, Toggle Full Screen, Play In to Out with Preroll/Postroll, Play from Playhead to Out Point, Export Frame, Set/Clear Poster Frame | Cmd+Shift+1/0, Cmd+Alt+Shift+1/0, Ctrl+`, Shift+Space, Ctrl+Space, Shift+E, Cmd+P / Alt+P |
| Panels | Maximize or Restore Active Frame / Frame Under Cursor, Select Next/Previous Panel, Toggle Source/Program Monitor Focus, Workspaces 7–9 | Shift+`, `, Ctrl+Shift+. / ,, Alt+Shift+7/8/9 |
| Audio | Increase/Decrease Clip Volume (1 dB) and …Many (Settings ▸ Audio ▸ Large Volume Adjustment), Nudge Volume ±1/±3 dB, Toggle Audio During Scrubbing | ] / [, Shift+] / Shift+[, Shift+S |
| Graphics | Increase/Decrease Font Size and Leading by One/Five Units, Left/Center/Right align text, Begin Text Editing, Nudge Selected Object by one/five (graphic layers, else Motion position) | Cmd+Alt+(Shift+)Left/Right, Alt+(Shift+)Up/Down, Cmd+Shift+L/C/R, Cmd+Alt+', Cmd+(Shift+)arrows (Program, Properties) |
| Project panel | List/Icon/Toggle View, Hover Scrub, Thumbnail Size Next/Previous (and = / -), Move/Extend Selection, Mark In / Out of the hover-scrubbed clip | Cmd+PageUp/PageDown, Shift+\, Shift+H, Shift+] / Shift+[, arrows, Home/End, PageUp/PageDown, I / O |
| Text panel | Navigate/Select to Previous/Next Word and Line, Start/End of Segment, Delete (lift), Ripple Delete (extract), Show Program Transcript, Merge/Split Segments | arrows, Shift+arrows, Home/End, Shift+Home, Cmd+Shift+Down, Alt+Backspace, Backspace, Shift+X, Alt+M / Alt+S |
| Other | Help (F1), Quit (Cmd+Q), Media Browser ▸ Open In Source Monitor (imports the file first), Send to Media Encoder (= Send to Export Queue, Alt+Shift+M), Metadata panel Play/Loop | |

Existing commands that only lacked their Premiere key were added to the table too (Close, Close
Project, Import from Media Browser, Get Media File Properties, Paste Attributes, Find, Edit
Original, Make Subclip, Audio Channels, the Graphics New Layer / Arrange / Select Layer commands,
Program monitor Show Rulers / Show Guides / Snap / Lock Guides, New Bin From Selection, Paste and
Paste Insert in the Timeline, the Audio Track Mixer Loop).

Clear acts on the focused panel, as in Premiere. In Effect Controls, Backspace and Delete
(`effectControls.clear`, both presets) remove the selected keyframes, else the selected effects, as
one undo step (`effects.clear`); with nothing selected they do nothing, and they never delete the
Timeline's clips. Clicking an effect's name selects it and clicking a keyframe in the lane selects
that (Shift or Cmd adds); the triangle folds the effect. Fixed effects (Motion, Opacity, Time
Remapping, Volume…) are never removed: Reset Effect resets them. Any other key a preset gives
Clear (`edit.clear`, `project.delete`) does the same while Effect Controls has focus, and a `.kys`
file's Effect Controls Clear key maps onto `effectControls.clear`.

FilmCraft Default keeps its own keys where they differ (Shift+E is Clip ▸ Enable there, so Export
Frame has no default key; the Premiere preset moves Shift+E to Export Frame and Enable to ⇧⌘E).

## Skipped Premiere default shortcuts

| Premiere command (key, scope) | Reason |
|---|---|
| Generative Media Tool (⇧V) | Adobe Firefly cloud generation; not part of FilmCraft. |
| Add to object / Subtract from object (⌘= / ⌘-) | Refine an AI object mask (Object Mask tool); FilmCraft has no AI object masks. |
| Change Draw Mode (⌥⌘L) | Premiere's shape draw modes for the Pen/shape tools; FilmCraft's shape tools have no draw modes. |
| Production panel: New Project, New Folder, Close Project, Make a Copy, Move Selection Home/End/Page Up/Page Down, Move To Trash, Open Project, Zoom In/Out (12 keys) | Productions (shared multi-project folders) are not implemented; there is no Production panel. |
| Search panel: Open in Source Monitor (⇧O) | No separate Search panel; Open Search (⇧⌘F) focuses the Project panel search. |
| Effect Controls: Loop During Audio-Only Playback (⌘L) | Effect Controls has no audio-only playback mode. |
| Effects panel: New Custom Bin (⌘/), Delete Custom Item (Delete) | The Effects panel has no custom bins (effect presets live in the Presets bin). |
| History panel: Delete (Delete) | History states cannot be selected and deleted individually. |
| Media Browser: Select Directory List / Select Media List (⇧← / ⇧→) | The directory tree and media list take no separate keyboard focus. |
| Project panel: Delete Selection with Options (⌘Delete) | Needs the "delete instances in sequences" dialog; plain Clear (Backspace) deletes. |
| Project panel: Next/Previous Column Field, Next/Previous Row Field (Tab, ⇧Tab, Return, ⇧Return) | The list view has no inline-editable metadata cells. |
| Text panel: Edit Segment (Return) | No inline transcript text editing (corrections go through `transcript.set`). |
| Text panel: Follow Active Monitor (⇧C), Show Source Transcript (⇧Z) | Only sequence transcripts exist; source clips have no transcript view. |
| Timeline: Set Work Area Bar In/Out Point (⌥[ / ⌥]) | FilmCraft has no work area bar (Premiere hides it by default; renders use In/Out). |
| Replace With After Effects Composition, Batch Capture, Range Selection tool, Effect Controls snapping options | No After Effects link, no tape capture, no such tool or option (reported by the `.kys` import). |

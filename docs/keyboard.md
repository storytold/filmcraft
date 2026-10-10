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
| Audio Track Mixer: Show/Hide Tracks… (⌥⌘T), Meter Input(s) Only (⌃⇧I) | The mixer has no track visibility dialog and no input metering (no hardware input recording). |
| Effect Controls: Remove Selected Effect (Delete), Loop During Audio-Only Playback (⌘L) | Effect Controls has no effect selection state and no audio-only playback mode; effects are removed from their context menu or `effects.remove`. |
| Effects panel: New Custom Bin (⌘/), Delete Custom Item (Delete) | The Effects panel has no custom bins (effect presets live in the Presets bin). |
| History panel: Delete (Delete) | History states cannot be selected and deleted individually. |
| Media Browser: Select Directory List / Select Media List (⇧← / ⇧→) | The directory tree and media list take no separate keyboard focus. |
| Project panel: Delete Selection with Options (⌘Delete) | Needs the "delete instances in sequences" dialog; plain Clear (Backspace) deletes. |
| Project panel: Next/Previous Column Field, Next/Previous Row Field (Tab, ⇧Tab, Return, ⇧Return) | The list view has no inline-editable metadata cells. |
| Text panel: Edit Segment (Return) | Return does not open segment editing; double-click a word for a spelling correction (`transcript.correctWord`). |
| Text panel: Follow Active Monitor (⇧C), Show Source Transcript (⇧Z) | The Transcript tab has Sequence / Source view buttons; these shortcuts and automatic monitor following are not yet mapped. |
| Timeline: Set Work Area Bar In/Out Point (⌥[ / ⌥]) | FilmCraft has no work area bar (Premiere hides it by default; renders use In/Out). |

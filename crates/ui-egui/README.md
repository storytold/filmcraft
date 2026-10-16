# FilmCraft interface

The egui frontend draws panels and dialogs, and dispatches project changes through the engine.
Interactive controls keep stable automation ids regardless of the interface language.

## Transition drop previews

Dragging a video or audio transition over a compatible clip shows its actual time span, the
selected cut and an In/Out label. Shared cuts center the span; isolated edges use a one-sided
span. The preview uses the engine's read-only transition planner, including Timeline duration
preferences, and release dispatches `effects.apply` with the same edge. Locked tracks and
incompatible clip types show no valid transition preview. Ordinary effects retain their clip
outline. `timeline.transitionDropPreview` exposes the span and edge to automation.

`transition_drop_ui` covers both edges of short video/audio clips, hover without project edits,
the committed span and undo. Set `FILMCRAFT_UI_SNAPSHOT_DIR` to render screenshots with wgpu.

## File-manager video drops

Drop video files from the file manager onto Source to import them into the current bin and
open the first successful import for preview. The timeline stays unchanged. Source shows a
border while files hover. Drop onto Timeline to place imported clips at the pointer with linked
sound; an empty Timeline creates a sequence matching the first clip. Multiple files are placed
back to back. Preferences ▸ Timeline ▸ "Place files dropped onto the Timeline directly on the
Timeline" controls timeline placement; Source preview works independently of that preference.
Drops elsewhere import into the bin. Covered panels do not receive the drop.

Failed and partially failed imports report their paths in the status bar. Batches over 1024
files are refused without changing the project. Native hosts can supply the cursor position
during file drags, when egui receives no motion events. `timeline_drop_ui` covers Source,
Timeline, matching sequence creation, linked sound, undo/redo, stale cursor input, panel
occlusion, preferences, corrupt/missing files and batch limits with original generated media.

On Linux, the desktop app prefers a working X11/XWayland connection because winit 0.30 does
not receive native Wayland file drops. X11 cursor queries use safe Rust bindings. Without a
working X11 server the default backend remains available. `FILMCRAFT_WINDOW_BACKEND=x11` or
`wayland` explicitly selects a backend; native Wayland currently requires Import… for files.
The browser's file-drop handler continues to import files into the bin.

## Timeline selection

Box selection previews and commits the same clips. Alt selects individual clips without their
linked partners; turning Linked Selection off does the same for an ordinary box. Release sends
the chosen link behavior through `timeline.select`. Clear/Delete acts on that exact selection,
so deleting an individually selected video preserves its unselected audio. Normal linked
selections still delete both sides. `timeline_marquee_ui` covers preview, release, deletion,
Shift addition and Escape cancellation; engine tests cover link overrides, add/toggle, malformed
overrides, explicit deletion targets and undo/redo.

## Localisation

Edit > Language offers English, Japanese, Spanish, Brazilian Portuguese, Ukrainian, Simplified
Chinese, German, Russian, French, Indonesian, Korean and Italian (which cover the whole interface). The language is stored in the engine's `general.interfaceLanguage` preference,
restored on startup, and also available in Settings > General.
Its default, System Language (`system`), follows the operating system: the first of the user's
preferred languages that the interface has (the host supplies them through
`HostHooks::system_languages`: `sys-locale` on the desktop, `navigator.languages` on the web),
otherwise English.
The `app.language.*` UI commands and `prefs.set` reach it through the control channel.
Japanese requires a craft-fonts build or a suitable installed font.

English source strings are lookup keys in `src/i18n/<code>.tsv`. `tl!` translates literals, `tlf!`
fills translated templates, and `i18n::t` translates names from registries. Placeholder values
(including user filenames containing braces) are inserted literally. Catalog translations are
original work using ordinary language, without proprietary localisation resources.

Spanish, Japanese, Simplified Chinese, French and Brazilian Portuguese cover menus, panels, dialogs, settings and registry labels. Searches accept both the
translated label and its English source, including Unicode capitals. Project content, command ids
and preference values retain their original values. Engine errors, CLI and MCP messages remain
English. Ukrainian and Russian currently cover the core menus, German every menu; they fall back
to English elsewhere.

Verification: `cargo test -p filmcraft-ui-egui` checks catalog syntax, duplicate keys, placeholders,
literal/menu/registry coverage and UI behaviour; `cargo xtask ci` runs the workspace gates. Visual
checks use the control channel to switch language and capture the resulting panels.

## Source playback

`source_playback.rs` auditions media independently of Program playback. It reuses MediaSource
decoding, output mapping, the device clock and the desktop play-ahead buffer. Only one monitor
owns the device at a time. Source frame jobs use the existing frame server and Source resolution.
The live project, undo stack and timeline playhead are unchanged. See [monitor documentation](../../docs/monitors.md)
for behavior, commands, limitations and regression tests. Source playback tests use synthetic
demo media and fake audio outputs; no third-party assets are introduced.

Source drag controls snapshot the selected span and dispatch `timeline.place` for video, audio,
or linked video/audio. Full-clip marks are implicit until set by the user. The engine owns edits,
range validation and undo; the UI owns the drag gesture. See `source_drag_ui` and the engine's
`source_placement_tests` for regression coverage.

Source range handles use transient gesture previews and the engine's integer-frame
`source_monitor::adjust_range`; release dispatches one mark edit. Monitor command routing
keeps Source navigation, markers and marked-range playback separate from Program.

## Pane layout controls

Dock transactions and pointer targets use `craft-ui::docking`; bounded split geometry and
keyboard-accessible resize handles use `craft-ui::layout`. FilmCraft adapts its existing saved
dock tree and panel identities to the shared model. Panel tabs or their header grip can move
between tab groups, split at an edge, reorder, or float when released with Cmd/Ctrl. Floating
panels move by their empty header and resize from the bottom-right corner. Timeline sequence
tabs retain their sequence-opening, closing and horizontal reorder behavior; the header grip
moves the Timeline panel. Workspace saves retain floating groups alongside the legacy layout
JSON, and workspaces without floating data still load. Automation IDs and workspace commands
remain the integration boundary.

Tools and dock tabs expose their actual egui widget IDs, paint keyboard focus, and support
Tab/Shift-Tab traversal and Space/Enter activation. Control-injected keys can reach focused
widgets in a background window. The optional `--control-input-only` native launch mode isolates
automation from native widget/menu input; `ui.inspect` reports native and effective focus
separately. Focus and input-ownership regressions cover document/playhead preservation.

Compact monitor layouts separate the current and duration timecodes from zoom, resolution and
settings. Source video/audio drag choices retain their existing placement behavior. Transport
buttons use `craft-ui::layout::fit_toolbar`; commands beyond the available width remain available
through More, including their selected state and Source/Program routing. A scrolling fallback
retains all commands if even More cannot fit. `monitor_geometry` checks the painted readouts,
control bounds, overflow actions and Source drag payloads across monitor widths and themes.

# FilmCraft interface

The egui frontend draws panels and dialogs, and dispatches project changes through the engine.
Interactive controls keep stable automation ids regardless of the interface language.

## Reactor generation

Window > Reactor opens a dockable Reactor panel for FastH3 Omni generation.
Its tab can be moved, split, floated, closed and reopened like other panels; workspaces retain
its placement. Closing it preserves the current form and does not stop a running generation.
Completed clips are
saved beside the active project in `<project file stem> Media/Reactor/` under unique MP4 names,
then automatically imported into the Project panel's current bin. An unsaved project must be
saved once before generation; the form offers the standard project Save action. Each generation
uses the project path and bin captured when it was queued. If another project is opened meanwhile,
the clip stays in its original media folder and its saved path is reported instead of importing
into the other project. Save As does not move already generated media.

The panel uses a FastH3 Omni heading, a bottom composer, square center-cropped image references,
and local generation cards. Grid view shows two columns; List view gives each card the panel's
width. Completed-card thumbnails come from the downloaded local file. This pass has no embedded
video player or cloud library. Reuse restores the prompt, aspect, duration, and original reference
paths without resubmitting a request. Missing or moved reference files report preview/upload errors.

Up to eight requests can be queued (including the active request); the desktop submits them one at
a time. Queued cards can be cancelled before submission. Progress uses the reported upload, queue,
generation and download stages, without invented completion percentages. Switching projects leaves
queued requests paused until their original project is active again. Closing the panel keeps the
queue running. Cards and queued requests are session-only, bounded to 64 recent jobs; they are not
saved in the project. Quitting the app does not recover an in-flight server generation on restart.

Image and audio decoding runs on bounded background workers with a 24-entry preview cache. Audio
references are square tiles beside image references, with inline playback and seeking over a decoded
waveform using the existing audio output. The prompt uses its placeholder without a repeated visible
heading, and the composer is measured and anchored close to the panel bottom. Timeline
or Source playback takes the output back. Current frame captures the Program monitor at its exact
playhead into a unique PNG in the project's Reactor media folder, without importing it as a clip or
asking for a filename. A project switch during capture never attaches it to the other project's
composer. API key and billing controls remain in Settings.

The `reactor.generate` UI command accepts `prompt`, `seconds`, `aspect`, optional
`reference_images` / `reference_audios` paths, and an optional `output_path`. Omitting `output_path`
uses the same project media folder as the form. The desktop worker creates and reserves a writable
local output before uploading references or submitting a billable request. The API key stays in
the OS credential store. The desktop host shares a zeroizing key cache for one app session:
saving a key populates it, an authorized store read is reused by subsequent generations, and
replacement/removal clears the previous cached key. Store failures do not cache a failed read.
macOS may still request authorization when a new build first reads a key saved by an earlier app
identity. Rebuilt ad-hoc debug binaries do not have the stable signing identity of a distributed
app; the packaging script supports Developer ID signing. The app leaves OS access controls in
place. No network calls or real credential-store access are needed by the panel, cache, and
output storage tests.

Additional UI commands: `reactor.view {view:"grid"|"list"}`, `reactor.references
{kind:"image"|"audio",paths:[absolute paths]}` (or `{kind,remove:index}`), `reactor.reuse {id}`,
`reactor.cancelQueued {id}`, `reactor.captureFrame {}`, `reactor.audio
{path,action:"play"|"pause"|"seek",seconds?}`, and read-only `reactor.jobs {}`.
The `reactor.*` automation IDs expose each control. Unit and GUI tests cover queue snapshots,
project switching, import, full reference reuse, bounded previews, audio seeking/output ownership,
keyboard focus while references change, and both gallery layouts. Optional
`FILMCRAFT_REACTOR_SNAPSHOT_DIR` saves gallery screenshots from the GUI test.

## Transition drop previews

Dragging a video or audio transition over a compatible clip shows its actual time span, the
selected cut and an In/Out label. Shared cuts center the span; isolated edges use a one-sided
span. The preview uses the engine's read-only transition planner, including Timeline duration
preferences, and release dispatches `effects.apply` with the same edge. Locked tracks and
incompatible clip types show no valid transition preview. Ordinary effects retain their clip
outline. `timeline.transitionDropPreview` exposes the span and edge to automation.

`transition_drop_ui` covers both edges of short video/audio clips, hover without project edits,
the committed span and undo. Set `FILMCRAFT_UI_SNAPSHOT_DIR` to render screenshots with wgpu.

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

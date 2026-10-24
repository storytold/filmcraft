# FilmCraft interface

The egui frontend draws panels and dialogs, and dispatches project changes through the engine.
Interactive controls keep stable automation ids regardless of the interface language.

## Script coverage

Before a script is created or imported, the Script panel shows a short getting-started guide for
text import, Project clip drops, candle coverage and Source playback, and frame-linked marks.
The guide wraps and scrolls in small panels and disappears when a script is open. A ? button at the
right of the Script toolbar opens the same guide in a scrollable help popover at any time.
Toolbar controls wrap onto additional rows when the panel is narrow, keeping Scene/Page labels
with their menus and placing search and the document below the measured toolbar height.

Plain text copied from another app can be pasted directly into Script. With no script open,
paste creates a script; otherwise it inserts rows after the selected range (or appends).
While editing a row, paste replaces the text selection at the caret. Each hard line break
adds a row, including blank and terminal rows, with CRLF treated as one break. Later rows
retain stable IDs, labels, candles and marks. A single Undo removes the paste while keeping
text typed beforehand. Clicking outside a Script text field commits the edit and releases input
focus, including clicks into blank canvas or another panel. Oversized text, lines and invalid
offsets fail before any paste edit.

Trimming a take candle hides annotations outside its coverage while preserving their line IDs,
mark IDs and source times. Expanding coverage restores marks, row colors and off-screen passages.
Hidden marks cannot be selected or used to cue Source and do not choose the default thumbnail.
Scene and Page menus retain repeated labels as separate navigation points in script order.
Double-clicking a candle, its mark or a setup thumbnail cues the linked clip and activates Source
for playback shortcuts. Thumbnails open the displayed candle at its thumbnail frame (or first
visible mark, then source start). A single mark click cues Source while retaining Script editing focus.
Setup thumbnails and take-number boxes sit above the first covered row; candle caps, endpoint
handles, off-screen notation and marks align with that row, including blank and wrapped lines.
Candles extend to the exact top and bottom boundaries of their covered rows. The short gray end
caps keep equal heights independently of the first row’s available space.
Clicking a script row anywhere across the canvas clears the active candle, thumbnail and mark
and selects that line; Shift-click extends the line range on press, retaining the endpoint if
the pointer slips or Shift is released before the mouse button. Clicking below the rows clears the
line selection too. Floating thumbnails, candles and marks retain their own click targets.
Backspace/Delete removes the selected mark, candle, or thumbnail's entire setup.
Deleting the final candle removes the empty setup. These commands retain script text and source
media, support Undo, and leave text-field editing alone. Selection clears after deletion to avoid
deleting another object on a repeated key press. Layout and search caches retain the document Arc,
so unrelated project edits do not rescan the script and document changes invalidate them safely.
Project clips can be dropped across the full canvas width. Selected rows anchor both the preview
and the new candle regardless of the pointer row. The dropped setup is centered at the pointer’s
horizontal position, including when scrolled, and stays separate even if its thumbnail overlaps
another setup when the pointer is outside its thumbnail. Dropping directly onto an online thumbnail
adds a candle to that setup, using selected rows or the last candle's coverage when no rows are selected.
A standard egui horizontal scrollbar spans the canvas when the take columns overflow,
using the Timeline's rounded track and thumb styling with scroll-only interaction. Without selected
rows, an explicit candle target takes precedence over creating a setup on the hovered line.
Missing-source thumbnails and candles are explicit recovery targets even with selected rows:
dropping an available Project clip reconnects them instead of creating another instance. Both
context menus also offer Rebind to Selected Project Clip, enabled for one compatible available
Project selection. Recovery reconnects matching candles across all scripts in one Undo step,
using the old Project source ID or retained footage fingerprint, never a filename match.
Matching footage preserves marks, coverage and styling; incompatible footage or a source span
excluding existing marks requires review before any candle changes. Without a retained source ID
or fingerprint, only the targeted candle can be identified safely. `script.rebindTake` exposes
this batch behavior through optional `allMatching:true`; its default remains a single take.
Script `layout.rs` shares row measurements and candle geometry across painting, endpoint targets and
off-screen notation. `selection.rs` owns document-local state transitions: switching scripts through
the toolbar, commands, creation/import or Find in Script clears old edits, drags and replacement
confirmations while retaining view preferences and the section clipboard.
The `script_ui` integration tests cover endpoint drags, hidden-mark targets and repeated labels;
engine tests cover saved data, undo/redo, section copies and deleting hidden lines.
`script.addTake` accepts optional `x` for a new setup in viewport-width units (0–10,000).
Positions beyond the first viewport require this version when reopening; older builds restrict them to 0–1.

## Control input limits

The control channel bounds `ui.click` to 10 clicks and `ui.drag` to 1024 movement steps (#2784).
The MCP bridge sends null for omitted counts/steps, so the validator treats null like an absent
field: one click or 12 drag steps. Negative, fractional, non-numeric and over-limit values still
return errors before queuing any input. Zero clicks emit no button events; 0–1 drag steps use 2.
`control::input_bounds_tests` covers defaults, rejection, unchanged pending input and valid
gestures; `keyboard_ui` verifies that rejected requests leave the UI usable. See the
[control protocol](../../docs/control-protocol.md).

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
Chinese, German, Russian, French, Indonesian, Korean and Italian (all but Ukrainian and Russian cover
the whole interface; those two cover the core menus). The language is stored in the engine's `general.interfaceLanguage` preference,
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

Spanish, Japanese, Simplified Chinese, French, Brazilian Portuguese and German cover menus, panels, dialogs, settings and registry labels. Searches accept both the
translated label and its English source, including Unicode capitals. Project content, command ids
and preference values retain their original values. Engine errors, CLI and MCP messages remain
English. Ukrainian and Russian currently cover the core menus; they fall back to English elsewhere.

A string a catalog lacks shows in English, so a missing translation never fails a test or a
build. `cargo xtask i18n-report` prints each language's missing strings (menus, `tl!` literals and
registry names) and its entries whose source the interface no longer has; `cargo xtask ci` prints
the same report in its log.

Verification: `cargo test -p filmcraft-ui-egui` checks catalog syntax, duplicate keys, placeholders
and UI behaviour in other languages (switching, search, font coverage); `cargo xtask ci` runs the
workspace gates. Visual checks use the control channel to switch language and capture the resulting
panels.

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

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

## Marker properties

Right-click a timeline or Markers-panel marker and choose **Edit Marker…**. The window
shows time/duration and edits the name, notes, author, and color. Existing presets are
shown as swatches; project custom swatches are below them. Done makes one undoable marker
edit; Cancel discards the marker draft.

The **+** square below the presets opens a separate wheel/hex editor. It includes saturation and
brightness sliders, keyboard-accessible hue, and exact six-digit sRGB hex entry. Add color
saves a reusable swatch to this project's palette through `colors.addCustom` (a separate
undoable operation) and selects it in the marker draft. Canceling the marker edit afterward
does not remove a deliberately saved palette color. The project stores at most 64 custom
swatches shared by markers and clips; duplicate additions are no-ops. Used custom colors also appear for reuse.
Marker/color dialogs belong to the opened project instance, close when that project is replaced,
and are not restored from saved UI preferences. Edits and undo within the same project keep them open.

Markers and clips now share the existing 16 clip preset colors; clip RGB values and preset
names are unchanged. Markers no longer collapse those names onto seven display colors.
Red/Rose, Orange/Mango, Cyan/Teal and Pink/Magenta resolve to the same presets. Exact custom colors
(including white, black, pink, and gray) override a marker's preset. Preset color filters
leave custom markers visible. Selecting a preset clears the override, preserving the author. The preset colors also follow Settings → Labels in both views.

The opaque sRGB editor lives in `widgets::color_picker`, independently of marker models,
dialogs and project commands. Callers provide a `ColorDraft`, automation registry and id
prefix. Its HSV conversion operates on sRGB channels; using linear-light saturation would
make the center too white. Lumetri's grading wheels remain separate: they edit grading
offsets. Existing effect/graphics pickers retain their alpha/float/color-space semantics.

The same author and hex values are available to Source marker commands. Existing markers
have an empty author; FilmCraft never infers authorship from the operating-system account.
Projects (schema 16) retain marker properties and the project palette. Marker CSV reports
and FilmCraft OTIO metadata retain individual marker authors and colors. OTIO also
retains custom clip label colors and the palette in FilmCraft metadata. Other interchange exports warn when they cannot preserve those marker properties.
Importing an OTIO document merges its saved swatches after the current project's swatches, omitting duplicates and respecting the 64-swatch limit.
This does not add AVB marker import or Avid track-bound markers.

Regression coverage includes project palette persistence/isolation/bounds/undo, marker
commands and reopen, separate picker and marker drafts, preset/custom swatch reuse,
sRGB wheel conversion, CSV escaping, and OTIO round trips.

Clip and Project-panel Label menus expose the same project custom palette below the presets.
Add custom color opens the shared picker; Add and apply saves the swatch and applies it to
the captured selection in one undoable operation. `edit.label` accepts a preset/alias or hex,
explicit `clips`/`items` targets, and optional `saveColor:true`. Choosing a named preset clears
that item's custom override. Duplicated items and newly inserted source clips retain the exact
custom label color. Bright custom timeline fills use dark text for readability.

The marker editor also edits exact frame time and duration, switches between point and range
markers, and deletes by marker ID. Double-click a timeline, monitor, or panel marker to edit it;
right-click exposes Edit Marker and Delete Marker. Source and sequence ownership is shown and
captured at opening, so changing the open clip or sequence does not redirect edits. Unchanged
time fields preserve imported subframe ticks. A source range cannot extend beyond its source.

The Markers panel switches between Sequence and Source, searches name/notes/author, filters
preset and custom colors, and supports Command/Ctrl-click, Shift-click, and Select all for the
visible rows. Bulk color changes and deletion are each one undo step. Hidden rows leave the
selection. `markers.edit` and `markers.delete` take an explicit `sequence` (program) or `item`
(source) and either `marker` or `markers`; invalid IDs reject the entire operation.

Manage Colors is available from the Markers panel and clip Label menus. The separate project
palette manager adds, names, edits, and removes swatches using the shared color picker. Editing
or removing a saved swatch preserves already assigned hex values; used colors remain available
for reuse. `colors.addCustom`, `colors.updateCustom`, and `colors.removeCustom` are undoable.
Names persist in the project and FilmCraft OTIO metadata; existing sixteen presets are unchanged.

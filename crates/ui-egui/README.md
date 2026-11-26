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

## Selected text strokes

With characters selected using the Type tool, both Stroke rows read and write that range's
enablement, colour, width and Outer/Center/Inner placement. Unstyled characters inherit each
layer property independently; without a character selection the controls edit the layer.
Overrides survive text edits, project saves and Undo/Redo. A width or colour drag is one undo
step. `graphics_ui::stroke_colour_pickers_edit_only_the_type_tool_selection` drives both rows
through the control channel; `FILMCRAFT_UI_SHOTS` enables a rendered screenshot.

## Export range warnings

The Export panel shows the selected range's length in sequence frames and seconds beside Range
(`export.range.duration`). When Source In/Out covers just one sequence frame and at least one
mark is set, `export.range.warning` explains how to export the whole sequence instead (#3364).
The warning is advisory: exporting one frame remains valid. Entire Source, Work Area and Custom
ranges do not show this warning. `export_ui::single_frame_in_out_warning_updates_with_range_and_marks`
checks the warning and duration as marks and range selections change; the same test can render
`export-single-frame-warning.png` with `FILMCRAFT_UI_SNAPSHOT_DIR`.

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
incompatible clip types show no valid transition preview. The drop snaps to Start at Cut, Center
at Cut or End at Cut by where the pointer is relative to the cut. Ordinary effects retain their
clip outline. `timeline.transitionDropPreview` exposes the span and alignment to automation.

`transition_drop_ui` covers the three alignments on video/audio cuts and a one-sided clip edge,
hover without project edits, the committed span and undo. Set `FILMCRAFT_UI_SNAPSHOT_DIR` to
render screenshots with wgpu.

## Localisation

Edit > Language offers English, Japanese, Spanish, Brazilian Portuguese, Ukrainian, Simplified
Chinese, German, Russian, French, Indonesian, Korean, Italian, Czech, Polish, Swedish and Turkish (all but Ukrainian cover
the whole interface; Ukrainian covers the core menus). The language is stored in the engine's `general.interfaceLanguage` preference,
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

Spanish, Japanese, Simplified Chinese, French, Brazilian Portuguese, German and Russian cover
menus, panels, dialogs, settings and registry labels. Searches accept both the
translated label and its English source, including Unicode capitals. Project content, command ids
and preference values retain their original values. Engine errors, CLI and MCP messages remain
English. Ukrainian currently covers the core menus; it falls back to English elsewhere.

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

The panels use `craft-ui::docking` with its split-tree settings: craft-ui draws the frames, tab
strips, floating windows and drag previews and owns the gestures; `dock_ui.rs` draws the panel
bodies, the ≡ panel menu and the Timeline's sequence tabs. Bounded split geometry and
keyboard-accessible resize handles use `craft-ui::layout`. Panel tabs or their group's drag bar
move between tab groups, split at a group's or the window's edge, reorder, or float when released
with Cmd/Ctrl; a release over no target cancels. Tools and the Audio Meters keep their widths
as the window resizes (fixed split sizes, which a divider drag keeps fixed); the other groups
resize with the window. Every group resizes down to the same minimum and has no maximum.
Timeline sequence tabs retain their sequence-opening, closing and horizontal reorder behavior.

On desktop each floating panel opens in its own OS window. Tool panels are palettes: they stay
above other windows and hide while another application is active. The video viewers (Program,
Source, Reference Monitor, Video Scopes) are normal windows, say on a second display: they
stack with the main window and stay shown in the background. A floating window moves by its
title bar, resizes from every edge and splits like the main window. While a modal dialog is open
(Settings, Recovery, Revert, a file dialog), the native windows are dimmed and take no input.
The main window's input rewrite (macOS Control+Click as a secondary click,
`--control-input-only`) also runs in each native window, and files dropped on one are imported
like files dropped on the main window; on the Timeline they land at the drop point. A drag from
a panel (a clip from the Source Monitor or the Project panel, an effect, a template) drops on a
panel in any of the app's windows. On Wayland
floating windows stay inside the main window. Where egui embeds viewports (tests), floating
windows stay inside the main window too.

Named workspaces save the layout, floating windows included; workspaces saved in the earlier
layout format still load. While every floating window is one tab group, a save also writes the
earlier format, so earlier versions still open the workspace. Automation IDs and workspace commands remain the integration boundary.

Tools and dock tabs expose their actual egui widget IDs, paint keyboard focus, and support
Tab/Shift-Tab traversal and Space/Enter activation. Control-injected keys can reach focused
widgets in a background window. The optional `--control-input-only` native launch mode isolates
automation from native widget/menu input; `ui.inspect` reports native and effective focus
separately. Focus and input-ownership regressions cover document/playhead preservation.

Compact monitor layouts separate the current and duration timecodes from zoom, resolution and
settings. Source video/audio drag choices retain their existing placement behavior. Transport
buttons use `widgets::toolbar_fit::fit_toolbar`; commands beyond the available width remain available
through More, including their selected state and Source/Program routing. A scrolling fallback
retains all commands if even More cannot fit. `monitor_geometry` checks the painted readouts,
control bounds, overflow actions and Source drag payloads across monitor widths and themes.

## Marker properties

Right-click a timeline or Markers-panel marker and choose **Edit Marker…**. The window
edits time/duration, name, notes, author, type, and color. Existing presets are
shown as swatches; project custom swatches are below them. Done makes one undoable marker
edit; Cancel discards the marker draft.
The compact form orders Name, Author, Time/Duration, scrolling Notes, then Color and
marker types. Previous/Next save valid changes before opening the neighbouring marker;
unchanged navigation adds no Undo step. Unchecked Duration saves a point marker even
when its disabled field retains a draft value. Timecodes require complete numeric fields
with colons; Done stays disabled until timing is valid. Text fields support normal
clipboard shortcuts and local text Undo/Redo.

The **+** square below the presets opens a separate color editor. macOS uses the native
Colors panel; other hosts and native-picker failures reuse the established egui picker.
FilmCraft retains exact six-digit sRGB hex entry and explicit confirmation. Add color
saves a reusable swatch to this project's palette through `colors.addCustom` (a separate
undoable operation) and selects it in the marker draft. Canceling the marker edit afterward
does not remove a deliberately saved palette color. The project stores at most 64 custom
swatches shared by markers and clips; duplicate additions are no-ops. Used custom colors also appear for reuse.
Marker/color dialogs belong to the opened project instance, close when that project is replaced,
and are not restored from saved UI preferences. Unrelated edits keep marker drafts open;
editing or removing that marker elsewhere closes its stale draft rather than overwriting newer data.

Markers and clips now share the existing 16 clip preset colors; clip RGB values and preset
names are unchanged. Markers no longer collapse those names onto seven display colors.
Red/Rose, Orange/Mango, Cyan/Teal and Pink/Magenta resolve to the same presets. Exact custom colors
(including white, black, pink, and gray) override a marker's preset. Preset color filters
leave custom markers visible. Selecting a preset clears the override, preserving the author. The preset colors also follow Settings → Labels in both views.

The opaque sRGB editor lives in `widgets::color_picker`, independently of marker models,
dialogs and project commands. Callers provide a `ColorDraft`, automation registry and id
prefix. Its HSV conversion operates on sRGB channels; using linear-light saturation would
make the center too white. Color Grading's grading wheels remain separate: they edit grading
offsets. Existing effect/graphics pickers retain their alpha/float/color-space semantics.

The same author and hex values are available to Source marker commands. Existing markers
have an empty author; FilmCraft never infers authorship from the operating-system account.
Projects (schema 17) retain marker properties, Web Link URLs and the project palette. Marker CSV reports
and FilmCraft OTIO metadata retain individual marker authors, URLs and colors. OTIO also
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

### Editable clip duration

The Clip Speed / Duration dialog lets you type a duration as timecode. Duration and
Speed describe the same source span: editing either updates the other. Duration uses
the sequence frame rate; invalid, zero, or out-of-range durations cannot be applied.
The field is available to automation as `speedDuration.duration`. OK uses the existing
speed command, including its linked-clip, ripple, neighbour, and Undo behaviour;
Cancel discards the draft. The selected clip's duration is the reference when more
than one clip is selected, matching the existing Speed control.

`clip_dialogs_ui::duration_text_edits_speed_and_rejects_invalid_input` exercises real
text input, validation, application, Cancel, and Undo/Redo.

## Export preview (#1388)

Export includes a seek bar, sequence timecode / last selected frame, Play/Stop, previous/next
frame and start/end buttons. Space, Left/Right and Shift+I/Shift+O address the sequence while
Export is open, even if Source previously had focus. Export-preview automation commands require
Export mode. Dragging the seek bar pauses playback and follows the existing scrub-audio preference
through the shared per-frame scrub handler. Playback uses the existing sequence audio clock,
prefetch and preroll; cached pictures remain visible while the next frame renders.

The seek bar and playback respect Entire Source, Source In/Out (inclusive Out), Work Area and
Custom. Playback stops on the last selected frame; Play there restarts at the range start.
If a range change excludes the playing frame, playback stops at the nearest new boundary,
including during preroll. Playback and in-range seeks do not show cached pictures outside
the selected range; explicitly positioned paused-frame inspection is preserved.
Export does not loop even if Program looping is enabled; the Program preference is preserved.
Navigation changes the shared sequence playhead without altering clips, marks, export settings,
dirty state or undo history. Audio-only formats keep audition and navigation controls.
Entering Export finishes any active editing/recording transport. Preview playback never starts
mixer automation, multicamera recording or voice-over capture, including with a mixer strip in Write mode.
The details below the picture scroll on smaller windows, leaving the Export buttons accessible.

The picture previews sequence composition at reduced resolution with output dimensions/aspect
and Fit/Fill/Stretch framing. It does not simulate encoder compression, output frame-rate
conversion, export-only overlays, colour conversion or audio encoding/processing. It auditions
the sequence mix; the Export Audio checkbox does not mute audition. Rendered-frame monitoring
during an export job is separate and is not added here.

Automation: `export.preview.seek {frame: i64}`, `.toggle`, `.first`, `.last`, `.stepBack` and
`.stepForward` are UI commands (`ui.menu.invoke`); the seek widget is `export.preview.scrub`,
the readout is `export.preview.timecode`. Invalid seek arguments fail without changing state.
Unit and headless UI regressions cover range boundaries, hostile values, keyboard routing,
play/stop, stepping, pointer scrubbing, and preservation of project/export state.

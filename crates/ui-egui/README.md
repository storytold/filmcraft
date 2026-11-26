# FilmCraft interface

The egui frontend draws panels and dialogs, and dispatches project changes through the engine.
Interactive controls keep stable automation ids regardless of the interface language.

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

## Prompter overlay

`panels/prompter.rs` draws the Program monitor's Prompter (`view.prompter`, Program wrench menu,
off by default; [monitor documentation](../../docs/monitors.md#prompter)). It draws only what
`filmcraft_engine::prompter::prompt_at` returns for the playhead; `prompter.inspect` returns the
same result over automation. The model's decisions (in `crates/engine/src/prompter.rs`):

- **Sections** are range markers; point markers are not. Where range markers overlap, the one that
  started last is shown while it lasts, so a nested marker interrupts its parent and the parent
  comes back after it; of markers starting together the shorter is shown first, then the first
  listed. "Next" is what is shown after the current section: a section starting inside it, a parent
  that resumes, or the next one after a gap.
- **Talking points** are lines starting with `-`, `*` or `•` and a space, optionally ending in
  `[m:ss]` or `[h:mm:ss]`; a bracket that is not such a duration stays in the text. Points without
  a duration share what the others leave equally and end with the section.
- **Overrun** is reported when the written durations exceed the section, or fill it and leave the
  untimed points no time. Points keep their written lengths; they are not squeezed.
- **Script** is every other line. Its position is linear in characters over the whole section.
- **Caption** is the one at the playhead on the top caption track that is turned on.
- **Next section** is announced in the last 10 s of a section (the second half of a shorter one)
  and always between sections.
- **Limits:** only the first 64 KiB of a comment and its first 100 points are read, and a point is
  cut at 500 characters; the overlay and the query say so (`truncated`).

The overlay's decisions:

- **Shape and edge:** a card centred at the bottom of the visible picture, inset from its edges by
  the action-safe band the Safe Margins draw (5% of the picture's shorter side, 8 to 40 pt), at
  most 60 base type sizes wide and 45% of the picture tall. It has a window's look: `panel_bg`,
  opaque, a 1 pt `field_border` edge, the theme's popup shadow and `radius_sm` corners scaled with
  the type. Opaque, so no picture (a burned-in caption included) shows through its text. It can
  repeat only a caption on one of the sequence's caption tracks (the top one turned on), and only
  when it has room for that row; text that is part of the picture itself, such as a title or a
  caption burned into the media, is covered and not repeated. The voice-over
  countdown at the top of the window stays clear. It is drawn with the monitor's painter only
  (never rendered or exported) and stays on in full-screen Program.
- **Regions, top to bottom:** a header (the marker's flag, the section name, the time; room is left
  before the time for a short readout), the progress bar, the body (talking points, script, or both
  side by side with the points on the left), then a footer (the next section, warnings, the caption).
  Beside the script the points take 42% of the width, more where that would leave the current
  point less than 10 of its type sizes; where that leaves the script less than 10 of its type
  sizes, the script goes under the points, across the card.
- **Hierarchy:** the current talking point is the most prominent line, semibold at 1.4 times the
  base size on the current-point highlight; the script is 1.3 times the base size with its reading
  line on the same highlight. The section name (semibold) and the time (tabular figures,
  `text_dim`) are at the base size. Upcoming points are at the base size in `text_dim`; the last
  finished point is smaller, with ✓. Points not listed are counted ("N more").
- **Talking points:** ▶ (the Play icon) marks the current point, ✓ finished ones, a hollow dot the
  upcoming ones, so state never rests on colour. Each listed point has its length (written or its
  share) in an outlined badge; the current point wraps to two lines (one where two do not fit) and
  has a bar under it showing how far through it the narrator is. Long names and points are cut
  with an ellipsis.
- **Progress:** a 0.4-em bar on a `control_well` groove, split into segments where one talking
  point ends and the next begins (a single bar when the segments would be too short).
- **Script:** the line holding the narrator's position sits on a reading line (the highlight and a
  bar at its left), the second of three lines or the first while the first line is read; what is
  already read is dimmed. The script moves up a whole line at a time, eased over the theme's
  animation time, so no line is shown cut in half; with animation turned off in Settings ▸
  Appearance it moves without easing. It is laid out once for its text and width; the reading
  position only changes which part is drawn dimmed, so reading on never lays it out again.
- **Marker colour:** the section's marker colour (its custom colour, else its label's) is the
  accent of the flag and the bars' fill. A marker can have any colour, so its lightness is moved,
  hue kept, just enough for 3:1 against the card and the groove (`craft_theme::ensure_contrast`).
  No text is ever drawn on it, and it is never the only signal: the name is beside the flag. The
  next section's flag has the next marker's colour.
- **Caption line:** drawn as the Timeline draws captions: the track's badge (C1) and the caption
  text in a block, both in the Timeline's caption block colour with white text.
- **Warnings:** bold text with ⚠ on a `pill_active_bg` block, one row each.
- **Scaling:** the base size is 3% of the picture's height (2.2% of its width for a narrow picture),
  from 11 pt (the theme's small text, the normal layout's Program panel) to 19 pt; spacing and
  radius are the theme's `gap` and `radius_sm` scaled by the same factor. When the card would pass
  45% of the picture, it keeps the header and the current point (or two script lines), then adds in
  this order what fits: the next point, the next section, the warnings, the caption, a third script
  line, the other upcoming points (three at most), the last finished point, the "N more" count.
  With the script under the points, two script lines come right after the current point. Where the
  header and a two-line current point do not fit in 45%, the point takes one line, and the card
  grows past 45% to hold that line, up to the picture's inset; on a picture too small even for
  that, the card holds the header alone. Between sections the card is only as wide as its message.
- **Accessibility text:** the section, its time, the last finished point, the current point and
  up to three next points (the rows the card lists when it has room), the count of the rest, a
  script excerpt, then the footer's notices. Rows the card leaves out for space are still in it.
- **Colours** are theme tokens: `panel_bg`, `field_border`, `text`, `text_dim`, `control_well`,
  `pill_active_bg`, and for the current-point highlight `row_selected`, or `pill_active_bg` or
  `hover` where `row_selected` does not stand apart from the card (Dark) or gives `text_dim` less
  than 4.5:1 (Light). The caption uses the Timeline's caption block colour.
- **Times:** lengths and countdowns round up to whole seconds, elapsed time rounds down.
- **Accessibility:** the automation element `program.prompter` and one `Label` node carry the
  overlay's text in reading order (a label node keeps its text in its value). What is dropped from
  the card for lack of room stays in that text.

Tests:

- Engine, `crates/engine/src/prompter_tests.rs`:
  `list_lines_are_talking_points_with_their_durations_and_the_rest_is_script`,
  `points_without_a_duration_share_what_the_timed_ones_leave_and_end_with_the_section`,
  `points_that_need_more_time_than_the_section_are_reported`,
  `the_current_point_and_the_script_position_follow_the_time`,
  `a_nested_section_interrupts_its_parent_and_next_names_what_is_shown_after`,
  `the_next_section_is_announced_near_the_end_and_between_sections`,
  `the_caption_line_comes_from_the_top_enabled_track_with_a_caption_there`,
  `prompter_inspect_reports_the_section_at_a_time_and_changes_nothing`,
  `hostile_comments_markers_and_times_never_panic`.
- Unit, `panels::prompter::tests`:
  `the_prompter_text_keeps_its_contrast_over_any_picture_in_every_theme` (in all three themes, with
  and without accessible contrast: the card is opaque; 4.5:1 for text and dimmed text on the card,
  on the current-point highlight and on the warning block, and for the caption's white on its
  block; 3:1 for the reading bar and a bar without a marker colour; the highlight stands apart from
  the card; and 3:1 against the card and the groove for the accent of 216 marker colours spread
  over the RGB cube) and `a_marker_colour_that_already_reads_is_kept_as_it_is` (the accent is the
  marker's own colour when it reads, and keeps its hue when it is lightened).
- Headless UI, `tests/it/prompter_ui.rs`:
  `the_prompter_is_off_by_default_and_the_view_command_toggles_it`,
  `the_prompter_follows_the_playhead_through_points_and_sections`,
  `the_prompter_runs_along_the_bottom_of_the_picture_and_stays_in_full_screen_program`,
  `points_longer_than_their_section_are_reported_on_the_overlay`,
  `the_accessibility_text_names_every_point_the_card_lists`.
- Unit, `panels::prompter::tests`: `the_current_point_stays_inside_the_card_on_a_small_picture`,
  `the_current_point_beside_a_script_shows_its_first_words` (pictures from 213×120 to 1600×900),
  `reading_on_through_the_script_lays_it_out_once`.

Results on Linux (2026-11-25): all 10 engine and 10 UI prompter tests pass, and the 4 headless tests
from the first version fail with the overlay's draw call removed. In the full UI suite, 1211 of
1212 passed under heavy load; the one that failed, `perf_ui::queued_frames_do_not_poll_the_ui`, is
a known intermittent test outside the prompter that fails the same way on `main` on the same
machine, and it passed 3 of 3 runs alone. The contrast unit tests fail when the current-point
highlight is `row_selected` in every theme, or when the accent ignores the marker's colour. The
review fixes' tests fail on the code before them: the card was 54 pt tall for a 101 pt current
point on a 213×120 picture, the current point beside a script showed "S…", the script was laid out
157 times for 157 reading positions, and the accessibility text named one of the three next
points.

Limitations: changes on the overlay, and turning it on or off by shortcut, are not announced
through a live region (no shared helper yet). The script's line-by-line easing follows the app's
animation setting, not a system reduced-motion setting. The card's text size follows the picture,
not the system text size. In the normal layout's small Program panel the card has room for the
header, the current point and one or two more rows, so the script beside points, and the caption,
are often among what it leaves out; it is then still in the accessibility text, and the burned-in caption under the card is not
visible. Right after a seek, the monitor shows the nearest cached picture until the new
frame renders, while the prompter already shows the new time.

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

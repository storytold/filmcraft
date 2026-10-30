# FilmCraft interface

The egui frontend draws panels and dialogs, and dispatches project changes through the engine.
Interactive controls keep stable automation ids regardless of the interface language.

## USB Speed Editor

`speed_editor.rs` maps Blackmagic Design Speed Editor snapshots and wheel reports to normal
engine/UI commands. The desktop host owns HID discovery, authentication, LEDs and reconnection;
the frontend owns monitor choice, Jog/Scroll/Shuttle, held trim modifiers and status. Preferences
→ General and `ui.inspect.speedEditor` show connection status. `speedEditor.input` exercises the
same path from the control channel. See [controls, setup and limitations](../../docs/speed-editor.md).
The module's tests cover cuts and undo/redo, key repeats, fractional/signed wheel movement,
Source/Timeline marks, trims, source placement and hostile parameters.

Program Shuttle reports retain pending preroll readiness and deadlines. While playing, a new
speed re-anchors the clock at the current playhead and rebuilds the audio mix at that rate;
unchanged reports keep the stream running. Direction changes use the normal preroll. Regression
tests cover continuous dial input, audio restarts, silent speeds and returning to audible playback.

Devices → DaVinci Speed Editor opens `speed_editor/debugger.rs`: a nonmodal photo overlay
with held/recent key highlights, dial movement, USB/battery status and a bounded input log.
Telemetry is collected before focus and editing guards, including for unsupported keys.
The default Monitor only mode suppresses editing while the window is open. Window state is
serde data and every control has a `speedEditor.debugger.*` automation id. The photograph's
user-confirmed open licence is recorded beside the asset; no device artwork is generated.

The Knob sensitivity slider (`speedEditor.debugger.sensitivity`) sets the persisted engine
preference `general.speedEditorSensitivity` (1–200%, default 100%). `devices.speedEditor`
also accepts `sensitivity`. Jog, Scroll and held-wheel edits accumulate integer raw units
times percentage; even 1% retains small movements. Shuttle gain uses the same percentage
and stays within ±8×. Raw diagnostics remain unscaled. Preference validation/persistence,
fractional motion, direction changes, source/trim/shuttle response and the actual slider
drag are covered by the Speed Editor tests.

`speed_editor/gestures.rs` recognizes secondary legends within 350 ms, with a release
between presses. It defers conflicting single actions, cancels stale input, and dispatches
Undo, Move/Slide, Mark, Set default duration, clear marks, whole-clip placement, Title,
Review, Y position and viewer zoom through existing commands. Native key edges retain
capture timestamps so queued reports preserve their timing. The debugger also recognizes
double taps in monitor-only mode and shows a purple pulse, count and secondary-action log.

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
Source, Reference Monitor, Lumetri Scopes) are normal windows, say on a second display: they
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
buttons use `craft-ui::layout::fit_toolbar`; commands beyond the available width remain available
through More, including their selected state and Source/Program routing. A scrolling fallback
retains all commands if even More cannot fit. `monitor_geometry` checks the painted readouts,
control bounds, overflow actions and Source drag payloads across monitor widths and themes.

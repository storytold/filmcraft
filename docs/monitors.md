# Monitor view options

The View menu and each monitor's wrench menu set how the Source and Program monitors show their
picture. It is all UI state (`MonitorView` in `crates/ui-egui/src/state.rs`, serde): agents read
it with `ui.inspect` (`ui.program`, `ui.source`) and set it with `ui.set {"program": {…}}`, or run
the `view.*` UI commands. Commands act on `params.monitor` (`"program"` / `"source"`), else on the
focused monitor (Program by default). Code: `crates/ui-egui/src/panels/monitor_view.rs`.

| Command | Menu | Effect |
|---|---|---|
| `view.playbackRes.<full\|half\|quarter\|eighth\|sixteenth>` | View ▸ Playback Resolution | render scale while playing |
| `view.pausedRes.<…>` | View ▸ Paused Resolution | render scale while stopped (default Full) |
| `view.highQualityPlayback` | View ▸ High Quality Playback | play at the paused resolution when it is higher |
| `view.display.<composite\|alpha\|red\|green\|blue>` | View ▸ Display Mode | composite, or one channel as greyscale (colour channels premultiplied). The channel modes render on the CPU instead of the GPU compositor, and Alpha renders the sequence live instead of using render previews (they are flattened over black), so playback in them is slower |
| `view.display.multicam` | Display Mode ▸ Multi-Camera | the Program's multi-camera view (`multicam.toggleView`) |
| `view.display.audioWaveform`, `view.display.videoAndWaveform` | Display Mode ▸ Audio Waveform / Video and Audio Waveform Split | Source Monitor: the clip's waveform (click to move the source playhead); audio-only clips always show it |
| `view.display.comparison` | Display Mode ▸ Comparison View | Program: the reference frame (left) beside the current frame; the reference starts at the playhead and is stepped or reset with the buttons under it, or `view.compare.setReference {time\|seconds}` |
| `view.magnification.<fit\|10\|25\|50\|75\|100\|150\|200\|400\|800\|1600>` | View ▸ Magnification and the zoom dropdown | 100% = one frame pixel per screen pixel. The mouse wheel (or a pinch) over a monitor zooms about the pointer, from Fit too, within 10%–1600%; drag with the middle mouse button, or with the Hand tool, to pan; Fit resets both. Each monitor keeps its own zoom and pan |
| `view.showRulers`, `view.showGuides`, `view.lockGuides`, `view.clearGuides` | View | toggles take `{"enabled": bool}` |
| `view.addGuide` | View ▸ Add Guide… | dialog, or `{"orientation": "vertical\|horizontal", "position": px}` |
| `view.snapInProgramMonitor` | View ▸ Snap in Program Monitor | graphic moves snap to the frame edges / centre and the guides (6 pt) |
| `view.transparencyGrid` | wrench menu ▸ Transparency Grid | a gray and white checkerboard (8 pt squares) behind the picture shows its transparent areas; off by default, each monitor keeps its own; takes `{"enabled": bool}`. The Program monitor renders on the CPU while the grid is on |
| `view.prompter` | Program wrench menu ▸ Prompter | the [Prompter](#prompter) at the bottom of the Program picture; off by default, Program monitor only, also shown in full-screen Program; takes `{"enabled": bool}` |
| `view.safeMargins` | Guide Templates ▸ Safe Margins | action- and title-safe boxes |
| `view.guideTemplates.save`, `.manage`, `.apply`, `.delete` | Guide Templates ▸ Save Guides as Template… / Manage Guides… | templates are stored in the user preferences (`guides.templates`) |

Guides are in frame pixels. Drag out of the top ruler for a horizontal guide or the left ruler for
a vertical one; drag a guide to move it (unless locked) and drop it outside the picture to remove it.

Automation ids: `<monitor>.zoom`, `<monitor>.zoom.<level>`, `<monitor>.settings.<command suffix>`
(wrench menu items, e.g. `program.settings.display.alpha`), `program.prompter`, `<monitor>.ruler.top|left`,
`<monitor>.guide.<n>`, `program.compare.reference|prev|next|set`, `source.waveform`, and in the
dialogs `guides.add.vertical|horizontal|position|ok|cancel`, `guides.save.name|ok|cancel`,
`guides.manage.row.<n>|apply|delete|close`.

## Prompter

For narration recorded to picture (voice-over recording) and for rehearsing it with normal
playback, the Program monitor can show a prompter at the bottom of the picture: Program wrench
menu ▸ Prompter, or `view.prompter`. It is off by default. It only reads the sequence: it does not
change playback, recording or the project, it is drawn by the monitor and never rendered or
exported, and it stays on in full-screen Program (`view.fullScreenProgram`), where the other
overlays are turned off.

**Sections are range markers.** A marker with a duration is a section: its name is the section's
name and its range is the section. Point markers are not sections. Where range markers overlap,
the one that started last is shown while it lasts, so a marker inside another interrupts it and
the outer one comes back after it; of markers starting together, the shorter is shown first.

**The marker's comment is the section's text.**

- A line starting with `-`, `*` or `•` and a space is a talking point. It can end in a duration in
  brackets, `[m:ss]` or `[h:mm:ss]`: `- Third line of the graph [0:30]`. Points without a duration
  share the time the others leave, equally. A bracket that is not such a duration stays part of the
  point's text.
- Every other line is script. It runs from the start of the section to its end, in proportion to
  its characters, so it finishes when the section does.

**What it shows** at the playhead, on a card centred at the bottom of the picture (inside the
action-safe margin, at most 45% of the picture's height):

- a header with the section's marker flag in the marker's colour, the section's name, and the time
  elapsed and the section's length; under it a progress bar in the marker's colour, split where
  one talking point ends and the next begins;
- the current talking point as the largest line, in bold on a highlight with ▶, its length in a
  badge and a bar under it showing how far through it you are; the last finished point above it,
  smaller, with ✓; up to three next points, each with a dot and its length; and how many more
  follow;
- the script, larger than the other text, with the line you are reading on a highlighted reading
  line marked by a bar, what is already read dimmed; it moves up a line at a time;
- the next section, with its marker's flag and how soon it starts, in the last 10 seconds of a
  section (the second half of a section shorter than 20 seconds) and between sections;
- warnings, each on its own block with ⚠: when the talking points' durations add up to more than
  the section or leave no time for the points without one, and when a comment was cut;
- the caption at the playhead, from the top caption track that is turned on, drawn as the Timeline
  draws captions: the track's badge (C1) beside the caption text in a caption block.

Talking points and script side by side share the card, points on the left; on a narrow card the
script goes under the points. The card's text and spacing grow with the picture, from the small
Program panel to full screen on a large display. When the picture is too small for everything, the
card keeps the header and the current point (on one line if two do not fit; the card grows to
hold it), or two script lines, and then, in this order, what fits: the next point, the next section, the
warnings, the caption, more script, the other points, the last finished point, the count. Between
sections it says so, and a sequence without range markers says how to make sections. Only the
first 64 KiB of a comment and its first 100 talking points are read.

**Next and Previous.** The prompter has its own position, the prompt time, which is normally the
playhead. Markers ▸ Prompter ▸ Next Point (`prompter.next`, Alt+Page Down) and Previous Point
(`prompter.previous`, Alt+Page Up) move it to the start of the next or previous talking point, or,
in a section without talking points, script paragraph (paragraphs are separated by a blank line).
A section with neither is one step. At the last point of a section Next goes to the start of the
next section, also across a gap between sections; Previous goes back to the start of the current
point once it is more than 2 seconds in, else to the point before, which at a section's first point
is the previous section's last point. Only what the prompter shows counts, so points hidden under a
nested section are skipped and the outer section is a step where it resumes. Neither changes
playback, the picture, a recording or the project, and neither is an undo step. From its new
position the prompter keeps moving with the playhead. Return to Playhead (`prompter.toPlayhead`,
Alt+Home) puts it back; so does starting playback or a voice-over recording. Seeking and stopping
playback keep the gap. The commands are disabled, saying why, without range markers, after the
last point (Next), at or before the first (Previous) and when the prompter is at the playhead
(Return to Playhead).

**Ahead/behind.** While the prompt time is not the playhead, the card's header shows the gap in a
readout badge before the time, an outlined pill with a chevron pointing forward or back: "ahead 0:08" when the prompter is later than the picture (the narrator moved
on first), "behind 0:12" when it is earlier (the picture moved on). It rounds up to whole seconds,
so any gap shows, and reads `h:mm:ss` from an hour on. In step it shows nothing. When the header is
short of room the section name is cut first and the time goes before the readout. Between sections
the card says "No section at the prompter's position" with the readout.

**Take log.** During a voice-over recording every move is logged with its time:
`prompter.takeLog` returns `inProgress` (the take being recorded) and `last` (the last finished
take), and `audio.voiceover.stop` returns the finished take's log as `promptLog`. A log has
`sequence`, `recordStart`, `entries` and `dropped` (moves past the first 10 000). The first entry
(`kind` `start`) is where the prompt was when the capture began; then one entry per move (`next`,
`previous`, `toPlayhead`) with `takeTime` (the playhead minus the record point, negative in the
pre-roll), `playhead`, `promptBefore`, `promptAfter` (ticks), and the `marker`, `point` and
`paragraph` the prompt landed on. Between entries the prompt runs with the playhead, so the prompt
time at any moment of the take follows from the last entry before it. The log is kept in the
session; it is not yet stored with the recorded clip.

**Automation.** The overlay's automation id is `program.prompter`; its label, and its accessibility
node, carry its text. The read-only engine query `prompter.inspect {"time"|"frame"|"seconds"|"timecode"?}`
returns the same content for the active sequence at any time (default: the prompt time), with the
prompter's `promptTime`, `playhead`, `gap` (prompt time minus playhead, ticks) and `direction`
(`ahead`, `behind` or null): `section`
(marker, name, start, duration, elapsed, remaining, progress, `points` with their text, written
duration, time range and `state` `done|current|upcoming`, `current`, `script` with its `offset` in
characters and its `paragraph`, `overrun`, `truncated`), `next` (name, `at`, `startsIn`, `soon`), `caption` and
`sections` (the number of range markers). It is disabled when no sequence is open. Code:
A time that is given but cannot be read (`{"timecode": "invalid"}`) is refused, not answered for the
playhead. `crates/engine/src/prompter.rs` (what is shown) and `crates/ui-egui/src/panels/prompter.rs`
(drawing). Tests: `cargo test -p filmcraft-engine prompter_tests::` and
`cargo test -p filmcraft-ui-egui --test it prompter_ui::`.

## Source playback

Video files, audio files, generated media and media subclips play in the Source monitor with its
Play/Pause button or Space while Source has focus. The Source clock and playhead are independent
of the active sequence; auditioning does not create timeline clips or change the project. Starting
Source stops Program playback, and starting Program stops Source, so only one monitor owns audio.
Scrubbing while playing seeks the Source clock and restarts its audio at that position. Replacing
the open source stops playback. Playback stops at the last frame, and Play there starts again.
Play auditions the full source span by default. Play In to Out and Play to Out
address the marked Source range when Source has focus. The Source wrench menu can loop the
marked range; Program keeps its separate loop setting.

Normal forward playback has sound. With Source focused, L plays forward and J backward; pressing
the same key again doubles the speed up to 8×, K stops, Shuttle Slow Right/Left play at ¼ speed,
and the left/right frame-step keys step the Source playhead. As in Program, shuttle speeds other
than normal forward play without sound, and backward playback stops on the first frame. Source
sequences are not yet supported and report an explanatory error. A failed audio device leaves
the picture advancing with a status message; a decoding error stops Source.

Commands: `source.playback.play`, `source.playback.stop`, `source.playback.toggle` (no parameters),
available through the UI command registry, CLI bridge and MCP. `playback.toggle`,
`playback.forward`, `playback.reverse`, `playback.slowForward` and `playback.slowReverse` also
accept `monitor: "source"|"program"` to override focus. `ui.inspect.sourcePlayback` reports playing,
audioClock and playhead. Test: `cargo test --release -p filmcraft-ui-egui --test it source_playback_ui::`.

## Drag a marked Source range to the timeline

An unmarked Source clip shows In at its beginning and Out on its last frame. These are implicit
limits: opening a clip does not write marks or dirty the project. Set In/Out with I/O or the
transport buttons; the saved marks are reused when reopening that clip. Out includes its frame.

Drag the **Video only**, **Audio only**, or **Video + audio** icons between Fit and playback
resolution to an open
sequence. Drop video on a video track and audio on an audio track. Both uses the dropped track
and its corresponding track of the other kind, creating linked clips in one undoable edit.
Unavailable stream choices are disabled. Dragging the video picture also brings its available
streams; a vertical drag from an audio waveform brings sound, while a horizontal drag scrubs.
The drag captures the selected range at its start. Source playback pauses when lifting a clip.
A locked or unavailable destination reports an error without partially placing the pair.

Automation ids: `source.drag.video`, `source.drag.audio`, `source.drag.both`, `source.picture`,
and `source.waveform`. Placement dispatches `timeline.place` with `sourceIn`, `duration`, `video`
and `audio`; the stream flags default to true for existing callers. Engine tests cover range
bounds, stream choices, linked placement, invalid parameters and undo/redo. UI tests exercise
actual drags: `cargo test --release -p filmcraft-ui-egui --test it source_drag_ui::`.

Ctrl+Shift+X (Cmd+Shift+X on macOS) clears In/Out on the focused Source clip, including split
points, and restores the implicit full span. It is undoable and leaves Program marks unchanged.
The command `markers.clearInOut` accepts `target: "source"|"program"`; omitting it addresses Program.
Source drag icons use the Windows hand cursor because its native Grab mapping is crossed arrows.

Source range braces use the playhead's theme color and the gray selection band is translucent,
so the ruler ticks remain visible. This shared egui implementation applies to Windows, macOS
and Linux. The clear shortcut uses the primary modifier: Ctrl on Windows/Linux, Cmd on macOS;
I/O, Space and the three drag gestures are common to all three platforms. Runtime verification
for this contribution was performed on Windows; macOS/Linux have not been run locally.

## Adjust Source marks on the ruler

Drag either In/Out handle to trim the selected span, or drag the interior gray band to move
both marks while keeping the duration. Handles show an original red bracket and arrows on
hover. Edits snap to source frames, stay within the clip and retain at least one frame.
A gesture previews without editing the project and commits one undoable `project.setMarks`
command on release. Escape cancels; opening another source or a concurrent project edit
also cancels the draft. Subclip handles are currently disabled.

Automation ids are `source.range.in`, `source.range.out`, `source.range.body` and
`source.settings.loop`. Clicking the band's interior still scrubs.

Source-focused Go to In/Out, clear individual/both marks, clip-marker commands and
previous/next marker address the open Source clip. Shift+Left/Right step multiple Source
frames using the preference. UI callers can override focus with `monitor: "source"|"program"`;
engine marker/navigation commands accept `target: "source"|"program"` and default to Program.
Source marker edits leave sequence markers and the Program playhead unchanged.

Coverage: engine `source_monitor::tests` validates frame bounds and hostile inputs; UI
`source_drag_ui` covers release, cancellation, range movement and command routing;
`source_playback_ui` covers marked-range stopping, looping and the last-frame boundary.

## Program range handles

Program uses the same In/Out hover cues, frame-snapped trimming, duration-preserving range
movement and one-edit undo behavior. It targets the active sequence through `project.setMarks`
and leaves an independently opened Source clip's marks and playhead unchanged. Program shows only explicitly set marks; clearing them hides the corresponding handles.
Range movement requires both Program marks. Empty sequences disable the handles.
Changing the active sequence cancels an unfinished draft. Automation ids: `program.range.in`,
`program.range.out` and `program.range.body`. Both monitors share the original brace artwork,
playhead-blue color and translucent gray band. Runtime review remains Windows-only.

The playhead has drag priority above the range band and trim handles, including a full-span
selection. Drag it to scrub; the unoccupied upper ruler can also scrub. Drag the gray band away
from the playhead to translate the marked range.

## Zoom the time ruler

Under each monitor's time ruler is a zoom scroll bar like the Timeline's. Its thumb is the part of
the clip or sequence the ruler shows: drag a handle at either end to zoom in or out, drag the
middle to scroll, or press beside the thumb to move it there. A thumb filling the bar shows the
whole duration; the ruler zooms in as far as five frames. The playhead, markers, In/Out and ticks
follow the part shown, and clicking or scrubbing the ruler sets a time within it; what lies
outside it is not drawn. While the monitor plays, a zoomed ruler follows the playhead as the
Timeline does (Settings ▸ Timeline ▸ Timeline Playback Auto-Scrolling): it pages when the playhead
runs off the right edge or jumps before the left one, or keeps the playhead in the middle with
smooth scrolling, and keeps its width; scrubbing and clicking never move it. Each monitor keeps its own range in its view state (`time_zoom`: start
and end as fractions of the duration, `null` for all of it). Automation ids: `<monitor>.zoomBar`
(the thumb) and `<monitor>.zoomBar.track`.

## Export Frame

The camera button in either monitor opens Export Frame. Drag its white header to move the dialog.
The suggested name is the source filename without its extension, or the sequence name, without an added `.Still` suffix. Choose a name, format and depth,
use Browse to choose the destination folder (or edit Path), and click OK. Existing files require
an explicit Replace confirmation. Cancel, the header close button, Escape, and clicking outside
the dialog (including the top menus or mode tabs) dismiss it without writing anything.
The dialog's own dropdowns and Browse button keep it open. The white
header and dark body use original generic controls. Source details and frame time appear below
the settings. Frame time uses the same hours/minutes/seconds/frames timecode as its monitor,
including the Program sequence's drop-frame setting, and stays captured while the dialog is open.
The last successful folder is reused during the current app session.

PNG and TIFF support 8 or 16 bits per channel. The 16-bit path quantizes directly from the
float image, preserving precision beyond 8 bits. JPEG (quality 95) and BMP support 8-bit output;
choosing either resets Depth to 8 Bit and disables 16 Bit. The command rejects unsupported
format/depth combinations. In the browser the still is offered as a download. Import into project
adds the saved still as one undoable edit. Failed writes retain the settings and report an error.

The monitor, item and frame time are captured when the settings open. Export uses the original
full-resolution media, even with proxies or reduced preview resolution enabled. Source exports
the source picture at its media playhead; Program exports the sequence composite, effects and
captions at its timeline playhead. Audio-only Source clips have no picture to export.

The shared `file.exportFrame` command accepts `target: "source" | "program"` (Program by default),
`format: "png" | "jpeg" | "tiff" | "bmp"`, `depth: 8 | 16` (default 8),
optional `item` for Source or `sequence` for Program, and an exact integer `time` in ticks.
Supplying `path` executes directly; invoking it through the UI without a path opens settings,
routed to the focused monitor unless an explicit monitor/target is provided.

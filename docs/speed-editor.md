# Blackmagic Design Speed Editor

FilmCraft's desktop app connects automatically to a **USB** Speed Editor (vendor `1edb`,
product `da0e`) on Linux and Windows. Connection status appears in Preferences → General
and through `speedEditor.status` / `ui.inspect`. Linux is verified against the connected
`/dev/hidraw3` board; Windows uses the native HID backend but still needs hardware validation.
macOS and Bluetooth transport are not implemented.

## Input debugger

Open **Devices → DaVinci Speed Editor** for a resizable, nonmodal diagnostics window.
It uses the supplied front-view photograph, with **green** overlays on held keys,
**amber** overlays for 600 ms after a press or release, and a **blue** dial indicator when
the wheel moves. **Purple** marks a detected double tap; the last secondary action and
double-tap count appear below the board and in the event history. All 43 physical keys have overlays, including keys whose editing functions
are not implemented. Connection status, the USB path, battery state, the latest signed dial
value and the last 64 input events are shown alongside the board.
On Linux, a disconnected device also shows a note that udev rules and read/write permissions
for USB `1edb:da0e` may need adjustment. The note disappears when the board connects.

**Monitor only** starts enabled. While the debugger is open in this mode, keys and dial
movements update diagnostics without changing the project, playhead or playback. Disable it
to observe the live editing bindings. Raw physical input remains visible when FilmCraft is
unfocused or a modal dialog prevents editing. Closing the debugger restores ordinary input;
keys still held must be released before their editing binding becomes available again.
**Clear history** resets counters and recent events while retaining genuinely held keys.

Use **Knob sensitivity** to adjust dial response from **1–200%**. Lower percentages move
fewer frames for the same turn; try **25%** for one quarter of the original speed. **100%**
retains the original response. This applies to Source/Timeline Jog and Scroll, held-wheel
trim/slip/slide edits, and Shuttle playback (still capped at ±8×). Small movements accumulate
until a full frame is reached, including at 1%; changing sensitivity discards the previous
fractional remainder so the next turn cannot jump unexpectedly.

Sensitivity saves immediately in the user's preferences and survives restarts, including
when the diagnostics window is closed. Raw dial values and the blue diagnostic indicator
remain unscaled. Disable Monitor only to test the adjusted editing response.

The 1280×818 background photograph was supplied by Pascal Maximilian Bremer under MIT,
with key and dial highlights mapped to its button positions. Its source and licence are
recorded in the asset's attribution sidecar and `ATTRIBUTION.md`.

Start FilmCraft normally; no Resolve process, mapping daemon or MIDI bridge is required.
The app discovers the board by its USB vendor/product IDs (`1edb:da0e`) on every connection
attempt, including after unplugging and reconnecting with a different hidraw number. No
device path needs to be configured. `FILMCRAFT_SPEED_EDITOR=off` disables USB input.
Close other Speed Editor clients while using FilmCraft; authenticating
the same board from a second process can interrupt its connection.

## Controls

SOURCE and TIMELINE choose the monitor for the dial, playback and In/Out marks, independently
of keyboard focus. SOURCE opens the selected Project item if no source is open. Timeline
edits use normal track targeting, source patching, locks and linked-clip rules.

| Board control | FilmCraft action |
| --- | --- |
| STOP/PLAY | Play or stop the selected monitor |
| JOG + dial | Scrub frame by frame, forward or backward; 64 frames per revolution at 100% sensitivity |
| SCRL + dial | Faster scrub, approximately one second per Jog frame |
| SHTL + dial | Sensitivity-scaled playback, capped at ±8×; return to the centre to stop |
| IN / OUT | Mark In / Out in the selected monitor |
| SPLIT / CUT | Add an edit at the timeline playhead on targeted tracks |
| RIPL DEL | Ripple delete the selected clips |
| Hold TRIM IN / TRIM OUT + dial | Ripple trim the nearest timeline In / Out edge |
| Hold ROLL + dial | Roll the nearest timeline edit |
| Hold SLIP SRC / SLIP DEST + dial | Slip / slide the first selected timeline clip, including linked partners |
| SMART INSRT | Insert the marked Source range at the timeline playhead |
| APPND | Append the marked Source range at the sequence end |
| PLACE ON TOP | Place the marked Source range on a new video track |
| VIDEO ONLY / AUDIO ONLY | Toggle the streams used for source placement; pressing again restores both |
| DIS | Apply Cross Dissolve to the timeline selection / edit |
| TRANS | Apply the default video transition |
| SNAP | Toggle timeline snapping; its LED follows the app's state |
| FULL VIEW | Maximize / restore the focused panel |
| CAM 1–9 | Select the corresponding multicamera angle |
| LIVE O/WR / SYNC BIN | Toggle multicamera recording / multicamera view |
| ESC | Stop playback and leave the controller's trim selection |
| Hold AUDIO LEVEL + dial | Adjust selected audio clips' gain (0.1 dB per Jog frame) |
| Hold TRANS DUR + dial | Adjust the selected or nearest unlocked transition's duration |

### Double taps and secondary labels

Press and release the same key twice within **350 ms**. For Move, Slide and Y Position,
keep the second press held while turning the dial; release it to return to scrubbing.

| Double-tapped key | Secondary action |
| --- | --- |
| ESC → UNDO | Undo once; another double tap undoes the preceding edit |
| SPLIT → MOVE | Hold the second press and turn the dial to move the selected clips on their tracks, including linked partners |
| AUDIO LEVEL → MARK | Add one marker in the chosen Source or Timeline monitor |
| TRANS DUR → SET | Save the current selected/nearest transition's duration as the default for its video/audio kind |
| ROLL → SLIDE | Hold the second press and turn the dial to slide the first selected clip, adjusting its neighbors |
| IN / OUT → CLR | Clear the corresponding mark in the chosen monitor |
| SMART INSRT / APPND → CLIP | Insert/append the entire Source clip, ignoring its marks |
| TRANS → TITLE | Add an editable text title at the timeline playhead |
| FULL VIEW → RVW | Maximize Program and review the nearest edit with the configured pre/post-roll |
| CLOSE UP → YPOS | Hold the second press and turn the dial to adjust the selected clip's Motion Y position |
| SNAP → viewer size | Toggle the chosen monitor between Fit and 100% picture zoom |

Single taps on Split, Smart Insert, Append, Trans, Full View, Snap and Live Overwrite wait
for the double-tap window to expire after release. A recognized double tap suppresses that
single action, preventing an extra cut, placement or transition. A single Split retains the
frame where the key was pressed. Turning the dial while holding Split also enters Move
without a preliminary cut. ROLL starts its ordinary roll trim when the dial turns.

Repeated held-key reports, intervening keys, wheel edits and long holds do not count as
double taps. Native USB capture timestamps preserve this distinction when input queues
behind rendering. Escape, disconnect, focus loss, monitor-only mode and modal dialogs cancel
pending single taps; a project, sequence or Source change also cancels stale pending edits.
Control-channel inputs use the same timing and bindings, including when USB is disabled.

RND's random Sync Bin insertion is recognized and reported as unsupported because FilmCraft
does not provide a Sync Bin. MARK adds a marker; the marker-color dial selector and TITLE's
font browser are not implemented. Review uses the nearest timeline edit rather than a
Resolve-specific record of the last edit.

SMART INSRT uses the current playhead, rather than Resolve's nearest-cut heuristic.
Trim and slip controls always edit the timeline, including when SOURCE is selected.
Each dial trim update is an undo step. Source placement and each split or delete are undoable
through Edit → Undo. Scrubbing does not change the project or undo history. Timeline Jog
uses the normal scrub-audio setting; source Jog updates the picture without grain audio.

Single-tap RIPL O/WR, SPL O/WR, CLOSE UP and SMTH CUT currently report an
unsupported-operation message. No approximate replacement edit is applied.

The illuminated JOG / SCRL / SHTL key follows the selected dial mode; VIDEO ONLY / AUDIO ONLY
also show their state. Physical input pauses while FilmCraft is unfocused, showing a modal dialog, or
running with `--control-input-only`. Buttons held while inactive must be released before
input resumes. Unplugging stops controller shuttle playback and releases trim modifiers.

## Linux and containers

The Rust `hidapi` native backend uses `basic-udev`; it needs **no additional apt packages** or
bundled C HID library. The existing `.devcontainer/devcontainer.json` passes `/dev/hidraw3`
and `/run/udev` through to the container. If the host assigns another hidraw number, update
that device passthrough and rebuild the container. Automatic ID discovery still requires
the device node to be visible and accessible inside the container.

The desktop user needs read/write permission on the hidraw node. On a local Linux desktop,
an administrator can install this rule as `/etc/udev/rules.d/70-filmcraft-speed-editor.rules`:

```udev
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1edb", ATTRS{idProduct}=="da0e", TAG+="uaccess"
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="1edb", ATTR{idProduct}=="da0e", TAG+="uaccess"
```

Reload udev rules (`sudo udevadm control --reload-rules`) and reconnect the board. Containers additionally need the host's device
permissions mapped to the container user. FilmCraft reports permission failures and retries
while editing remains available; running the whole editor as root is unnecessary.

For UI validation without access to the host display, the devcontainer's `postCreateCommand`
installs `xvfb` and `xauth`, plus `fonts-noto-cjk` for the existing multilingual UI tests.
The USB integration itself needs none of these packages; no font files are added to the repository.

On a Wayland desktop, run `FILMCRAFT_WAYLAND=1 cargo run -p filmcraft -- --demo` to use
the forwarded Wayland display instead of the app's default XWayland preference.

## Automation and validation

These UI commands are reachable through `engine.execute` on the control channel and the
MCP bridge, including while the window is unfocused. They use the same bindings as physical USB input:

| Command | Parameters |
| --- | --- |
| `speedEditor.status` | `{}` → connection, monitor, mode, stream choices, held keys, battery |
| `devices.speedEditor` | `{"open":true,"monitorOnly":true,"sensitivity":25}`; optional flags and integer percentage 1–200; opens by default |
| `prefs.get` / `prefs.set` | `general.speedEditorSensitivity` is the saved dial percentage |
| `speedEditor.debugger.clear` | `{}` → clear diagnostics history and counters |
| `speedEditor.input` | `{"button":"split","pressed":true}`; send `pressed:false` to release |
| `speedEditor.input` | `{"wheel":128,"mode":0}`; signed 32-bit raw value; mode optional |
| `playback.shuttle` | `{"speed":-2,"monitor":"program"}`; finite −8…8, zero stops |

Key names use camelCase, e.g. `stopPlay`, `trimIn`, `rippleDelete`, `source`, `timeline`.
Wheel mode 0 (also 2) means relative Jog/Scroll; 3 means centred absolute Shuttle. Reports
from a previously selected wheel mode are ignored. Fractional Jog movement accumulates;
hostile relative values are capped to ten revolutions per report. Six simultaneous keys
and 256 pending reports are the maximum. Overflow discards pending edits and reconnects.
Adjacent wheel reports combine while the UI renders; button edges keep their original order.
Known HID reports accept trailing zero padding from native backends; truncated reports and
nonzero padding remain errors. Program Shuttle speed updates preserve a pending preroll and
re-anchor running playback at the current playhead, rebuilding audio for the new rate.
Repeating the same speed leaves the running clock and audio stream alone; changing direction
prepares new frames through the normal preroll.
`speedEditor.status.telemetry` contains raw pressed keys, pulse deadlines, dial motion and
bounded event history. `ui.inspect.ui.speed_editor_debugger` contains the window's open and
monitor-only state. Its widgets use the `speedEditor.debugger.*` automation-id prefix.

```sh
cargo test -p filmcraft-ui-egui --lib speed_editor
cargo test -p filmcraft-ui-egui --test it speed_editor_ui
cargo test -p filmcraft --bin filmcraft speed_editor
cargo test -p filmcraft --bin filmcraft usb_speed_editor_smoke -- --ignored --nocapture
```

Tests cover key edges, signed/fractional scrubbing, monitor routing, trim release,
source append, undo/redo, shuttle stopping, malformed parameters, truncated and mutated
HID reports, zero-padded native reports, continuous Shuttle speed changes and audio restarts,
queue overflow and authentication replies. Gesture tests cover single/double timing,
queued native timestamps, Move/Slide undo, Source/Timeline markers and clear marks, whole-clip
append, title placement, default-transition persistence and cancelled pending cuts.
The opt-in hardware test performs
the real handshake, sets and clears LEDs, and polls briefly without editing a project.

Protocol sources: [bmdse 0.1.0](https://docs.rs/bmdse/0.1.0/bmdse/),
[its MIT-licensed driver](https://github.com/BaukeWestendorp/bmdse),
[Blackmagic's keyboard controls](https://www.blackmagicdesign.com/products/davinciresolve/keyboard), and
[Blackmagic's Speed Editor guide](https://documents.blackmagicdesign.com/SupportNotes/DaVinci_Resolve_17_New_Features_Guide.pdf).
The adapted authentication code retains bmdse's MIT copyright and permission notice.

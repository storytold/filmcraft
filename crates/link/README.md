# filmcraft-link

The FilmCraft side of Craft Link (L5). It is not wasm-compatible: `craft-ipc` uses local sockets
and threads, so no web crate may depend on it. `cargo xtask wasm` proves that.

| Module | What it holds |
|---|---|
| `commands` | `install(&mut Session)`: sets the `link.editIn` hook and registers the dynamic commands (owner `craft.link`). `run_edit_in` is the whole flow. |
| `edit_in` | `edit_in`: nest, OTIO export, `link.open`, offline media item, swap, `craft.link` metadata, one undo step. `Snapshot` restores project, state, history, revision and journal on any failure. |
| `prepare` | `resolve`: clips of the nest whose source has no file (generators, titles, nested sequences) are rendered to ProRes 4444 movies in the link cache, keyed by a content hash, and the OTIO export reads a project copy that points at them. Runs as an export job, never on the UI thread. `state`/`cancel` serve the `Preparing` dialog. |
| `link_ops` | `focus`, `unlink`, `refresh` and the `craft.link` record lookup. `bring_online` clears the offline flag of the linked item once the result file opens (no undo step). `ResultPoller` looks once a second for result files that appear. |
| `peer` | `SocketPeer` (find or start EffectCraft, `link.hello`, `link.open`, `link.close`, `app.activate`), `select_instance`, `app_to_update`. |
| `spawn` | `Launch`: a started EffectCraft. `poll` looks once and never waits. The child is stopped when it exits (fails at once), on cancel, and at `LAUNCH_CEILING` (60 s), never before. `spawn_and_wait` is the blocking form. |
| `host` | `Host`: window calls for focus handoff, the launcher, the runtime directory. `EguiWindow` is the real window backend. `SystemLauncher::interactive` answers `Starting` and keeps the child between frames. `blocking` (CLI) waits. |
| `prompt`, `ui` | The questions a command cannot answer alone (not installed, did not start, choose an instance, update an app, preparing a clip, preparing failed) and the dialog that draws them. `ui` also draws the hand-off banner. |
| `lift` | `lift_unimported`: moves the effects EffectCraft cannot import from the one inner clip of the nest to the outer clip, before the export. |
| `notice` | `LinkNotice`: what the banner after `link.editIn` shows, and `undo`, which takes the nest back in one step and sends `link.close`. |
| `source` | `LinkedSource`, a `MediaSource` that shows the linked comp live. `video_frame(t)` asks a `FrameSupply` (`PeerFrames`: a `craft_ipc::fetch::FrameFetcher` per scale bucket) with a deadline of one frame duration while playing and 250 ms otherwise, and shows the result file's frame when the live one is late, failed or gone. Audio always comes from the result file. |
| `manager` | `LinkManager` (thread: finds EffectCraft once a second, `link.hello`, `link.attach`, events `link.changed` / `link.result` / `app.closing`, one `link.refresh` in flight per link through the `ControlSink`) and the `Installer` (UI thread, `manager::pump` each frame: wraps pool sources, shows the slate for `NotLive{NoResult}`, fills `Session::link_badges`). States: `Pending`, `Live`, `NotLive{Offline, NotInstalled, VersionMismatch, NoResult}`, `Broken`. |
| `place`, `record` | Editor place capture and restore. The `craft.link` record serde. |

## Nest first, keep or undo

`link.editIn` never asks before it nests. It nests the selection, opens EffectCraft, swaps in the linked item
and returns. When something does not reach EffectCraft, the reply carries `lifted_effects` and `unsupported`, and
a banner (`ui::show_notice`) says: "Click Continue to nest your clip. This keeps the effect in FilmCraft, but you
will not see it applied in EffectCraft." Continue keeps the link. Undo (`notice::undo`) pops the one `link.editIn`
undo step, which restores the clip, its effects and the selection, then sends `link.close`. EffectCraft unbinds
the comp and keeps it with any edits, and the toast names the comp when it holds work. An edit made after the
nest removes the banner, because Undo would then undo something else.

## Effects EffectCraft cannot import

EffectCraft imports `motion`, `opacity` and `time_remap`. For a nest with one inner video clip that plays at
normal speed (no reverse, hold or time remap) and spans the same time as the outer clip, `lift::lift_unimported`
moves every other effect, with its masks and keyframes, to the outer clip. They then:

- are not in the OTIO payload, so EffectCraft reports nothing for them;
- apply to the result movie after the EffectCraft output, not before its motion and opacity as before;
- stay on the outer clip through `link.focus` and Back, because those only reopen the result file;
- return to the original clip when the user undoes the `link.editIn` step.

`link.unlink` restores the nested sequence as the clip source. The lifted effects stay on the outer clip then.
Several inner video clips that carry the same static, unmasked effects and never overlap in time lift too, as one
grade on the outer clip. Any other shape is ambiguous (a different or missing effect on a clip, an animated or
masked effect, overlapping clips): nothing moves, the effects stay on the inner clips, and the banner lists them
as not in the EffectCraft result.

## Why `link.editIn` is a static command

`Registry::register` reserves every id that does not start with `plugin.<owner>.`. So the dynamic
commands are `plugin.craft.link.focus`, `.unlink` and `.refresh`, and the control channel maps the
wire methods `link.focus` and `link.refresh` to them. `link.editIn` keeps its short id as a static
row in `filmcraft-engine` that calls the hook installed here.

## Known gaps

- Linux focus hand-off cannot raise a window. Stock winit 0.30 has an empty Wayland `focus_window()`
  and no call that applies an activation token to an existing window
  (rust-windowing/winit#3633). `EguiWindow::can_apply_token` is `false`, so `app.activate` answers
  `false` and the window asks for user attention. eframe can mint a token
  (`Frame::request_activation_token`, `Frame::take_activation_token`), but `EguiWindow::mint_token`
  returns `None`: it has no `Frame`, and a token has no user until winit can apply one. Nobody has
  seen focus move on Wayland with this code.
- macOS activation (`yield_activation_to`, `activate_application`) is not written. It answers `false`.
- `link.editIn` runs on the UI thread, so it cannot wait for a slow EffectCraft and keep drawing. In a
  window session it starts EffectCraft, answers `EffectCraft is starting`, and leaves the
  `LinkStarting` dialog (progress bar and Cancel). `ui::show` looks at the child every frame. When the
  record appears it runs the call again. If the child exits it shows `LinkLaunchFailed`. Cancel
  kills the child. Callers that draw nothing pass `wait: true` and block up to the 60 s ceiling.
  `Launch` kills only the process it started, not its children.
- The prompt dialog is drawn by `ui::show` through `HostHooks::per_frame`. It has no UI test beyond
  the answer logic. `ui.inspect` reports it in `dialog` through `HostHooks::dialog_label`.

## Pool replacements (what reinstalls a linked source)

The manager puts its `LinkedSource` in the media pool with `MediaPool::insert`. Anything that replaces or
drops the pool entry has to be handled:

Reinstall, on `Change { cause: Load }` (the installer drains the change feed each frame):

- `install_project` replaces the pool.
- `new_from_template` calls `clear`.

Do not reinstall after `MediaPool::remove`. Each caller deletes or replaces the item on purpose:

- import relink.
- project Clear.
- `remove_unused`.
- `consolidate_duplicates`.
- `edit_subclip` convert.

`link_ops::sync_result` (refresh, focus, the result poller) opens the result file again and
`insert_file`s it. It then calls `manager::after_open`, which wraps the new file source again when the
link is Live.

## Badges

`Session::link_badges` holds the badge text per linked item: `linked`, `linked, not live: <reason>`,
`link broken: <why>`. The Project panel (`media_badge`, `paint_badges`) and the timeline clip label read it.
`NotLive{NoResult}` also installs the offline slate with the reason as its text.

# ADR 0002: window-system calls in `crates/platform` (file drops, cursor position)

- **Status:** proposed (2026-10-21), for the project owner to accept or reject
- **Issues:** #2714 (file drag-and-drop from a file manager on Linux Wayland); #171 (cursor position for
  drops on Windows)

## Context

[ADR 0001](0001-platform-ffi.md) allows `unsafe` in `crates/platform` for OS *media* FFI only, and
says anything else needs a new decision. Two window-system needs fall outside it:

1. **Files dropped on the window on Wayland.** eframe 0.36 / 0.37 are built on winit 0.30, whose
   Wayland backend implements no drag-and-drop: no `HoveredFile` / `DroppedFile` events ever arrive,
   so dragging a clip from Dolphin, Nautilus or any other file manager into the Project panel or the
   Timeline does nothing on a Wayland desktop (KDE Plasma and GNOME default to Wayland). winit 0.31
   adds Wayland drag-and-drop, but no eframe release uses it yet. Running under Xwayland restores
   drops but costs native Wayland scaling (blurry with fractional scales) and is not available in the
   Flatpak, which has X11 access only when there is no Wayland.
2. **The cursor position during a drop on Windows** (`cursor.rs`, #171): winit reports no pointer
   while files are dragged in, so the Timeline could not place a drop where it landed. That call,
   `GetCursorPos`, already lives in `crates/platform` without a written decision; this record
   covers it.

Receiving Wayland drops needs the window's own Wayland connection: a drag is delivered to the
client that owns the surface under the pointer, so a second, separate connection never sees drops
on winit's window. Joining winit's connection means taking its `wl_display` pointer, which is
`unsafe` (`wayland_backend::Backend::from_foreign_display`). egui already does exactly this for the
clipboard (`smithay-clipboard`, through egui-winit).

## Decision

Extend `crates/platform` to a second, narrow job: **window-system calls the windowing layer
(winit / eframe) does not provide**, today `cursor::cursor_screen_position` (Windows) and
`wayland_dnd` (Linux). Every containment rule of ADR 0001 applies unchanged, plus:

1. **One `unsafe` call for Wayland,** in `wayland_dnd::borrow_display`, the only item there with
   `#[allow(unsafe_code)]`. The window's `wl_surface` is compared by address and never dereferenced;
   protocol handling uses `wayland-client`'s safe API (MIT; already in the lockfile through winit,
   with the libwayland backend winit enables). No SCTK: its dispatch code panics on unexpected state,
   ours has no panicking path.
2. **The borrow ends before winit's connection does.** Only the worker thread and `WaylandDrops`
   use the borrowed connection; `WaylandDrops::drop` stops the worker (a `wl_display.sync` on its
   queue wakes it) and joins it. The app holds `WaylandDrops` in its host hooks, and eframe drops the
   app inside its event loop (`save_and_destroy`), before winit's `EventLoop`, which owns the
   display, is dropped.
3. **The worker cannot hang or crash the app.** It runs under `catch_unwind`; a dropped list is read
   through a socket pair with a 50 ms read slice and a 5 s deadline, so a file manager that never
   finishes writing cannot block shutdown; lists are capped (4 MiB, 10 000 paths). Failure to start
   (no Wayland, no `wl_data_device_manager`) only leaves drops unavailable. A drop is `finish`ed only
   when the compositor chose `copy`: finishing before any action is a protocol error, which would
   close the shared connection, and after a `move` the file manager would delete the files we
   import by path.
4. **The public API is safe** and carries no FFI types: `WaylandDrops::start(&window, wake)` takes
   eframe's creation context (the `raw-window-handle` traits) and yields plain `DragEvent`s;
   `cursor_screen_position()` returns `Option<(i32, i32)>`.
5. **Remove when the windowing layer catches up:** `wayland_dnd` when eframe moves to winit 0.31,
   `cursor_screen_position` on Windows when winit reports the pointer during an OS drag.

Audio output, dialogs, menus and other OS integration still do not go in this crate, and anything
else that wants `unsafe` still needs a new decision.

## Consequences

- Files dropped from a file manager reach the Project panel and the Timeline on native Wayland
  (including the Flatpak), at the position the compositor reports.
- The crate's description changes from "OS media FFI" to "OS media FFI and the window-system calls
  above"; `AGENTS.md` §0.3, the crate README and `docs/architecture.md` say so.
- Reviewers of `crates/platform` now also check the lifetime argument in rule 2 whenever the app's
  start-up or shutdown changes.

## Alternatives considered

- **Start on Xwayland when `DISPLAY` is set** (as DeckCraft does; #2715). No `unsafe`, but blurry
  with fractional scaling, loses the Wayland app id, and does nothing for the Flatpak. With this
  decision native Wayland is the default again; `FILMCRAFT_X11=1` still starts on Xwayland.
- **Patch or fork winit / eframe.** A fork to maintain, and winit 0.31's API differs from 0.30's.
- **Wait for eframe on winit 0.31.** No date; drops stay broken on Wayland meanwhile.

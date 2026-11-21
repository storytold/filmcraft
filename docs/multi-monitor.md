# Multi-monitor panels

Any panel (Program monitor, Source monitor, Timeline, Project, Effect Controls, …) can float into
its own OS window and be moved to another display, as in Premiere's undock / "Program monitor on a
second screen". Floating is craft-ui's native-window docking (`native-windows` feature); see
[control-protocol.md](control-protocol.md) for `window.floatPanel` / `window.dockPanel` /
`window.closePanel`. Named workspace saves include the floating windows.

## What this adds: knowing which displays exist

craft-ui stores, with each floating window, the monitor it is on, and on restore keeps a window on
its monitor, moves it onto the main window's monitor when that monitor is gone, and pulls it back
when too little of its title bar would be reachable. It can only do that with a list of the
connected displays. eframe keeps winit's `available_monitors` to itself and the egui fork reports
none, so without a list the dock guessed from the main window's monitor size, which fails for an
unplugged monitor and for monitors of different sizes or scales.

- `filmcraft_platform::monitors::connected_displays()` asks the OS (read-only). Windows uses
  `EnumDisplayMonitors` + `GetDpiForMonitor`, installed by the desktop app as `HostHooks::display_list` (ui-egui stays OS-independent); other platforms return an empty list and keep the
  earlier behaviour. This is the only `unsafe`, in the one crate allowed it (ADR 0001).
- `crates/ui-egui/src/monitors.rs` polls it every 2 s (so hot-plugging is noticed), converts the
  physical pixels to desktop points at the main window's `pixels_per_point` (the unit egui reports
  window positions in), and passes the result to `DockArea::monitors`.

## Hostile numbers

The OS report is treated as input: displays with a non-positive or over-large size or an absurd
position are dropped, a bad scale falls back to 1, at most 16 displays are kept, and a clock that
runs backwards only causes an extra poll. An empty result means "no list", which keeps craft-ui's
fallback. Saved window geometry itself is validated by craft-ui (non-finite or out-of-range rects
are rejected or clamped); see its `placement.rs` tests.

## Control channel

`ui.monitors` returns `{displays: [{name, rect: [x,y,w,h] px, dpiScale}], monitors: [{name,
rect, scale}]}` (the second list is what the dock sees, in points).

## Not done

- macOS and Linux display enumeration (they fall back to the earlier behaviour).
- Mixed-DPI desktops are converted at the main window's scale; per-monitor logical coordinates
  would need the egui fork to report monitors itself.

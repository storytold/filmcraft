# filmcraft-plugins

The FilmCraft side of the suite plug-in host (craft-abi 1, L5). Native only: the lifecycle of
`craft-plugin-host` is native only, so no web crate may depend on this one. `cargo xtask wasm`
proves that.

| Module | What it holds |
|---|---|
| `bridge` | `Bridge`: how a host call of a guest reaches the `Session` (`Services::lend_session` lends the session to host calls for the length of a guest call, other threads queue jobs for `pump`), and the checkpoint that a faulted call restores. A guest's `cmd.execute` runs with `Origin::Plugin`. |
| `services` | `AppServices`: `craft_plugin_host::services::Services` for FilmCraft. |
| `doc` | `doc.query` answers (`what`: `project`, `sequence`, `selection`). |
| `host` | `PluginHost::start`: manager, hook layer, surfaces, bridge, host commands. The install flow (`install_bytes`, `install_path`, `answer`). |
| `commands` | The host's own commands `plugin.app.filmcraft.*` (manage, list, install, grant, enable, disable, remove, revoke). A plug-in can neither list nor run them. |
| `formats` | `format` contributions: `attach` registers a `MediaPool` opener that reads unknown file types through the plug-in `FormatBook`. |
| `surfaces` | `Surfaces`: the `SurfaceSink` that keeps live panels and effects. |
| `ui` | `frame`: serves queued host calls, then draws the permission prompt, the manager view and panel windows. |

## Install, permissions, remove

The host commands take the same ids on the control channel, in the command palette and over MCP.

- `plugin.app.filmcraft.install` with `{"path": "<plug-in package>"}` installs a package. A plug-in that
  asks for permissions answers `needs-grant` with the lists `required`, `new_required` and `optional`, and
  waits for the user. The manager window shows the same prompt.
- `plugin.app.filmcraft.grant` with `{"id", "accept"?, "optional"?}` answers that prompt. `accept` defaults to
  `true`. `optional` lists the optional permissions the user grants.
- `plugin.app.filmcraft.remove` with `{"id"}` removes a plug-in.
- `plugin.app.filmcraft.revoke` with `{"id", "permission"}` takes one permission back.
  `enable`, `disable`, `list` and `manage` complete the set.

A plug-in has no permission until the user grants it. The permissions and their prompt levels are in
section 5 of [`crates/plugin-abi/ABI.md`](https://github.com/storytold/craft-libs/blob/main/crates/plugin-abi/ABI.md)
in craft-libs. Build plug-ins with the craft-plugin-sdk
([craft-libs#455](https://github.com/storytold/craft-libs/pull/455)).

## Why the session is lent

`Services` methods take `&self`, and a command body gets `&mut Session` only for its own call.
The guest runs inside that call. The plug-in host wraps the guest call in `Services::lend_session`
(`Bridge::lend_any`), which swaps the real session into a box on a stack for the length of the guest
call; a host call such as `cmd.execute` borrows it from the stack. `filmcraft-engine` has no seam for it. Callers on other threads (`after` and `change` hooks) put a job in a
mailbox that the app serves once per frame (`ui::frame`).

## Known gaps

- The checkpoint covers the project, the editor state, the undo and redo stacks and the command journal.
  It cannot un-write a file or stop a job that a command started.
- A `before` gate runs while the session is borrowed above it, so `cmd.execute` and `doc.query`
  from a gate fail (`E_UNSUPPORTED` / a failure string). Gates are pure decisions.
- `doc.query` serves `metadata` only. `pixels` answers `E_UNSUPPORTED`.
- The plug-in windows are not in the automation registry (`ui.elements`); the host commands are the
  agent-drivable surface.
- `format` contributions open files (Import of an unknown extension). Save is not wired: FilmCraft's export has no still-image writer to route through a plug-in.
- Plug-in panels open in floating windows. They are not docked.
- The macOS native menu bar does not show the Plugins menu (it is built from the fixed menu list).

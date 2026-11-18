# Plug-ins

FilmCraft runs **command plug-ins**: WebAssembly modules that act on the open project through the
same commands as the menus, the CLI, the control channel and MCP. The sandbox, the manifest
(`craft-plugin.json`), the `.craftplugin` bundle, the per-user store, catalogs and GitHub installs
come from the shared `craft-plugin` crate in
[craft-libs](https://github.com/storytold/craft-libs/tree/main/crates/plugin) (RFC-0046); its
README describes the ABI and how to write and publish a plug-in. FilmCraft's side is
`crates/engine/src/plugin.rs` (with `plugin/`), and the Plug-ins window in
`crates/ui-egui/src/panels/plugins.rs`.

A plug-in calls commands in their JSON form, as `engine.execute` does; `filmcraft-cli commands`
lists them with their parameters. Times go as the commands document them (ticks, `frame`,
`timecode`). The selection a plug-in receives is the selected timeline clips' ids. A plug-in only
installs when its manifest names `filmcraft` in `apps`.

## Using plug-ins

- **Window ▸ Plug-ins** lists each installed plug-in's commands, then **Manage Plug-ins…**. A
  command with parameters asks for them in a dialog generated from its manifest.
- **Manage Plug-ins…** lists the installed plug-ins with their commands, and installs from a
  `.craftplugin` file, from a GitHub repository (`owner/repo` or `owner/repo@tag`: its release's
  `.craftplugin`, checked against the release's `SHA256SUMS`) or from a catalog (`index.json` on
  GitHub's hosts, or a `file://` one). **Check for Updates** looks at each plug-in's catalog or
  repository. Downloads run in the background.
- Every install shows a confirmation first: the plug-in's access, the commands it may call, its
  kind, where it comes from and its SHA-256, and says plainly when a repository is in no catalog
  you use. Nothing is written before **Install**.
- The desktop app keeps plug-ins in `Plug-ins/` in its data directory, next to
  `preferences.json`. The CLI, MCP and the web build keep them for the session only, and install
  from GitHub or https catalogs only in the desktop app.

## Commands

| Command | Parameters |
|---|---|
| `plugin.list` | `{}` |
| `plugin.install` | `{path}`, `{data: base64, name?}`, `{repo: owner/repo[@tag]}` or `{catalog, id, version?}`; `review: true` checks and describes the bundle without installing; `sha256` installs only that bundle |
| `plugin.update` | `{id?, apply?}`: lists newer releases; `apply` installs those asking for no other permissions |
| `plugin.remove` | `{id}`: the plug-in and its settings |
| `plugin.run` | `{id, command?, params?, fuel?}` |
| `plugin.catalog.list` | `{url?}`: the packages FilmCraft can run |
| `plugin.catalog.add` | `{url, remove?}` |

## What a run may do

- A run is **one undo step** named after the plug-in (Edit ▸ Undo takes the whole run back). A
  run that fails (a trap, an exhausted instruction budget, a non-zero status, a refusal it passes
  on) puts the project, the selection and the rest of the editing state, and the undo history
  back as they were. A run that changes nothing records no step.
- `permissions.access: "read"` plug-ins may call any allowed command, but a call that changes the
  project (or records an undo step) is put back and refused. Selecting, moving the playhead and
  inspecting are allowed. `permissions.commands` narrows the commands further.
- Whatever the access, a call that changes FilmCraft's settings (`preferences.json`) or starts
  background work (a job) is put back, its job cancelled, and refused.
- Moves keep FilmCraft's identity rule: a `timeline.move` that lists a clip twice is refused, and
  the run with it.
- Settings: each plug-in keeps up to 64 KB of its own settings (keys up to 128 characters), saved
  with the preferences (`pluginData`; resetting Settings ▸ Plugins keeps them).
- **Refused commands**, whatever the manifest asks: everything `craft-plugin` refuses in every app
  (`plugin.*`, `plugins.*`, `edit.undo`, `edit.redo`, `file.*`, `app.*`, `pref.*`, `window.*`);
  the undo history (`history.*`); preferences and libraries (`prefs.*`, `shortcuts.*`,
  `presets.*` but `presets.list` and `presets.apply`, `labels.*` but `labels.presets`, `lut.*`,
  `graphics.template.install` / `remove` / `export`, `essentialSound.savePreset` /
  `deletePreset`, `project.viewPreset.*`); media on disk (`media.*` but `media.status` and
  `media.colorInfo`, `mediaBrowser.*`, `mediaCache.*`, `clip.extractAudio`,
  `clip.updateMetadata`); files by name (`export.*`, `captions.import`, `captions.export`,
  `markers.exportCsv`, `lumetri.exportCube`, `lumetri.presetThumbnails`, `graphics.newFromFile`,
  `edit.editOriginal`); the clipboard (`edit.copy`, `edit.cut`, `edit.paste`, `edit.pasteInsert`,
  `edit.pasteAttributes`, `edit.pasteMedia`); rendering and background work
  (`sequence.render*`, `sequence.deleteRenderFiles*`, `masks.track*`, `clip.sceneEditDetection`,
  `sequence.transcribe`, `transcript.generate`, `transcript.downloadModel`, `tts.*` but
  `tts.voices` and `tts.inspect`, `jobs.*`); recording and playback (`audio.voiceover.*`,
  `mixer.recordStart` / `Stop`, `multicam.recordStart` / `Stop`, `trim.shuttle*`, `trim.tick`,
  `trim.playAround`, `trim.cancelDynamic`, `playback.*`); and the app's own state (`events.*`,
  `perf.*`, `help.*`).
- Parameters that name files (`path`, `paths`, `file`, `files`, `dir`, `folder`, `directory`,
  `destination`, `location` holding text, at any depth) are refused; a mask's `path` (points) is
  allowed. A plug-in gets no disk, network, clock or other project.

Because `craft-plugin` refuses `file.*` in every app, plug-ins can't use File ▸ New (new
sequences, bins, mattes, adjustment layers…), whose ids FilmCraft takes from the reference app's
menus.

Not yet: plug-in panels (`ui_show` runs report that FilmCraft doesn't show them), offering files
to save (`saveFile`), and refreshing the macOS menu bar's Window ▸ Plug-ins when plug-ins are
installed or removed while the app runs (the Plug-ins window always lists them).

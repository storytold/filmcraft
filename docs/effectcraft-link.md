# EffectCraft link

Replace a clip with an [EffectCraft](https://github.com/storytold/effectcraft) composition, edit it
there, and see the result in the FilmCraft timeline: the same workflow as Premiere Pro's Replace With
After Effects Composition and Dynamic Link, built on files and processes. FilmCraft links no
EffectCraft code (EffectCraft already depends on FilmCraft's codecs, so a link in the other direction
would be a cycle); it runs `effectcraft-cli` to write and render projects and `effectcraft` to edit them.

Code: `crates/engine/src/effectcraft_link.rs`, tests in `effectcraft_link_tests.rs`. Run the ones that
render with `EFFECTCRAFT_CLI=<path of effectcraft-cli> cargo test -p filmcraft-engine effectcraft_link`;
without it they return at once.

## What you can do

| Command | |
|---|---|
| `effectcraft.replaceClip` (Clip menu, timeline context menu) | Replace the selected clip's footage with a composition: writes `<clip> EffectCraft.ecproj` (one composition with the part of the footage the clip shows, nothing more), renders it to ProRes 4444 with alpha in `EffectCraft Linked Media/` next to the saved project (the system temporary folder for an unsaved one), points the clip and its linked clips at the render, gives them the Dynamic Link label (Settings ▸ Labels) and opens the project in EffectCraft. Files that already exist: `existing` = `ask` (the interface shows Overwrite / Keep Old / Cancel), `overwrite`, or `keepOld` (`… EffectCraft-2`, `-3`…) |
| `file.import` of a `.ecproj` (File ▸ Import, a drop on the Project panel or the timeline, the Media Browser) | An item named after the project appears at once, with the size, frame rate and length of its composition (the one open when it was saved, else the first), and can be placed on the timeline; the composition renders in the background and the movie is bound to the item when done. Importing the same project again gives the same item |
| `effectcraft.edit` (Clip menu) | Open a linked clip's project in EffectCraft |
| `effectcraft.openProject {path}` | Open any `.ecproj` in EffectCraft (double-click in the Media Browser, which lists `.ecproj` files) |
| `effectcraft.link {project, comp?}` | Link a clip whose footage is a render of an existing project |
| `effectcraft.sync` | Render the linked projects that changed since their last render, now |
| `linkedProject.onDemand {item, enabled:true}` | Switch an active EffectCraft link to frames on demand; also available as **Render frames on demand** in Manage linked items. The setting is saved with the project |

The composition picker also offers **Render frames on demand** before Import. This avoids an initial
full-composition render; API callers use `linkedProject.import {path, object, onDemand:true}`.

While a linked project is saved in EffectCraft, FilmCraft notices (its file changed), renders it again
in the background and switches the clips to the new movie. A movie FilmCraft is playing can't be
replaced on Windows, so each re-render goes to the next numbered name (`… EffectCraft v2.mov`, `v3`)
and published movies remain available to saved documents and Undo. Persistent links use an immutable,
revision-addressed cache. Cache quota exhaustion stops new renders and retains the last good movie;
it never silently deletes an older document's footage.

## Progress

A render is a job (Events panel, Progress panel), and the status bar shows its percentage, frame and
time left. The clip being rendered is dimmed on the timeline with a progress bar, and the Program
monitor shows the same over the picture while the playhead is on it (the Source monitor too).
Progress comes from `effectcraft-cli render --progress`, which prints `progress <done> <total>
<elapsed> <left|->` lines on stderr; an older `effectcraft-cli` renders without them.

## Finding the tools

`cli` / `app` parameters, then `EFFECTCRAFT_CLI` / `EFFECTCRAFT_APP`, then next to the running
program or in an `EffectCraft/target/release` folder beside one of its parent folders (a side-by-side
checkout), then the `PATH`.

## Limits

- Links are saved in the FilmCraft document. After reopening, enable the installed renderer in
  File ▸ Dynamic Link to resume updates. Executable paths are session authorization, never trusted
  from a document. The last good movie remains available while a source is missing or rendering.
- Relink a moved project or composition through Manage linked items. Placement, source trim and
  effects stay on the same timeline item. Export refuses a changed or missing linked source.
- Bake writes an exclusive, durable `.mov` snapshot; it cannot overwrite an existing destination.
  Restore link resumes saved-project updates. Undo and Redo preserve the published snapshots.
- An imported project is placed as picture only: if its composition has sound, the audio is in the
  render but not on the timeline.
- While an imported project renders, the Program monitor shows the offline slate under the progress
  panel (the movie does not exist yet).
- Full-render mode re-renders the composition after a saved change. With **Render frames on demand**,
  a save prepares a new revision without rendering the whole composition. Video requests cache up to four
  adjacent full-resolution ProRes 4444 frames; audio requests cache one-second PCM sections. Preview and export
  use the same revision and caches. Bake materializes an ordinary full-length movie.
- On-demand mode keeps one private `effectcraft-cli mcp --empty` process and its project loaded.
  The renderer must support MCP `command_run` and the Render Queue commands (tested with 0.22.0).
  Switching linked sources/revisions replaces that process, bounding renderer memory. Cached frames
  bypass it; playhead requests take priority over waiting thumbnails and playback prefetch.
  Four-frame blocks amortize transport/encoding costs. Cancellation, source release and document
  changes stop the renderer. A dead process is recreated on the next uncached request.
  Render Queue edits and Undo history stay in the private session; auto-save is disabled there.
  Files remain the transport, so this does not guarantee realtime playback or share GPU textures.
- Native rendering is bounded and cancellable. The prototype limits individual renders to 1 GiB,
  composition dimensions to 16 megapixels, duration to one hour and dependency files to a combined
  512 MiB. Image-sequence dependencies require baking. Source edits must be saved in EffectCraft.

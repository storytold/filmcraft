# Experimental saved-project links (#388)

This native-desktop prototype adds a persistent reference to an external project's object.
EffectCraft is the first adapter. PhotoCraft documents and VectorCraft artboards are future
adapters, not supported formats in this PR. No new EffectCraft dependency is linked into FilmCraft.

## Try it

1. Install EffectCraft with `effectcraft-cli` (tested against 0.16.0).
2. Choose **File > Dynamic Link > Import EffectCraft Composition**. Enter the absolute path to the installed CLI
   executable and enable it. This is session-only: a project file cannot select an executable.
3. Browse folders in the centered Import modal, select an `.ecproj`, choose a composition and click Import. A project path can also be entered directly. Drag the new item onto
   the timeline like ordinary media. The initial placeholder is transparent; the dialog reports
   render activity and errors. No manual export/import is needed.
4. Change the composition in EffectCraft and save. FilmCraft checks about every five seconds,
   renders in the background and replaces the item's full-resolution source. Timeline instances,
   source trims, transforms and effects keep their identity and values.
5. Bake to a **new** `.mov` path to freeze an ordinary media file. Restore link resumes automatic
   updates; undo/redo also preserve the link. Existing files are never overwritten by Bake.
6. Reopen the FilmCraft project: enable the installed renderer again. The source path and selected
   object ID survive. Use the path/composition fields and Relink for a moved project or deleted comp.

Export mode shows a persistent warning when the selected sequence uses a linked composition whose
renderer is disabled for this session. **Configure linked projects…** opens the renderer controls;
enable your installed renderer and wait for the link to refresh before exporting. Baking the link
is an alternative when live updates are no longer needed. Unused and baked links do not block export.

File > Import also accepts `.ecproj` once its renderer is enabled. A multi-composition project needs
the explicit selection in Linked Projects (or the `object` command parameter).

### Commands

All engine commands are available through CLI/MCP/control, with JSON parameters:

```text
linkedProject.configure {"executable":"/absolute/path/to/effectcraft-cli"}
linkedProject.inspect   {"path":"/absolute/path/title.ecproj"}
linkedProject.import    {"path":"/absolute/path/title.ecproj","object":"1"}
linkedProject.status    {}
linkedProject.refresh   {}
linkedProject.cancel    {}
linkedProject.relink    {"item":42,"path":"/new/path/title.ecproj","object":"1"}
linkedProject.bake      {"item":42,"path":"/new/path/title-baked.mov"}
linkedProject.bake      {"item":42,"restore":true}
```

Cancel pauses automatic retries until Refresh, a new import or renderer enablement. Refresh also
increments the persistent cache generation, forcing a new render for environmental changes (fonts,
external resources not enumerated as footage). Neither configure nor executable paths are journaled
into a shareable project; renderer selection is a local explicit trust decision.
Trusted CLI/MCP/control callers have the same explicit configuration authority as the UI.
Loading a project cannot select or enable an executable.

## Contract and cache

`Project::project_links` maps stable FilmCraft media item IDs to versioned, application-neutral
`ProjectLink` descriptors: provider, source project path, provider-owned object ID, display name,
successful revision, manual refresh generation and baked state. Schema 16 prevents older readers
from silently dropping the link. Paths are locators; moving a project requires explicit relinking.
There is no universal cross-app document UUID in this prototype.

`ProjectAdapter` enumerates renderable objects, computes an input revision and renders a source to a
cache path. It supplies the output extension; a future still-image adapter need not produce a movie.
The EffectCraft adapter owns JSON schema validation and CLI flags. A frame-server transport can
replace this adapter's rendering path later; the timeline and stored source identity remain intact.

One worker per session checks/renders links serially. Work is panic-guarded. The native CLI has no
window, stdin or shell; arguments are passed separately. Cancel kills and reaps the active child;
there is a ten-minute subprocess timeout. Status exposes the current object/phase, not a fabricated
frame percentage. Results from a removed/undone/relinked descriptor are discarded.

Cache retention remains a prototype limitation: distinct revisions and interrupted-render files
can accumulate in the host's temporary directory. Cross-platform private cache placement and a
quota/cleanup policy still need design. Age alone is not a safe eviction rule because saved projects
and undo history can reference older revisions; this prototype does not automatically delete them.

The SHA-256 revision includes project bytes, object ID, directly listed footage contents and
resolved paths, renderer path/size/mtime, render contract and explicit refresh generation. This
conservatively invalidates every linked composition after any project edit. Sources changed during
a render are rejected and retried. Whole-dependency hashing happens on the worker on each scan;
optimizing that cost without weakening invalidation is follow-up work.

EffectCraft renders the complete composition at source resolution/rate as ProRes 4444 RGBA, with
audio off. The cache is disposable and lives in a per-user `FilmCraftLinkedProjects-v1` folder:
under `~/Library/Caches/FilmCraft` on macOS, `$XDG_CACHE_HOME/filmcraft` (or `~/.cache/filmcraft`)
on other Unix systems, and the user's temp directory on Windows. On Unix the folder is created
`0700`, and one this user cannot `chmod` (another user's folder) or a symlink is refused before any
cached file is trusted. The cache survives app restarts until the OS or the user clears it. Outputs must be readable visual media and at most 1 GiB. Files publish only after successful rendering; existing
readers retain complete files. Successful cache replacement invalidates FilmCraft's source/view
caches without adding undo steps. Last-good media stays visible after an update fails. Status reports
the error; final export refuses missing/changed sources instead of silently exporting last-good media.
Final export uses the same full-quality source, never a low-resolution preview. Its export worker
hashes dependencies and validates the cache before writing output; the interactive caller captures
the authorized adapters without hashing project files.

## Deliberate first-step limits / remaining review work

- Saved changes only: no unsaved editor connection, frame server or realtime guarantee.
- Full compositions, not used ranges; cache accounting/eviction and render-bar integration remain.
- EffectCraft schema 2, composition IDs and directly listed footage. Image-sequence dependencies
  are refused; indirect expression reads, external fonts/LUT changes and other unlisted resources
  require explicit Refresh. No dependency-server protocol is claimed.
- Project input: 16 MiB, 100,000 items; up to 256 compositions and 16 megapixels per frame, dimensions 1..16384; positive duration up to one hour;
  nonzero bounded frame-rate components and at most 240 fps; direct dependencies total at most 512 MiB.
- Bake currently supports EffectCraft `.mov` output up to 128 MiB. Larger copies need a streaming,
  background host file-copy service. Baking writes a durable ordinary file; restoring keeps the
  original descriptor. Packaging/collecting linked dependencies is not implemented.
- Renderer location is entered manually and enabled each session. Automatic discovery, persistent
  trusted configuration, file pickers, richer progress and platform installation handling remain.
- Native process hosts only. Web cannot render links; use baked media. Flatpak needs an approved
  renderer packaging/host interface; this PR does not add a sandbox-escape permission.
- No PhotoCraft/VectorCraft adapter, editable templates, reverse links or shared project model yet.

## Verification

Engine regressions cover the generic adapter (a synthetic still/artboard provider), stable item
identity, updates/cache reuse, round-trip persistence, failure recovery, stale-export rejection,
undo/redo while work is pending, cancellation and malformed parameters/metadata. UI tests verify the
dialog and automation IDs. The explicit real-renderer test generates synthetic content through the
installed EffectCraft CLI, saves a colour edit, checks the timeline updates with unchanged placement,
trim and effects, and compares the linked frame with a direct render. It also exercises bake/restore.

```powershell
$env:FILMCRAFT_EFFECTCRAFT_CLI = 'C:\path\to\effectcraft-cli.exe'
cargo test -p filmcraft-engine project_links::tests
cargo test -p filmcraft-engine native_effectcraft_saved_edit_updates_timeline -- --ignored
```

A standalone procedural fixture is in [examples/linked-project-demo.jsx](examples/linked-project-demo.jsx).
No private project, production footage, system font or generated binary fixture is committed.
See #388 for design discussion and #1024 for the broader interop tracking scope.

### Local validation record (2026-10-24)

Windows / EffectCraft CLI 0.16.0 built from main `eb510878`: formatting, workspace Clippy,
engine tests, the opt-in real renderer integration, layers, asset attribution and wasm (49 crates)
pass. All 198 UI unit tests pass, including stable watch text/layout regressions; the styled picker
and linked-project automation integration tests pass. The UI integration suite has 480 passes and
one timing-sensitive proxy-attachment failure that passes alone. The contributor also confirmed
that the Dev dialog no longer flickers.
The unfiltered workspace run is not green: its NVENC SDR sample-entry assertion previously
reproduced in a pre-feature binary and is tracked by #2386. No encoder changes are included.

## Linked-item appearance

Active EffectCraft links use EffectCraft's icon pink (`#E0368F`): the Project item's label swatch
and the timeline clip's base color. Project cards retain their normal selection outline. Linked compositions use the same composition glyph as EffectCraft in list view and on thumbnail cards. Selection and disabled-clip shading
still apply. Ordinary media, other providers and baked fallbacks keep normal label colors.
The Import picker uses FilmCraft's shared centered modal styling. Renderer configuration and
existing-link/cache actions remain available in expandable sections; Replace and Create New
Composition workflows are not part of this step.

## Export preflight

Only links reachable from the exported sequence (including nested sequences and subclip parents)
are checked. Unused, disabled or stale links elsewhere in the project do not block an export.
The export worker computes fresh content hashes and reopens each used render cache, requiring
video media before encoding or writing caption sidecars. Background content scans remain automatic
at a five-second cadence; metadata-only revision caching is not used. Failed asynchronous preflight
is reported through the normal export job status. Explicit wait mode performs the same checks inline.

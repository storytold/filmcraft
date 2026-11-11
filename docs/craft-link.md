# Craft Link: Edit in EffectCraft

Craft Link connects FilmCraft to other Craft apps on the same machine. FilmCraft uses it to send clips to
EffectCraft and to show the result on the timeline. The code is in `crates/link` (see its
[README](../crates/link/README.md)) and the transport is `craft-ipc` from craft-libs.

## The user flow

1. Select one or more video clips in the timeline and choose Clip ▸ Edit in EffectCraft
   (`⌘⌥⇧E`, `Ctrl+Alt+Shift+E` on Windows and Linux). The command needs an open sequence and at least one
   selected video clip. A selection that mixes linked and unlinked clips is refused.
2. An unsaved project asks where to save first, because the result movie is written next to the project file.
3. EffectCraft must be installed. If it is not, a dialog opens the download page. If it is installed and
   not running, FilmCraft starts it and shows a dialog with a progress bar and Cancel. FilmCraft waits at most
   60 seconds. When several EffectCraft windows run, a dialog asks which one to use.
4. FilmCraft nests the selection. It renders every source that has no file (generators, titles, nested
   sequences) to a ProRes 4444 movie with alpha in the [cache folder](#the-cache-folder), with 12 frames of
   handle on each side. A dialog shows the progress. A movie with the same content is reused.
5. FilmCraft exports the nest as OpenTimelineIO JSON and sends it to EffectCraft in the `link.open` call. It
   does not write an OTIO file to disk. The call also carries the place in the editor (time, selection, view).
6. The result movie is written to `<project>/Project Links/<link id>.mov`. It replaces the nest on the
   timeline as a linked media item named `<clip> (EffectCraft)`. The nest stays in the project as
   `<clip> (nest)`. The whole step is one undo step.

Choosing the command on a linked clip opens the existing link again instead of nesting.

### What the clip shows

- **Badge.** `linked` while EffectCraft runs and the link is attached. `linked, connecting` while the link attaches. `linked, not live: <reason>` when EffectCraft is not running, is not installed, needs an
  update, has not rendered a result yet, or stopped in the middle of a render. `link broken: <why>` when the link
  cannot work. The Project panel and the timeline clip label show it.
- **Live frames.** While EffectCraft runs, the clip plays frames that EffectCraft renders. When a live frame
  is late or EffectCraft is gone, the clip shows the frame of the result movie. Audio always comes from
  the result movie.
- **Refresh.** EffectCraft renders the result movie again after its comp changes, and FilmCraft picks up the
  new file.

### Effects EffectCraft cannot import

EffectCraft imports Motion, Opacity and Time Remap. FilmCraft moves every other effect, with its masks and
keyframes, to the outer clip before the export. The payload never names them. They apply to the result
movie after the EffectCraft output. A nest moves its effects in two cases:

- It holds one video clip that plays at normal speed and spans the same time as the outer clip.
- It holds several video clips that all carry the same effects, with no animation and no masks, and no two
  clips are on screen at once.

In every other shape nothing moves, and EffectCraft reports the effects it dropped. Effects on overlapping
clips stay where they are, because an effect that runs on each clip before the composite (a key, a blend)
gives another picture than the same effect on the composite.

After the swap, a banner says: "Click Continue to nest your clip. This keeps the effect in FilmCraft, but
you will not see it applied in EffectCraft." Continue keeps the link. Undo pops the one `link.editIn` step,
which restores the original clip, its effects and the selection, and sends `link.close`. EffectCraft keeps
the comp with any edits. An edit made after the nest removes the banner, because Undo would then undo
something else.

`link.unlink` makes the nested sequence the clip source again. Effects that were moved stay on the outer clip.

## The link socket

FilmCraft opens a Craft Link socket for every desktop process.

- It is always on. There is no flag to enable it and no setting to turn it off.
- It is one local socket per process, named `filmcraft-<pid>` in the per-user runtime directory (on Linux
  `$XDG_RUNTIME_DIR/craft`). It uses no TCP port, so it cannot conflict with another app or with
  `--control`.
- It is local only. On Unix the socket file is mode `0600` in a `0700` directory.
- `craft-ipc` checks every connection. The peer user id must match the user who runs FilmCraft, and the
  connection must present the 64 hex character token from the instance record within two seconds.
  At most 16 connections are open and at most 4 of them unauthenticated.
- Each app writes a discovery record (mode `0600`) in the same directory. EffectCraft and FilmCraft find
  each other through these records.

The socket is separate from the `--control` TCP server. See
[control-protocol.md](control-protocol.md).

## The cache folder

Rendered movies for sources without a file go to a private folder per user:

| Platform | Folder |
|---|---|
| Linux | `$XDG_CACHE_HOME/artcraft/link/prepared`, or `~/.cache/artcraft/link/prepared` |
| macOS | `~/Library/Caches/artcraft/link/prepared` |
| Windows | `%LOCALAPPDATA%\artcraft\link\prepared` |

`CRAFT_LINK_CACHE_DIR` names the folder that holds `prepared`, so the movies go to
`$CRAFT_LINK_CACHE_DIR/prepared`. The path must be absolute.

On Unix `craft-ipc` creates each missing folder with mode `0700`. It refuses a folder that is a symlink,
has another owner or has looser permissions. FilmCraft never falls back to the shared temp folder. When no
cache folder can be chosen, `link.editIn` fails with a message that names `CRAFT_LINK_CACHE_DIR`.

A movie is named by a hash of what it shows, so a later run with the same source reuses it. When a render
starts and the folder holds more than 20 GiB, the oldest movies go first.

## Project Links

`<project>/Project Links/` holds one `.mov` result per link, named by the link id. It sits next to the
`.fcproj` file. FilmCraft creates the folder when the first link is made. Each link record holds the
absolute path of its result movie.

## Platform limits

- Linux focus hand-off cannot raise a window. Stock winit 0.30 has an empty Wayland `focus_window()` and no
  call that applies an activation token to an existing window
  ([rust-windowing/winit#3633](https://github.com/rust-windowing/winit/issues/3633)). `app.activate`
  answers `false` and the window asks for user attention. Nobody has seen focus move on Wayland with this
  code.
- macOS activation is not written. It answers `false`.
- The `craft-ipc` Windows paths (named pipe, token user check) have not been run on Windows.
- `link.editIn` runs on the UI thread. In a window session it starts EffectCraft, answers
  `EffectCraft is starting` and shows the progress dialog. It does not block drawing.

## Design decisions

[ADR 0002](adr/0002-craft-link-and-plugin-decisions.md) records why FilmCraft nests first, lifts effects, restores the
document after a plug-in fault and shares openers as closures.

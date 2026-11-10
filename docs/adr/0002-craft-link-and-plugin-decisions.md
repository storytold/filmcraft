# ADR 0002: Craft Link and plug-in host decisions

- **Status:** proposed, with [PR #4351](https://github.com/storytold/filmcraft/pull/4351) (2026-11-09)
- **Code:** `crates/link`, `crates/plugins`, `crates/media`, `crates/codecs`

## Context

PR #4351 adds Edit in EffectCraft (Craft Link, `crates/link`) and a host for WebAssembly plug-ins
(`crates/plugins`). Four choices in it shape how the rest of the app behaves. This ADR records them and
the alternatives that the code or the commit history shows. The user flow is in
[craft-link.md](../craft-link.md).

## Decisions

### 1. Nest the selection first, and never ask

`link.editIn` nests the selected clips, then lifts effects, renders sources that have no file, exports the
nest as OpenTimelineIO, opens EffectCraft and swaps in the linked result. It runs as one undo step. It
does not ask before it nests. When something does not reach EffectCraft, the reply carries
`lifted_effects` and `unsupported`, and a banner reports them.

Alternative considered: a blocking dialog before the nest ("EffectCraft cannot keep everything") with a
Continue button. Rejected. Users pressed Continue, edited in EffectCraft and then saw no effect after Back.
The banner takes the same information and leaves the decision to Undo.

### 2. Lift effects EffectCraft cannot import onto the outer clip

EffectCraft imports `motion`, `opacity` and `time_remap`. `lift::lift_unimported` moves every other effect,
with its masks and keyframes, from the inner clip to the outer clip before the export. The payload never
names them, they survive `link.focus` and Back, and the one Undo step puts them back. A banner says:
"Click Continue to nest your clip. This keeps the effect in FilmCraft, but you will not see it applied in
EffectCraft." Continue keeps the link. Undo pops the `link.editIn` step and sends `link.close`.

The rule for what moves is narrow. One inner clip at normal speed that spans the outer clip moves. Several
inner clips move only when they all carry the same static, unmasked effects and no two overlap in time. Any
other shape is ambiguous: nothing moves and EffectCraft reports the drop. Effects on overlapping clips stay
put, because an effect that runs on each clip before the composite (a key reveals the clip below only
then) gives another picture than the same effect on the composite.

Alternatives considered:

- Leave the effects inside the nest and let EffectCraft report them as dropped. The code keeps this
  for the ambiguous shapes. For a clear shape it loses the effect on the result movie.
- Move the shared effects of overlapping clips as well. Rejected in a later fix, for the reason above.

The cost is order. An effect that ran on the inner clip before EffectCraft's motion and opacity now runs
after the whole EffectCraft result.

### 3. Restore the document when a plug-in call faults

A plug-in changes the document only through `cmd.execute`. The first command of a guest call takes a
`Checkpoint`. When the call faults, the bridge puts it back. The checkpoint holds the project, the editor
state, the undo and redo stacks, the command journal, the path the document saves to, the media pool with
the document of each stored media path, and the saved revision, so a dirty document comes back dirty. A
rollback runs only while the session revision is still the one the plug-in's last command left. Otherwise
the plug-in's edits stay and the user gets a message, so the rollback never undoes work that is not the
plug-in's.

The checkpoint cannot undo a file a command wrote or a job it started.

Alternative considered: a narrower checkpoint of the project, state, history and journal. That was the
first version. It left a changed save path, a swapped or cleared media pool and a wrong saved revision
behind after `file.newProject`, `file.open` and `file.newProjectFromTemplate`. The checkpoint grew to cover
each of them.

### 4. `Opener` is a shared closure, so each host owns its format book

`filmcraft_media::Opener` is `Arc<dyn Fn(&str, Arc<[u8]>) -> Option<Result<SharedSource>> + Send + Sync>`.
`OpenerFn` keeps the plain function shape and `opener_of` wraps one as an `Opener`. The built-in container
openers stay plain functions in `codecs::openers()`. The plug-in host registers a closure that holds the
format book of the session it was attached to with `MediaPool::register_opener`.

Alternative considered: a plain-function `Opener` that searches a process-wide list of format books and
uses the first live one that handles the extension. Rejected. With two hosts in one process the second
session opened files through the first host's plug-in, or failed without trying its own. The global list is
gone.

## Consequences

- A user who nests a clip and sees a banner they did not want presses Undo once. The nest, the lifted
  effects and the selection come back.
- Lifted effects run after the EffectCraft result. A grade that depended on running before motion changes
  the picture.
- A plug-in fault leaves no change in the project, state, history, path or pool. Files and jobs that its
  commands started stay.
- Adding a built-in container opener is unchanged: add a plain function to the `plain` array in
  `codecs::openers()`. A new opener that needs state is a closure.
- Two plug-in hosts in one process do not see each other's formats.

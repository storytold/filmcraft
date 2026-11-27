# ADR 0002: Craft Link decisions

- **Status:** proposed, with [PR #4351](https://github.com/storytold/filmcraft/pull/4351) (2026-11-09)
- **Code:** `crates/link`, `crates/engine`

## Context

PR #4351 adds Edit in EffectCraft (Craft Link, `crates/link`). Two choices in it shape how the rest of the
app behaves. This ADR records them and the alternatives that the code or the commit history shows. The user flow is in
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

## Consequences

- A user who nests a clip and sees a banner they did not want presses Undo once. The nest, the lifted
  effects and the selection come back.
- Lifted effects run after the EffectCraft result. A grade that depended on running before motion changes
  the picture.

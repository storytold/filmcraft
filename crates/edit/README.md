# filmcraft-edit

Pure timeline edit algebra: functions on `&mut Sequence` that either make the whole edit or leave
the sequence unchanged. The engine calls them inside `Session::edit_sequence`, which makes each
command one undo step and checks `Sequence::check` afterwards. The crate root's documentation
lists the edits and their rules (insert, overwrite, razor, lift, extract, ripple delete, trims,
roll, slip, slide, rate stretch); this file records the decisions and limits of the parts that
have them.

## Conform (`conform.rs`, #5265)

`conform::conform(seq, fits, keep, ctx)` changes the sequence at each section end by a `Fit`
(`end`, `delta`) and ripples everything after it, keeping one clip as it is. The engine
(`crates/engine/src/conform.rs`) works the fits out from a voice-over take's prompter log and moves
the markers; this module is only the timeline edit.

Decisions:

- **Longer** opens time at the section end on every track that ripples, with the primitives an
  insert uses (`insert_track_gap`, `captions::insert_gap`), and fills it on each video track with a
  frame hold segment of the track's last frame before the end: a copy of the clip with `frame_hold`
  and `source_in` at that frame (the representation `clip.insertFrameHoldSegment` uses), linked to
  nothing, with Hold Filters on so its effects freeze too. Each video track holds its own last
  frame, so the stacked picture holds as it was. A clip spanning the end is split and the hold goes
  between the halves; a clip that already is a frame hold continues as it was (same frame, same
  Hold Filters). A video track with a gap before the end, audio tracks and caption tracks get empty
  time (silence on audio); clips and captions that span the end are split and their later part
  moves.
- **Shorter** removes `[end + delta, end)` with `extract` and no targeted track, so it is exactly
  what Extract does to that range.
- **Tracks.** As for an insert or extract with no targeted track: an unlocked, sync-locked track
  ripples; a track with sync lock off stays where it is. A locked, sync-locked track with material
  after the first change would stop the ripple: refused, naming the track (`locked_in_the_way`).
- **Transitions.** A transition in progress where a fit cuts is refused (`obstacle`): at the end of
  an extended section, across or at the edges of a removed range, and on video tracks over the last
  frame a hold would show (a held frame of one clip cannot show a mix of two). Transitions after
  the change move with their clips.
- **The kept clip** (the take) is taken off its track before the edit and put back unchanged, so it
  is never split or moved while the rest of its track ripples. Refused when another clip would then
  overlap it, or when it has a transition with another clip.
- **Order.** Fits are applied from the last to the first, so each `end` is in the original
  sequence time; a test checks this equals applying them one at a time from the first, each at its
  end as the earlier ones moved it.
- Markers are not moved here (the engine uses `sequence_tools::ripple_markers`).

Tests (`src/conform/tests.rs`, 11): the hold between split halves and after a clip ending at the
end; empty time, split sound and split captions elsewhere; an existing hold continues; a trim equals
Extract; the kept clip unchanged while its track ripples; a clip sliding under it refuses; a locked
track named, sync lock off stays; a transition at the end refuses. Properties: the length changes by
exactly the sum of the fits, content before the first change and the kept clip are untouched and
`Sequence::check` holds; together equals one at a time; hostile fits never panic and a refusal
changes nothing. Result on 2026-11-25: `cargo nextest run -p filmcraft-edit`, all pass (see the pull
request for counts).

Limits: a hold freezes the frame and its effects, but a graphic or nested clip that animates on its
own is held through `frame_hold` like any clip; a section that ends during a transition cannot be
conformed until the transition is moved; there are no targeted tracks, so a track the user wants
left alone needs its sync lock off.

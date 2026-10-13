# FilmCraft engine

The L3 engine owns editing sessions, command dispatch, undo/redo, media services,
and jobs. UI, CLI and automation clients use the same command registry. Project
edits go through session history; rendering and interchange stay in lower layers.

`effects.reorder` moves a complete nonintrinsic effect instance to a final stack
index. It preserves parameters, keyframes and masks, leaves intrinsic positions
fixed, and rejects invalid indices atomically. A drop is one history step; moving
to the same index creates none. Selection follows duplicate-id instances, including
undo/redo. Indistinguishable duplicates clear ambiguous selection on history moves.

`effect_order_tests` covers hostile inputs, history and selection, and checks that
reordering Rounded Crop and Drop Shadow changes rendered pixels. Run the workspace
release tests and `cargo xtask ci` before submitting changes; GUI gestures and
screenshots are verified in the UI crate.

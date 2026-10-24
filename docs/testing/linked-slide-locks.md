# Linked slide edits and track locks

With linked selection enabled, `timeline.slide` treats the requested clip and its linked partners as one edit. If any member's track is locked, it returns the existing locked-track error before changing any member. The project and undo/redo history stay unchanged. Unlock the affected tracks to slide the linked clips together.

This matches the existing `timeline.slideLeft`, `timeline.slideRight`, `timeline.slideLeft5` and `timeline.slideRight5` commands. Both paths pass the complete selection to `filmcraft_edit::slide_items`, which probes every member on a cloned sequence before applying their common clamped delta. Filtering out locked members first can move picture without its linked sound, or sound without picture, while retaining the link identifier.

Turning off `sequence.linkedSelection` or explicitly unlinking clips still permits an intentional independent slide on an unlocked target. A directly locked target still rejects. This correction is scoped to slide edits; it does not change other editing commands, schemas or source-time calculations.

Generated regressions live in `crates/engine/src/locked_clip_edits_tests.rs` under the `linked_slide_` prefix:

- Either/both locked tracks, either named target, both directions and reversed neighbors: rejected with exact project/history preservation.
- Existing keyboard slide commands: same locked-track rejection.
- Unlocking: both partners move together with valid sequence state, common source limits and exact undo/redo restoration.
- Linked selection off and explicitly unlinked clips: deliberate independent behavior retained.
- Missing clip: error without mutation or history entry.

Run `cargo test --release -p filmcraft-engine --lib linked_slide_`, then the complete contributor gates. Offline generated fixtures exercise state/history only; real-media picture/audio validation is separate. Do not commit private recordings, derived pictures or production screenshots to accompany these tests.

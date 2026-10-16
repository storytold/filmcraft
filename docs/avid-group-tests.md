# Avid group-clip import: next fixture

## What is established

The private Phase 15–16 sample contains one group, `A + B + C sources`, grouped by in-points.
The user identified A as video with dual-mono audio, B as video only, and C as stereo audio only.
Each group output track has its own SLCT selector. Its alternatives reference tracks of an
internal composition (usage 5). A video selector's second alternative belongs to B, while an
audio selector's second alternative belongs to C. Alternative position is not a global angle ID.

The native normalized graph now preserves those alternatives, their source offsets and raw
selection fields. Selection and audio-follow behavior are not established. Group items remain
explicitly reported as unsupported; timeline selectors remain diagnosed placeholders.

FilmCraft already has editable multicam source sequences, but its existing audio modes always
mix audio-only cameras alongside camera audio. Mapping this group directly to that default could
play A and C together when Avid selected only one. Establish the actual saved choices before
choosing or extending FilmCraft's routing model.

## Phase 19 — Isolate saved picture and audio selections

Use a disposable test project and a copy of the existing Phase 15 or 16 bin. Reuse its group and
media. No media imports, database rebuilds, or full project copies are needed.

1. Create a short sequence from `A + B + C sources`, with picture and both audio tracks. Use
   roughly five seconds within the overlap of all sources. Choose **A picture and A audio**.
   Save and close the bin; copy its AVB into `Phase 19/00_A_picture_A_audio/`.
2. Reopen the working bin. Change only the sequence's picture choice to **B**, leaving **A audio**.
   Keep every edit boundary and the overall duration unchanged. Save and close; copy the AVB
   into `Phase 19/01_B_picture_A_audio/`.
3. Reopen it. Keep **B picture**, but switch the sequence's audio to **C**, including both channels
   of its stereo pair. Save and close; copy into `Phase 19/02_B_picture_C_audio/`.
4. Reopen it and split the existing segment at about two seconds. Set the first section to
   **A picture / A audio** and the second to **B picture / C audio**. Keep the overall duration
   unchanged. Save and close; copy into `Phase 19/03_switched_edit/`.
5. Reopen the last bin to verify those choices persisted. Include one timeline screenshot and a
   short note with the frame rate, switch timecode, and whether audio-follow-video was enabled.

Apply each change to the sequence edit itself, not just to a preview in the source monitor.
If Avid will not permit one of these combinations, report what it allows instead of adding a
workaround. That behavior is useful evidence.

## Implementation and acceptance sequence

1. Decode and preserve selector alternatives with bounded expansion, cycle detection and
   cancellation. Never infer active selection from the raw field's value alone.
2. Compare the controlled snapshots to establish saved choices. Resolve alternatives through
   unique live internal-composition identities and media-kind/track pairs, including offsets,
   fillers and supported wrappers. Reject ambiguous joins and retain unsupported evidence.
3. Build FilmCraft multicam sources with explicit camera-to-track mappings. Establish audio
   routing independently of picture selection; retain raw sync evidence without inventing a
   grouping method. Expand the native model only where its current modes cannot represent it.
4. Convert timeline uses, preserving per-edit selected sources, source starts, durations and
   channel pairing. Verify offline/relinked states, project roundtrip, undo/redo and playback.
5. Test Avid MultiGroup clips separately: disjoint spans, changing angle availability, duplicate
   source identities and timecode synchronization. The current group fixture does not prove
   these cases. Keep complex Phase 13 sequences as robustness evidence rather than a visual
   equivalence oracle.

All development remains local. Source AVBs and media are read-only; fixtures stay outside Git.

# Reading detailed undo history

`history.list` without arguments still returns exactly the existing `undo` and `redo` arrays
of labels. Clients that only display those labels do not need to change.

To inspect what actually changed, opt in:

```json
{"id":"history.list","params":{"detail":true,"limit":10}}
```

The response keeps those arrays and adds `details.undo` and `details.redo`. Both use the same
stack order as the labels: the last entry is the next Undo or Redo. A detail describes the
original forward edit even when that edit is on the redo stack.

Each entry has:

- `index`: position in its label stack, not a persistent event id.
- `label`: the undo label, shortened if necessary.
- `items`: changed project-item ids, added/removed/changed status, and old/new display names.
- `clips`: changed timeline-clip ids, their sequence ids, status, changed field names, and before/after summaries.
- `itemCount`, `clipCount`, `itemsOmitted`, `clipsOmitted`: totals and explicit truncation.
- `binsChanged`: whether the bin tree differs.

A clip summary identifies the source item, sequence and track, plus exact integer ticks for
`start`, `duration` and `sourceIn`. It includes speed, reverse, enabled state and audio routing.
Clip identity is scoped to its sequence: duplicating a sequence can retain its clip ids. A move between tracks within one sequence retains one clip identity with different track ids; a transfer between sequences is a removal and an addition. An added clip has no `before`;
a removed clip has no `after`. Effects carry their stack index, old/new ids and enabled states,
and changed parameter ids. Numeric, boolean and choice parameters expose their before/after
values. Structured or text values are summarized, and animation reports keyframe counts
without repeating potentially large keyframe arrays.

## What the details mean

Details compare the project snapshots Undo already retains. They are not a second command log.
This covers direct UI edits as well as commands, merged gestures and grouped edits. A fader
gesture has one net transition; a successful group describes its combined effect; a cancelled
gesture or failed group leaves no invented detail. A no-op edit has zero changes.

The response does not claim a command id or original parameters. Some UI changes do not pass
through `Session::execute`, and several commands can share one undo entry. Inferring an id from
a repeated label would be misleading. The changed ids and snapshot values provide the useful
part of the history without that guess.

This is an on-demand scan, not a cached log. It performs no project mutation, adds no undo step
and does not add itself to the recovery journal. Non-finite automatic effect points compare
consistently instead of making every untouched clip look changed.

## Limits and omissions

`limit` is an integer from 1 through 200, default 20. It selects the nearest entries independently
on each stack; the legacy label arrays remain complete. `undoOmitted` and `redoOmitted` count
older detail entries left out. Each entry includes at most 100 item and 100 clip changes. Each
clip includes at most 20 effect changes, with at most 20 changed parameters per effect. Audio routing lists contain at most 32 channel ids, with `sourceChannelsOmitted` for the rest. All
omissions are counted. Detail display hints are capped at 160 Unicode characters plus `...`.

History is session-local and may be pruned by its existing retention limit. Indices can change
when old entries are dropped. Full saved-project diffs remain the right tool for exhaustive
metadata, caption, transition, structured parameter or keyframe analysis. This API reports
changed item ids but does not promise a field-level diff for every project data type.

Large projects can make detailed scans expensive. Request only the nearest entries needed;
ordinary `history.list` does not perform this scan.

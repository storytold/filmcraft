# Nested sequences: Premiere reference and parity plan

What Premiere does with nested sequences, what FilmCraft does today, and the work that closes the
difference. This is the reference for the `NEST*` tasks; read it before changing nesting.

- **Premiere column:** taken from Adobe's public help pages (listed under [Sources](#sources),
  read 2026-10-06). Nobody ran Premiere for this. A cell marked **unverified** is a behaviour the
  help pages do not state; it must be checked in the latest Premiere before the task that depends
  on it starts. Where Premiere and this file disagree, Premiere wins: fix the file.
- **FilmCraft column:** read from the code on `main` at `a2f6ba8`. "Untested" means the code path
  exists but no test pins the behaviour for a nest.

## Behaviour matrix

| # | Behaviour | Premiere | FilmCraft today | Task |
|---|---|---|---|---|
| 1 | Clip ▸ Nest… | Asks for a name; the new sequence replaces the selected clips and appears in the Project panel [N]. Track layout of the new sequence, and where transitions at the edge of the selection go: **unverified**. | Replaces the selection with one linked video + audio clip on the lowest tracks used. The new sequence has the parent's track count. **Transitions between the nested clips are dropped; track names and channel layouts are not copied.** The name dialog is in PR #78. | NEST2 |
| 2 | Make Subsequence | Creates a sequence from the selection without replacing it (community answers, not a help page). | Does this, keeping transitions, links, track names and channels (`sequence_tools.rs` `make_subsequence`). | none |
| 3 | Does Nest / Make Subsequence open the new sequence? | **Unverified.** | No. | NEST0 |
| 4 | A sequence inside itself | Not allowed; edit it in as individual clips instead (a training course, not a help page). The message Premiere shows: **unverified**. | **Not prevented.** The picture stops at 8 levels and draws nothing. The sound has no limit: `nested_audio` → `mix_graph` → `nested_audio` recurses until the stack overflows. | NEST1 |
| 5 | Nest toggle ("Insert and overwrite sequences as nests or individual clips") | On: a sequence edits in as one nest. Off: its individual clips are added [N]. Also offered as *Nest Source Sequence* in the source indicator's context menu; off keeps the clips on their tracks with edit points and transitions [S]. | The button is drawn always on and does nothing; every sequence edits in as a nest. | NEST4 |
| 6 | Sequence as a source | A sequence can be loaded in the Source Monitor and dragged from the Project panel or Source Monitor [N]. Source indicators show all its tracks, including several video tracks, and you choose which to edit in [S]. Whether a drag from the Project panel uses the sequence's In/Out: **unverified**. | Source Monitor accepts a sequence and honours its In/Out. `timeline.place` (drag and drop) ignores a sequence's In/Out. One video and one audio source indicator only. Untested for nests. | NEST5 |
| 7 | Inner sequence gets longer | Existing nest clips keep their length; trim them out to reveal the new material [M]. | The trim limit is the inner sequence's current length (`media_duration`), so this should hold. Untested. | NEST3 |
| 8 | Inner sequence gets shorter | The nest shows black video or silent audio, which you can trim off [M]. Hatching on the clip: tutorials only. | Renders empty past the contents. No hatching is drawn. Untested. | NEST3 |
| 9 | Different frame size, timebase, pixel aspect ratio | Allowed; the nest is still a single linked video or audio clip [M]. Default scaling and Scale/Set to Frame Size on a nest: **unverified**. | Allowed. Scale to Frame Size exists; Set to Frame Size does not. Untested for mismatched nests. | NEST9 |
| 10 | Mono / 5.1 nests in a stereo sequence | **Unverified.** | The nest's mix is converted to the clip's layout (`nested_audio`). Untested. | NEST9 |
| 11 | Captions inside a nest | Only the active caption track shows when the sequence is nested (community answer). | Never shown: nests render with `captions: false`. | NEST10 |
| 12 | Open a nest | Double-click the nest clip: its source becomes the active sequence [R]. | Double-click is in PR #78. | none |
| 13 | Reveal Nested Sequence | Opens the source sequence with the playhead on the matching frame. Help page: Ctrl+Shift+F (Windows), Shift+T (macOS) [R]. | Same behaviour (`keyboard.rs` `reveal_nested`). The Premiere preset binds Cmd+Alt+F, which came from the 26.5 keyboard dump; **the two sources disagree, verify.** | NEST0 |
| 14 | Match Frame, Reveal in Project on a nest | **Unverified** in detail. | Match Frame loads the inner sequence in the Source Monitor at the matching time; untested. No Reveal in Project command. | NEST5, NEST6 |
| 15 | Effects, speed, time remapping on a nest | A nest is moved, trimmed and given effects like any clip [N]. | Same code path as other clips. Untested for nests. | NEST10 |
| 16 | Changes inside update every instance | Yes [N][R]. | Yes (nests reference the sequence). Cache invalidation of the outer sequence's thumbnails, waveforms and previews is untested. | NEST3 |
| 17 | Timeline tabs | One tab per open sequence; double-click a sequence in the Project panel to open it in a new tab; a tab can be dragged to another dock area to make a second Timeline [T]. Tab context menu and reordering: **unverified**. | One tab per open sequence is in PR #78. One Timeline panel only. No reordering, no tab menu. | NEST13 |
| 18 | View state per sequence (zoom, scroll, track heights) | **Unverified.** | Playhead and track targeting are per sequence; zoom, scroll and track heights are shared. | NEST12 |
| 19 | Reopening a project | A forum report says only the last active sequence reopens; a feature request refers to a "restore open sequences when opening project" option. **Unverified.** | Opens the first sequence only. The `restore_open_sequences` setting exists and nothing reads it. | NEST14 |
| 20 | Render and Replace | **Nested sequences cannot be rendered and replaced**, nor can adjustment layers or synthetics [RR]. | No Render and Replace command at all. | out of scope here |
| 21 | AAF / OMF / EDL / FCP XML export of nests | **Unverified** for every format. | FCP7 XML, FCPXML and OTIO carry nests. AAF and OMF leave them as gaps with a warning. EDL writes each nest as one source. | NEST15 |
| 22 | Speed of nests | n/a | Only multi-camera nests have a GPU path; other nests render on the CPU. The audio half of a nest clip has no waveform. | NEST11 |

## What the research changed

- **Render and Replace is not part of nested parity.** Premiere does not offer it for nests, so the
  earlier idea of using it for AAF export of nests is gone. It stays a missing feature for ordinary
  clips, tracked outside this plan.
- **The nest toggle has a second entry point**, *Nest Source Sequence* in the source indicator's
  context menu, and with it off Premiere keeps the source sequence's tracks. That needs source
  indicators for every source track, which FilmCraft does not have yet.
- **Premiere allows several Timeline panels.** FilmCraft has one. Added to NEST13 as an open
  question rather than a task.

## Tasks

One task id per commit. Sizes: S under a day, M one to three days, L a week or more.

### Phase 0: reference
- **NEST0 (S, needs a person with Premiere).** Check every **unverified** cell above in the latest
  Premiere and update this file.

### Phase 1: safety and correctness
- **NEST1 (S–M). A sequence can never contain itself.** Refuse any edit that would put a sequence
  inside itself, directly or through another nest, and limit the depth of nested audio mixing like
  the picture's. Crash class: do it first, starting with the regression test.
- **NEST2 (S). Nest… keeps what was selected.** Transitions between the nested clips, track names
  and channel layouts, by sharing the copy code with Make Subsequence.
- **NEST3 (M). A nest follows its contents.** Tests for rows 7, 8 and 16; hatching past the end of
  the contents; invalidate the outer sequence's caches when the inner one changes.

### Phase 2: editing
- **NEST4 (M).** The nest toggle, with the individual-clips path.
- **NEST5 (S–M).** Sequence In/Out on drag and drop; tests for Source Monitor edits, Match Frame
  and time mapping with speed, reverse and frame holds.
- **NEST6 (S).** Reveal in Project.
- **NEST8 (S).** Multi-Camera submenu in the clip context menu.

### Phase 3: rendering fidelity
- **NEST9 (M).** Mismatched frame size, rate, pixel aspect, colour space and audio layout; Set to
  Frame Size.
- **NEST10 (M).** Effects, masks, blend modes, speed, time remapping and frame holds on a nest;
  captions inside a nest.
- **NEST11 (M–L).** GPU frame plan through nests, a cache of rendered nest frames, waveforms on
  nest clips.

### Phase 4: Timeline panel
- **NEST12 (S–M).** Zoom and scroll per sequence.
- **NEST13 (M).** Tab reordering, tab context menu, close on inactive tabs, overflow list. Open
  question: a second Timeline panel.
- **NEST14 (S–M).** Save and restore open tabs with the project.

### Phase 5: interchange
- **NEST15 (M).** Match Premiere for AAF, OMF and EDL once row 21 is verified; round-trip tests for
  nests in FCPXML and OTIO.

(NEST7 was Render and Replace; see above.)

## Sources

Adobe help pages, found by web search on 2026-10-06 and quoted from the search summaries (the
pages refuse automated fetches). Rows without a letter cite forum or tutorial material and are
weaker.

- [N] [Nest sequences in Premiere](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/nest-a-sequence-in-another-sequence.html)
- [M] [Manage nested sequences in Premiere](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/about-nested-sequences.html)
- [R] [Reveal and edit clips in nested sequences in Premiere](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/reveal-a-clip-in-a-nested-source-sequence.html)
- [S] [Edit from sequences loaded into Source Monitor](https://helpx.adobe.com/premiere-pro/using/edit-sequences-loaded-source-monitor.html)
- [T] [Navigate sequences in the timeline](https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/navigate-sequences-in-the-timeline.html)
- [RR] [Render and replace media in a sequence](https://helpx.adobe.com/premiere/desktop/render-and-export/render-sequences-for-playback/render-and-replace-media-in-a-sequence.html)

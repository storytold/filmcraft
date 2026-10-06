# Nested sequences: Premiere reference and parity plan

What Premiere does with nested sequences, what FilmCraft does today, and the work that closes the
difference. This is the reference for the `NEST*` tasks; read it before changing nesting.

- **Premiere column.** Cells marked **✔** were observed in Premiere Pro 26.5.2 on macOS on
  2026-10-06, by driving the app and looking at the result (test project: three clips on V1/A1;
  screenshots in the maintainer-local `plan/premiere/nested/`). Cells with a letter come from
  Adobe's public help pages (see [Sources](#sources)) and were not checked in the app. A cell marked
  **unverified** is neither. Where Premiere and this file disagree, Premiere wins: fix the file.
- **FilmCraft column:** read from the code on `main` at `a2f6ba8`. "Untested" means the code path
  exists but no test pins the behaviour for a nest.

## Behaviour matrix

| # | Behaviour | Premiere | FilmCraft today | Task |
|---|---|---|---|---|
| 1 | Clip ▸ Nest… | ✔ Dialog "Nested Sequence Name": one Name field, preselected, Cancel / OK. The new sequence replaces the selected clips, appears in the Project panel and is selected there; it is not opened, and nothing is left selected in the timeline. The default name counts on (after "01" was undone the next was "02"). No default shortcut. | Replaces the selection and leaves the nest clips selected. Default name is always "Nested Sequence 01" (numbered, with the dialog, in PR #78). | NEST2 |
| 1a | Nest…: which track the nest goes on | ✔ The lowest selected video track when it is free across the nest's span; when another clip is in the way there, the next free track above (seen: V1 blocked → V2). | Always the lowest selected track, **even when an unselected clip there overlaps the nest's span** (the clips then overlap on one track). | NEST2 |
| 1b | Nest…: clips on several tracks | ✔ Video on one track and audio on one track: one linked video + audio nest. Video on two or more tracks (three cases): the nest is **video only**; the selected audio clips stay where they are in the parent and are also copied into the nest. Video on one track with audio on several: **unverified**. | Always a linked video + audio nest; the selected audio is removed from the parent. | NEST2 |
| 1c | Nest…: tracks of the new sequence | ✔ Video: V1 up to the highest selected track, positions kept (V1+V3 → V1, empty V2, V3; V2+V3 → empty V1, V2, V3; V1 → V1 only). Audio seen: A1 → one track; A1+A3 → two tracks, clips on A1 and A2; A3 alone → two tracks, clip on A2. The audio rule is **unverified**. | The parent's track count, with default names and stereo layouts. | NEST2 |
| 1d | Nest…: transitions | ✔ Transitions whose clips are all nested go into the nest. A transition shared with a clip outside the selection is removed from both. One-sided transitions on outside clips stay. | **All transitions between the nested clips are dropped.** | NEST2 |
| 2 | Make Subsequence | ✔ No dialog. Creates `<sequence>_Sub_01` from the selection and leaves the selection in place. The new sequence is selected in the Project panel and loaded in the Source Monitor, not opened in the Timeline; its start timecode is the selection's start. Shift+U. | Creates `<sequence>_Sub_NN`, keeping transitions, links, track names and channels. Does not load it in the Source Monitor. | NEST2 |
| 3 | Does Nest / Make Subsequence open the new sequence in the Timeline? | ✔ No, neither. | No. | none |
| 4 | A sequence inside itself | ✔ Dragging a sequence from the Project panel into its own timeline adds nothing and shows no message. The same when it would close a loop through another sequence (into a sequence nested in it; a subsequence that holds the target). | **Not prevented.** The picture stops at 8 levels and draws nothing. The sound has no limit: `nested_audio` → `mix_graph` → `nested_audio` recurses until the stack overflows. | NEST1 |
| 5 | Nest toggle ("Insert and overwrite sequences as nests or individual clips") | ✔ On: a sequence drops in as one linked nest. Off: its clips drop in with their transitions; the source's tracks that hold clips map to consecutive tracks from the drop track (source V1, V3 → V1, V2), each shown as a source indicator, and missing tracks are added. Also offered as *Nest Source Sequence* in the source indicator's context menu [S]. | The button is drawn always on and does nothing; every sequence edits in as a nest. | NEST4 |
| 6 | Sequence as a source | ✔ Dragging a sequence from the Project panel ignores In/Out marks set in its timeline: the whole sequence comes in. Editing from the Source Monitor with In/Out: **unverified** [S]. | `timeline.place` (drag and drop) ignores a sequence's In/Out, like Premiere. The Source Monitor path honours them. One video and one audio source indicator only. Untested for nests. | NEST5 |
| 7 | Inner sequence gets longer | Existing nest clips keep their length; trim them out to reveal the new material [M]. Not checked in the app. | The trim limit is the inner sequence's current length (`media_duration`), so this should hold. Untested. | NEST3 |
| 8 | Inner sequence gets shorter | ✔ The nest keeps its length; the part past the contents is hatched on the video and the audio clip (below the name row). Whether it can then be trimmed out further: **unverified**. | Renders empty past the contents. No hatching is drawn. Untested. | NEST3 |
| 9 | Different frame size, timebase, pixel aspect ratio | Allowed; the nest is still a single linked video or audio clip [M]. Default scaling and Scale/Set to Frame Size on a nest: **unverified**. | Allowed. Scale to Frame Size exists; Set to Frame Size does not. Untested for mismatched nests. | NEST9 |
| 10 | Mono / 5.1 nests in a stereo sequence | **Unverified.** | The nest's mix is converted to the clip's layout (`nested_audio`). Untested. | NEST9 |
| 11 | Captions inside a nest | Only the active caption track shows when the sequence is nested (community answer). **Unverified.** | Never shown: nests render with `captions: false`. | NEST10 |
| 12 | Open a nest | ✔ Double-click the nest clip: it opens in a new Timeline tab and becomes the active sequence. Undoing the Nest closes that tab. | Double-click is in PR #78. | none |
| 13 | Reveal Nested Sequence | ✔ ⌥⌘F in the default keyboard preset; it is in no menu. (The help page's Shift+T is out of date.) Opens the source sequence with the playhead on the matching frame [R]. | Same shortcut in the Premiere preset and the same behaviour (`keyboard.rs` `reveal_nested`). | none |
| 14 | Match Frame, Reveal in Project on a nest | **Unverified** in detail. | Match Frame loads the inner sequence in the Source Monitor at the matching time; untested. No Reveal in Project command. | NEST5, NEST6 |
| 15 | Effects, speed, time remapping on a nest | A nest is moved, trimmed and given effects like any clip [N]. | Same code path as other clips. Untested for nests. | NEST10 |
| 16 | Changes inside update every instance | Yes [N][R]. | Yes (nests reference the sequence). Cache invalidation of the outer sequence's thumbnails, waveforms and previews is untested. | NEST3 |
| 17 | Timeline tabs | ✔ One tab per open sequence, with the sequence's label colour; × on the active tab only. Right-clicking a tab gives the panel menu: Close Panel, Undock Panel, Close Other Panels in Group, Close Other Timeline Panels, Panel Group Settings ▸, display options, Reveal Sequence in Project, Label ▸. A tab can be dragged to another dock area to make a second Timeline [T]. Reordering by drag: **unverified**. | One tab per open sequence is in PR #78 (its menu says "Close Sequence"). One Timeline panel only. No reordering. | NEST13 |
| 18 | View state per sequence | ✔ Zoom is per sequence and is saved with the project. Scroll and track heights: **unverified**. | Playhead and track targeting are per sequence; zoom, scroll and track heights are shared. | NEST12 |
| 19 | Reopening a project | ✔ Every open sequence tab comes back, in the same order, with the same one active. | Opens the first sequence only. The `restore_open_sequences` setting exists and nothing reads it. | NEST14 |
| 20 | Render and Replace | **Nested sequences cannot be rendered and replaced**, nor can adjustment layers or synthetics [RR]. | No Render and Replace command at all. | out of scope here |
| 21 | AAF / OMF / EDL / FCP XML export of nests | **Unverified** for every format. | FCP7 XML, FCPXML and OTIO carry nests. AAF and OMF leave them as gaps with a warning. EDL writes each nest as one source. | NEST15 |
| 22 | Speed of nests | n/a | Only multi-camera nests have a GPU path; other nests render on the CPU. The audio half of a nest clip has no waveform. | NEST11 |

## What the research changed

From the app (26.5.2):

- **Nest… does more than replace clips with one clip** (rows 1a–1c): the nest moves up a track when
  its own track is taken, and a selection on several video tracks gives a video-only nest with the
  audio left in the parent.
- **A sequence dropped into itself is refused silently** (row 4), also through other sequences.
- **Dragging a sequence ignores its In/Out** (row 6), so that is not a gap.
- **The Reveal Nested Sequence shortcut in our Premiere preset is right**; the help page is not.
- **Tabs, zoom per sequence and reopening** (rows 17–19) are confirmed as targets.

From the help pages:

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
- **NEST0 (mostly done).** Rows marked ✔ were checked in Premiere 26.5.2. Still to check: the
  cells marked **unverified** (rows 1b, 1c, 6, 8–11, 14, 17, 18, 21) and the help-page rows 7, 15,
  16.

### Phase 1: safety and correctness
- **NEST1 (S–M). A sequence can never contain itself.** Refuse any edit that would put a sequence
  inside itself, directly or through another nest, and limit the depth of nested audio mixing like
  the picture's. Crash class: do it first, starting with the regression test.
- **NEST2 (M). Nest… and Make Subsequence like Premiere's** (rows 1–2): transitions go into the
  nest; the nest moves to a free track instead of overlapping; a selection on several video tracks
  gives a video-only nest and leaves the audio; the new sequence has video tracks up to the highest
  one used; nothing is selected afterwards and the new sequence is selected in the Project panel;
  Make Subsequence loads its result in the Source Monitor.
- **NEST3 (M). A nest follows its contents.** Tests for rows 7, 8 and 16; hatching past the end of
  the contents; invalidate the outer sequence's caches when the inner one changes.

### Phase 2: editing
- **NEST4 (M).** The nest toggle, with the individual-clips path as in row 5 (transitions kept,
  source tracks mapped to consecutive tracks, missing tracks added).
- **NEST5 (S–M).** Tests for Source Monitor edits of a sequence, Match Frame and time mapping
  with speed, reverse and frame holds; a source indicator per source track.
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
- **NEST13 (M).** The tab's right-click menu as in row 17 (Close Panel, Close Other Timeline
  Panels, Reveal Sequence in Project, Label), overflow list, reordering if Premiere has it. Open
  question: a second Timeline panel.
- **NEST14 (S–M).** Save and restore open tabs with the project.

### Phase 5: interchange
- **NEST15 (M).** Match Premiere for AAF, OMF and EDL once row 21 is verified; round-trip tests for
  nests in FCPXML and OTIO.

(NEST7 was Render and Replace; see above.)

## Sources

Rows marked ✔ are first-hand observations of Premiere Pro 26.5.2. The letters are Adobe help
pages, found by web search on 2026-10-06 and quoted from the search summaries (the pages refuse
automated fetches). They can be out of date: row 13 was.

- [N] [Nest sequences in Premiere](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/nest-a-sequence-in-another-sequence.html)
- [M] [Manage nested sequences in Premiere](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/about-nested-sequences.html)
- [R] [Reveal and edit clips in nested sequences in Premiere](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/reveal-a-clip-in-a-nested-source-sequence.html)
- [S] [Edit from sequences loaded into Source Monitor](https://helpx.adobe.com/premiere-pro/using/edit-sequences-loaded-source-monitor.html)
- [T] [Navigate sequences in the timeline](https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/navigate-sequences-in-the-timeline.html)
- [RR] [Render and replace media in a sequence](https://helpx.adobe.com/premiere/desktop/render-and-export/render-sequences-for-playback/render-and-replace-media-in-a-sequence.html)

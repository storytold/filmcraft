# Captions

FilmCraft keeps captions on **caption tracks** in the sequence, shown in their own area above the
video tracks (as Premiere does). Caption tracks are saved in the `.fcproj` (project format
version 2; version 1 projects open with no caption tracks).

## Model

- **Caption track** (`CaptionTrack`, `crates/project/src/caption.rs`): name, format (`Subtitle`,
  `CEA-608`, `CEA-708`, `Teletext`), language, output eye (shown in the Program monitor and
  burned in on export), lock, sync lock, and a **track style**:
  font family and font style (`fontStyle`, default `SemiBold`, resolved like graphic text: the
  nearest weight when the family has no face of that name), font size (pixels at 1080 lines, scaled to the frame), text colour, background box and its colour,
  outline width and colour, alignment (left/centre/right), position (top/middle/bottom) and
  margin (a fraction of the frame height, default 0.08; `captions.setStyle` clamps it to 0–0.45),
  line spacing.
- **Caption**: in/out in exact `Tick`s, text (lines separated by `\n`; `<i>`, `<b>`, `<u>` kept
  as written), optional speaker, and the WebVTT cue id and cue settings, kept for round trips.
  Captions on a track never overlap.

## Files

| Format | Import | Export | Notes |
|---|---|---|---|
| SubRip `.srt` | ✓ | ✓ | Forgiving reader (BOM, UTF-16, Windows-1252, CRLF, missing indexes, `,`/`.` ms, missing hours). |
| WebVTT `.vtt` | ✓ | ✓ | Keeps cue ids, cue settings, `<v Speaker>`, STYLE/REGION/NOTE blocks. `line:N%` and `align:` are honoured when drawing. |
| Scenarist SCC `.scc` | ✓ | ✓ | CEA-608 at 29.97 fps. Reads pop-on, roll-up and paint-on; writes pop-on with drop-frame (default) or non-drop timecode. |

`File ▸ Import` (or dropping a file) recognises caption files by extension and content and adds a
new caption track to the open sequence (a sequence is created when none is open). Cue times are
snapped to the sequence's frames. `File ▸ Export ▸ Captions…` (and **Export captions…** in the Text
panel) opens Caption Export Settings: pick the File Format (SRT, WebVTT, SCC, MCC, EBU STL, TTML or
DFXP; the last choice is offered next time), then the file name in a save panel that offers only
that format. A name typed with another caption format's extension (`subs.srt` with WebVTT chosen)
is written in that format, since that is the file the save panel asked about; any other name gets
the chosen format's extension (`subs` becomes `subs.vtt`). If a file with that new name already
exists, nothing is exported, because the save panel only asked about replacing the name as typed.
For example, with WebVTT chosen, typing `keep` when `keep.vtt` already exists leaves `keep.vtt`
untouched and the status line says so; choosing `keep.vtt` itself in the save panel, which then asks
before replacing it, replaces it. Given a `path` (agents, the CLI), `captions.export` takes the
format from `format` or from the file extension.

Details of each format and the SCC encoder are in [`crates/captions/README.md`](../crates/captions/README.md).

## Editing

| Command | Menu / shortcut | Params |
|---|---|---|
| `captions.newTrack` | Sequence ▸ Captions ▸ Add New Caption Track… (⌥⌘A) | `format`, `name`, `language` |
| `captions.add` | Sequence ▸ Captions ▸ Add Caption at Playhead (⌥⌘C) | `track`, `text`, `time`, `durationSeconds` (3) |
| `captions.next` / `captions.previous` | Sequence ▸ Captions ▸ Go to Next/Previous Caption Segment (⌥⌘↓/↑) | |
| `captions.showAll` / `captions.hideAll` | Sequence ▸ Captions | |
| `captions.split` | Text panel ✂; Razor tool on a caption (⇧: every caption track) | `caption`, `time` (default: every caption under the playhead) |
| `captions.merge` | Text panel | `captions` (consecutive, one track) |
| `captions.setText` | Text panel text field | `caption`, `text`, `speaker` |
| `captions.setTimes` | Text panel in/out fields | `caption`, `start…`/`end…` (`Time` ticks, `Frame`, `Seconds`, `Timecode`) |
| `captions.trim` | drag a caption edge in the timeline | `caption`, `edge` (`in`/`out`), `delta` or `deltaFrames` |
| `captions.move` | drag a caption in the timeline | `captions`, `delta` or `deltaFrames` |
| `captions.duplicate` | ⌥-drag a caption in the timeline | `captions`, `delta` or `deltaFrames` — copies on the same tracks, overwriting what is there |
| `captions.delete` | Edit ▸ Clear / Ripple Delete with captions selected | `captions`, `ripple` |
| `edit.copy` / `edit.cut` / `edit.paste` / `edit.pasteInsert` | ⌘C / ⌘X / ⌘V / ⇧⌘V with only captions selected | captions paste at the playhead on the tracks they came from, overwriting (Paste Insert pushes later captions on); Paste places whichever of clips or captions was copied last |
| `captions.select`, `captions.goTo` | click / number button | `captions`, `add` / `caption` |
| `captions.setTrack` | caption track header (eye, lock) | `track`, `name`, `format`, `language`, `enabled`, `locked`, `syncLock` |
| `captions.setStyle` | Text panel style strip | `track`, `font`, `fontStyle`, `size`, `color`, `background`, `backgroundColor`, `outline`, `outlineColor`, `align`, `anchor`, `margin`, `lineSpacing`, `reset` |
| `captions.deleteTrack` | | `track` |
| `captions.import` / `captions.export` | File ▸ Export ▸ Captions… | `path`, `format`, `track`, `dropFrame` |
| `captions.list` | (query) | `track` — tracks, styles and captions with timecodes (a `track` that names no caption track is an error) |

Tracks are addressed by id or `"C1"`, `"C2"`… (top first). Edits are limited by neighbouring
captions and a one-frame minimum. Sync-locked caption tracks follow insert and extract edits on
the media tracks (a caption across an insert point is split). `Add Edit to All Tracks` (⇧⌘K)
also splits captions.

## Timeline and Text panel

- Caption rows: header with `C1…` badge, lock, output eye, name and format; caption blocks in
  violet showing their text (Arabic and Hebrew in reading order from the block's right edge). Click selects (⌘/⇧ adds), drag moves (the whole selection; ⌥-drag
  copies), edge drag trims, the Razor tool cuts, double-click
  opens the Text panel. Automation ids: `timeline.caption.<id>`,
  `timeline.captionTrack.C1[.locked|.enabled]`.
- **Text panel ▸ Captions**: search, track picker (also creates tracks of each format), add /
  split / merge / delete / export buttons, the list of segments with editable in/out timecodes
  and text (committed when the field loses focus; Arabic and Hebrew show in reading order and are
  edited in logical order while the field has focus), and the track style strip (font, font style, size, colour, box,
  alignment, position). The list keeps the caption under the playhead in view (centred as the
  playhead reaches it, not while a field is being edited). Automation ids: `text.tab.Captions`, `text.captions.add|split|merge|delete|export|search|track`,
  `text.captions.<id>.in|out|text|goto|row`, `text.captions.style.*`.

## Burn-in

The Program monitor shows the visible caption tracks. On export, `file.exportMedia`
`{"burnCaptions": true}` (Export mode: **Burn Captions Into Video**) draws them into every frame.
Captions are set in the track's font and font style (Inter SemiBold by default, OFL, bundled) by the text engine (`crates/text`: shaping,
kerning, bidi); italics/bold markup is not yet drawn differently. The captions of a nested sequence
are part of the nest's picture: they are drawn wherever the nest is shown, in the Program monitor
and in every export, whether or not the outer sequence shows or burns in its own.

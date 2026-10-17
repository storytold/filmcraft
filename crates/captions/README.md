# filmcraft-captions

Caption files and caption burn-in for FilmCraft (layer L2: depends on `filmcraft-time`,
`filmcraft-color`, `filmcraft-project`, `filmcraft-text` and `roxmltree` for TTML).

The caption *model* (caption tracks in a sequence, captions, track style) lives in
`filmcraft-project` (`caption.rs`) because it is part of the `.fcproj` document. This crate reads
and writes caption files, converts them to and from caption tracks, and draws captions.

## Formats

| Format | Read | Write | Notes |
|---|---|---|---|
| SubRip `.srt` | yes | yes | BOM, UTF-16, Windows-1252 fallback, CRLF/CR, missing or wrong indexes, `,` or `.` milliseconds, 1–9 fraction digits, missing hours, SSA coordinates, cues not separated by blank lines. Inline tags are kept as text. |
| WebVTT `.vtt` | yes | yes | Cue identifiers, cue settings (verbatim), a leading `<v Speaker>` voice span (as the caption's speaker) and STYLE / REGION / NOTE blocks are kept. A missing `WEBVTT` signature is tolerated. |
| Scenarist SCC `.scc` | yes | yes | CEA-608 at 29.97 fps, drop-frame or not. Reads pop-on, roll-up and paint-on (channel 1); writes pop-on. |
| MacCaption MCC `.mcc` | yes | yes | One SMPTE ST 334-2 caption distribution packet (CDP) per frame in MacCaption's hex text with letter abbreviations. Writes 29.97 fps (`30DF` / `30`) with CEA-608 field 1 (the SCC schedule) **and** CEA-708 service 1 (two-window pop-on emulation). Reads CEA-608 field 1 when present, else CEA-708 service 1; time code rates 30DF, 30, 60DF, 60, 25, 24, 50. |
| EBU STL `.stl` | yes | yes | EBU Tech 3264-E: GSI block + 128-byte TTI blocks, `STL25.01` (25 fps) or `STL30.01` (read/written as 29.97 fps, non-drop labels), ISO/IEC 6937 Latin text (accents as diacritic + letter), italics / underline codes as `<i>` / `<u>`, extension blocks for long subtitles, start-of-programme (TCP) subtracted. |
| TTML `.ttml` (IMSC1) | yes | yes | Writes the IMSC1 Text profile (TTML2 namespaces): `<p>` per cue, `<br/>`, styled `<span>`s for `<i>` / `<b>` / `<u>`, `ttm:agent` speakers, exact frame times (`123f`) at the sequence rate. Reads any TTML (TTML1, TTML2, the 2006 `ttaf1` drafts): clock / frame / offset / tick times, `dur`, nested time containers (`body` / `div` begins offset and ends clip their contents; `seq` containers are read as `par` with a warning), timed spans, referenced styles. |
| DFXP `.dfxp` | yes | yes | TTML1 with the DFXP presentation profile; clock times in milliseconds (frame-exact after `snap_to_frames`). |

All cue times are exact `Tick`s. Milliseconds convert exactly (1 ms = 254 016 000 ticks); SCC
frames map through `FrameRate::FPS_29_97`. `Document::snap_to_frames` snaps cues to a sequence's
frame grid (the engine does this on import) and removes overlaps.

### TTML speaker metadata

TTML and DFXP exports define each distinct speaker label once in the document head and refer to
it with a stable, unique `ttm:agent` ID. Names are XML-escaped full-name text with preserved
whitespace; punctuation, Unicode and literal underscores survive a round trip. Agent type is
`other` because a caption's speaker label does not say whether it denotes a person or a group.

On import, agent definitions directly under `head` or `head/metadata` resolve paragraph and timed
span speakers. Speaker metadata uses the standard TTML namespace or the April and October 2006
TTaf1 draft namespaces. Agent IDs use `xml:id`, except in the April draft, which uses unqualified
`id`. Agent definitions from unrelated application metadata are ignored. Recognized `ttm:agent`
attributes take precedence over the legacy local-name `agent` fallback. A full name is preferred,
otherwise the first nonempty name (or an explicitly empty name) is used. Multiple agent references become one display label, joined with `, ` in
reference order; the project does not retain the individual agent identities. Older FilmCraft
files without agent definitions remain readable through the previous underscore-to-space
fallback. Other agent metadata, including actor relationships, is not retained.

This mapping follows [W3C TTML1, third edition (2018), §§12.1.5–12.2.1](https://www.w3.org/TR/2018/REC-ttml1-20181108/#metadata-vocabulary-agent).
The [April 2006 TTaf1 draft, §§7.2.1 and 12.1.5–12.2.1](https://www.w3.org/TR/2006/WD-ttaf1-dfxp-20060427/)
defines its earlier metadata namespace and agent ID form.

**Invariant — TTML root text styles:** cue text preserves resolved italic/bold/underline styles
from the body and from paragraphs or timed spans emitted as cues, with balanced tags at descendant
overrides. Referenced styles retain explicit normal/noUnderline resets and leave unspecified
properties inherited; named-style chaining is not resolved. Collapsed runs keep the first surviving
space's source style. Spaces adjacent to line breaks and leading/trailing line breaks are discarded;
interior line breaks are preserved. Styling alone must not create an empty cue.
Styling attributes use the modern or 2006 draft styling namespaces, with unqualified attributes
as a compatibility fallback. Precedence is modern, October `#style`, October `#styling`, April
`#style`, April `#styling`, then unqualified, independent of XML attribute order. Styling
attributes in foreign namespaces and foreign style definitions cannot override these styles. Genuine XML style IDs take
precedence over unqualified IDs. This filtering covers styling; content traversal, cue IDs and
timing retain the reader's permissive local-name matching.

**Invariant — literal comparisons versus caption markup:** display text extraction and TTML
export share `filmcraft_project::caption::is_markup_tag`. ASCII tag names, declarations and WebVTT inline
timestamps are markup. Legacy padding around `i`/`b`/`u` style controls remains accepted;
non-tag-shaped comparisons such as `1 < 2` and `2 < 3 > 1` remain text,
including inside generated style tags. STL retains its existing tag recognition and literal
declaration/timestamp spelling. Caption spelling and raw-ampersand roundtrip property coverage
remain unchanged. Alphabetic tag-like, padded style-control and entity-looking literal text
retain their existing markup ambiguity; this is not a general escape/serialization migration.
TTML collapses only XML whitespace (space, tab, CR, LF), preserving nonbreaking and other Unicode
spacing characters. SSA override blocks and unsupported `xml:space="preserve"` handling remain
outside this guarantee. Inline timestamp syntax follows
[WebVTT, 20 May 2026 CR Draft](https://www.w3.org/TR/2026/CRD-webvtt1-20260520/#webvtt-timestamp).

### SCC writer

Each caption is sent as a pop-on load (RCL, ENM, then per row a preamble address code and tab
offset for centring, then text; control codes doubled) into non-displayed memory, followed by End
Of Caption exactly on the caption's first frame. The load is placed in the latest run of free
frames before the caption, so back-to-back captions load while the previous one is on screen. An
Erase Displayed Memory clears the caption at its out point unless the next caption replaces it.
Text is wrapped at 32 columns, bottom-aligned (last row 15) and centred. Characters come from the
608 basic, special and extended sets (extended characters are preceded by a basic fallback
character, as the standard requires); others are dropped. If there is no room to load a caption
before its in point (for example a caption at 00:00:00:00), it appears as soon as it is loaded.

## Burn-in

`burn::render_caption` lays out a caption with the track style (font size relative to a 1080-line
frame, colour, background box, outline, alignment, top/middle/bottom anchor and margin; WebVTT
`line:N%` and `align:` settings override them), wraps lines to 90% of the frame width and
rasterises it to a small premultiplied linear-light RGBA overlay. `filmcraft-render` composites the
overlays of visible caption tracks over the finished frame (Program monitor and export burn-in).

Text is set in **Inter SemiBold** (`assets/fonts/Inter-SemiBold.ttf`, SIL OFL 1.1, attributed in
`ATTRIBUTION.md`) by the `filmcraft-text` engine: shaped with kerning and ligatures, bidi-ordered,
and drawn from its sub-pixel positioned glyph cache.

Specifications used: SMPTE ST 334-2 (CDP structure), CEA-608 / CEA-708 (public descriptions of
the caption commands), the MacCaption MCC format description (line syntax and abbreviations),
EBU Tech 3264-E (1991), ISO/IEC 6937, W3C TTML1 (2nd ed.), TTML2 and IMSC 1.1. No other
implementation's code was consulted.

## Tests

- Unit tests per module (timestamps, decoding, sloppy SRT, WebVTT blocks and voices, CEA-608
  tables, PAC round trips, known SCC pop-on and roll-up streams, layout).
- `tests/roundtrip.rs`: property tests — SRT (also with BOM + CRLF), WebVTT (ids, speakers,
  settings, STYLE blocks), SCC and MCC (drop-frame and non-drop, gaps and back-to-back captions;
  MCC also through CEA-708 alone), EBU STL (25 and 29.97 fps, accents, extension blocks), TTML
  (23.976 / 25 / 29.97 / 59.94, ids, speakers) round-trip frame-exactly; DFXP round-trips after
  snapping to frames; readers never panic on random bytes (also a valid STL header followed by
  random blocks, and random MCC lines); SRT → VTT → SCC keeps text and frames.

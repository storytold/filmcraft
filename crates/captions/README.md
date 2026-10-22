# filmcraft-captions

Caption files and caption burn-in for FilmCraft (layer L2: depends on `filmcraft-time`,
`filmcraft-color`, `filmcraft-project`, `filmcraft-text` and `roxmltree` for TTML).

The caption *model* (caption tracks in a sequence, captions, track style) lives in
`filmcraft-project` (`caption.rs`) because it is part of the `.fcproj` document. This crate reads
and writes caption files, converts them to and from caption tracks, and draws captions.

## Caption shadows

Caption track styles persist an optional text shadow in `.fcproj` files. It is disabled by
default, including when loading older styles. The Text panel's Captions tab has a Shadow popup;
the same values are available through `captions.setStyle` and reported by `captions.list`:

```json
{"track":"C1","shadow":true,"shadowColor":"#000000","shadowOpacity":75,"shadowAngle":135,"shadowDistance":10,"shadowSize":0,"shadowBlur":40}
```

Opacity is 0–100 percent and multiplies the colour's alpha. Angle is 0–360 degrees clockwise
from up (135 casts down/right), matching graphic text. Distance, size (outward spread) and blur
are 0–1000 pixels at 1080 lines and scale with frame height. Blur uses the text engine's three
box passes with sigma = blur / 3, as graphic text does. The shadow follows glyphs and their
outline, not the background rectangle; it is composited above the box and behind the text.
Zero opacity or transparent colour draws exactly the same pixels as disabling the shadow.

Program rendering, GPU caption overlays and burned-in exports use the same `burn` renderer.
Caption sidecar formats do not preserve these FilmCraft appearance settings. Resetting a style
also resets its shadow; partial updates preserve unspecified settings, and changes are undoable.
Malformed shadow command values return an error without changing the project. Raster allocations
allow one frame of coverage, with a minimum of 4,194,304 and maximum of 33,554,432 pixels.
This retains ordinary multiline captions at 1080p, 4K and 8K. If shadow padding exceeds that
budget, rendering retries once without the shadow so the base caption remains visible.
Invalid or oversized base text/geometry is not rasterised. The original size, line-spacing
and outline calculations are retained; the renderer does not impose the command UI's style ranges.
Outline dilation only processes glyph bounds plus outline support, independent of empty shadow
padding. Allocation limits alone are not a rendering-time guarantee.

Upgrade Caption to Graphic carries every shadow setting, including colour alpha and disabled
settings, and scales distance, spread and blur to sequence-pixel units.

Synthetic tests cover colour/opacity, direction, resolution scaling, spread, blur padding,
disabled identity, multiline 8K captions, excessive-padding fallback and hostile geometry.
Engine tests cover persistence, legacy defaults, undo/redo, parameter errors and upgrade to
graphics. An export test compares burned-in PNG pixels with the Program reference at 1080p,
4K and 8K and checks that disabling burn-in removes the shadow and captions.

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

### TTML tick timing

When `ttp:tickRate` is omitted and `ttp:frameRate` is present, tick offsets use the effective
frame rate (`frameRate` times `frameRateMultiplier`) times `subFrameRate`. The rate stays
rational, so a 30-tick offset at 30 times 1000/1001 fps is exactly 1.001 seconds. An explicit
`tickRate` takes precedence; without `frameRate`, the default is one tick per second.
This follows [W3C TTML1, third edition (2018), §6.2.10](https://www.w3.org/TR/2018/REC-ttml1-20181108/#parameter-attribute-tickRate).

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

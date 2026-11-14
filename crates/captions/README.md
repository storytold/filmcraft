# filmcraft-captions

Caption files and caption burn-in for FilmCraft (layer L2: depends on `filmcraft-time`,
`filmcraft-color`, `filmcraft-project`, `filmcraft-text` and craft-libs' `craft-captions`).

The caption *model* (caption tracks in a sequence, captions, track style) lives in
`filmcraft-project` (`caption.rs`) because it is part of the `.fcproj` document. This crate
converts caption files to and from caption tracks, and draws captions.

## Formats

SubRip, WebVTT, Scenarist SCC, MacCaption MCC, EBU STL and TTML / DFXP are read and written by
[`craft-captions`](https://github.com/storytold/craft-libs/tree/main/crates/captions) in
craft-libs. This crate re-exports it. The formats, the SCC and MCC encoders, the TTML timing and
speaker rules, and their tests are documented there, and format fixes go there.

What stays here:

- `track_from_document` / `document_from_track`: a parsed document to and from a caption track
  (language, WebVTT blocks, speakers, cue ids and settings).
- `track_format`: the caption track format a file maps to (SCC: CEA-608, MCC: CEA-708, STL:
  Teletext, the rest: subtitle).
- `write_options`: write options that name FilmCraft as the creating program in MCC and EBU STL
  files.

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

`cargo test -p filmcraft-captions`: the track mapping, the program name in written files, and
burn-in. The format tests are in craft-captions.

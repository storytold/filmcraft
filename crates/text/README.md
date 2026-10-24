# filmcraft-text

FilmCraft's text engine (layer L1: depends on `filmcraft-color`, `skrifa`, `harfrust`,
`unicode-bidi` and `unicode-linebreak`). It draws graphic-clip text and shapes, caption burn-in
and the Timecode / Clip Name effects. Pure Rust; it builds for `wasm32-unknown-unknown`.

| Module | What |
|---|---|
| `fonts` | Font database. Bundled faces (always present, also on the web): Inter Regular / Medium / SemiBold / Bold / Italic, JetBrains Mono Regular, Noto Serif Regular — all SIL OFL 1.1, unmodified, in `assets/fonts/` with `.attribution` sidecars. More faces come from `FontSource`s: `DirectorySource::system()` scans the platform font folders on first need (only the `name` and `OS/2` tables are read, via `sfnt`); on wasm the scan finds nothing and hosts can call `add_font_data`. `resolve(family, style)` picks a face, synthesises bold/italic when the family lacks them, and falls back to Inter (flagged `missing`). `fallback_for` finds a face for characters the chosen face lacks: bundled faces first, then the optional craft-fonts faces (`CRAFT_FONTS`, embedded by `build.rs` when built with `CRAFT_FONTS_DIR`; Mincho for serif text, Gothic otherwise, nearest weight), then loaded system faces, then (scanning the system folders on first need) any installed face that covers the character, remembered per character; each font file is read once per process for its character ranges (only the ranges are kept). |
| `layout` | Paragraph layout: per-character font fallback → bidi levels (UAX #9) → items (runs of one face, direction and size) shaped with harfrust (OpenType kerning, ligatures, marks, complex scripts) → tracking → line breaking at UAX #14 opportunities (area text) → visual reordering per line → alignment (left / centre / right / justify), leading (added to the natural line height), baseline shift, all caps / synthesised small caps (78 %), underline. Every character boundary gets a caret stop (ligatures are split evenly), so editors can place carets, hit-test and draw selections. Layouts are cached by text + style (512 entries, LRU). |
| `layout_rich` | Per-character style runs (`StyleRun`: a byte range with its own `TextStyle`): runs split shaping items (font, size, tracking, caps, faux styles), baseline shift and underline apply per run, a line holding a larger run is pushed down from the line above (baseline distance = max(base line height, previous descent + this line's ascent + leading)), and every glyph / underline carries its style index so renderers can colour runs (`render::draw_run`). |
| `raster` | Our anti-aliased rasteriser (non-zero winding, 5 sub-scanlines, exact horizontal coverage, active-edge list), contour paths with transforms, rounded rectangles, ellipses, polygons and outline emboldening. Coverage is linear, so compositing in linear light is correct. |
| `render` | Glyph outlines (cached per face + glyph) and glyph masks (cached per size in ⅛ px, ¼-px horizontal phase, synthetic style). Axis-aligned uniform transforms use the mask cache with sub-pixel x positioning; rotated / skewed / non-uniform transforms fill the transformed outlines of the whole run in one pass. Synthetic bold grows outlines by 2.5 % of the size; synthetic italic slants by 12°. |
| `mask` | Outer / centre / inner strokes from an exact Euclidean distance transform (Felzenszwalb–Huttenlocher) with sub-pixel refinement, a three-pass box blur (≈ Gaussian) for shadows, offsets. |

## Coordinates

Pixels, y down. Point text: the origin is the alignment point on the first baseline (left edge,
centre or right edge). Area text (`ParagraphStyle::width`): the origin is the box's top-left.

## Performance

A three-line, centred, 96 px bold title at 1080p (`perf::three_line_title_is_fast`), release build
on an M-series Mac: about 1.6 ms cold (shaping, outline extraction, glyph rasterisation) and
0.15 ms warm (layout cache + glyph cache). The test fails if the warm path exceeds 40 ms.

## Not yet

OpenType
feature toggles beyond kerning/ligatures, variable-font axes, hinting, colour (emoji) glyphs, and
hyphenation. Words longer than an area-text line overflow instead of breaking mid-word.

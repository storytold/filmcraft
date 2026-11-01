use super::*;

fn st(size: f32) -> TextStyle {
    TextStyle { size, ..Default::default() }
}

#[test]
fn kerning_and_ligatures_change_advances() {
    let kern = measure("AVAVAV", &st(100.0));
    let nokern = measure("AVAVAV", &TextStyle { kerning: false, ..st(100.0) });
    assert!(kern < nokern - 5.0, "kerning tightens AV: {kern} vs {nokern}");
    // tracking adds 1/1000 em per cluster
    let tracked = measure("AVAVAV", &TextStyle { tracking: 100.0, ..st(100.0) });
    assert!((tracked - kern - 6.0 * 10.0).abs() < 0.5, "{tracked} {kern}");
}

#[test]
fn point_text_alignment() {
    let l = layout("Hello", &st(50.0), &ParagraphStyle { align: Align::Center, ..Default::default() });
    let line = &l.lines[0];
    assert!((line.x + line.width / 2.0).abs() < 0.01);
    let r = layout("Hello", &st(50.0), &ParagraphStyle { align: Align::Right, ..Default::default() });
    assert!((r.lines[0].x + r.lines[0].width).abs() < 0.01);
    assert_eq!(l.glyphs.len(), 5);
    assert!(l.bounds[1] < -30.0 && l.bounds[3] > 5.0, "{:?}", l.bounds);
}

#[test]
fn wraps_area_text_and_justifies() {
    let p = ParagraphStyle { width: Some(300.0), align: Align::Justify, ..Default::default() };
    let l = layout("the quick brown fox jumps over the lazy dog again and again", &st(40.0), &p);
    assert!(l.lines.len() >= 3, "{}", l.lines.len());
    for line in &l.lines[..l.lines.len() - 1] {
        assert!((line.width - 300.0).abs() < 0.5, "justified to the box: {}", line.width);
    }
    // the lines tile the text
    assert_eq!(l.lines[0].range.start, 0);
    for w in l.lines.windows(2) {
        assert_eq!(w[0].range.end, w[1].range.start);
        assert!(w[1].baseline > w[0].baseline);
    }
    let left = layout("the quick brown fox jumps over the lazy dog", &st(40.0), &ParagraphStyle { width: Some(300.0), ..Default::default() });
    assert!(left.lines.iter().all(|l| l.width <= 300.0 + 0.01));
}

#[test]
fn a_box_height_leaves_out_the_lines_that_do_not_fit() {
    let text = "the quick brown fox jumps over the lazy dog again and again";
    let free = layout(text, &st(40.0), &ParagraphStyle { width: Some(300.0), ..Default::default() });
    assert!(free.lines.len() >= 3 && !free.overflow);
    // room for two lines only
    let h = free.lines[1].baseline + free.lines[1].descent + 1.0;
    let boxed = layout(text, &st(40.0), &ParagraphStyle { width: Some(300.0), height: Some(h), ..Default::default() });
    assert_eq!(boxed.lines.len(), 2);
    assert!(boxed.overflow);
    assert_eq!(boxed.lines[..], free.lines[..2]);
    assert_eq!(boxed.glyphs.len(), free.lines[1].glyphs.end);
    assert_eq!(boxed.bounds, [0.0, 0.0, 300.0, h], "the box is the bounds");
    assert_eq!(boxed.text_len, text.len());
    // a box tall enough for everything hides nothing; a box too short for one line keeps the first
    let tall = layout(text, &st(40.0), &ParagraphStyle { width: Some(300.0), height: Some(5000.0), ..Default::default() });
    assert_eq!(tall.lines.len(), free.lines.len());
    assert!(!tall.overflow);
    let tiny = layout(text, &st(40.0), &ParagraphStyle { width: Some(300.0), height: Some(2.0), ..Default::default() });
    assert_eq!(tiny.lines.len(), 1);
    assert!(tiny.overflow);
    // point text has no box: the height is ignored
    let point = layout("a\nb\nc", &st(40.0), &ParagraphStyle { height: Some(2.0), ..Default::default() });
    assert_eq!(point.lines.len(), 3);
    assert!(!point.overflow);
}

#[test]
fn newlines_make_paragraphs_and_leading_adds() {
    let l = layout("one\ntwo\n\nfour", &st(30.0), &ParagraphStyle::default());
    assert_eq!(l.lines.len(), 4);
    assert_eq!(l.lines[1].range, 4..7);
    assert_eq!(l.lines[2].range, 8..8);
    let gap = l.lines[1].baseline - l.lines[0].baseline;
    let l2 = layout("one\ntwo", &st(30.0), &ParagraphStyle { leading: 10.0, ..Default::default() });
    assert!((l2.lines[1].baseline - l2.lines[0].baseline - gap - 10.0).abs() < 1e-3);
}

#[test]
fn japanese_vertical_titles_with_an_installed_font() {
    // Japanese glyphs come from the craft-fonts (built with CRAFT_FONTS_DIR) or a system font:
    // skip where neither is present. `covers_text` probes a face without keeping its file:
    // `has_char` over every installed face would load each font file (a CJK collection once
    // per face in it) and keep them all, gigabytes on a machine with Noto CJK installed.
    crate::fonts::scan_system();
    let Some(face) = crate::fonts::all_faces().into_iter().find(|f| f.covers_text("日本語縦書き")) else {
        eprintln!("SKIPPED: no Japanese font installed");
        return;
    };
    let style = TextStyle { family: face.info.family.clone(), style: face.info.style.clone(), ..st(40.0) };
    let l = layout("日本語\n縦書き", &style, &ParagraphStyle { vertical: true, ..Default::default() });
    assert!(l.vertical);
    assert_eq!(l.glyphs.len(), 6);
    assert!(l.glyphs.iter().all(|g| g.id != 0));
    assert!(l.glyphs[1].y > l.glyphs[0].y);
    assert!(l.glyphs[3].x < l.glyphs[0].x);
}

#[test]
fn japanese_vertical_punctuation_uses_alternates_at_the_top_right() {
    use skrifa::{
        MetadataProvider,
        instance::{LocationRef, Size},
    };
    fonts::scan_system();
    let face = fonts::all_faces().into_iter().find(|f| {
        ((f.info.family.contains("Hiragino") && !f.info.family.contains(" GB") && !f.info.family.contains(" TC"))
            || f.info.family.contains("Shippori")
            || f.info.family.contains("BIZ UD"))
            && "国、。「」ー".chars().all(|c| f.has_char(c))
            && (f.has_feature(b"vert") || f.has_feature(b"vrt2"))
    });
    let Some(face) = face else {
        eprintln!("SKIPPED: no Japanese font with vertical alternates available");
        return;
    };
    let style = TextStyle { family: face.info.family.clone(), style: face.info.style.clone(), ..st(48.0) };
    let text = "国、。「」ーABC";
    let horizontal = layout_uncached(text, &style, &ParagraphStyle::default());
    let vertical = layout_uncached(text, &style, &ParagraphStyle { vertical: true, ..Default::default() });
    assert_eq!(vertical.glyphs.len(), 9);
    assert_eq!(vertical.glyphs[0].id, horizontal.glyphs[0].id);
    for i in 1..6 {
        assert_ne!(vertical.glyphs[i].id, horizontal.glyphs[i].id, "{}: vertical form for glyph {i}", face.info.family);
    }
    for i in [1, 2] {
        let g = &vertical.glyphs[i];
        let line = &vertical.lines[i];
        let font = fonts::face(g.face);
        let bounds = font.font().unwrap().glyph_metrics(Size::unscaled(), LocationRef::default()).bounds(skrifa::GlyphId::new(g.id)).unwrap();
        let scale = g.size / font.units_per_em();
        let ink_x = g.x + (bounds.x_min + bounds.x_max) * scale / 2.0;
        let ink_y = g.y - (bounds.y_min + bounds.y_max) * scale / 2.0;
        let centre_x = line.x + line.width / 2.0;
        let cell_top = line.baseline - line.ascent;
        assert!(ink_x > centre_x, "{}: punctuation ink on the right ({ink_x}, {centre_x})", face.info.family);
        assert!(ink_y < cell_top + g.size / 2.0, "{}: punctuation ink at the top ({ink_y}, {cell_top})", face.info.family);
    }
    for i in 6..9 {
        assert_eq!(vertical.glyphs[i].id, horizontal.glyphs[i].id, "Latin shaping stays unchanged");
    }
    assert_eq!(layout_uncached(text, &style, &ParagraphStyle::default()), horizontal);
    eprintln!("verified vertical punctuation with {} / {}", face.info.family, face.info.style);
}

#[test]
fn vertical_text_stacks_characters_in_columns() {
    let p = ParagraphStyle { vertical: true, ..Default::default() };
    let l = layout("abc\nde", &st(40.0), &p);
    assert!(l.vertical);
    assert_eq!(l.lines.len(), 5, "one line per character");
    // first column: same centre x, increasing baselines
    let cx = |i: usize| l.lines[i].x + l.lines[i].width / 2.0;
    assert!((cx(0) - cx(2)).abs() < 1e-3 && l.lines[1].baseline > l.lines[0].baseline && l.lines[2].baseline > l.lines[1].baseline);
    // second column is to the left of the first
    assert!(cx(3) < cx(0) - 20.0, "{} {}", cx(3), cx(0));
    assert_eq!(l.lines[3].range, 4..5);
    // taller than wide for a single column
    let one = layout("abcdef", &st(40.0), &p);
    assert!(one.bounds[3] - one.bounds[1] > 3.0 * (one.bounds[2] - one.bounds[0]), "{:?}", one.bounds);
    // hit testing finds the character in the second column
    let (x, b, _) = l.caret(4);
    assert_eq!(l.hit(x + 1.0, b - 5.0), 4);
    // differs from horizontal layout in the cache
    let h = layout("abc\nde", &st(40.0), &ParagraphStyle::default());
    assert_eq!(h.lines.len(), 2);
}

#[test]
fn vertical_underline_runs_down_the_column() {
    let s = TextStyle { underline: true, ..st(60.0) };
    let h = layout("A", &s, &ParagraphStyle::default());
    let v = layout("A", &s, &ParagraphStyle { vertical: true, ..Default::default() });
    assert_eq!(h.underlines.len(), 1);
    assert_eq!(v.underlines.len(), 1, "vertical cells get an underline");
    let [hx0, hy0, hx1, hy1] = h.underlines[0];
    let [vx0, vy0, vx1, vy1] = v.underlines[0];
    assert!(hx1 - hx0 > hy1 - hy0, "horizontal underline is a wide strip");
    assert!(vy1 - vy0 > vx1 - vx0, "vertical underline is a tall strip");
    let runs = [StyleRun { range: 0..1, style: s.clone() }];
    let rich = layout_rich("A", &st(60.0), &runs, &ParagraphStyle { vertical: true, ..Default::default() });
    assert_eq!(rich.underlines.len(), 1);
    assert_eq!(rich.underline_runs, vec![1]);
}

#[test]
fn strikethrough_crosses_the_glyphs() {
    let s = TextStyle { strikethrough: true, ..st(60.0) };
    let l = layout("Hx", &s, &ParagraphStyle::default());
    assert_eq!(l.underlines.len(), 1);
    let [x0, y0, x1, y1] = l.underlines[0];
    let line = &l.lines[0];
    assert!((x0 - line.x).abs() < 0.01 && (x1 - line.x - line.width).abs() < 0.01, "spans the line");
    // above the baseline (y down) and below the x-height
    let xh = fonts::face(fonts::resolve(&s.family, &s.style).face).metrics(60.0).x_height;
    assert!(y1 < line.baseline && y0 > line.baseline - xh, "{y0} {y1} baseline {} x-height {xh}", line.baseline);
    assert!(y1 - y0 > 0.5, "has a thickness");
    // with underline: two strips, the underline below the baseline
    let both = layout("Hx", &TextStyle { underline: true, ..s.clone() }, &ParagraphStyle::default());
    assert_eq!(both.underlines.len(), 2);
    assert!(both.underlines[0][1] > line.baseline && both.underlines[1][3] < line.baseline);
    // vertical: a tall strip through the middle of the cell
    let v = layout("A", &s, &ParagraphStyle { vertical: true, ..Default::default() });
    let [vx0, vy0, vx1, vy1] = v.underlines[0];
    let vl = &v.lines[0];
    assert!(vy1 - vy0 > vx1 - vx0, "vertical strikethrough is a tall strip");
    assert!(((vx0 + vx1) / 2.0 - (vl.x + vl.width / 2.0)).abs() < 0.5, "centred on the cell");
}

#[test]
fn style_runs_strike_through_only_their_range() {
    let base = st(40.0);
    let runs = [StyleRun { range: 3..5, style: TextStyle { strikethrough: true, ..base.clone() } }];
    let l = layout_rich("ab cd ef", &base, &runs, &ParagraphStyle::default());
    assert_eq!(l.underlines.len(), 1, "only the struck-through run");
    assert_eq!(l.underline_runs, vec![1]);
    let u = l.underlines[0];
    assert!(u[0] >= l.caret(3).0 - 0.01 && u[2] <= l.caret(5).0 + 0.01, "{u:?}");
    assert!(u[3] < l.lines[0].baseline, "above the baseline");
    let v = layout_rich(
        "ab",
        &base,
        &[StyleRun { range: 0..1, style: TextStyle { strikethrough: true, ..base.clone() } }],
        &ParagraphStyle { vertical: true, ..Default::default() },
    );
    assert_eq!(v.underlines.len(), 1);
    assert_eq!(v.underline_runs, vec![1]);
}

#[test]
fn vertical_cells_keep_combining_marks_with_their_base() {
    let style = st(32.0);
    let para = ParagraphStyle { vertical: true, ..Default::default() };
    let decomposed = layout_uncached("e\u{301}x", &style, &para);
    let composed = layout_uncached("éx", &style, &para);
    assert_eq!(decomposed.lines.len(), 2, "the combining acute must not get another vertical cell");
    assert_eq!(decomposed.lines[0].range, 0..3);
    assert_eq!(decomposed.lines[1].range, 3..4);
    assert_eq!(decomposed.text_len, 4, "retain the original UTF-8 source offsets");
    assert_eq!(decomposed.glyphs.iter().map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>(), composed.glyphs.iter().map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>());
    assert_eq!(decomposed.bounds, composed.bounds);
    assert_eq!(decomposed.caret(0).1, decomposed.caret(1).1, "base and combining mark occupy the same cell");
    for text in ["葛\u{e0101}x", "👩\u{200d}👧x"] {
        let lay = layout_uncached(text, &style, &para);
        assert_eq!(lay.lines.len(), 2, "variation selectors and ZWJ sequences stay with their base");
        assert_eq!(lay.lines[0].range, 0..text.len() - 1);
        assert_eq!(lay.lines[1].range, text.len() - 1..text.len());
    }
}

#[test]
fn vertical_kana_keep_decomposed_dakuten_in_the_same_cell() {
    fonts::scan_system();
    let Some(face) = fonts::all_faces().into_iter().find(|f| {
        (f.info.family.contains("Hiragino") || f.info.family.contains("Shippori") || f.info.family.contains("BIZ UD"))
            && "かがはぱ\u{3099}\u{309a}".chars().all(|c| f.has_char(c))
    }) else {
        eprintln!("SKIPPED: no Japanese font covering combining dakuten/handakuten");
        return;
    };
    let style = TextStyle { family: face.info.family.clone(), style: face.info.style.clone(), ..st(48.0) };
    let para = ParagraphStyle { vertical: true, ..Default::default() };
    let nfd = layout_uncached("か\u{3099}は\u{309a}", &style, &para);
    let nfc = layout_uncached("がぱ", &style, &para);
    assert_eq!(nfd.lines.len(), 2);
    assert_eq!(nfd.lines[0].range, 0..6);
    assert_eq!(nfd.lines[1].range, 6..12);
    assert_eq!(nfd.glyphs.iter().map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>(), nfc.glyphs.iter().map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>());
    assert_eq!(nfd.bounds, nfc.bounds);
}

#[test]
fn carets_and_hit_testing() {
    let l = layout("abc\nde", &st(40.0), &ParagraphStyle::default());
    let (x0, _, li0) = l.caret(0);
    let (x1, _, _) = l.caret(1);
    let (x3, _, _) = l.caret(3);
    assert!(x0.abs() < 1e-3 && x1 > x0 && x3 > x1);
    assert_eq!(li0, 0);
    let (_, b4, li4) = l.caret(4);
    assert_eq!(li4, 1);
    assert!(b4 > 0.0);
    assert_eq!(l.hit(x1 + 1.0, 0.0), 1);
    assert_eq!(l.hit(1000.0, b4), 6);
    let rects = l.selection_rects(1, 5);
    assert_eq!(rects.len(), 2);
}

#[test]
fn bidi_reorders_rtl_runs() {
    // Hebrew letters are laid out right-to-left: the first logical letter is rightmost.
    let l = layout("ab \u{5d0}\u{5d1}\u{5d2} cd", &st(40.0), &ParagraphStyle::default());
    let x_of = |byte: usize| l.glyphs.iter().find(|g| g.cluster == byte).map(|g| g.x).unwrap();
    let alef = "ab ".len();
    let gimel = alef + 4;
    assert!(x_of(alef) > x_of(gimel), "alef right of gimel");
    assert!(x_of(0) < x_of(gimel) && x_of(alef) < x_of("ab \u{5d0}\u{5d1}\u{5d2} ".len()));
}

#[test]
fn caps_and_small_caps() {
    let s = st(50.0);
    let up = measure("HELLO", &s);
    let all = measure("hello", &TextStyle { caps: Caps::All, ..s.clone() });
    assert!((up - all).abs() < 0.01);
    let small = measure("hello", &TextStyle { caps: Caps::Small, ..s.clone() });
    assert!(small < up * 0.9 && small > up * 0.6, "{small} {up}");
    let l = layout("Hi", &TextStyle { baseline_shift: 10.0, underline: true, ..s }, &ParagraphStyle::default());
    assert!((l.glyphs[0].y + 10.0).abs() < 1e-3);
    assert_eq!(l.underlines.len(), 1);
}

#[test]
fn ligature_caret_interpolates() {
    // Inter has an "fi"-like ligature only via calt in some versions; use "ffi" which most
    // fonts ligate. Whatever shaping does, every char boundary has a caret.
    let l = layout("office", &st(40.0), &ParagraphStyle::default());
    let stops: Vec<usize> = l.lines[0].carets.iter().map(|c| c.0).collect();
    assert_eq!(stops, vec![0, 1, 2, 3, 4, 5, 6]);
    let xs: Vec<f32> = l.lines[0].carets.iter().map(|c| c.1).collect();
    assert!(xs.windows(2).all(|w| w[1] >= w[0]), "{xs:?}");
}

#[test]
fn fallback_font_for_missing_glyphs() {
    // Inter lacks Devanagari/Hebrew; Noto Serif lacks Hebrew too → glyphs still produced (notdef)
    let l = layout("A\u{3b1}", &TextStyle { family: "Noto Serif".into(), ..st(40.0) }, &ParagraphStyle::default());
    assert_eq!(l.glyphs.len(), 2);
    assert!(l.glyphs.iter().all(|g| g.id != 0));
}

#[test]
fn style_runs_split_items_and_grow_lines() {
    let base = st(40.0);
    let big = TextStyle { size: 80.0, ..base.clone() };
    let text = "small BIG small";
    let plain = layout(text, &base, &ParagraphStyle::default());
    let runs = [StyleRun { range: 6..9, style: big.clone() }];
    let rich = layout_rich(text, &base, &runs, &ParagraphStyle::default());
    // glyphs carry their style index; the run is wider than the plain text
    let tagged: Vec<u16> = rich.glyphs.iter().map(|g| g.run).collect();
    assert_eq!(tagged.iter().filter(|r| **r == 1).count(), 3, "{tagged:?}");
    assert!(rich.glyphs.iter().filter(|g| g.run == 1).all(|g| g.size == 80.0));
    assert!(rich.lines[0].width > plain.lines[0].width + 30.0);
    assert!(rich.lines[0].ascent > plain.lines[0].ascent * 1.5, "the line grows to the run");
    // carets still cover every character boundary
    assert_eq!(rich.lines[0].carets.len(), text.len() + 1);
    // a line holding a big run moves down from the line above it
    let two = layout_rich("next\nBIG", &base, &[StyleRun { range: 5..8, style: big }], &ParagraphStyle::default());
    let two_plain = layout("next\nBIG", &base, &ParagraphStyle::default());
    assert!(two.lines[1].baseline > two_plain.lines[1].baseline + 10.0);
    // no runs: identical to the plain layout
    assert_eq!(*layout_rich(text, &base, &[], &ParagraphStyle::default()), *plain);
}

#[test]
fn style_runs_shift_underline_and_track() {
    let base = st(40.0);
    let text = "ab cd ef";
    let runs = [
        StyleRun { range: 3..5, style: TextStyle { underline: true, baseline_shift: 10.0, ..base.clone() } },
        StyleRun { range: 6..8, style: TextStyle { tracking: 500.0, faux_bold: true, ..base.clone() } },
    ];
    let l = layout_rich(text, &base, &runs, &ParagraphStyle::default());
    assert_eq!(l.underlines.len(), 1, "only the underlined run");
    assert_eq!(l.underline_runs, vec![1]);
    let c = l.glyphs.iter().find(|g| g.cluster == 3).unwrap();
    let a = l.glyphs.iter().find(|g| g.cluster == 0).unwrap();
    assert!((c.y - (a.y - 10.0)).abs() < 1e-3, "baseline shift per run");
    let u = l.underlines[0];
    assert!(u[0] >= l.caret(3).0 - 0.01 && u[2] <= l.caret(5).0 + 0.01, "{u:?}");
    assert!(l.glyphs.iter().filter(|g| g.run == 2).all(|g| g.synth_bold));
    // tracking widens only the tracked run: 2 clusters × 0.5 em
    let plain = layout(text, &base, &ParagraphStyle::default());
    assert!((l.lines[0].width - plain.lines[0].width - 2.0 * 20.0).abs() < 0.5, "{} {}", l.lines[0].width, plain.lines[0].width);
}

#[test]
fn empty_text_has_a_caret() {
    let l = layout("", &st(40.0), &ParagraphStyle::default());
    assert_eq!(l.lines.len(), 1);
    assert_eq!(l.caret(0).0, 0.0);
    assert!(l.bounds[3] > l.bounds[1]);
}

/// 縦中横: 2–4 consecutive half-width characters of a `tate_chu_yoko` run share one cell of a
/// vertical column — side by side across it, upright, inside one row — while the characters
/// that follow move up by the cells the group saved. Without the flag every character keeps
/// its own cell, and horizontal text ignores it.
#[test]
fn tate_chu_yoko_sets_half_width_characters_side_by_side_in_one_cell() {
    let style = st(40.0);
    let vertical = ParagraphStyle { vertical: true, ..Default::default() };
    let tcy = TextStyle { tate_chu_yoko: true, ..style.clone() };
    let text = "あ12い";
    let plain = layout_rich(text, &style, &[], &vertical);
    assert_eq!(plain.lines.len(), 4, "without the flag every character keeps its own cell");
    let row = plain.lines[1].baseline - plain.lines[0].baseline;
    assert!(row > 0.0);
    let runs = [StyleRun { range: 3..5, style: tcy.clone() }];
    let l = layout_rich(text, &style, &runs, &vertical);
    assert!(l.vertical);
    assert_eq!(l.lines.len(), 3, "the two digits share one cell: {:?}", l.lines);
    let cell = &l.lines[1];
    assert_eq!(cell.range, 3..5, "the cell covers both digits: {:?}", l.lines);
    let digits: Vec<&Glyph> = l.glyphs.iter().filter(|g| (3..5).contains(&g.cluster)).collect();
    assert_eq!(digits.len(), 2, "one glyph per digit: {digits:?}");
    assert!((digits[0].y - digits[1].y).abs() < 0.01, "side by side, not stacked: {digits:?}");
    assert!(digits[1].x > digits[0].x, "in reading order: {digits:?}");
    // One caret stop per character boundary: a caret or a click still works inside the group.
    let stops: Vec<usize> = cell.carets.iter().map(|c| c.0).collect();
    for b in 3..=5 {
        assert!(stops.contains(&b), "caret at {b}: {stops:?}");
    }
    // The glyphs use each sub-slot's left edge, the same origin as the carets, and stay inside
    // the cell: with the half-character offset the last digit crosses the cell's right edge.
    assert!((digits[0].x - cell.x).abs() < 0.01, "first digit at the cell's left edge: {} vs {}", digits[0].x, cell.x);
    let stop = |b: usize| cell.carets.iter().find(|c| c.0 == b).map_or(f32::MAX, |c| c.1);
    let advance = stop(5) - stop(4);
    assert!(
        digits[1].x + advance <= cell.x + cell.width + 0.01,
        "the digits stay inside the cell: {}..{} vs {}..{}",
        digits[0].x,
        digits[1].x + advance,
        cell.x,
        cell.x + cell.width
    );
    assert!(cell.width > style.size * 0.4 && cell.width < style.size * 1.6, "about one cell across the column: {}", cell.width);
    // The group takes one row down the column: what follows moves up by the cells it saved.
    assert!((cell.baseline - l.lines[0].baseline - row).abs() < 0.01, "{:?}", l.lines);
    assert!((l.lines[2].baseline - cell.baseline - row).abs() < 0.01, "{:?}", l.lines);
    assert!(
        (plain.lines[3].baseline - l.lines[2].baseline - row).abs() < 0.01,
        "the character after the group sits one row higher than without the flag: {:?} against {:?}",
        plain.lines,
        l.lines
    );
    let (x, baseline, li) = l.caret(4);
    assert_eq!((li, baseline), (1, cell.baseline));
    assert!((x - digits[1].x).abs() < 0.01, "the caret between the digits sits at the second digit's origin: {x} vs {}", digits[1].x);
    assert_eq!(l.hit(x, baseline), 4, "a click between them lands there");
    // A whole run flagged at the base style groups too.
    let all = layout("12", &tcy, &vertical);
    assert_eq!(all.lines.len(), 1, "the run is one cell: {:?}", all.lines);
    assert_eq!(all.glyphs.len(), 2);
    assert!((all.glyphs[0].y - all.glyphs[1].y).abs() < 0.01 && all.glyphs[1].x > all.glyphs[0].x);
    // Horizontal text ignores the flag.
    let horizontal = ParagraphStyle::default();
    let h = layout_rich(text, &style, &[StyleRun { range: 3..5, style: tcy }], &horizontal);
    let plain_h = layout_rich(text, &style, &[], &horizontal);
    let glyphs = |l: &Layout| l.glyphs.iter().map(|g| (g.id, g.x, g.y, g.cluster)).collect::<Vec<_>>();
    assert_eq!(glyphs(&h), glyphs(&plain_h), "horizontal layout is unchanged");
}

/// A 縦中横 group wider than its column overflows into the neighbouring columns by design; the
/// layout bounds must cover the whole group, or selection and clipping lose its ends.
#[test]
fn a_tate_chu_yoko_group_wider_than_the_column_widens_the_bounds() {
    let vertical = ParagraphStyle { vertical: true, ..Default::default() };
    let tcy = TextStyle { tate_chu_yoko: true, ..st(40.0) };
    // The second column's centre lies one column width to the left of the first.
    let two = layout("1234\na", &tcy, &vertical);
    let col_w = -(two.lines[1].x + two.lines[1].width / 2.0);
    let l = layout("1234", &tcy, &vertical);
    assert_eq!(l.lines.len(), 1, "the four digits share one cell: {:?}", l.lines);
    let cell = &l.lines[0];
    assert!(cell.width > col_w, "the group is wider than one column: {} vs {col_w}", cell.width);
    assert!(l.bounds[0] <= cell.x, "the left end of the group is inside the bounds: {} vs {}", cell.x, l.bounds[0]);
    assert!(cell.x + cell.width <= l.bounds[2], "the right end of the group is inside the bounds: {} vs {}", cell.x + cell.width, l.bounds[2]);
}

/// A rich 縦中横 cell underlines each member at its own sub-slot, the way a single-character
/// cell puts the strip to the right of its character, rather than the first underlined
/// member's style at the right edge of the whole cell.
#[test]
fn a_tate_chu_yoko_cell_keeps_each_underlined_member_in_its_slot() {
    let vertical = ParagraphStyle { vertical: true, ..Default::default() };
    let base = TextStyle { tate_chu_yoko: true, ..st(40.0) };
    let underlined = TextStyle { underline: true, ..base.clone() };
    let runs = [StyleRun { range: 0..1, style: underlined.clone() }];
    let l = layout_rich("12", &base, &runs, &vertical);
    assert_eq!(l.lines.len(), 1, "the digits share one cell: {:?}", l.lines);
    let cell = &l.lines[0];
    assert_eq!(l.underlines.len(), 1, "only the first digit is underlined");
    assert_eq!(l.underline_runs, vec![1]);
    // The strip runs down the right edge of the first digit's slot, measured from the same
    // style in a single-character cell.
    let one = layout("1", &underlined, &vertical);
    let offset = one.underlines[0][0] - (one.lines[0].x + one.lines[0].width);
    let slot_edge = cell.x + one.lines[0].width;
    let [ux, top, ux1, bottom] = l.underlines[0];
    assert!((ux - (slot_edge + offset)).abs() < 0.01, "the strip follows the first digit's slot: {ux} vs {}", slot_edge + offset);
    assert!(ux < cell.x + cell.width - 1.0, "not the right edge of the cell: {ux} vs {}", cell.x + cell.width);
    assert!(bottom - top > ux1 - ux, "a vertical strip");
    assert!((top - (cell.baseline - cell.ascent)).abs() < 0.01, "the strip shares the cell's top");
    assert!((bottom - top - (one.underlines[0][3] - one.underlines[0][1])).abs() < 0.01, "one row tall");
    // With both digits underlined, each keeps its own strip and run, even in different styles.
    let two = [StyleRun { range: 0..1, style: underlined.clone() }, StyleRun { range: 1..2, style: TextStyle { tracking: 20.0, ..underlined } }];
    let both = layout_rich("12", &base, &two, &vertical);
    assert_eq!(both.lines.len(), 1);
    assert_eq!(both.underlines.len(), 2, "one strip per underlined digit: {:?}", both.underlines);
    assert_eq!(both.underline_runs, vec![1, 2]);
    assert!(both.underlines[0][0] < both.underlines[1][0], "left to right, as the digits stand: {:?}", both.underlines);
}

/// 縦中横 groups only half-width printable ASCII: a grapheme cluster that carries a combining
/// mark (`A` + U+0301) is not one, even though its first character is ASCII.
#[test]
fn tate_chu_yoko_does_not_group_a_grapheme_with_a_combining_mark() {
    let style = TextStyle { tate_chu_yoko: true, ..st(40.0) };
    let para = ParagraphStyle { vertical: true, ..Default::default() };
    let l = layout("A\u{301}B", &style, &para);
    assert_eq!(l.lines.len(), 2, "the combining mark keeps its base in one cell and B gets its own: {:?}", l.lines);
    assert_eq!(l.lines[0].range, 0..3);
    assert_eq!(l.lines[1].range, 3..4);
}

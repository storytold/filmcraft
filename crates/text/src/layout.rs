//! Paragraph layout: font fallback, bidi (UAX #9), shaping (OpenType via harfrust: kerning,
//! ligatures, mark positioning, complex scripts), line breaking (UAX #14), alignment, leading,
//! tracking, baseline shift, all caps / small caps, underline. Results are cached.
//!
//! **Per-character styles** ([`layout_rich`]): [`StyleRun`]s give byte ranges their own
//! [`TextStyle`] (font, size, tracking, baseline shift, faux bold / italic, caps, underline).
//! Runs split shaping items, lines grow to fit the largest run, and every [`Glyph`] carries the
//! index of the style it was set in (`run`: 0 = the base style, `i + 1` = `runs[i]`), so renderers
//! can colour runs.
//!
//! Coordinates are pixels, y down. For **point text** (`ParagraphStyle::width == None`) the
//! origin is the alignment point on the first baseline: x = 0 is the left edge (left / justify),
//! the centre (centre) or the right edge (right). For **area text** (`width == Some(w)`) the
//! origin is the top-left of the text box and lines wrap at `w`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use harfrust::{Direction, Feature, Tag, UnicodeBuffer};
use unicode_bidi::{BidiInfo, Level};
use unicode_segmentation::UnicodeSegmentation;

use crate::fonts::{self, FaceId, Resolved};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Caps {
    #[default]
    Normal,
    All,
    Small,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Character formatting.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: String,
    pub style: String,
    /// Font size in pixels.
    pub size: f32,
    /// Tracking in 1/1000 em (added after every cluster).
    pub tracking: f32,
    /// Metric kerning (`kern`).
    pub kerning: bool,
    /// Standard ligatures (`liga`, `clig`).
    pub ligatures: bool,
    /// Baseline shift in pixels (positive = up).
    pub baseline_shift: f32,
    pub faux_bold: bool,
    pub faux_italic: bool,
    pub caps: Caps,
    pub underline: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: fonts::DEFAULT_FAMILY.into(),
            style: "Regular".into(),
            size: 100.0,
            tracking: 0.0,
            kerning: true,
            ligatures: true,
            baseline_shift: 0.0,
            faux_bold: false,
            faux_italic: false,
            caps: Caps::Normal,
            underline: false,
        }
    }
}

/// Paragraph formatting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParagraphStyle {
    pub align: Align,
    /// Extra line spacing in pixels added to the font's natural line height (Premiere's Leading).
    pub leading: f32,
    /// Wrap width (area text); None = point text.
    pub width: Option<f32>,
    /// Box height (area text only): lines that end below it are left out, except the first.
    /// None = as tall as the text.
    pub height: Option<f32>,
    /// Base direction: None = from the first strong character.
    pub rtl: Option<bool>,
    /// Vertical text: characters stack top to bottom, paragraphs are columns laid out right to
    /// left. Alignment applies along the column (Left = top, Center, Right = bottom).
    pub vertical: bool,
}

fn hf(h: &mut impl Hasher, v: f32) {
    v.to_bits().hash(h);
}

impl Hash for TextStyle {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.family.hash(h);
        self.style.hash(h);
        hf(h, self.size);
        hf(h, self.tracking);
        hf(h, self.baseline_shift);
        (self.kerning, self.ligatures, self.faux_bold, self.faux_italic, self.caps, self.underline).hash(h);
    }
}

impl Hash for ParagraphStyle {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.align.hash(h);
        hf(h, self.leading);
        self.width.map(f32::to_bits).hash(h);
        self.height.map(f32::to_bits).hash(h);
        self.rtl.hash(h);
        self.vertical.hash(h);
    }
}

/// A positioned glyph. `(x, y)` is the glyph origin on its baseline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub face: FaceId,
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    /// Byte offset of the cluster in the source text.
    pub cluster: usize,
    pub synth_bold: bool,
    pub synth_italic: bool,
    /// Style the glyph was set in: 0 = the base style, `i + 1` = `runs[i]` of [`layout_rich`].
    pub run: u16,
}

/// A byte range of the text set in its own style ([`layout_rich`]). Later runs win where runs
/// overlap; ranges are clamped to the text.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleRun {
    pub range: Range<usize>,
    pub style: TextStyle,
}

impl Hash for StyleRun {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.range.hash(h);
        self.style.hash(h);
    }
}

/// The styles of one layout: `list[0]` is the base style, `list[i + 1]` is run `i`.
struct Styles<'a> {
    list: Vec<&'a TextStyle>,
    prim: Vec<Resolved>,
    /// Style index of every byte (None = all base style).
    per_byte: Option<Vec<u16>>,
}

impl<'a> Styles<'a> {
    fn new(text: &str, base: &'a TextStyle, runs: &'a [StyleRun]) -> Self {
        let mut list = vec![base];
        list.extend(runs.iter().map(|r| &r.style));
        let prim = list.iter().map(|s| fonts::resolve(&s.family, &s.style)).collect();
        let per_byte = (!runs.is_empty()).then(|| {
            let mut v = vec![0u16; text.len() + 1];
            for (i, r) in runs.iter().enumerate() {
                let (a, b) = (r.range.start.min(text.len()), r.range.end.min(text.len()));
                for x in v.iter_mut().take(b).skip(a) {
                    *x = (i + 1).min(u16::MAX as usize) as u16;
                }
            }
            v
        });
        Styles { list, prim, per_byte }
    }
    fn at(&self, byte: usize) -> usize {
        self.per_byte.as_ref().and_then(|v| v.get(byte)).map_or(0, |&i| i as usize)
    }
    fn rich(&self) -> bool {
        self.per_byte.is_some()
    }
}

/// One laid-out line.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Byte range in the source text (without the terminating newline).
    pub range: Range<usize>,
    pub baseline: f32,
    /// Left edge and width of the line's content (trailing spaces excluded).
    pub x: f32,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    pub glyphs: Range<usize>,
    /// Caret stops `(byte offset, x)` for every character boundary in the line, by byte offset.
    pub carets: Vec<(usize, f32)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub glyphs: Vec<Glyph>,
    pub lines: Vec<Line>,
    /// Underline rectangles `[x0, y0, x1, y1]`.
    pub underlines: Vec<[f32; 4]>,
    /// Style index of each underline (parallel to `underlines`; see [`Glyph::run`]).
    pub underline_runs: Vec<u16>,
    /// Logical bounds `[x0, y0, x1, y1]` (line boxes; area text spans the box width).
    pub bounds: [f32; 4],
    /// The face requested was missing (substituted).
    pub missing_font: bool,
    /// Lines were left out because they do not fit the box height.
    pub overflow: bool,
    pub text_len: usize,
    /// Laid out vertically (one line per grapheme; columns right to left).
    pub vertical: bool,
}

impl Layout {
    /// Line index for a caret at byte `pos`.
    pub fn line_of(&self, pos: usize) -> usize {
        let mut li = 0;
        for (i, l) in self.lines.iter().enumerate() {
            if l.range.start <= pos {
                li = i;
            }
        }
        li
    }
    /// Caret position `(x, baseline, line)` for byte offset `pos`.
    pub fn caret(&self, pos: usize) -> (f32, f32, usize) {
        let li = self.line_of(pos);
        let Some(l) = self.lines.get(li) else { return (0.0, 0.0, 0) };
        let x = l.carets.iter().min_by_key(|(b, _)| b.abs_diff(pos)).map_or(l.x, |c| c.1);
        (x, l.baseline, li)
    }
    /// Nearest caret byte offset to point `(x, y)`.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        let d = |l: &Line| {
            if self.vertical {
                let dx = if x < l.x - l.ascent * 0.5 { l.x - l.ascent * 0.5 - x } else { (x - l.x - l.width - l.ascent * 0.5).max(0.0) };
                line_dist(l, y) + dx
            } else {
                line_dist(l, y)
            }
        };
        let Some(l) = self.lines.iter().min_by(|a, b| d(a).total_cmp(&d(b))) else {
            return 0;
        };
        l.carets.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(l.range.start, |c| c.0)
    }
    /// Selection highlight rectangles `[x0, y0, x1, y1]` for the byte range `a..b`.
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<[f32; 4]> {
        let (a, b) = (a.min(b), a.max(b));
        let mut out = Vec::new();
        for l in &self.lines {
            let s = a.max(l.range.start);
            let e = b.min(l.range.end);
            if s > e || (s == e && !(a < l.range.start && b > l.range.end)) {
                continue;
            }
            let xs: Vec<f32> = l.carets.iter().filter(|(p, _)| *p >= s && *p <= e).map(|c| c.1).collect();
            if xs.len() < 2 && b <= l.range.end {
                continue;
            }
            let x0 = xs.iter().copied().fold(f32::MAX, f32::min);
            let mut x1 = xs.iter().copied().fold(f32::MIN, f32::max);
            if b > l.range.end {
                x1 += l.ascent * 0.25; // selected newline
            }
            out.push([x0.min(x1), l.baseline - l.ascent, x1, l.baseline + l.descent]);
        }
        out
    }
}

fn line_dist(l: &Line, y: f32) -> f32 {
    let (t, b) = (l.baseline - l.ascent, l.baseline + l.descent);
    if y < t {
        t - y
    } else if y > b {
        y - b
    } else {
        0.0
    }
}

struct ShapedGlyph {
    face: FaceId,
    id: u32,
    cluster: usize,
    adv: f32,
    dx: f32,
    dy: f32,
    size: f32,
}

struct Item {
    range: Range<usize>,
    rtl: bool,
    glyphs: Vec<ShapedGlyph>,
    /// Style index ([`Styles`]).
    style: usize,
}

fn upper_single(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

const SMALL_CAPS_SCALE: f32 = 0.78;

fn uses_vertical_form(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFE10..=0xFE1F | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

fn shape_item(text: &str, chars: &[(usize, char, char)], rtl: bool, face: FaceId, size: f32, style: &TextStyle, vertical: bool) -> Vec<ShapedGlyph> {
    let f = fonts::face(face);
    let (Some(font), Some(data)) = (f.font(), f.shaper_data()) else { return Vec::new() };
    let shaper = data.shaper(&font).build();
    let mut buf = UnicodeBuffer::new();
    for &(b, _, sc) in chars {
        buf.add(if sc == '\t' { ' ' } else { sc }, b as u32);
    }
    buf.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
    buf.guess_segment_properties();
    let mut feats = Vec::new();
    if vertical && chars.first().is_some_and(|c| uses_vertical_form(c.1)) {
        feats.push(Feature::new(Tag::new(b"vert"), 1, ..));
        feats.push(Feature::new(Tag::new(b"vrt2"), 1, ..));
    }
    if !style.kerning {
        feats.push(Feature::new(Tag::new(b"kern"), 0, ..));
    }
    if !style.ligatures {
        feats.push(Feature::new(Tag::new(b"liga"), 0, ..));
        feats.push(Feature::new(Tag::new(b"clig"), 0, ..));
    }
    let out = shaper.shape(buf, harfrust::ShapeOptions::new().features(&feats));
    let k = size / f.units_per_em();
    let _ = text;
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| ShapedGlyph {
            face,
            id: i.glyph_id,
            cluster: i.cluster as usize,
            adv: p.x_advance as f32 * k,
            dx: p.x_offset as f32 * k,
            dy: p.y_offset as f32 * k,
            size,
        })
        .collect()
}

struct ParaLine {
    range: Range<usize>,
    glyphs: Vec<Glyph>,
    carets: Vec<(usize, f32)>,
    content: f32,
    left_trim: f32,
    ascent: f32,
    descent: f32,
    /// Underlined segments `(x0, x1, style)` in line coordinates (rich layouts only).
    underlines: Vec<(f32, f32, u16)>,
}

/// Lay out one paragraph (no newlines) whose text starts at byte `base` of the whole string.
fn paragraph(text: &str, base: usize, sty: &Styles, para: &ParagraphStyle) -> Vec<ParaLine> {
    let style = sty.list[0];
    let primary = sty.prim[0];
    let pm = fonts::face(primary.face).metrics(style.size);
    if text.is_empty() {
        return vec![ParaLine {
            range: base..base,
            glyphs: vec![],
            carets: vec![(base, 0.0)],
            content: 0.0,
            left_trim: 0.0,
            ascent: pm.ascent,
            descent: pm.descent,
            underlines: Vec::new(),
        }];
    }
    let default_level = para.rtl.map(|r| if r { Level::rtl() } else { Level::ltr() });
    let bidi = BidiInfo::new(text, default_level);
    let pinfo = &bidi.paragraphs[0];
    let base_rtl = pinfo.level.is_rtl();
    // per char: (byte, char, shaped char), face, level, small, style
    let chars: Vec<(usize, char, char, FaceId, bool, bool, usize)> = text
        .char_indices()
        .map(|(b, c)| {
            let si = sty.at(base + b);
            let (sc, small) = match sty.list[si].caps {
                Caps::Normal => (c, false),
                Caps::All => (upper_single(c), false),
                Caps::Small if c.is_lowercase() => (upper_single(c), true),
                Caps::Small => (c, false),
            };
            let pf = sty.prim[si].face;
            let face = if sc.is_whitespace() { pf } else { fonts::fallback_for(sc, pf) };
            (b, c, sc, face, bidi.levels[b].is_rtl(), small, si)
        })
        .collect();
    // items: runs of equal (face, rtl, small, style)
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (face, rtl, small, si) = (chars[i].3, chars[i].4, chars[i].5, chars[i].6);
        let mut j = i + 1;
        while j < chars.len() && chars[j].3 == face && chars[j].4 == rtl && chars[j].5 == small && chars[j].6 == si {
            j += 1;
        }
        let st = sty.list[si];
        let sub: Vec<(usize, char, char)> = chars[i..j].iter().map(|c| (c.0, c.1, c.2)).collect();
        let size = if small { st.size * SMALL_CAPS_SCALE } else { st.size };
        let mut glyphs = shape_item(text, &sub, rtl, face, size, st, para.vertical);
        // tracking after each cluster
        if st.tracking != 0.0 {
            let t = st.tracking * st.size / 1000.0;
            for k in 0..glyphs.len() {
                if k + 1 == glyphs.len() || glyphs[k + 1].cluster != glyphs[k].cluster {
                    glyphs[k].adv += t;
                }
            }
        }
        let end = if j < chars.len() { chars[j].0 } else { text.len() };
        items.push(Item { range: chars[i].0..end, rtl, glyphs, style: si });
        i = j;
    }
    // advance per char byte (cluster start)
    let mut char_adv = vec![0.0f32; text.len() + 1];
    for it in &items {
        for g in &it.glyphs {
            char_adv[g.cluster.min(text.len())] += g.adv;
        }
    }
    let range_w = |r: Range<usize>| -> f32 { char_adv[r].iter().sum() };
    let trailing_ws = |r: &Range<usize>| -> (usize, f32) {
        let mut end = r.end;
        let mut w = 0.0;
        for (b, c) in text[r.clone()].char_indices().rev() {
            if !c.is_whitespace() {
                break;
            }
            end = r.start + b;
            w += char_adv[r.start + b];
        }
        (end, w)
    };
    // line breaking
    let mut ranges: Vec<Range<usize>> = Vec::new();
    match para.width {
        None => ranges.push(0..text.len()),
        Some(maxw) => {
            let mut start = 0usize;
            let mut last_ok: Option<usize> = None;
            for (p, _) in unicode_linebreak::linebreaks(text) {
                loop {
                    let r = start..p;
                    let (ce, _) = trailing_ws(&r);
                    let w = range_w(start..ce);
                    if w <= maxw || last_ok.is_none() {
                        last_ok = Some(p);
                        break;
                    }
                    let Some(b) = last_ok.take() else { break };
                    ranges.push(start..b);
                    start = b;
                }
            }
            ranges.push(start..text.len());
            ranges.retain(|r| !r.is_empty());
            if ranges.is_empty() {
                ranges.push(0..text.len());
            }
        }
    }
    let nlines = ranges.len();
    let mut out = Vec::new();
    for (li, r) in ranges.into_iter().enumerate() {
        let (ce, tw) = trailing_ws(&r);
        let content = range_w(r.start..ce);
        // justification
        let mut space_extra = 0.0;
        if para.align == Align::Justify
            && li + 1 < nlines
            && let Some(maxw) = para.width
        {
            let spaces = text[r.start..ce].chars().filter(|c| *c == ' ').count();
            if spaces > 0 {
                space_extra = ((maxw - content) / spaces as f32).max(0.0);
            }
        }
        let (levels, runs) = bidi.visual_runs(pinfo, r.clone());
        let mut pen = 0.0f32;
        let mut glyphs = Vec::new();
        let mut extents: HashMap<usize, (f32, f32, bool)> = HashMap::new();
        for run in runs {
            let rtl = levels[run.start].is_rtl();
            let mut idx: Vec<usize> = (0..items.len()).filter(|&k| items[k].range.start < run.end && items[k].range.end > run.start).collect();
            if rtl {
                idx.reverse();
            }
            for k in idx {
                let si = items[k].style;
                let (st, pr) = (sty.list[si], sty.prim[si]);
                for g in items[k].glyphs.iter().filter(|g| run.contains(&g.cluster)) {
                    let x0 = pen;
                    glyphs.push(Glyph {
                        face: g.face,
                        id: g.id,
                        x: pen + g.dx,
                        y: -g.dy - st.baseline_shift,
                        size: g.size,
                        cluster: base + g.cluster,
                        synth_bold: pr.synth_bold || st.faux_bold,
                        synth_italic: pr.synth_italic || st.faux_italic,
                        run: si as u16,
                    });
                    pen += g.adv;
                    if space_extra > 0.0 && text[g.cluster..].starts_with(' ') && g.cluster < ce {
                        pen += space_extra;
                    }
                    let e = extents.entry(g.cluster).or_insert((x0, pen, items[k].rtl));
                    e.0 = e.0.min(x0);
                    e.1 = e.1.max(pen);
                }
            }
        }
        // caret stops for each char boundary in the line
        let mut carets = Vec::new();
        let mut cluster_starts: Vec<usize> = extents.keys().copied().collect();
        cluster_starts.sort_unstable();
        let char_bytes: Vec<usize> = text[r.clone()].char_indices().map(|(b, _)| r.start + b).collect();
        for (ci, &cs) in cluster_starts.iter().enumerate() {
            let (x0, x1, rtl) = extents[&cs];
            let next = cluster_starts.get(ci + 1).copied().unwrap_or(r.end);
            let members: Vec<usize> = char_bytes.iter().copied().filter(|b| *b >= cs && *b < next).collect();
            let n = members.len().max(1) as f32;
            for (mi, b) in members.iter().enumerate() {
                let f = mi as f32 / n;
                let x = if rtl { x1 - (x1 - x0) * f } else { x0 + (x1 - x0) * f };
                carets.push((base + b, x));
            }
        }
        // underlined segments of rich layouts: clusters set in an underlined style, merged
        let mut underlines: Vec<(f32, f32, u16)> = Vec::new();
        if sty.rich() {
            let mut segs: Vec<(f32, f32, u16)> = cluster_starts
                .iter()
                .filter(|&&cs| cs < ce)
                .filter_map(|&cs| {
                    let si = sty.at(base + cs);
                    sty.list[si].underline.then(|| (extents[&cs].0, extents[&cs].1, si as u16))
                })
                .collect();
            segs.sort_by(|a, b| a.0.total_cmp(&b.0));
            for sg in segs {
                match underlines.last_mut() {
                    Some(l) if l.2 == sg.2 && (sg.0 - l.1).abs() < 0.01 => l.1 = l.1.max(sg.1),
                    _ => underlines.push(sg),
                }
            }
        }
        // end of line caret
        let end_x = match chars.iter().rev().find(|c| c.0 < r.end && c.0 >= r.start) {
            Some(c) => {
                let cs = cluster_starts.iter().rev().find(|s| **s <= c.0).copied();
                match cs.and_then(|s| extents.get(&s)) {
                    Some(&(x0, x1, rtl)) => {
                        if rtl {
                            x0
                        } else {
                            x1
                        }
                    }
                    None => pen,
                }
            }
            None => 0.0,
        };
        carets.push((base + r.end, end_x));
        carets.sort_by_key(|c| c.0);
        carets.dedup_by_key(|c| c.0);
        let mut asc = pm.ascent;
        let mut desc = pm.descent;
        for g in &glyphs {
            if g.face != primary.face || (sty.rich() && g.size != style.size) {
                let m = fonts::face(g.face).metrics(g.size);
                asc = asc.max(m.ascent);
                desc = desc.max(m.descent);
            }
        }
        let content_w = if space_extra > 0.0 { para.width.unwrap_or(content) } else { content };
        out.push(ParaLine {
            range: base + r.start..base + r.end,
            glyphs,
            carets,
            content: content_w,
            left_trim: if base_rtl { tw } else { 0.0 },
            ascent: asc,
            descent: desc,
            underlines,
        });
    }
    out
}

/// Lay out `text` (paragraphs separated by `\n`).
pub fn layout_uncached(text: &str, style: &TextStyle, para: &ParagraphStyle) -> Layout {
    layout_rich_uncached(text, style, &[], para)
}

/// Lay out `text` with per-character style runs (see [`StyleRun`]); `style` is the base style.
pub fn layout_rich_uncached(text: &str, style: &TextStyle, runs: &[StyleRun], para: &ParagraphStyle) -> Layout {
    let sty = Styles::new(text, style, runs);
    let pm = fonts::face(sty.prim[0].face).metrics(style.size);
    let line_h = (pm.ascent + pm.descent + pm.line_gap).max(style.size * 0.5) + para.leading;
    let mut lay = Layout { missing_font: sty.prim.iter().any(|p| p.missing), text_len: text.len(), ..Default::default() };
    if para.vertical {
        return layout_vertical(text, &sty, para, line_h, lay);
    }
    let mut base = 0usize;
    let mut plines = Vec::new();
    for p in text.split('\n') {
        let p_clean = p.strip_suffix('\r').unwrap_or(p);
        plines.extend(paragraph(p_clean, base, &sty, para));
        base += p.len() + 1;
    }
    let rich = sty.rich();
    let first_baseline = match (para.width.is_some(), rich) {
        (false, _) => 0.0,
        (true, false) => pm.ascent,
        (true, true) => plines.first().map_or(pm.ascent, |l| l.ascent),
    };
    let mut baseline = first_baseline;
    let mut prev_desc = 0.0f32;
    let box_h = para.width.and(para.height);
    for (i, pl) in plines.into_iter().enumerate() {
        if i > 0 {
            // rich text: a line holding a larger run pushes its baseline down
            baseline += if rich { line_h.max(prev_desc + pl.ascent + para.leading) } else { line_h };
            if box_h.is_some_and(|h| baseline + pl.descent > h + 0.01) {
                lay.overflow = true;
                break;
            }
        }
        prev_desc = pl.descent;
        let shift = match (para.width, para.align) {
            (None, Align::Left | Align::Justify) => 0.0,
            (None, Align::Center) => -pl.content / 2.0,
            (None, Align::Right) => -pl.content,
            (Some(_), Align::Left | Align::Justify) => 0.0,
            (Some(w), Align::Center) => (w - pl.content) / 2.0,
            (Some(w), Align::Right) => w - pl.content,
        } - pl.left_trim;
        let g0 = lay.glyphs.len();
        lay.glyphs.extend(pl.glyphs.into_iter().map(|mut g| {
            g.x += shift;
            g.y += baseline;
            g
        }));
        let x = shift + pl.left_trim;
        if rich {
            for (x0, x1, si) in &pl.underlines {
                let st = sty.list[*si as usize];
                let um = fonts::face(sty.prim[*si as usize].face).metrics(st.size);
                let y = baseline - st.baseline_shift + um.underline_pos;
                lay.underlines.push([x0 + shift, y, x1 + shift, y + um.underline_thickness]);
                lay.underline_runs.push(*si);
            }
        } else if style.underline && pl.content > 0.0 {
            lay.underlines.push([x, baseline + pm.underline_pos, x + pl.content, baseline + pm.underline_pos + pm.underline_thickness]);
            lay.underline_runs.push(0);
        }
        lay.lines.push(Line {
            range: pl.range,
            baseline,
            x,
            width: pl.content,
            ascent: pl.ascent,
            descent: pl.descent,
            glyphs: g0..lay.glyphs.len(),
            carets: pl.carets.into_iter().map(|(b, cx)| (b, cx + shift)).collect(),
        });
    }
    let (mut x0, mut x1) = (f32::MAX, f32::MIN);
    for l in &lay.lines {
        x0 = x0.min(l.x);
        x1 = x1.max(l.x + l.width);
    }
    if let Some(w) = para.width {
        x0 = 0.0;
        x1 = x1.max(w);
    }
    if let (Some(first), Some(last)) = (lay.lines.first(), lay.lines.last()) {
        lay.bounds = [x0, first.baseline - first.ascent, x1.max(x0), last.baseline + last.descent];
    }
    if let Some(h) = box_h {
        // a sized box is its own bounds, however much text it holds
        lay.bounds[1] = 0.0;
        lay.bounds[3] = h.max(0.0);
    }
    lay
}

/// Vertical layout: each grapheme is its own line (so carets, hit testing and selection work
/// unchanged), stacked top to bottom and centred on its column; columns run right to left.
fn layout_vertical(text: &str, sty: &Styles, para: &ParagraphStyle, col_w: f32, mut lay: Layout) -> Layout {
    let style = sty.list[0];
    let pm = fonts::face(sty.prim[0].face).metrics(style.size);
    let row = (pm.ascent + pm.descent).max(style.size * 0.5) + style.tracking * style.size / 1000.0;
    let flat = ParagraphStyle { vertical: true, ..Default::default() };
    lay.vertical = true;
    let mut base = 0usize;
    let mut ncols = 0usize;
    for (c, p) in text.split('\n').enumerate() {
        ncols = c + 1;
        let p_clean = p.strip_suffix('\r').unwrap_or(p);
        let mut cells: Vec<ParaLine> = Vec::new();
        if p_clean.is_empty() {
            cells.extend(paragraph("", base, sty, &flat));
        }
        for (off, cluster) in p_clean.grapheme_indices(true) {
            cells.extend(paragraph(cluster, base + off, sty, &flat));
        }
        let h = row * cells.len() as f32;
        let y0 = match para.align {
            Align::Center => -h / 2.0,
            Align::Right => -h,
            _ => 0.0,
        };
        let cx = -(c as f32) * col_w;
        for (k, pl) in cells.into_iter().enumerate() {
            let baseline = y0 + row * k as f32;
            let shift = cx - pl.content / 2.0 - pl.left_trim;
            let g0 = lay.glyphs.len();
            lay.glyphs.extend(pl.glyphs.into_iter().map(|mut g| {
                g.x += shift;
                // The alternate glyph hangs from its vertical origin. A horizontal ascent
                // is not the top bearing of a vertical punctuation glyph (OpenType VORG/vmtx).
                let font = fonts::face(g.face);
                if text.get(g.cluster..).and_then(|s| s.chars().next()).is_some_and(uses_vertical_form)
                    && let Some(origin) = font.vertical_origin(g.id, g.size)
                {
                    g.y += baseline - pl.ascent + origin;
                } else {
                    g.y += baseline;
                }
                g
            }));
            lay.lines.push(Line {
                range: pl.range,
                baseline,
                x: shift + pl.left_trim,
                width: pl.content,
                ascent: pl.ascent,
                descent: pl.descent,
                glyphs: g0..lay.glyphs.len(),
                carets: pl.carets.into_iter().map(|(b, x)| (b, x + shift)).collect(),
            });
        }
        base += p.len() + 1;
    }
    let (mut y0, mut y1) = (f32::MAX, f32::MIN);
    for l in &lay.lines {
        y0 = y0.min(l.baseline - l.ascent);
        y1 = y1.max(l.baseline + l.descent);
    }
    lay.bounds = [-(ncols.max(1) as f32 - 0.5) * col_w, y0, col_w / 2.0, y1.max(y0)];
    lay
}

fn cache() -> &'static Mutex<HashMap<u64, (u64, Arc<Layout>)>> {
    static C: OnceLock<Mutex<HashMap<u64, (u64, Arc<Layout>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Lay out `text` (cached by text and styles).
pub fn layout(text: &str, style: &TextStyle, para: &ParagraphStyle) -> Arc<Layout> {
    layout_rich(text, style, &[], para)
}

/// Lay out `text` with per-character style runs (cached by text, styles and runs).
pub fn layout_rich(text: &str, style: &TextStyle, runs: &[StyleRun], para: &ParagraphStyle) -> Arc<Layout> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    style.hash(&mut h);
    para.hash(&mut h);
    runs.hash(&mut h);
    let key = h.finish();
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(e) = cache().lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key) {
        e.0 = now;
        return e.1.clone();
    }
    let l = Arc::new(layout_rich_uncached(text, style, runs, para));
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= 512 {
        // evict the oldest quarter
        let mut ages: Vec<u64> = c.values().map(|v| v.0).collect();
        ages.sort_unstable();
        let cut = ages[ages.len() / 4];
        c.retain(|_, v| v.0 > cut);
    }
    c.insert(key, (now, l.clone()));
    l
}

/// Width of a single line of `text` (no wrapping).
pub fn measure(text: &str, style: &TextStyle) -> f32 {
    let l = layout(text, style, &ParagraphStyle::default());
    l.lines.iter().map(|l| l.width).fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
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
        // skip where neither is present.
        crate::fonts::scan_system();
        if !crate::fonts::all_faces().iter().any(|f| "日本語縦書き".chars().all(|c| f.has_char(c))) {
            eprintln!("SKIPPED: no Japanese font installed");
            return;
        }
        let l = layout("日本語\n縦書き", &st(40.0), &ParagraphStyle { vertical: true, ..Default::default() });
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
    fn vertical_cells_keep_combining_marks_with_their_base() {
        let style = st(32.0);
        let para = ParagraphStyle { vertical: true, ..Default::default() };
        let decomposed = layout_uncached("e\u{301}x", &style, &para);
        let composed = layout_uncached("éx", &style, &para);
        assert_eq!(decomposed.lines.len(), 2, "the combining acute must not get another vertical cell");
        assert_eq!(decomposed.lines[0].range, 0..3);
        assert_eq!(decomposed.lines[1].range, 3..4);
        assert_eq!(decomposed.text_len, 4, "retain the original UTF-8 source offsets");
        assert_eq!(
            decomposed.glyphs.iter().map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>(),
            composed.glyphs.iter().map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>()
        );
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
}

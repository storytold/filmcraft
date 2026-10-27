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
        let mut first_member = 0usize;
        for (ci, &cs) in cluster_starts.iter().enumerate() {
            let (x0, x1, rtl) = extents[&cs];
            let next = cluster_starts.get(ci + 1).copied().unwrap_or(r.end);
            while first_member < char_bytes.len() && char_bytes[first_member] < cs {
                first_member += 1;
            }
            let member_start = first_member;
            while first_member < char_bytes.len() && char_bytes[first_member] < next {
                first_member += 1;
            }
            let members = &char_bytes[member_start..first_member];
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
            let y = baseline - style.baseline_shift + pm.underline_pos;
            lay.underlines.push([x, y, x + pl.content, y + pm.underline_thickness]);
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
    let rich = sty.rich();
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
            // Underline runs down the column, to the right of the cell (the "below" side of vertical-rl).
            let y_top = baseline - pl.ascent;
            if rich {
                for (_x0, x1, si) in &pl.underlines {
                    let st = sty.list[*si as usize];
                    let um = fonts::face(sty.prim[*si as usize].face).metrics(st.size);
                    let x = *x1 + shift - st.baseline_shift + um.underline_pos;
                    lay.underlines.push([x, y_top, x + um.underline_thickness, y_top + row]);
                    lay.underline_runs.push(*si);
                }
            } else if style.underline && pl.content > 0.0 {
                let x = shift + pl.left_trim + pl.content + pm.underline_pos;
                lay.underlines.push([x, y_top, x + pm.underline_thickness, y_top + row]);
                lay.underline_runs.push(0);
            }
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
mod tests;

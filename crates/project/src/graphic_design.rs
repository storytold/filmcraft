//! Responsive design and per-character styles of graphic clips (M10.7).
//!
//! - **Per-character styles** ([`StyleRun`], [`CharStyle`]): a text layer's runs override font,
//!   style, size, fill, faux bold / italic, underline, tracking, baseline shift and caps for a
//!   range of characters. Ranges are in *characters* (not bytes), sorted and non-overlapping;
//!   [`apply_char_style`] merges a new style into a range and [`adjust_runs`] keeps runs on the
//!   right characters when the text is edited (typed characters take the style of the character
//!   before them).
//! - **Responsive Design – Position** ([`Pin`]): a layer's edges pinned to another layer (by its
//!   [`LayerExtra::uid`]) or to the video frame keep their distance to the same edges of the
//!   target, so a box pinned to a text layer grows with the text. [`resolve_pins`] works on plain
//!   bounding boxes; the renderer turns the result into position / size changes.
//! - **Responsive Design – Time** ([`GraphicMeta`]): the intro and outro of a graphic keep their
//!   timing when the clip is trimmed or extended; the middle stretches ([`remap_time`]).
//! - **Rolls and crawls** ([`Roll`]): the whole graphic moves up (Roll) or sideways (Crawl Left /
//!   Right), optionally starting and ending off screen, with preroll, ease-in, ease-out and
//!   postroll ([`roll_offset`], [`roll_progress`]).

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use crate::effect::EffectInstance;

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Extra data of one graphic layer, stored on its effect instance (`EffectInstance::layer`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerExtra {
    /// Stable id of the layer within its clip (0 = not assigned yet). Pins and template controls
    /// refer to layers by it, so they survive reordering.
    #[serde(skip_serializing_if = "is_zero")]
    pub uid: u64,
    /// Per-character style overrides of a text layer.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<StyleRun>,
    /// Responsive Design – Position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
}

impl LayerExtra {
    pub fn is_empty(&self) -> bool {
        self.uid == 0 && self.runs.is_empty() && self.pin.is_none()
    }
}

/// Character formatting overrides (None = inherit the layer's value).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CharStyle {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_style: Option<String>,
    /// Font size in px.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    /// Fill colour (sRGB, straight alpha).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faux_bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faux_italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    /// Tracking in 1/1000 em.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracking: Option<f32>,
    /// Baseline shift in px (positive = up).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_shift: Option<f32>,
    /// 0 normal, 1 all caps, 2 small caps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps: Option<u32>,
}

impl CharStyle {
    pub fn is_empty(&self) -> bool {
        *self == CharStyle::default()
    }
    /// `self` with every field `over` sets replaced.
    pub fn merged(&self, over: &CharStyle) -> CharStyle {
        CharStyle {
            font: over.font.clone().or_else(|| self.font.clone()),
            font_style: over.font_style.clone().or_else(|| self.font_style.clone()),
            size: over.size.or(self.size),
            fill: over.fill.or(self.fill),
            faux_bold: over.faux_bold.or(self.faux_bold),
            faux_italic: over.faux_italic.or(self.faux_italic),
            underline: over.underline.or(self.underline),
            tracking: over.tracking.or(self.tracking),
            baseline_shift: over.baseline_shift.or(self.baseline_shift),
            caps: over.caps.or(self.caps),
        }
    }
}

/// A range of characters `start..end` with its own formatting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StyleRun {
    pub start: usize,
    pub end: usize,
    #[serde(flatten)]
    pub style: CharStyle,
}

/// Per-character styles of a text of `n` characters.
fn expand(runs: &[StyleRun], n: usize) -> Vec<CharStyle> {
    let mut v = vec![CharStyle::default(); n];
    for r in runs {
        for c in v.iter_mut().take(r.end.min(n)).skip(r.start.min(n)) {
            *c = c.merged(&r.style);
        }
    }
    v
}

/// Runs from per-character styles (adjacent equal styles merged, unstyled characters omitted).
fn compress(v: &[CharStyle]) -> Vec<StyleRun> {
    let mut out: Vec<StyleRun> = Vec::new();
    for (i, s) in v.iter().enumerate() {
        if s.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(r) if r.end == i && r.style == *s => r.end = i + 1,
            _ => out.push(StyleRun { start: i, end: i + 1, style: s.clone() }),
        }
    }
    out
}

/// Normalise runs for a text of `n` characters: clamped, sorted, non-overlapping, merged.
pub fn normalize_runs(runs: &[StyleRun], n: usize) -> Vec<StyleRun> {
    compress(&expand(runs, n))
}

/// Merge `style` into characters `start..end` of a text of `n` characters.
pub fn apply_char_style(runs: &[StyleRun], n: usize, start: usize, end: usize, style: &CharStyle) -> Vec<StyleRun> {
    let mut v = expand(runs, n);
    for c in v.iter_mut().take(end.min(n)).skip(start.min(n)) {
        *c = c.merged(style);
    }
    compress(&v)
}

/// Remove every override from characters `start..end`.
pub fn clear_char_style(runs: &[StyleRun], n: usize, start: usize, end: usize) -> Vec<StyleRun> {
    let mut v = expand(runs, n);
    for c in v.iter_mut().take(end.min(n)).skip(start.min(n)) {
        *c = CharStyle::default();
    }
    compress(&v)
}

/// Keep runs on their characters when `old` text becomes `new` (one contiguous replacement:
/// common prefix and suffix are kept; inserted characters take the style of the character before
/// the insertion point, or after it at the very start).
pub fn adjust_runs(old: &str, new: &str, runs: &[StyleRun]) -> Vec<StyleRun> {
    if runs.is_empty() || old == new {
        return runs.to_vec();
    }
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max_suf = a.len().min(b.len()) - pre;
    let suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    let v = expand(runs, a.len());
    let inserted = b.len() - pre - suf;
    let fill = if pre > 0 { v[pre - 1].clone() } else { v.get(a.len() - suf).cloned().unwrap_or_default() };
    let mut out: Vec<CharStyle> = v[..pre].to_vec();
    out.extend(std::iter::repeat_n(fill, inserted));
    out.extend_from_slice(&v[a.len() - suf..]);
    compress(&out)
}

/// What a pinned layer follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PinTarget {
    /// The video frame (the graphic's canvas).
    #[default]
    Frame,
    /// Another layer of the same clip, by [`LayerExtra::uid`].
    Layer(u64),
}

/// Responsive Design – Position: which edges of this layer follow the same edges of the target,
/// and their distances (this layer's edge minus the target's edge, canvas pixels) captured when
/// the pin was made.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pin {
    pub to: PinTarget,
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    /// Distances `[left, top, right, bottom]`.
    pub offsets: [f64; 4],
}

impl Pin {
    pub fn any(&self) -> bool {
        self.left || self.top || self.right || self.bottom
    }
    /// A pin of `edges` (left, top, right, bottom) from layer bounds `own` to target bounds `target`.
    pub fn new(to: PinTarget, edges: [bool; 4], own: [f64; 4], target: [f64; 4]) -> Pin {
        Pin {
            to,
            left: edges[0],
            top: edges[1],
            right: edges[2],
            bottom: edges[3],
            offsets: [own[0] - target[0], own[1] - target[1], own[2] - target[2], own[3] - target[3]],
        }
    }
}

/// One layer for [`resolve_pins`]: its uid, pin, unpinned bounds `[x0, y0, x1, y1]` (canvas px)
/// and whether pinning opposite edges may resize it (shapes) or only move it (text).
#[derive(Clone, Debug)]
pub struct PinNode {
    pub uid: u64,
    pub pin: Option<Pin>,
    pub bounds: [f64; 4],
    pub resizable: bool,
}

/// New bounds of every layer after pinning (same order as `nodes`). Targets are resolved before
/// the layers pinned to them; pins in a cycle, or to a missing layer, are ignored.
pub fn resolve_pins(nodes: &[PinNode], frame: [f64; 4]) -> Vec<[f64; 4]> {
    let mut out: Vec<Option<[f64; 4]>> = vec![None; nodes.len()];
    fn solve(i: usize, nodes: &[PinNode], frame: [f64; 4], out: &mut Vec<Option<[f64; 4]>>, visiting: &mut Vec<usize>) -> [f64; 4] {
        if let Some(b) = out[i] {
            return b;
        }
        let n = &nodes[i];
        let own = n.bounds;
        let Some(pin) = n.pin.as_ref().filter(|p| p.any()) else {
            out[i] = Some(own);
            return own;
        };
        if visiting.contains(&i) {
            return own; // cycle: this pin is ignored
        }
        visiting.push(i);
        let target = match pin.to {
            PinTarget::Frame => Some(frame),
            PinTarget::Layer(uid) => nodes.iter().position(|m| m.uid == uid && uid != 0).filter(|&j| j != i).map(|j| solve(j, nodes, frame, out, visiting)),
        };
        visiting.pop();
        let Some(t) = target else {
            out[i] = Some(own);
            return own;
        };
        let axis = |lo_on: bool, hi_on: bool, lo: usize, hi: usize| -> (f64, f64) {
            let size = own[hi] - own[lo];
            match (lo_on, hi_on) {
                (true, true) if n.resizable => (t[lo] + pin.offsets[lo], t[hi] + pin.offsets[hi]),
                (true, _) => (t[lo] + pin.offsets[lo], t[lo] + pin.offsets[lo] + size),
                (false, true) => (t[hi] + pin.offsets[hi] - size, t[hi] + pin.offsets[hi]),
                (false, false) => (own[lo], own[hi]),
            }
        };
        let (x0, x1) = axis(pin.left, pin.right, 0, 2);
        let (y0, y1) = axis(pin.top, pin.bottom, 1, 3);
        let b = [x0, y0, x1, y1];
        out[i] = Some(b);
        b
    }
    let mut visiting = Vec::new();
    (0..nodes.len()).map(|i| solve(i, nodes, frame, &mut out, &mut visiting)).collect()
}

/// Roll / crawl direction (Responsive Design – Time ▸ Roll).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RollMode {
    #[default]
    Off,
    /// Moves up (credits).
    Roll,
    /// Moves right to left (tickers).
    CrawlLeft,
    /// Moves left to right.
    CrawlRight,
}

impl RollMode {
    pub const ALL: [RollMode; 4] = [RollMode::Off, RollMode::Roll, RollMode::CrawlLeft, RollMode::CrawlRight];
    pub fn label(self) -> &'static str {
        match self {
            RollMode::Off => "Off",
            RollMode::Roll => "Roll",
            RollMode::CrawlLeft => "Crawl Left",
            RollMode::CrawlRight => "Crawl Right",
        }
    }
    pub fn from_name(s: &str) -> Option<RollMode> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        RollMode::ALL.into_iter().find(|m| m.label().to_ascii_lowercase().replace(' ', "") == n || (n == "none" && *m == RollMode::Off))
    }
}

/// Roll / crawl options. Times are clip-relative durations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Roll {
    pub mode: RollMode,
    /// The content starts just outside the frame (below it for a roll, right of it for a left
    /// crawl, left of it for a right crawl); otherwise it starts where it was laid out.
    pub start_off_screen: bool,
    /// The content leaves the frame completely; otherwise it stops when its last edge reaches the
    /// frame edge (content that fits stays where it was laid out).
    pub end_off_screen: bool,
    /// Hold before moving.
    pub preroll: Tick,
    /// Accelerate over this long.
    pub ease_in: Tick,
    /// Decelerate over this long.
    pub ease_out: Tick,
    /// Hold after moving.
    pub postroll: Tick,
}

/// Fraction (0..=1) of the roll's travel at clip time `u` of a clip `d` long: still during the
/// preroll, then constant speed with linear acceleration over `ease_in` and deceleration over
/// `ease_out` (a trapezoidal speed profile), still during the postroll.
pub fn roll_progress(r: &Roll, u: Tick, d: Tick) -> f64 {
    let ta = r.preroll.0.max(0) as f64;
    let tb = (d.0 - r.postroll.0.max(0)) as f64;
    let t_total = tb - ta;
    let u = u.0 as f64;
    if t_total <= 0.0 {
        return if u <= ta { 0.0 } else { 1.0 };
    }
    let tau = (u - ta).clamp(0.0, t_total);
    let ei = (r.ease_in.0.max(0) as f64).min(t_total);
    let eo = (r.ease_out.0.max(0) as f64).min(t_total - ei);
    let v = 1.0 / (t_total - ei / 2.0 - eo / 2.0);
    let s = if tau < ei {
        v * tau * tau / (2.0 * ei)
    } else if tau <= t_total - eo {
        v * (ei / 2.0 + (tau - ei))
    } else {
        let rest = t_total - tau;
        1.0 - v * rest * rest / (2.0 * eo)
    };
    s.clamp(0.0, 1.0)
}

/// Offset (canvas px) of the whole graphic for a roll / crawl at clip time `u` of a clip `d`
/// long. `canvas` is the frame size and `content` the union of the layers' bounds
/// `[x0, y0, x1, y1]` where they were laid out.
pub fn roll_offset(r: &Roll, u: Tick, d: Tick, canvas: (u32, u32), content: [f64; 4]) -> (f64, f64) {
    let (w, h) = (canvas.0 as f64, canvas.1 as f64);
    let [x0, y0, x1, y1] = content;
    let lerp = |a: f64, b: f64| a + (b - a) * roll_progress(r, u, d);
    match r.mode {
        RollMode::Off => (0.0, 0.0),
        RollMode::Roll => {
            let from = if r.start_off_screen { h - y0 } else { 0.0 };
            let to = if r.end_off_screen { -y1 } else { (h - y1).min(0.0) };
            (0.0, lerp(from, to))
        }
        RollMode::CrawlLeft => {
            let from = if r.start_off_screen { w - x0 } else { 0.0 };
            let to = if r.end_off_screen { -x1 } else { (w - x1).min(0.0) };
            (lerp(from, to), 0.0)
        }
        RollMode::CrawlRight => {
            let from = if r.start_off_screen { -x1 } else { 0.0 };
            let to = if r.end_off_screen { w - x0 } else { (-x0).max(0.0) };
            (lerp(from, to), 0.0)
        }
    }
}

/// Graphic-level settings of a graphic clip (`TrackItem::graphic`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GraphicMeta {
    #[serde(skip_serializing_if = "is_roll_off")]
    pub roll: Roll,
    /// Responsive Design – Time: protected intro duration (from the clip start).
    pub intro: Tick,
    /// Protected outro duration (up to the clip end).
    pub outro: Tick,
    /// Media time of the clip start when the intro / outro were set; keyframes are read relative
    /// to it, so trimming the head keeps the intro playing from the new start.
    pub design_in: Tick,
    /// Clip length (media time) when the intro / outro were set.
    pub design_duration: Tick,
    /// The graphics template this graphic was made from, with its editable properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<crate::gtemplate::TemplateLink>,
}

fn is_roll_off(r: &Roll) -> bool {
    *r == Roll::default()
}

impl GraphicMeta {
    pub fn is_empty(&self) -> bool {
        *self == GraphicMeta::default()
    }
    pub fn has_responsive_time(&self) -> bool {
        (self.intro.0 > 0 || self.outro.0 > 0) && self.design_duration.0 > 0
    }
}

/// Responsive Design – Time: the media time at which to read the graphic's keyframes at
/// clip-relative time `u` of a clip `d` long. Inside the intro, time runs from `design_in`;
/// inside the outro, it runs up to the designed end; the middle is stretched linearly between
/// them. Without an intro or outro this is `design_in + u` (the plain clip time).
pub fn remap_time(m: &GraphicMeta, u: Tick, d: Tick) -> Tick {
    if !m.has_responsive_time() {
        return m.design_in + u;
    }
    let (intro, outro, dd) = (m.intro.0.max(0), m.outro.0.max(0), m.design_duration.0);
    let (u, d) = (u.0, d.0);
    let local = if u < intro {
        u
    } else if d - u <= outro {
        dd - (d - u)
    } else {
        let (a, b) = (intro as i128, (d - outro) as i128);
        let (aa, bb) = (intro as i128, (dd - outro) as i128);
        if b <= a { aa as i64 } else { (aa + (u as i128 - a) * (bb - aa) / (b - a)) as i64 }
    };
    m.design_in + Tick(local)
}

/// A source graphic (Graphics and Titles ▸ Upgrade to Source Graphic): the layers and settings
/// shared by every clip of one project item. Editing one instance updates the others.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceGraphic {
    pub layers: Vec<EffectInstance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<GraphicMeta>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_time::TICKS_PER_SECOND;

    fn sec(s: f64) -> Tick {
        Tick((s * TICKS_PER_SECOND as f64).round() as i64)
    }
    fn bold() -> CharStyle {
        CharStyle { faux_bold: Some(true), ..Default::default() }
    }

    #[test]
    fn char_styles_merge_split_and_follow_edits() {
        let runs = apply_char_style(&[], 10, 2, 6, &bold());
        assert_eq!(runs, vec![StyleRun { start: 2, end: 6, style: bold() }]);
        let red = CharStyle { fill: Some([1.0, 0.0, 0.0, 1.0]), ..Default::default() };
        let runs = apply_char_style(&runs, 10, 4, 8, &red);
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[1].start, runs[1].end), (4, 6));
        assert_eq!(runs[1].style, bold().merged(&red));
        assert_eq!((runs[2].start, runs[2].end), (6, 8));
        // clear the middle
        let c = clear_char_style(&runs, 10, 3, 7);
        assert_eq!(c.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(), vec![(2, 3), (7, 8)]);
        // typing before the run shifts it; typing inside extends it
        let r = vec![StyleRun { start: 6, end: 11, style: bold() }];
        let a = adjust_runs("Hello World", "Oh, Hello World", &r);
        assert_eq!((a[0].start, a[0].end), (10, 15));
        let b = adjust_runs("Hello World", "Hello Wonderful World", &r);
        assert_eq!((b[0].start, b[0].end), (6, 21));
        // deleting the styled word removes the run
        assert!(adjust_runs("Hello World", "Hello ", &r).is_empty());
        // runs normalise and clamp
        let n = normalize_runs(&[StyleRun { start: 5, end: 50, style: bold() }, StyleRun { start: 0, end: 5, style: bold() }], 8);
        assert_eq!(n, vec![StyleRun { start: 0, end: 8, style: bold() }]);
    }

    #[test]
    fn pins_follow_targets_and_resize_shapes() {
        // text at x 100..300, box pinned on all edges with 10 px padding
        let text = [100.0, 50.0, 300.0, 100.0];
        let boxb = [90.0, 40.0, 310.0, 110.0];
        let pin = Pin::new(PinTarget::Layer(1), [true; 4], boxb, text);
        assert_eq!(pin.offsets, [-10.0, -10.0, 10.0, 10.0]);
        // the text grows to 500 px wide: the box follows
        let nodes = vec![
            PinNode { uid: 2, pin: Some(pin.clone()), bounds: boxb, resizable: true },
            PinNode { uid: 1, pin: None, bounds: [100.0, 50.0, 600.0, 100.0], resizable: false },
        ];
        let out = resolve_pins(&nodes, [0.0, 0.0, 1920.0, 1080.0]);
        assert_eq!(out[0], [90.0, 40.0, 610.0, 110.0]);
        // text layers only move: right edge pinned keeps the distance to the target's right edge
        let p2 = Pin::new(PinTarget::Layer(1), [false, false, true, false], [320.0, 50.0, 400.0, 90.0], text);
        let nodes = vec![
            PinNode { uid: 3, pin: Some(p2), bounds: [320.0, 50.0, 400.0, 90.0], resizable: false },
            PinNode { uid: 1, pin: None, bounds: [100.0, 50.0, 600.0, 100.0], resizable: false },
        ];
        assert_eq!(resolve_pins(&nodes, [0.0; 4])[0], [620.0, 50.0, 700.0, 90.0]);
        // chains resolve in order; cycles are ignored
        let a =
            PinNode { uid: 1, pin: Some(Pin::new(PinTarget::Layer(2), [true, false, false, false], [0.0; 4], [0.0; 4])), bounds: [0.0; 4], resizable: false };
        let b =
            PinNode { uid: 2, pin: Some(Pin::new(PinTarget::Layer(1), [true, false, false, false], [0.0; 4], [0.0; 4])), bounds: [0.0; 4], resizable: false };
        let _ = resolve_pins(&[a, b], [0.0; 4]);
        // pinned to the frame: a different frame size moves it
        let f = Pin::new(PinTarget::Frame, [false, false, true, true], [1700.0, 900.0, 1820.0, 1000.0], [0.0, 0.0, 1920.0, 1080.0]);
        let n = [PinNode { uid: 1, pin: Some(f), bounds: [1700.0, 900.0, 1820.0, 1000.0], resizable: false }];
        assert_eq!(resolve_pins(&n, [0.0, 0.0, 1280.0, 720.0])[0], [1060.0, 540.0, 1180.0, 640.0]);
    }

    #[test]
    fn roll_progress_is_exact() {
        let d = sec(10.0);
        let mut r = Roll { mode: RollMode::Roll, ..Default::default() };
        // linear without easing
        assert_eq!(roll_progress(&r, Tick::ZERO, d), 0.0);
        assert!((roll_progress(&r, sec(2.5), d) - 0.25).abs() < 1e-12);
        assert_eq!(roll_progress(&r, d, d), 1.0);
        // preroll 2 s, postroll 2 s: still, then 6 s of travel
        r.preroll = sec(2.0);
        r.postroll = sec(2.0);
        assert_eq!(roll_progress(&r, sec(1.0), d), 0.0);
        assert!((roll_progress(&r, sec(5.0), d) - 0.5).abs() < 1e-12);
        assert_eq!(roll_progress(&r, sec(9.0), d), 1.0);
        // ease in 2 s, ease out 2 s over 6 s: v = 1/(6-1-1) = 0.25/s
        r.ease_in = sec(2.0);
        r.ease_out = sec(2.0);
        assert!((roll_progress(&r, sec(3.0), d) - 0.25 * 1.0 / (2.0 * 2.0)).abs() < 1e-12);
        assert!((roll_progress(&r, sec(4.0), d) - 0.25).abs() < 1e-12);
        assert!((roll_progress(&r, sec(5.0), d) - 0.5).abs() < 1e-12);
        assert!((roll_progress(&r, sec(7.0), d) - (1.0 - 0.25 / 4.0)).abs() < 1e-12);
    }

    #[test]
    fn roll_and_crawl_offsets() {
        let d = sec(4.0);
        let canvas = (1920, 1080);
        let content = [200.0, 100.0, 1700.0, 2500.0];
        let r = Roll { mode: RollMode::Roll, start_off_screen: true, end_off_screen: true, ..Default::default() };
        // starts with the top at the frame bottom, ends with the bottom at the frame top
        assert_eq!(roll_offset(&r, Tick::ZERO, d, canvas, content), (0.0, 980.0));
        assert_eq!(roll_offset(&r, d, d, canvas, content), (0.0, -2500.0));
        assert_eq!(roll_offset(&r, sec(2.0), d, canvas, content), (0.0, (980.0 - 2500.0) / 2.0));
        // not ending off screen: stops when the bottom reaches the frame bottom
        let r2 = Roll { end_off_screen: false, ..r.clone() };
        assert_eq!(roll_offset(&r2, d, d, canvas, content), (0.0, 1080.0 - 2500.0));
        // short content without off-screen options does not move
        let r3 = Roll { mode: RollMode::Roll, ..Default::default() };
        assert_eq!(roll_offset(&r3, d, d, canvas, [0.0, 100.0, 10.0, 200.0]), (0.0, 0.0));
        let cl = Roll { mode: RollMode::CrawlLeft, start_off_screen: true, end_off_screen: true, ..Default::default() };
        let line = [50.0, 900.0, 4050.0, 980.0];
        assert_eq!(roll_offset(&cl, Tick::ZERO, d, canvas, line), (1870.0, 0.0));
        assert_eq!(roll_offset(&cl, d, d, canvas, line), (-4050.0, 0.0));
        let cr = Roll { mode: RollMode::CrawlRight, ..cl.clone() };
        assert_eq!(roll_offset(&cr, Tick::ZERO, d, canvas, line), (-4050.0, 0.0));
        assert_eq!(roll_offset(&cr, d, d, canvas, line), (1870.0, 0.0));
        assert_eq!(RollMode::from_name("crawl left"), Some(RollMode::CrawlLeft));
    }

    #[test]
    fn responsive_time_protects_intro_and_outro() {
        let m = GraphicMeta { intro: sec(1.0), outro: sec(1.0), design_in: Tick::ZERO, design_duration: sec(5.0), ..Default::default() };
        // design length: identity
        for s in [0.0, 0.5, 1.0, 2.5, 4.0, 4.9] {
            assert_eq!(remap_time(&m, sec(s), sec(5.0)), sec(s));
        }
        // extended to 9 s: intro unchanged, outro aligned to the end, middle stretched 3 s → 7 s
        assert_eq!(remap_time(&m, sec(0.5), sec(9.0)), sec(0.5));
        assert_eq!(remap_time(&m, sec(8.5), sec(9.0)), sec(4.5));
        assert_eq!(remap_time(&m, sec(8.0), sec(9.0)), sec(4.0));
        assert_eq!(remap_time(&m, sec(4.5), sec(9.0)), sec(2.5));
        // trimmed to 3 s: intro and outro still play in full, the middle shrinks
        assert_eq!(remap_time(&m, sec(0.75), sec(3.0)), sec(0.75));
        assert_eq!(remap_time(&m, sec(2.25), sec(3.0)), sec(4.25));
        // no responsive time: plain clip time from design_in
        let plain = GraphicMeta { design_in: sec(1.0), ..Default::default() };
        assert_eq!(remap_time(&plain, sec(2.0), sec(3.0)), sec(3.0));
    }
}

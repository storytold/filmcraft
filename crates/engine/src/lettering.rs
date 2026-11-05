//! Lettering (`lettering.*`): text that becomes the piece in a moment of the video.
//!
//! A caption carries speech; a lettering makes the key words the picture. This module is the
//! designer's half of it, with no AI and no network:
//!
//! - `hierarchy` ranks the words of a phrase (numbers, caps, negations, long or stretched words,
//!   words before a pause; articles and prepositions never) and picks the 1–3 hero words.
//! - `compose` breaks the phrase into at most three lines with no widow, balanced by minimum
//!   raggedness (the cost TeX's paragraph builder and `text-wrap: balance` minimise), never
//!   breaking after an article or preposition, and sets the heroes bigger. It returns one rectangle
//!   per word, so each word can become its own animated layer.
//! - `lettering.plan` exposes both as a command that only reports (nothing is edited).
//!
//! Widths are measured with the real shaper (`filmcraft_text::measure`), so kerning and the
//! studio's font are honoured. The layout itself works on plain numbers ([`WordMetric`]) so it is
//! testable without any font.

use serde::Serialize;
use serde_json::{Value, json};

use filmcraft_geom::Vec2;
use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
use filmcraft_project::{EffectInstance, Interpolation, Keyframe, Mask, MaskPath, Param, ParamValue};
use filmcraft_text::TextStyle;
use filmcraft_time::Tick;

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

/// Smallest fraction of the base size a lettering may shrink to before it asks to be split.
pub const MIN_SCALE: f32 = 0.55;
/// Most lines a lettering may have.
pub const MAX_LINES: usize = 3;

// ---------------------------------------------------------------------------------------------
// Hierarchy
// ---------------------------------------------------------------------------------------------

/// A word of the phrase, with the time it is spoken at (seconds; all zero when there is no speech).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// PT-BR words that carry no weight on their own: articles, prepositions, conjunctions, pronouns,
/// the commonest auxiliaries. They are never heroes and a line never ends on the ones that bind
/// to the next word (see [`binds_forward`]).
const STOP: &[&str] = &[
    "a", "o", "as", "os", "um", "uma", "uns", "umas", "de", "do", "da", "dos", "das", "em", "no", "na", "nos", "nas", "por", "pelo", "pela", "pelos", "pelas", "para",
    "pra", "pro", "com", "e", "ou", "mas", "que", "se", "ao", "aos", "à", "às", "num", "numa", "me", "te", "lhe", "nos", "vos", "meu", "minha", "seu", "sua", "teu", "tua",
    "isso", "isto", "esse", "essa", "este", "esta", "aquele", "aquela", "é", "são", "foi", "era", "ser", "tem", "ter", "há", "já", "só", "também", "como", "quando", "onde",
    "então", "aí", "né", "tipo", "ele", "ela", "eles", "elas", "eu", "tu", "você", "vocês", "nós", "lá", "aqui", "até", "ainda", "porque", "pois", "sobre", "entre", "sem",
    "the", "an", "of", "to", "in", "on", "and", "or", "is", "are", "was", "it", "for", "at", "by", "with", "you", "your",
];

/// Negations: small words that flip the sense, so they weigh like content words.
const NEGATION: &[&str] = &["não", "nunca", "jamais", "nada", "ninguém", "nenhum", "nenhuma", "sem", "never", "no", "not", "nothing"];

/// Words a line must not end on: they belong to what follows ("de", "o", "para", "e", "que", …).
/// Negations bind forward too ("não faça"), as do the numeral articles.
fn binds_forward(norm: &str) -> bool {
    (STOP.contains(&norm) && !matches!(norm, "isso" | "isto" | "ele" | "ela" | "eles" | "elas" | "aqui" | "lá" | "já" | "só" | "ainda" | "também" | "é" | "são" | "foi" | "era" | "tem" | "há" | "eu" | "tu"))
        || NEGATION.contains(&norm)
}

/// The word as compared: lower case, edge punctuation off.
pub fn normalize(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '%' && c != '$').to_lowercase()
}

fn has_digit(w: &str) -> bool {
    w.chars().any(|c| c.is_ascii_digit())
}

fn all_caps(w: &str) -> bool {
    let letters: Vec<char> = w.chars().filter(|c| c.is_alphabetic()).collect();
    letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase())
}

/// How much a word deserves to be a hero. Positive = weighty. Rules, not a model: every term is
/// one the editor can name.
pub fn score(words: &[Word], i: usize) -> f32 {
    let w = &words[i];
    let n = normalize(&w.text);
    if n.is_empty() {
        return -10.0;
    }
    let mut s = 0.0f32;
    if NEGATION.contains(&n.as_str()) {
        s += 2.6;
    } else if STOP.contains(&n.as_str()) {
        s -= 3.0;
    }
    if has_digit(&w.text) || w.text.contains('%') || w.text.contains('$') {
        s += 3.0;
    }
    if all_caps(&w.text) {
        s += 2.5;
    }
    let len = n.chars().count();
    s += match len {
        0..=3 => -0.4,
        4..=5 => 0.5,
        6..=7 => 1.2,
        _ => 1.8,
    };
    // Speech: a stretched word is a stressed one; a word before a pause closes an idea.
    let dur = (w.end - w.start).max(0.0);
    if dur > 0.0 && len > 0 && dur / len as f64 > 0.095 {
        s += 1.0;
    }
    if let Some(next) = words.get(i + 1) {
        if next.start - w.end >= 0.35 {
            s += 1.0;
        }
    }
    if i > 0 && w.start - words[i - 1].end >= 0.5 {
        s += 0.5;
    }
    if w.text.ends_with('!') || w.text.ends_with('?') {
        s += 0.5;
    }
    // The last content word of a phrase usually carries it ("…sistema de edição mais RÁPIDO").
    if i > 0 && words[i + 1..].iter().all(|x| STOP.contains(&normalize(&x.text).as_str()) && !NEGATION.contains(&normalize(&x.text).as_str())) {
        s += 0.4;
    }
    s
}

/// Indices of the hero words: the best-scoring ones above a threshold, at most one in four words
/// (1 for up to 4 words, 2 up to 9, 3 beyond), so the accent colour stays near 10% of the text.
/// Two neighbouring heroes read as one phrase ("LINHAS GUIAS") and count as one pick.
pub fn heroes(words: &[Word]) -> Vec<usize> {
    let cap = match words.len() {
        0..=4 => 1,
        5..=9 => 2,
        _ => 3,
    };
    let scores: Vec<(usize, f32)> = (0..words.len()).map(|i| (i, score(words, i))).collect();
    let mut ranked: Vec<(usize, f32)> = scores.iter().copied().filter(|(_, s)| *s >= 1.5).collect();
    if ranked.is_empty() {
        // A lettering always has a centre: with no clear winner, the best content word takes it.
        ranked = scores.iter().copied().filter(|(_, s)| *s >= 0.8).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
        ranked.truncate(1);
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut picked: Vec<usize> = Vec::new();
    let mut groups = 0;
    for (i, _) in ranked {
        let joins = picked.iter().any(|&p| p + 1 == i || i + 1 == p);
        if !joins {
            if groups == cap {
                continue;
            }
            groups += 1;
        }
        picked.push(i);
    }
    picked.sort_unstable();
    picked
}

// ---------------------------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Composition {
    /// Lines share the same guides: left edge flush, long lines tracked out to the widest one.
    Block,
    /// Each line steps forward along the brand's 8° diagonal.
    Stair,
}

/// What the layout needs to know about one word: its width at size 100 and its role.
#[derive(Clone, Debug, PartialEq)]
pub struct WordMetric {
    pub width100: f32,
    pub hero: bool,
    /// The word must not end a line (article, preposition, negation, number before its unit).
    pub binds_next: bool,
    /// The word ends a clause: a break after it is welcome.
    pub clause_end: bool,
}

#[derive(Clone, Debug)]
pub struct Params {
    pub box_w: f32,
    pub box_h: f32,
    /// Size of the support words, px.
    pub size: f32,
    /// Hero size = `size` × this.
    pub hero_scale: f32,
    /// Space between words at size 100.
    pub space100: f32,
    /// Baseline-to-baseline distance as a fraction of the line's biggest size.
    pub leading: f32,
    pub composition: Composition,
    /// Horizontal step of each line in [`Composition::Stair`], in em of the support size.
    pub stair_step: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self { box_w: 880.0, box_h: 1200.0, size: 64.0, hero_scale: 1.7, space100: 26.0, leading: 0.98, composition: Composition::Block, stair_step: 0.9 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Placed {
    pub index: usize,
    pub line: usize,
    /// Left edge, px from the box's left.
    pub x: f32,
    /// First baseline of the word, px from the box's top.
    pub baseline: f32,
    pub size: f32,
    pub width: f32,
    pub hero: bool,
    /// Extra tracking (1/1000 em) the word needs to fill a justified line.
    pub tracking: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Composed {
    pub lines: Vec<Vec<usize>>,
    pub words: Vec<Placed>,
    /// Final scale applied to the requested sizes (1 = untouched).
    pub scale: f32,
    pub width: f32,
    pub height: f32,
    /// True when the phrase cannot fit in 3 lines above [`MIN_SCALE`]; `split_at` says where to cut.
    pub overflow: bool,
    pub split_at: Option<usize>,
}

fn size_of(m: &WordMetric, p: &Params) -> f32 {
    if m.hero { p.size * p.hero_scale } else { p.size }
}

fn word_w(m: &WordMetric, p: &Params) -> f32 {
    m.width100 * size_of(m, p) / 100.0
}

/// Space between word `i - 1` and word `i` at the requested sizes. Next to a hero the space is
/// tighter: it is set at the support size, and the hero's own side bearings already add air.
fn gap(ms: &[WordMetric], p: &Params, i: usize) -> f32 {
    let base = p.space100 * p.size / 100.0;
    if ms[i - 1].hero || ms[i].hero { base * 0.7 } else { base }
}

/// Width of words `a..b` set on one line at the requested sizes.
fn line_w(ms: &[WordMetric], p: &Params, a: usize, b: usize) -> f32 {
    let mut w = 0.0;
    for (k, m) in ms[a..b].iter().enumerate() {
        if k > 0 {
            w += gap(ms, p, a + k);
        }
        w += word_w(m, p);
    }
    w
}

/// Cost of one way to break the phrase after the word indices in `cuts` (ascending, exclusive ends).
/// `None` = not allowed. Lower is better.
fn partition_cost(ms: &[WordMetric], p: &Params, ends: &[usize]) -> Option<(f32, f32)> {
    let mut starts = vec![0usize];
    starts.extend_from_slice(&ends[..ends.len() - 1]);
    let widths: Vec<f32> = starts.iter().zip(ends).map(|(&a, &b)| line_w(ms, p, a, b)).collect();
    let widest = widths.iter().copied().fold(0.0, f32::max);
    if widest <= 0.0 {
        return None;
    }
    // Stair adds a step per line, which has to fit as well.
    let stair = if p.composition == Composition::Stair { p.stair_step * p.size * (ends.len() - 1) as f32 } else { 0.0 };
    let fit = (p.box_w / (widest + stair)).min(1.0);
    if fit < MIN_SCALE {
        return None;
    }
    let mut cost = 0.0f32;
    let last = ends.len() - 1;
    // Widow: the last line may not be a lone support word. A lone hero, or a lone word that is at
    // least 45% as wide as the widest line, closes the phrase well.
    let (a, b) = (starts[last], ends[last]);
    if ends.len() > 1 && b - a == 1 && !ms[a].hero && widths[last] < 0.45 * widest {
        return None;
    }
    // A lone support word in the middle of a block is also a stray.
    for k in 1..last {
        let (a, b) = (starts[k], ends[k]);
        if b - a == 1 && !ms[a].hero && widths[k] < 0.35 * widest {
            cost += 40.0;
        }
    }
    // Minimum raggedness: squared slack of every line against the widest, relative to the box.
    for (k, w) in widths.iter().enumerate() {
        let slack = (widest - w) / p.box_w.max(1.0);
        let weight = if k == last { 0.5 } else { 1.0 };
        cost += 100.0 * slack * slack * weight;
    }
    // A break after a word that binds forward, or inside a number+unit pair.
    for &e in &ends[..last] {
        if ms[e - 1].binds_next {
            cost += 60.0;
        }
        if ms[e - 1].clause_end {
            cost -= 6.0;
        }
    }
    // Fewer lines read faster; more shrinking costs.
    cost += 4.0 * ends.len() as f32;
    cost += 120.0 * (1.0 - fit);
    Some((cost, fit))
}

/// Places the words of one phrase. `ms[i]` describes word `i`.
pub fn compose(ms: &[WordMetric], p: &Params) -> Composed {
    let n = ms.len();
    if n == 0 {
        return Composed { lines: vec![], words: vec![], scale: 1.0, width: 0.0, height: 0.0, overflow: false, split_at: None };
    }
    // Every set of `k-1` interior break points, k = 1..=MAX_LINES.
    let mut best: Option<(f32, f32, Vec<usize>)> = None;
    let mut consider = |ends: Vec<usize>| {
        if let Some((c, fit)) = partition_cost(ms, p, &ends) {
            if best.as_ref().is_none_or(|b| c < b.0) {
                best = Some((c, fit, ends));
            }
        }
    };
    consider(vec![n]);
    for a in 1..n {
        consider(vec![a, n]);
        for b in a + 1..n {
            consider(vec![a, b, n]);
        }
    }
    let Some((_, mut fit, ends)) = best else {
        // Does not fit above the minimum scale: say where to cut it into two letterings.
        return overflow(ms, p);
    };
    let mut starts = vec![0usize];
    starts.extend_from_slice(&ends[..ends.len() - 1]);
    let widths: Vec<f32> = starts.iter().zip(&ends).map(|(&a, &b)| line_w(ms, p, a, b)).collect();
    let widest = widths.iter().copied().fold(0.0, f32::max);

    // Height: shrink further when the lines do not fit the box height.
    let line_h = |k: usize, s: f32| {
        let big = ms[starts[k]..ends[k]].iter().map(|m| size_of(m, p)).fold(0.0, f32::max);
        big * s * p.leading
    };
    let total_h = |s: f32| (0..ends.len()).map(|k| line_h(k, s)).sum::<f32>();
    while total_h(fit) > p.box_h && fit > MIN_SCALE {
        fit *= 0.97;
    }
    if total_h(fit) > p.box_h * 1.001 {
        return overflow(ms, p);
    }

    let mut words = Vec::with_capacity(n);
    let mut baseline = 0.0f32;
    let block_w = widest * fit;
    for (k, (&a, &b)) in starts.iter().zip(&ends).enumerate() {
        let big = ms[a..b].iter().map(|m| size_of(m, p)).fold(0.0, f32::max) * fit;
        baseline += if k == 0 { big * 0.8 } else { big * p.leading };
        let lw = widths[k] * fit;
        let x0 = match p.composition {
            Composition::Block => 0.0,
            Composition::Stair => p.stair_step * p.size * fit * k as f32,
        };
        // Block: a line close to the widest is tracked out to it, so the guides hold on both edges.
        let gaps = (b - a).saturating_sub(1);
        let justify = p.composition == Composition::Block && lw >= 0.88 * block_w && lw < block_w;
        let mut extra_per_gap = if justify && gaps > 0 { (block_w - lw) / gaps as f32 } else { 0.0 };
        // Stretching is for a line that is nearly there. Past a fifth of the type size per gap the
        // words drift apart and read as separate pieces: leave the line flush-left instead.
        if extra_per_gap > 0.2 * p.size * fit {
            extra_per_gap = 0.0;
        }
        let mut x = x0;
        for (j, i) in (a..b).enumerate() {
            let s = size_of(&ms[i], p) * fit;
            let w = word_w(&ms[i], p) * fit;
            words.push(Placed { index: i, line: k, x, baseline, size: s, width: w, hero: ms[i].hero, tracking: 0.0 });
            x += w + if i + 1 < b { gap(ms, p, i + 1) * fit } else { 0.0 } + if j < gaps { extra_per_gap } else { 0.0 };
        }
    }
    let width = words.iter().map(|w| w.x + w.width).fold(0.0, f32::max);
    let height = baseline + ms.iter().map(|m| size_of(m, p)).fold(0.0, f32::max) * fit * 0.25;
    Composed { lines: starts.iter().zip(&ends).map(|(&a, &b)| (a..b).collect()).collect(), words, scale: fit, width, height, overflow: false, split_at: None }
}

/// The phrase cannot be set in 3 lines at a readable size: cut it where it reads best (after the
/// clause end nearest the middle, never after a word that binds forward).
fn overflow(ms: &[WordMetric], p: &Params) -> Composed {
    let total: f32 = line_w(ms, p, 0, ms.len());
    let mut best = (f32::MAX, ms.len() / 2);
    for i in 1..ms.len() {
        let mut c = (line_w(ms, p, 0, i) - total / 2.0).abs();
        if ms[i - 1].binds_next {
            c += total;
        }
        if ms[i - 1].clause_end {
            c -= total * 0.1;
        }
        if c < best.0 {
            best = (c, i);
        }
    }
    Composed { lines: vec![], words: vec![], scale: 1.0, width: 0.0, height: 0.0, overflow: true, split_at: Some(best.1.max(1)) }
}

// ---------------------------------------------------------------------------------------------
// Looks
// ---------------------------------------------------------------------------------------------

/// How a word arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reveal {
    /// Slides in along the 8° diagonal while fading in (the first version).
    Slide,
    /// Rises from under a line only its own box shows: a mask wipe, no fade.
    Mask,
}

/// A look: everything about the lettering that is taste, not layout.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub name: &'static str,
    /// Support size as a fraction of the frame width.
    pub size_k: f32,
    pub hero_scale: f32,
    pub leading: f32,
    pub sup_face: &'static str,
    pub hero_face: &'static str,
    pub sup_caps: bool,
    pub hero_caps: bool,
    /// 1/1000 em.
    pub sup_track: f64,
    pub hero_track: f64,
    pub hero_italic: bool,
    pub composition: Composition,
    pub reveal: Reveal,
    /// A thin guide line over the block, drawn in with the words.
    pub rule: bool,
    /// Slow drift after settling, as a fraction of the frame width over the clip (heroes move more).
    pub drift: f64,
    /// Heroes land from this scale (1 = no scale move).
    pub hero_from_scale: f64,
}

impl Look {
    /// The first version: kept so nothing already applied changes.
    pub fn classic() -> Self {
        Self { name: "classic", size_k: 0.06, hero_scale: 1.7, leading: 0.98, sup_face: "Light", hero_face: "ExtraBold", sup_caps: false, hero_caps: false, sup_track: 0.0, hero_track: 0.0, hero_italic: false, composition: Composition::Block, reveal: Reveal::Slide, rule: false, drift: 0.0, hero_from_scale: 1.0 }
    }
    /// A: one huge word and little else; tight; every word rises from behind a mask.
    pub fn monumental() -> Self {
        Self { name: "monumental", size_k: 0.04, hero_scale: 2.7, leading: 0.84, sup_face: "Regular", hero_face: "ExtraBold", sup_caps: false, hero_caps: false, sup_track: 10.0, hero_track: -30.0, hero_italic: false, composition: Composition::Block, reveal: Reveal::Mask, rule: false, drift: 0.0, hero_from_scale: 1.0 }
    }
    /// B: small tracked capitals against an italic giant, a guide line over it all.
    pub fn editorial() -> Self {
        Self { name: "editorial", size_k: 0.028, hero_scale: 3.3, leading: 0.9, sup_face: "Bold", hero_face: "ExtraBold", sup_caps: true, hero_caps: false, sup_track: 140.0, hero_track: -25.0, hero_italic: true, composition: Composition::Block, reveal: Reveal::Mask, rule: true, drift: 0.0, hero_from_scale: 1.0 }
    }
    /// C: the stair on the 8° diagonal, big heroes that land from a push-in, a slow drift after.
    pub fn cinetico() -> Self {
        Self { name: "cinetico", size_k: 0.04, hero_scale: 2.6, leading: 0.88, sup_face: "Regular", hero_face: "ExtraBold", sup_caps: false, hero_caps: false, sup_track: 0.0, hero_track: -30.0, hero_italic: false, composition: Composition::Stair, reveal: Reveal::Slide, rule: false, drift: 0.012, hero_from_scale: 1.18 }
    }
    pub fn by_name(n: &str) -> Option<Self> {
        match n {
            "classic" => Some(Self::classic()),
            "monumental" => Some(Self::monumental()),
            "editorial" => Some(Self::editorial()),
            "cinetico" => Some(Self::cinetico()),
            _ => None,
        }
    }
    /// The text as set (capitalised when the look says so).
    pub fn shown(&self, text: &str, hero: bool) -> String {
        if (hero && self.hero_caps) || (!hero && self.sup_caps) { text.to_uppercase() } else { text.to_string() }
    }
}

// ---------------------------------------------------------------------------------------------
// Measuring with the real font
// ---------------------------------------------------------------------------------------------

/// Where the lettering gets its type: the studio's Host Grotesk, support light and hero extra bold.
#[derive(Clone, Debug)]
pub struct Faces {
    pub family: String,
    pub support: String,
    pub hero: String,
}

impl Default for Faces {
    fn default() -> Self {
        Self { family: "Host Grotesk Lettering".into(), support: "Light".into(), hero: "ExtraBold".into() }
    }
}

fn style(family: &str, face: &str) -> TextStyle {
    TextStyle { family: family.into(), style: face.into(), size: 100.0, ..Default::default() }
}

/// Builds the metrics of `words` (the hero flags decide which face each is measured in).
pub fn measure_words(words: &[Word], hero: &[bool], faces: &Faces) -> (Vec<WordMetric>, f32) {
    measure_words_with(words, hero, faces, &Look::classic())
}

/// As [`measure_words`], with the look's capitalisation and tracking (1/1000 em) in the widths.
pub fn measure_words_with(words: &[Word], hero: &[bool], faces: &Faces, look: &Look) -> (Vec<WordMetric>, f32) {
    let sup = style(&faces.family, &faces.support);
    let her = style(&faces.family, &faces.hero);
    let space = filmcraft_text::measure("a a", &sup) - filmcraft_text::measure("aa", &sup);
    let ms = words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let n = normalize(&w.text);
            let shown = look.shown(&w.text, hero[i]);
            let track = if hero[i] { look.hero_track } else { look.sup_track };
            WordMetric {
                width100: filmcraft_text::measure(&shown, if hero[i] { &her } else { &sup }) + track as f32 / 10.0 * shown.chars().count() as f32,
                hero: hero[i],
                // a number binds to its unit ("10 mil", "3 vezes")
                binds_next: binds_forward(&n) || (has_digit(&w.text) && words.get(i + 1).is_some_and(|x| x.text.chars().all(|c| c.is_alphabetic()) && x.text.chars().count() <= 4)),
                clause_end: w.text.ends_with([',', '.', ';', ':', '?', '!', '…', '—']),
            }
        })
        .collect();
    (ms, space)
}

// ---------------------------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------------------------

/// `"word word"` → words with no timing.
fn words_from_text(text: &str) -> Vec<Word> {
    text.split_whitespace().map(|t| Word { text: t.into(), start: 0.0, end: 0.0 }).collect()
}

fn words_param(p: &Value) -> Vec<Word> {
    match p.get("words").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|w| {
                let t = w.get("text").and_then(Value::as_str)?;
                Some(Word { text: t.into(), start: w.get("start").and_then(Value::as_f64).unwrap_or(0.0), end: w.get("end").and_then(Value::as_f64).unwrap_or(0.0) })
            })
            .collect(),
        None => words_from_text(str_p(p, "text").unwrap_or("")),
    }
}

/// The full plan of one lettering: heroes, lines, a rectangle per word.
pub fn plan(p: &Value) -> Result<Value> {
    let words = words_param(p);
    if words.is_empty() {
        return Err(bad("lettering.plan", "give `text` or `words`"));
    }
    let hero_idx: Vec<usize> = match p.get("heroes").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(|u| u as usize)).filter(|&i| i < words.len()).collect(),
        None => heroes(&words),
    };
    let mut hero = vec![false; words.len()];
    for &i in &hero_idx {
        hero[i] = true;
    }
    filmcraft_text::fonts::scan_system();
    let mut faces = Faces::default();
    if let Some(v) = str_p(p, "family") {
        faces.family = v.into();
    }
    if let Some(v) = str_p(p, "supportStyle") {
        faces.support = v.into();
    }
    if let Some(v) = str_p(p, "heroStyle") {
        faces.hero = v.into();
    }
    let (ms, space100) = measure_words(&words, &hero, &faces);
    let def = Params::default();
    let bx = p.get("box").and_then(Value::as_array).filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)));
    let params = Params {
        box_w: bx.map_or(def.box_w, |b| b.0 as f32),
        box_h: bx.map_or(def.box_h, |b| b.1 as f32),
        size: f64_p(p, "size").map_or(def.size, |v| v as f32),
        hero_scale: f64_p(p, "heroScale").map_or(def.hero_scale, |v| v as f32),
        space100,
        leading: f64_p(p, "leading").map_or(def.leading, |v| v as f32),
        composition: if str_p(p, "composition") == Some("stair") { Composition::Stair } else { Composition::Block },
        stair_step: f64_p(p, "stairStep").map_or(def.stair_step, |v| v as f32),
    };
    if params.box_w <= 0.0 || params.box_h <= 0.0 || params.size <= 0.0 {
        return Err(bad("lettering.plan", "box and size must be positive"));
    }
    let c = compose(&ms, &params);
    let lines: Vec<String> = c.lines.iter().map(|l| l.iter().map(|&i| words[i].text.as_str()).collect::<Vec<_>>().join(" ")).collect();
    Ok(json!({
        "words": words.iter().enumerate().map(|(i, w)| json!({"index": i, "text": w.text, "hero": hero[i], "score": score(&words, i)})).collect::<Vec<_>>(),
        "faces": {"family": faces.family, "support": faces.support, "hero": faces.hero},
        "lines": lines,
        "layout": c,
    }))
}


// ---------------------------------------------------------------------------------------------
// Apply: one animated graphic clip, one text layer per word
// ---------------------------------------------------------------------------------------------

/// The studio's colours (`Empresa/Branding/Paleta de Cores.md`).
const TERRACOTA: &str = "#CE4E21";
const OFF_WHITE: &str = "#FFFCF7";
const PRETO: &str = "#000000";

/// "Entra com convicção. Fica em paz." Quick departure, long settle, no overshoot: the brand's
/// `cubic-bezier(0.16, 1, 0.3, 1)` as the app's temporal ease (a flat start handle of 8% and a
/// settle handle of 85%).
const ENTER_OUT: f64 = 0.08;
const ENTER_IN: f64 = 0.85;
/// The brand's exit, `cubic-bezier(0.55, 0, 0.25, 1)`, is exactly a 55% / 75% temporal ease.
const EXIT_OUT: f64 = 0.55;
const EXIT_IN: f64 = 0.75;
/// The brand's italic is 8° and the gesture always goes forward.
const FORWARD_DEG: f64 = 8.0;

/// Timing of the animation, seconds (the brand's `DURACAO`).
const ENTER_SECS: f64 = 0.5;
const STAGGER_SECS: f64 = 0.09;
const EXIT_SECS: f64 = 0.3;
/// A word enters this long before it is said, so the eye is there when the voice arrives.
const LEAD_SECS: f64 = 0.1;
/// How long the finished lettering stays after the last word.
const HOLD_SECS: f64 = 0.8;

fn color(hex: &str) -> ParamValue {
    ParamValue::Color(filmcraft_color::parse_hex(hex).unwrap_or([1.0, 1.0, 1.0, 1.0]))
}

fn kf(t: f64, value: ParamValue, interp: Interpolation, out_influence: f64, in_influence: f64) -> Keyframe {
    let mut k = Keyframe::new(Tick::from_seconds_f64(t), value);
    k.interp = interp;
    k.out_influence = out_influence;
    k.in_influence = in_influence;
    k
}

/// Where a word sits and how big it is.
struct Geo {
    /// Left of the baseline, canvas px.
    pos: Vec2,
    size: f64,
    width: f64,
    hero: bool,
    /// Frame width, px.
    fw: f64,
}

fn bez(t: f64, v: ParamValue) -> Keyframe {
    kf(t, v, Interpolation::Bezier, ENTER_OUT, ENTER_IN)
}

/// A word's keyframes: it arrives at `t0` (clip seconds) and leaves with everyone else at `exit`.
fn animate(layer: &mut EffectInstance, g: &Geo, t0: f64, exit: f64, how: &str, look: &Look) {
    let (pos, size) = (g.pos, g.size);
    let enter_end = t0 + ENTER_SECS;
    let end = exit + EXIT_SECS;
    // slow drift once settled: heroes travel twice as far as the support words, a little parallax
    let drift = Vec2::new(-look.drift * g.fw * if g.hero { 1.0 } else { 0.5 }, 0.0);
    let rest = pos + drift;
    let move_keys = |from: Vec2| -> Vec<Keyframe> {
        let mut v = vec![bez(t0, ParamValue::Vec2(from)), kf(enter_end, ParamValue::Vec2(pos), Interpolation::Linear, 0.33, 0.33)];
        if look.drift != 0.0 && end > enter_end {
            v[1] = kf(enter_end, ParamValue::Vec2(pos), Interpolation::Bezier, 0.33, 0.33);
            v.push(kf(end, ParamValue::Vec2(rest), Interpolation::Linear, 0.33, 0.33));
        }
        v
    };
    let fade_out = |peak_first: Vec<Keyframe>| -> Vec<Keyframe> {
        let mut v = peak_first;
        v.push(kf(exit, ParamValue::Float(100.0), Interpolation::Bezier, EXIT_OUT, EXIT_IN));
        v.push(kf(end, ParamValue::Float(0.0), Interpolation::Linear, 0.33, EXIT_IN));
        v
    };
    match look.reveal {
        Reveal::Mask => {
            // The word stays on its baseline; the window it shows through opens from the bottom up
            // while the word rises a little inside it. Only the exit fades.
            let rise = Vec2::new(pos.x, pos.y + size * 0.55);
            layer.params.insert("position".into(), Param { value: ParamValue::Vec2(pos), keyframes: move_keys(rise) });
            let (x0, x1) = (pos.x - size * 0.3, pos.x + g.width + size * 0.35);
            let (top, bottom) = (pos.y - size * 1.0, pos.y + size * 0.4);
            let closed = MaskPath::rect(x0, bottom, x1, bottom);
            let open = MaskPath::rect(x0, top, x1, bottom);
            let mut m = Mask::new("Revela", open.clone());
            m.feather = Param::new(ParamValue::Float(0.0));
            m.path = Param { value: ParamValue::Path(open.clone()), keyframes: vec![bez(t0, ParamValue::Path(closed)), kf(t0 + ENTER_SECS * 0.8, ParamValue::Path(open), Interpolation::Linear, 0.33, 0.33)] };
            layer.masks.push(m);
            layer.params.insert("opacity".into(), Param { value: ParamValue::Float(100.0), keyframes: fade_out(vec![kf(t0, ParamValue::Float(100.0), Interpolation::Linear, 0.33, 0.33)]) });
        }
        Reveal::Slide => {
            let from = match how {
                "up" => Vec2::new(pos.x, pos.y + size * 0.3),
                // along the brand's 8° diagonal, coming from behind: always forward
                _ => {
                    let d = size * 0.45;
                    let r = FORWARD_DEG.to_radians();
                    Vec2::new(pos.x - d * r.cos(), pos.y + d * r.sin())
                }
            };
            if how != "fade" {
                layer.params.insert("position".into(), Param { value: ParamValue::Vec2(pos), keyframes: move_keys(from) });
            }
            layer.params.insert(
                "opacity".into(),
                Param { value: ParamValue::Float(100.0), keyframes: fade_out(vec![bez(t0, ParamValue::Float(0.0)), kf(enter_end, ParamValue::Float(100.0), Interpolation::Linear, 0.33, 0.33)]) },
            );
        }
    }
    // heroes land from a push-in: bigger, settling to size
    if g.hero && look.hero_from_scale != 1.0 {
        layer.params.insert(
            "scale".into(),
            Param { value: ParamValue::Float(100.0), keyframes: vec![bez(t0, ParamValue::Float(look.hero_from_scale * 100.0)), kf(enter_end, ParamValue::Float(100.0), Interpolation::Linear, 0.33, 0.33)] },
        );
    }
}

/// Words to set: `text`/`words` as in `lettering.plan`, or the sequence transcript between `from`
/// and `to` (sequence seconds), with the times they are spoken at. The clip's own zero is the
/// first word's start less the lead; the result is (words with clip-relative times, placement).
fn source_words(s: &Session, p: &Value) -> Result<(Vec<Word>, Option<f64>)> {
    if let (Some(a), Some(b)) = (f64_p(p, "from"), f64_p(p, "to")) {
        let ws: Vec<_> = crate::transcript::sequence_words(s).into_iter().filter(|w| w.start.seconds() >= a - 1e-6 && w.end.seconds() <= b + 1e-6).collect();
        if ws.is_empty() {
            return Err(bad("lettering.apply", format!("no transcript words between {a} and {b} s (Transcribe first)")));
        }
        let t0 = (ws[0].start.seconds() - LEAD_SECS).max(0.0);
        let words = ws.iter().map(|w| Word { text: w.text.clone(), start: w.start.seconds() - t0, end: w.end.seconds() - t0 }).collect();
        return Ok((words, Some(t0)));
    }
    let mut words = words_param(p);
    if words.is_empty() {
        return Err(bad("lettering.apply", "give `text`, `words`, or `from` and `to` (sequence seconds of the transcript)"));
    }
    if words.iter().all(|w| w.end <= w.start) {
        // no speech: the words arrive one after another
        for (i, w) in words.iter_mut().enumerate() {
            w.start = LEAD_SECS + 0.18 * i as f64;
            w.end = w.start + 0.18;
        }
    }
    Ok((words, None))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "lettering.apply";
    let (words, at) = source_words(s, p)?;
    let (fw, fh) = s.active_sequence().map(|q| (q.settings.width as f32, q.settings.height as f32)).ok_or(EngineError::NoSequence)?;

    let hero_idx: Vec<usize> = match p.get("heroes").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(|u| u as usize)).filter(|&i| i < words.len()).collect(),
        None => heroes(&words),
    };
    let mut hero = vec![false; words.len()];
    for &i in &hero_idx {
        hero[i] = true;
    }
    filmcraft_text::fonts::scan_system();
    let mut look = match str_p(p, "look") {
        Some(n) => Look::by_name(n).ok_or_else(|| bad(ID, format!("unknown look `{n}` (classic, monumental, editorial, cinetico)")))?,
        None => Look::classic(),
    };
    if let Some(c) = str_p(p, "composition") {
        look.composition = if c == "stair" { Composition::Stair } else { Composition::Block };
    }
    if let Some(r) = bool_p(p, "rule") {
        look.rule = r;
    }
    if bool_p(p, "heroItalic").unwrap_or(look.hero_italic) {
        look.hero_italic = true;
    }
    let faces = Faces {
        family: str_p(p, "family").map_or_else(|| Faces::default().family, Into::into),
        support: str_p(p, "supportStyle").unwrap_or(look.sup_face).into(),
        hero: str_p(p, "heroStyle").unwrap_or(look.hero_face).into(),
    };
    // an italic hero is measured in its italic face
    let measure_faces = Faces { hero: if look.hero_italic { format!("{} Italic", faces.hero) } else { faces.hero.clone() }, ..faces.clone() };
    let (ms, space100) = measure_words_with(&words, &hero, &measure_faces, &look);
    let params = Params {
        box_w: f64_p(p, "boxWidth").map_or(fw * 0.84, |v| v as f32),
        box_h: f64_p(p, "boxHeight").map_or(fh * 0.5, |v| v as f32),
        size: f64_p(p, "size").map_or(fw * look.size_k, |v| v as f32),
        hero_scale: f64_p(p, "heroScale").map_or(look.hero_scale, |v| v as f32),
        space100,
        leading: look.leading,
        composition: look.composition,
        stair_step: Params::default().stair_step,
    };
    if params.box_w <= 0.0 || params.box_h <= 0.0 || params.size <= 0.0 {
        return Err(bad(ID, "boxWidth, boxHeight and size must be positive"));
    }
    let c = compose(&ms, &params);
    if c.overflow {
        // Nothing is placed: the caller cuts the phrase and applies each half.
        return Ok(json!({"applied": false, "overflow": true, "splitAt": c.split_at, "reason": "the phrase does not fit in 3 lines at a readable size"}));
    }

    // Where the block sits in the frame: centred by default, or against a margin.
    let margin = fw * 0.08;
    let left = match str_p(p, "align") {
        Some("left") => margin,
        Some("right") => fw - margin - c.width,
        _ => (fw - c.width) / 2.0,
    };
    let top = match str_p(p, "zone") {
        Some("upper") => fh * 0.18,
        // the new looks sit on the bottom margin: under the face, over the shirt and the desk
        Some("lower") => fh * if look.name == "classic" { 0.80 } else { 0.88 } - c.height,
        _ => (fh - c.height) / 2.0,
    };

    let light_text = str_p(p, "theme") != Some("light"); // light letters on the picture by default
    // The picture can swallow a brand colour (terracotta on an orange wall): the caller may override either.
    let (support_hex, hero_hex) = (
        str_p(p, "supportColor").unwrap_or(if light_text { OFF_WHITE } else { PRETO }),
        str_p(p, "heroColor").unwrap_or(TERRACOTA),
    );
    let how = str_p(p, "animation").unwrap_or("forward");
    let hero_italic = look.hero_italic;

    let last_end = words.iter().map(|w| w.end).fold(0.0, f64::max);
    let last_start = words.iter().map(|w| w.start).fold(0.0, f64::max);
    let exit = match f64_p(p, "hold") {
        // a short stay for a lettering that must clear the way for the next one
        Some(h) => (last_start - LEAD_SECS).max(0.0) + ENTER_SECS + h.max(0.0),
        None => (last_end + HOLD_SECS).max(last_start + ENTER_SECS + 0.4),
    };
    let duration = exit + EXIT_SECS;

    let mut layers = Vec::with_capacity(words.len());
    for pl in &c.words {
        let w = &words[pl.index];
        let pos = Vec2::new((left + pl.x) as f64, (top + pl.baseline) as f64);
        let shown = look.shown(&w.text, pl.hero);
        let mut e = new_text_layer(&shown, pos, pl.size as f64);
        let face = if pl.hero { &faces.hero } else { &faces.support };
        let face = if pl.hero && hero_italic { format!("{face} Italic") } else { face.clone() };
        for (id, v) in [("font", ParamValue::Text(faces.family.clone())), ("font_style", ParamValue::Text(face)), ("name", ParamValue::Text(w.text.clone()))] {
            e.params.insert(id.into(), Param::new(v));
        }
        e.params.insert("fill_color".into(), Param::new(color(if pl.hero { hero_hex } else { support_hex })));
        // hard drop shadow, 3 px right and 3 px down (at 1080p), no blur: it lifts the word off the picture
        if bool_p(p, "shadow").unwrap_or(true) {
            let k = fw as f64 / 1920.0;
            let d = 3.0 * std::f64::consts::SQRT_2 * k;
            for (id, v) in [
                ("shadow", ParamValue::Bool(true)),
                ("shadow_color", color("#000000")),
                ("shadow_opacity", ParamValue::Float(100.0)),
                ("shadow_angle", ParamValue::Float(135.0)),
                ("shadow_distance", ParamValue::Float(d)),
                ("shadow_size", ParamValue::Float(0.0)),
                ("shadow_blur", ParamValue::Float(0.0)),
            ] {
                e.params.insert(id.into(), Param::new(v));
            }
        }
        // kerning on, ligatures off: a lettering is set, not typed
        e.params.insert("ligatures".into(), Param::new(ParamValue::Bool(false)));
        let track = if pl.hero { look.hero_track } else { look.sup_track };
        if track != 0.0 {
            e.params.insert("tracking".into(), Param::new(ParamValue::Float(track)));
        }
        // enters just before it is said, behind the previous word by the brand's stagger at least
        let t0 = (w.start - LEAD_SECS).max(0.0).max(if pl.index > 0 { (words[pl.index - 1].start - LEAD_SECS).max(0.0) + STAGGER_SECS } else { 0.0 });
        let geo = Geo { pos, size: pl.size as f64, width: pl.width as f64, hero: pl.hero, fw: fw as f64 };
        animate(&mut e, &geo, t0, exit, how, &look);
        layers.push(e);
    }
    // the guide line: over the block, drawn left to right with the first word
    if look.rule {
        let k = fw as f64 / 1920.0;
        let h = (3.0 * k).max(2.0);
        let w = c.width as f64;
        let y = (top - c.words.iter().map(|w| w.size).fold(0.0, f32::max) * 0.02) as f64 - 6.0 * k - h;
        let x0 = left as f64;
        let mut line = new_shape_layer(0, Vec2::new(x0 + w / 2.0, y), Vec2::new(w, h), Vec::new());
        line.params.insert("fill_color".into(), Param::new(color(support_hex)));
        line.params.insert("name".into(), Param::new(ParamValue::Text("Linha".into())));
        let t_in = (words[0].start - LEAD_SECS).max(0.0);
        let t_end = t_in + ENTER_SECS * 1.3;
        line.params.insert(
            "size".into(),
            Param { value: ParamValue::Vec2(Vec2::new(w, h)), keyframes: vec![kf(t_in, ParamValue::Vec2(Vec2::new(1.0, h)), Interpolation::Bezier, ENTER_OUT, ENTER_IN), kf(t_end, ParamValue::Vec2(Vec2::new(w, h)), Interpolation::Linear, 0.33, 0.33)] },
        );
        line.params.insert(
            "position".into(),
            Param { value: ParamValue::Vec2(Vec2::new(x0 + w / 2.0, y)), keyframes: vec![kf(t_in, ParamValue::Vec2(Vec2::new(x0 + 0.5, y)), Interpolation::Bezier, ENTER_OUT, ENTER_IN), kf(t_end, ParamValue::Vec2(Vec2::new(x0 + w / 2.0, y)), Interpolation::Linear, 0.33, 0.33)] },
        );
        line.params.insert(
            "opacity".into(),
            Param { value: ParamValue::Float(100.0), keyframes: vec![kf(exit, ParamValue::Float(100.0), Interpolation::Bezier, EXIT_OUT, EXIT_IN), kf(exit + EXIT_SECS, ParamValue::Float(0.0), Interpolation::Linear, 0.33, EXIT_IN)] },
        );
        layers.insert(0, line);
    }

    // A black plate behind the hero word only (a run of neighbouring heroes shares one), so the
    // terracotta reads on any picture while the support words stay free on the image.
    if bool_p(p, "scrim").unwrap_or(false) {
        let peak = f64_p(p, "scrimOpacity").unwrap_or(92.0);
        let mut order: Vec<&Placed> = c.words.iter().collect();
        order.sort_by_key(|w| w.index);
        let mut runs: Vec<Vec<&Placed>> = Vec::new();
        for pl in order.into_iter().filter(|w| w.hero) {
            match runs.last_mut() {
                Some(r) if r.last().is_some_and(|l| l.index + 1 == pl.index && l.line == pl.line) => r.push(pl),
                _ => runs.push(vec![pl]),
            }
        }
        let mut plates = Vec::new();
        for r in runs {
            let (first, last) = (r[0], r[r.len() - 1]);
            let size = r.iter().map(|w| w.size).fold(0.0, f32::max) as f64;
            let (px, py) = (size * 0.14, size * 0.1);
            let x0 = (left + first.x) as f64 - px;
            let x1 = (left + last.x + last.width) as f64 + px;
            let base = (top + first.baseline) as f64;
            let (y0, y1) = (base - size * 0.8 - py, base + size * 0.24 + py);
            let mut band = new_shape_layer(0, Vec2::new((x0 + x1) / 2.0, (y0 + y1) / 2.0), Vec2::new(x1 - x0, y1 - y0), Vec::new());
            band.params.insert("fill_color".into(), Param::new(color("#000000")));
            band.params.insert("name".into(), Param::new(ParamValue::Text("Placa".into())));
            let t_in = (words[first.index].start - LEAD_SECS).max(0.0).max(if first.index > 0 { (words[first.index - 1].start - LEAD_SECS).max(0.0) + STAGGER_SECS } else { 0.0 });
            band.params.insert(
                "opacity".into(),
                Param {
                    value: ParamValue::Float(peak),
                    keyframes: vec![
                        kf(t_in, ParamValue::Float(0.0), Interpolation::Bezier, ENTER_OUT, ENTER_IN),
                        kf(t_in + ENTER_SECS, ParamValue::Float(peak), Interpolation::Linear, 0.33, 0.33),
                        kf(exit, ParamValue::Float(peak), Interpolation::Bezier, EXIT_OUT, EXIT_IN),
                        kf(exit + EXIT_SECS, ParamValue::Float(0.0), Interpolation::Linear, 0.33, EXIT_IN),
                    ],
                },
            );
            plates.push(band);
        }
        for (i, band) in plates.into_iter().enumerate() {
            layers.insert(i, band);
        }
    }

    let name = format!("Lettering · {}", words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "));
    let name: String = name.chars().take(60).collect();
    let mut placement = serde_json::Map::new();
    placement.insert("seconds".into(), json!(duration));
    if let Some(at) = at {
        placement.insert("time".into(), json!(Tick::from_seconds_f64(at).0));
    } else if let Some(t) = p.get("time").cloned() {
        placement.insert("time".into(), t);
    }
    if let Some(t) = u64_p(p, "track") {
        placement.insert("track".into(), json!(t));
    }
    let clip = crate::graphics::new_graphic_clip_layers(s, ID, layers, &name, &Value::Object(placement))?;
    // The caption under the lettering says the same thing: take it off while the lettering is up.
    let mut hidden = 0;
    if bool_p(p, "hideCaptions").unwrap_or(true) {
        let t0 = at.or_else(|| p.get("time").and_then(Value::as_u64).map(|t| Tick(t as _).seconds()));
        if let Some(t0) = t0 {
            let (a, b) = (t0 + words[0].start, t0 + last_end);
            let ids: Vec<filmcraft_project::ClipId> = s
                .active_sequence()
                .map(|q| {
                    q.caption_tracks
                        .iter()
                        .flat_map(|tr| tr.captions.iter())
                        .filter(|c| {
                            let (cs, ce) = (c.start.seconds(), (c.start + c.duration).seconds());
                            // under the lettering for a third of its time or more
                            (ce.min(b) - cs.max(a)) > 0.3 * (ce - cs)
                        })
                        .map(|c| c.id)
                        .collect()
                })
                .unwrap_or_default();
            if !ids.is_empty() {
                hidden = ids.len();
                crate::captions::delete(s, &ids, false)?;
            }
        }
    }
    Ok(json!({
        "applied": true,
        "clip": clip.0,
        "duration": duration,
        "lines": c.lines.iter().map(|l| l.iter().map(|&i| words[i].text.as_str()).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>(),
        "heroes": hero_idx.iter().map(|&i| words[i].text.clone()).collect::<Vec<_>>(),
        "scale": c.scale,
        "hiddenCaptions": hidden,
    }))
}

/// Words (with the times they are said at) of the captions in `ids`: the transcript between the
/// first and the last caption when there is one, else the caption text spread over its time.
fn words_of_captions(s: &Session, ids: &[filmcraft_project::ClipId]) -> Result<(Vec<Word>, f64, f64)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut caps: Vec<(f64, f64, String)> =
        ids.iter().filter_map(|id| q.find_caption(*id)).map(|(_, c)| (c.start.seconds(), (c.start + c.duration).seconds(), c.text.replace('\n', " "))).collect();
    if caps.is_empty() {
        return Err(bad("lettering.fromSelection", "no caption selected"));
    }
    caps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (a, b) = (caps[0].0, caps.iter().map(|c| c.1).fold(0.0, f64::max));
    let said: Vec<_> = crate::transcript::sequence_words(s).into_iter().filter(|w| w.start.seconds() >= a - 0.05 && w.end.seconds() <= b + 0.05).collect();
    if !said.is_empty() {
        return Ok((said.iter().map(|w| Word { text: w.text.clone(), start: w.start.seconds(), end: w.end.seconds() }).collect(), a, b));
    }
    // no transcript: the words share the caption's time by their length
    let mut words = Vec::new();
    for (start, end, text) in &caps {
        let toks: Vec<&str> = text.split_whitespace().collect();
        let total: f64 = toks.iter().map(|t| t.chars().count() as f64 + 1.0).sum();
        let mut t = *start;
        for tok in toks {
            let d = (end - start) * (tok.chars().count() as f64 + 1.0) / total;
            words.push(Word { text: tok.into(), start: t, end: t + d });
            t += d;
        }
    }
    Ok((words, a, b))
}

/// The text of the selected graphic clip's text layers (the selected layers, else all of them).
fn text_of_graphic(s: &Session) -> Option<(String, f64)> {
    let q = s.active_sequence()?;
    let clip = *s.state.selection.first()?;
    let (_, it) = q.find_item(clip)?;
    let idx = filmcraft_project::graphic::layer_indices(&it.effects);
    let picked: Vec<usize> = if s.state.graphic_layers.is_empty() { idx.clone() } else { s.state.graphic_layers.iter().filter_map(|n| idx.get(*n).copied()).collect() };
    let text: Vec<String> = picked
        .iter()
        .filter_map(|&i| it.effects.get(i))
        .filter_map(|e| match &e.params.get("text")?.value {
            ParamValue::Text(t) if !t.trim().is_empty() => Some(t.replace('\n', " ")),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then(|| (text.join(" "), it.start.seconds()))
}

/// `lettering.apply` on what is selected: the captions (timed to the voice) or the text of a
/// graphic clip, which stays where it is.
fn from_selection(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "lettering.fromSelection";
    let mut q = if p.is_object() { p.clone() } else { json!({}) };
    for k in ["text", "words", "from", "to", "time"] {
        q.as_object_mut().map(|m| m.remove(k));
    }
    let caps: Vec<filmcraft_project::ClipId> = if !s.state.caption_selection.is_empty() {
        s.state.caption_selection.clone()
    } else if s.state.selection.is_empty() {
        let t = s.playhead();
        s.active_sequence().and_then(|q| q.caption_tracks.iter().find_map(|tr| tr.caption_at(t).map(|c| c.id))).into_iter().collect()
    } else {
        Vec::new()
    };
    if !caps.is_empty() {
        let (words, _, _) = words_of_captions(s, &caps)?;
        let t0 = (words[0].start - LEAD_SECS).max(0.0);
        q["words"] = json!(words.iter().map(|w| json!({"text": w.text, "start": w.start - t0, "end": w.end - t0})).collect::<Vec<_>>());
        q["time"] = json!(Tick::from_seconds_f64(t0).0);
        return apply(s, &q);
    }
    let (text, start) = text_of_graphic(s).ok_or_else(|| bad(ID, "select a caption or a graphic clip with text"))?;
    q["text"] = json!(text);
    q["time"] = json!(Tick::from_seconds_f64(start).0);
    apply(s, &q)
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "lettering.plan",
        label: "Plan Lettering",
        menu: &[],
        shortcut: None,
        params: r#"{"text":str? | "words":[{text,start?,end?}]?,"box":[w,h]=[880,1200],"size":px=64,"heroScale":f=1.7,"composition":"block|stair"="block","heroes":[index]? (default: chosen by the rules),"family":str?,"supportStyle":str="Light","heroStyle":str="ExtraBold","leading":f=0.98,"stairStep":f=0.9}"#,
        enabled: always,
        run: |_: &mut Session, p: &Value| plan(p),
        journal: false,
    },
    CommandSpec {
        id: "lettering.apply",
        label: "Lettering",
        menu: &["Graphics and Titles", "New Layer"],
        shortcut: None,
        params: r#"{"text":str? | "words":[{text,start?,end?}]? | "from":sec,"to":sec (sequence seconds: the transcript between them, timed to the voice),"time":ticks? (where the clip starts; default the playhead; with from/to, a little before the first word),"zone":"center|upper|lower"="center","align":"center|left|right"="center","composition":"block|stair"="block","animation":"forward|up|fade"="forward","theme":"dark|light"="dark" (dark = light letters on the picture),"heroes":[index]?,"look":"classic|monumental|editorial|cinetico"="classic" (type, scale, tracking, reveal; the other options override it),"rule":bool? (guide line over the block),"hold":sec? (how long it stays after the last word has arrived; default until 0.8 s after the voice),"heroItalic":bool=false,"scrim":bool=false (black plate behind the hero word only),"scrimOpacity":0..100=92,"shadow":bool=true (hard 3 px drop shadow on every word),"hideCaptions":bool=true (remove the captions under it),"heroColor":hex? (default terracotta),"supportColor":hex?,"size":px?,"heroScale":f=1.7,"boxWidth":px?,"boxHeight":px?,"track":index?}"#,
        enabled: has_seq,
        run: apply,
        journal: true,
    },
    CommandSpec {
        id: "lettering.fromSelection",
        label: "Lettering from Selection",
        menu: &["Graphics and Titles"],
        shortcut: None,
        params: r#"{same options as lettering.apply, without text/words/from/to/time: the selected captions (timed to the voice; the caption under the playhead when none is selected) or the text of the selected graphic clip}"#,
        enabled: has_seq,
        run: from_selection,
        journal: true,
    }]
}

#[cfg(test)]
mod tests;

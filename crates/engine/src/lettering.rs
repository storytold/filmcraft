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

use filmcraft_text::TextStyle;

use crate::commands::{CommandSpec, always, bad, f64_p, str_p};
use crate::{Result, Session};

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
        ranked = scores.iter().copied().filter(|(_, s)| *s >= 1.0).collect();
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

/// Width of words `a..b` set on one line at the requested sizes.
fn line_w(ms: &[WordMetric], p: &Params, a: usize, b: usize) -> f32 {
    let mut w = 0.0;
    for (k, m) in ms[a..b].iter().enumerate() {
        if k > 0 {
            // a space has the support size, also between heroes: tight, like one phrase
            w += p.space100 * p.size / 100.0;
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
        let extra_per_gap = if justify && gaps > 0 { (block_w - lw) / gaps as f32 } else { 0.0 };
        let mut x = x0;
        for (j, i) in (a..b).enumerate() {
            let s = size_of(&ms[i], p) * fit;
            let w = word_w(&ms[i], p) * fit;
            words.push(Placed { index: i, line: k, x, baseline, size: s, width: w, hero: ms[i].hero, tracking: 0.0 });
            x += w + p.space100 * p.size * fit / 100.0 + if j < gaps { extra_per_gap } else { 0.0 };
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
    let sup = style(&faces.family, &faces.support);
    let her = style(&faces.family, &faces.hero);
    let space = filmcraft_text::measure("a a", &sup) - filmcraft_text::measure("aa", &sup);
    let ms = words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let n = normalize(&w.text);
            WordMetric {
                width100: filmcraft_text::measure(&w.text, if hero[i] { &her } else { &sup }),
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
    }]
}

#[cfg(test)]
mod tests;

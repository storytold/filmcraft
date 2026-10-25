//! Text-based editing: the sequence transcript and the edits made through it.
//!
//! Clip transcripts (`filmcraft_project::Transcript`) hold words in **media time**. The sequence
//! transcript ([`sequence_words`]) maps them through the audio track items that play them:
//!
//! - audio tracks are read top first (A1, A2…); a word is taken from the first track whose
//!   transcribed clip covers the word's midpoint, so a dialogue clip duplicated on two tracks (or a
//!   stereo pair split over two mono tracks) reads once;
//! - a word belongs to a clip when its midpoint, mapped through the clip's speed, falls inside the
//!   clip; its timeline bounds are clamped to the clip, so a word cut by an edit is shown cut;
//! - disabled clips, reversed clips and frame holds contribute no words (they don't play speech).
//!
//! Text edits turn word ranges into timeline ranges ([`word_range`]) snapped outward to frames, then
//! reuse the ordinary [`crate::extract`] / [`crate::lift`] edits. Pause and filler-word removal
//! ([`find_pauses`], [`find_fillers`]) produce many ranges that [`ripple_delete_ranges`] removes in
//! one pass, right to left. Captions come from [`caption_blocks`].

use std::collections::BTreeMap;
use std::sync::Arc;

use filmcraft_project::{Caption, ClipId, ItemId, Sequence, TrackId, Transcript};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use crate::EditCtx;

/// A word of the sequence transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct SeqWord {
    pub text: String,
    /// Sequence time.
    pub start: Tick,
    pub end: Tick,
    /// The track item it is heard through and that item's media.
    pub clip: ClipId,
    pub item: ItemId,
    /// Index of the word in the media item's transcript.
    pub index: usize,
    /// Audio track index (0 = A1).
    pub track: usize,
    pub speaker: Option<String>,
    pub confidence: f32,
}

impl SeqWord {
    pub fn normalized(&self) -> String {
        filmcraft_project::transcript::normalize_word(&self.text)
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end.max(self.start))
    }
}

/// Transcripts by media item (as in `Project::transcripts`).
pub type Transcripts = BTreeMap<ItemId, Arc<Transcript>>;

fn ticks(t: f64) -> Tick {
    Tick(t.round() as i64)
}

/// The sequence transcript: words of every transcribed clip on the audio tracks, in sequence
/// time order (see the module docs for the rules).
pub fn sequence_words(seq: &Sequence, transcripts: &Transcripts) -> Vec<SeqWord> {
    let mut out: Vec<SeqWord> = Vec::new();
    // timeline ranges already served by a higher track's transcribed clips
    let mut claimed: Vec<TimeRange> = Vec::new();
    for (ti, track) in seq.audio_tracks.iter().enumerate() {
        let mut mine = Vec::new();
        for it in &track.items {
            if !it.enabled || it.reverse || it.frame_hold.is_some() || it.speed <= 0.0 {
                continue;
            }
            let Some(tr) = transcripts.get(&it.item) else { continue };
            mine.push(it.range());
            let speed = it.speed;
            let media_end = it.source_in + ticks(it.duration.0 as f64 * speed);
            let to_tl = |m: Tick| it.start + ticks((m - it.source_in).0 as f64 / speed);
            for wi in tr.words_in(TimeRange::from_bounds(it.source_in, media_end.max(it.source_in))) {
                let w = &tr.words[wi];
                let (a, b) = (to_tl(w.start), to_tl(w.end.max(w.start)));
                let mid = Tick(a.0 + (b.0 - a.0) / 2);
                if mid < it.start || mid >= it.end() || claimed.iter().any(|r| r.contains(mid)) {
                    continue;
                }
                out.push(SeqWord {
                    text: w.text.clone(),
                    start: a.max(it.start),
                    end: b.min(it.end()).max(a.max(it.start)),
                    clip: it.id,
                    item: it.item,
                    index: wi,
                    track: ti,
                    speaker: tr.speaker_name(w),
                    confidence: w.confidence,
                });
            }
        }
        claimed.extend(mine);
    }
    out.sort_by_key(|w| (w.start, w.track));
    out
}

/// Index of the word being spoken at `t` (the last word starting at or before `t` whose end is
/// after `t`).
pub fn word_at(words: &[SeqWord], t: Tick) -> Option<usize> {
    let i = words.partition_point(|w| w.start <= t).checked_sub(1)?;
    (t < words[i].end).then_some(i)
}

/// Paragraphs (Text panel segments): runs of words split where the speaker changes or at a pause
/// of at least `gap`.
pub fn paragraphs(words: &[SeqWord], gap: Tick) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut a = 0;
    for i in 1..=words.len() {
        if i == words.len() || words[i].speaker != words[i - 1].speaker || words[i].start - words[i - 1].end >= gap {
            if a < i {
                out.push(a..i);
            }
            a = i;
        }
    }
    out
}

/// Matches of `query` (one or more words, case and punctuation ignored; the last query word may be
/// a prefix) as word index ranges.
pub fn search(words: &[SeqWord], query: &str) -> Vec<std::ops::Range<usize>> {
    let q: Vec<String> = query.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|s| !s.is_empty()).collect();
    if q.is_empty() {
        return Vec::new();
    }
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let mut out = Vec::new();
    for i in 0..norm.len().saturating_sub(q.len() - 1) {
        let ok = q.iter().enumerate().all(|(k, qw)| if k + 1 == q.len() { norm[i + k].starts_with(qw.as_str()) } else { norm[i + k] == *qw });
        if ok {
            out.push(i..i + q.len());
        }
    }
    out
}

/// Timeline range of words `a..=b`, snapped outward to frames.
pub fn word_range(words: &[SeqWord], a: usize, b: usize, rate: FrameRate) -> Option<TimeRange> {
    let (a, b) = (a.min(b), a.max(b));
    let (s, e) = (words.get(a)?.start, words.get(b)?.end);
    let s = rate.snap(s);
    let mut e2 = rate.snap(e);
    if e2 < e || e2 <= s {
        e2 += rate.frame_duration();
    }
    Some(TimeRange::from_bounds(s, e2))
}

/// Media range of words `a..=b` of a clip transcript, snapped outward to the media's frames (for
/// Source-monitor In/Out from a text selection).
pub fn media_word_range(t: &Transcript, a: usize, b: usize, rate: FrameRate) -> Option<TimeRange> {
    let (a, b) = (a.min(b), a.max(b));
    let (s, e) = (t.words.get(a)?.start, t.words.get(b)?.end);
    let s = rate.snap(s);
    let mut e2 = rate.snap(e);
    if e2 < e || e2 <= s {
        e2 += rate.frame_duration();
    }
    Some(TimeRange::from_bounds(s, e2))
}

/// Pauses between consecutive words of at least `min`, as the ranges to remove: each keeps `keep`
/// of silence next to both words and is snapped inward to frames (pauses shorter than one frame
/// after that are skipped).
pub fn find_pauses(words: &[SeqWord], min: Tick, keep: Tick, rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for p in words.windows(2) {
        let (a, b) = (p[0].end, p[1].start);
        if b - a < min || b <= a {
            continue;
        }
        let s = a + keep;
        let e = b - keep;
        let mut s2 = rate.snap(s);
        if s2 < s {
            s2 += rate.frame_duration();
        }
        let e2 = rate.snap(e);
        if e2 > s2 {
            out.push(TimeRange::from_bounds(s2, e2));
        }
    }
    out
}

/// The default filler words and phrases (configurable in preferences and per command).
pub const DEFAULT_FILLERS: &[&str] = &["um", "uh", "umm", "uhm", "erm", "er", "ah", "hmm", "mm", "mhm"];

/// Filler words: word index ranges matching one of `fillers` (each a word or a phrase such as
/// "you know"; case and punctuation ignored).
pub fn find_fillers(words: &[SeqWord], fillers: &[String]) -> Vec<std::ops::Range<usize>> {
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let mut phrases: Vec<Vec<String>> = fillers
        .iter()
        .map(|f| f.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .filter(|p| !p.is_empty())
        .collect();
    // longest phrases first, so "you know" wins over a lone "you"
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut out = Vec::new();
    let mut i = 0;
    while i < norm.len() {
        let hit = phrases.iter().find(|p| i + p.len() <= norm.len() && p.iter().enumerate().all(|(k, w)| norm[i + k] == *w));
        match hit {
            Some(p) => {
                out.push(i..i + p.len());
                i += p.len();
            }
            None => i += 1,
        }
    }
    out
}

/// Timeline ranges removing the filler words `hits`: each word range snapped to the nearest frames
/// (never into the neighbouring words' frames).
pub fn filler_ranges(words: &[SeqWord], hits: &[std::ops::Range<usize>], rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for h in hits {
        let (a, b) = (h.start, h.end - 1);
        let mut s = rate.snap_nearest(words[a].start);
        let mut e = rate.snap_nearest(words[b].end);
        if a > 0 && s < words[a - 1].end {
            s = rate.snap(words[a - 1].end) + rate.frame_duration();
        }
        if let Some(n) = words.get(b + 1)
            && e > n.start
        {
            e = rate.snap(n.start);
        }
        if e > s {
            out.push(TimeRange::from_bounds(s, e));
        }
    }
    out
}

/// Sort and merge ranges (overlapping or touching).
pub fn merge_ranges(mut r: Vec<TimeRange>) -> Vec<TimeRange> {
    r.sort_by_key(|x| x.start);
    let mut out: Vec<TimeRange> = Vec::new();
    for x in r {
        match out.last_mut() {
            Some(l) if x.start <= l.end() => *l = TimeRange::from_bounds(l.start, l.end().max(x.end())),
            _ => out.push(x),
        }
    }
    out
}

/// Ripple-delete every range on every unlocked track (and sync-locked caption tracks), right to
/// left so earlier ranges keep their positions. Returns the total time removed.
pub fn ripple_delete_ranges(seq: &mut Sequence, ranges: Vec<TimeRange>, ctx: &mut EditCtx) -> Tick {
    let tracks: Vec<TrackId> = seq.all_tracks().filter(|t| !t.locked).map(|t| t.id).collect();
    let mut total = Tick::ZERO;
    for r in merge_ranges(ranges).into_iter().rev() {
        if r.duration <= Tick::ZERO {
            continue;
        }
        crate::extract(seq, &tracks, r, ctx);
        total += r.duration;
    }
    total
}

/// Rules for Create Captions from a transcript (Premiere's dialog defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionRules {
    /// Maximum characters per line.
    pub max_chars: usize,
    /// Lines per caption (1 = single, 2 = double).
    pub lines: usize,
    /// Minimum caption duration; a short caption is extended into the silence after it, never
    /// over the next one.
    pub min_duration: Tick,
    /// Longest caption.
    pub max_duration: Tick,
    /// Frames left empty between consecutive captions.
    pub gap_frames: i64,
    /// A pause at least this long starts a new caption.
    pub break_pause: Tick,
    /// Balance lines semantically, prevent single-word widow lines, and avoid ending lines or
    /// blocks on prepositions, articles or conjunctions.
    pub smart_balance: bool,
}

impl Default for CaptionRules {
    fn default() -> Self {
        Self {
            max_chars: 42,
            lines: 2,
            min_duration: Tick(TICKS_PER_SECOND),
            max_duration: Tick(7 * TICKS_PER_SECOND),
            gap_frames: 0,
            break_pause: Tick(TICKS_PER_SECOND),
            smart_balance: false,
        }
    }
}

/// Apply domain glossary and terminology rules to a transcript (e.g. Abravidro / glass industry
/// terms, phonetic corrections, proper casing and hyphenated titles).
pub fn apply_glossary(t: &mut Transcript) {
    let split_punct = |s: &str| -> (String, String, String) {
        let lead: String = s.chars().take_while(|c| !c.is_alphanumeric()).collect();
        let trail: String = s.chars().rev().take_while(|c| !c.is_alphanumeric()).collect::<Vec<_>>().into_iter().rev().collect();
        let core_end = s.len().saturating_sub(trail.len());
        let core = if lead.len() <= core_end { &s[lead.len()..core_end] } else { "" };
        (lead, core.to_string(), trail)
    };

    // 1. Multi-word phrase standardization and hyphenation merges
    let mut i = 0;
    while i < t.words.len() {
        let rem = t.words.len() - i;
        let norm_at = |offset: usize| -> String {
            let (_, core, _) = split_punct(&t.words[i + offset].text);
            core.to_lowercase()
        };
        let no_mid_punct = |count: usize| -> bool {
            (0..count.saturating_sub(1)).all(|k| {
                let (_, _, tr) = split_punct(&t.words[i + k].text);
                tr.is_empty()
            })
        };

        // "Diretor da Unidade de Negócios de Vidros Arquitetônicos" (8 words)
        if rem >= 8
            && no_mid_punct(8)
            && norm_at(0) == "diretor"
            && norm_at(1) == "da"
            && norm_at(2) == "unidade"
            && norm_at(3) == "de"
            && (norm_at(4) == "negócios" || norm_at(4) == "negocios")
            && norm_at(5) == "de"
            && norm_at(6) == "vidros"
            && (norm_at(7) == "arquitetônicos" || norm_at(7) == "arquitetonicos")
        {
            let replacements = ["Diretor", "da", "Unidade", "de", "Negócios", "de", "Vidros", "Arquitetônicos"];
            for (k, rep) in replacements.iter().enumerate() {
                let (l, _, tr) = split_punct(&t.words[i + k].text);
                t.words[i + k].text = format!("{l}{rep}{tr}");
            }
            i += 8;
            continue;
        }

        // "Vice-presidente-executivo" from 3 words ("vice" "presidente" "executivo")
        if rem >= 3 && no_mid_punct(3) && norm_at(0) == "vice" && norm_at(1) == "presidente" && norm_at(2) == "executivo" {
            let (l, _, _) = split_punct(&t.words[i].text);
            let (_, _, tr) = split_punct(&t.words[i + 2].text);
            t.words[i].text = format!("{l}Vice-presidente-executivo{tr}");
            t.words[i].end = t.words[i + 2].end;
            t.words.drain(i + 1..=i + 2);
            i += 1;
            continue;
        }

        // "Vice-presidente-executivo" from 2 words ("vice-presidente" "executivo")
        if rem >= 2 && no_mid_punct(2) && norm_at(0) == "vice-presidente" && norm_at(1) == "executivo" {
            let (l, _, _) = split_punct(&t.words[i].text);
            let (_, _, tr) = split_punct(&t.words[i + 1].text);
            t.words[i].text = format!("{l}Vice-presidente-executivo{tr}");
            t.words[i].end = t.words[i + 1].end;
            t.words.remove(i + 1);
            i += 1;
            continue;
        }

        // "Panorama Abravidro" (2 words)
        if rem >= 2 && no_mid_punct(2) && norm_at(0) == "panorama" && norm_at(1) == "abravidro" {
            let (l0, _, tr0) = split_punct(&t.words[i].text);
            let (l1, _, tr1) = split_punct(&t.words[i + 1].text);
            t.words[i].text = format!("{l0}Panorama{tr0}");
            t.words[i + 1].text = format!("{l1}Abravidro{tr1}");
            i += 2;
            continue;
        }

        // "ABNT Coleção" (2 words)
        if rem >= 2 && no_mid_punct(2) && norm_at(0) == "abnt" && (norm_at(1) == "coleção" || norm_at(1) == "colecao") {
            let (l0, _, tr0) = split_punct(&t.words[i].text);
            let (l1, _, tr1) = split_punct(&t.words[i + 1].text);
            t.words[i].text = format!("{l0}ABNT{tr0}");
            t.words[i + 1].text = format!("{l1}Coleção{tr1}");
            i += 2;
            continue;
        }

        // "setor vidreiro" phonetic fix ("setor vibreiro" / "setor livreiro")
        if rem >= 2 && no_mid_punct(2) && norm_at(0) == "setor" && (norm_at(1) == "vibreiro" || norm_at(1) == "livreiro") {
            let (l1, _, tr1) = split_punct(&t.words[i + 1].text);
            t.words[i + 1].text = format!("{l1}vidreiro{tr1}");
            i += 2;
            continue;
        }

        // Single-word fixes
        let (l, core, tr) = split_punct(&t.words[i].text);
        let lower = core.to_lowercase();
        let fixed = match lower.as_str() {
            "abravidro" => Some("Abravidro"),
            "abnt" => Some("ABNT"),
            "vibreiro" => Some("vidreiro"),
            "vibreiros" => Some("vidreiros"),
            "vice-presidente-executivo" => Some("Vice-presidente-executivo"),
            _ => None,
        };
        if let Some(rep) = fixed {
            t.words[i].text = format!("{l}{rep}{tr}");
        }
        i += 1;
    }
}

fn is_connector_word(word: &str) -> bool {
    if word.ends_with(['.', '!', '?', ',', ';', ':', '—']) {
        return false;
    }
    let norm = filmcraft_project::transcript::normalize_word(word);
    matches!(
        norm.as_str(),
        "o" | "a"
            | "os"
            | "as"
            | "um"
            | "uma"
            | "uns"
            | "umas"
            | "de"
            | "da"
            | "do"
            | "das"
            | "dos"
            | "dum"
            | "duma"
            | "em"
            | "na"
            | "no"
            | "nas"
            | "nos"
            | "num"
            | "numa"
            | "por"
            | "pelo"
            | "pela"
            | "pelos"
            | "pelas"
            | "para"
            | "pra"
            | "pro"
            | "pras"
            | "pros"
            | "ao"
            | "aos"
            | "à"
            | "às"
            | "com"
            | "sem"
            | "sob"
            | "sobre"
            | "entre"
            | "até"
            | "após"
            | "contra"
            | "desde"
            | "e"
            | "ou"
            | "mas"
            | "nem"
            | "que"
            | "se"
            | "meu"
            | "minha"
            | "meus"
            | "minhas"
            | "seu"
            | "sua"
            | "seus"
            | "suas"
            | "nosso"
            | "nossa"
            | "este"
            | "esta"
            | "esse"
            | "essa"
            | "aquele"
            | "aquela"
            | "the"
            | "an"
            | "of"
            | "in"
            | "on"
            | "at"
            | "to"
            | "for"
            | "with"
            | "by"
            | "from"
            | "and"
            | "or"
            | "but"
    )
}

fn is_genitive_word(word: &str) -> bool {
    let norm = filmcraft_project::transcript::normalize_word(word);
    matches!(norm.as_str(), "de" | "da" | "do" | "das" | "dos" | "of")
}

fn slice_char_len(slice: &[SeqWord]) -> usize {
    if slice.is_empty() { 0 } else { slice.iter().map(|w| w.text.chars().count()).sum::<usize>() + slice.len() - 1 }
}

fn join_slice(slice: &[SeqWord]) -> String {
    slice.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
}

/// Find the best 1- or 2-line layout for `slice` obeying `max_chars` and `max_lines`, avoiding
/// single-word widow lines and hanging connector words at the end of Line 1.
fn best_balanced_lines(slice: &[SeqWord], max_chars: usize, max_lines: usize) -> Option<(Vec<String>, i32)> {
    if slice.is_empty() {
        return None;
    }
    if slice.len() == 1 {
        return Some((vec![slice[0].text.clone()], 250));
    }
    let total_len = slice_char_len(slice);
    if max_lines == 1 {
        return (total_len <= max_chars).then(|| {
            let pen = (max_chars as i32 - total_len as i32) + if is_connector_word(&slice[slice.len() - 1].text) { 400 } else { 0 };
            (vec![join_slice(slice)], pen)
        });
    }

    let mut best: Option<(Vec<String>, i32)> = None;
    // Option A: fit on a single line if compact enough
    if total_len <= max_chars {
        let mut pen = (max_chars as i32 - total_len as i32) * 2;
        if total_len > 24 && slice.len() >= 4 {
            pen += 60;
        }
        best = Some((vec![join_slice(slice)], pen));
    }

    // Option B: split into 2 balanced lines at `k`
    for k in 1..slice.len() {
        let l1 = slice_char_len(&slice[..k]);
        let l2 = slice_char_len(&slice[k..]);
        if l1 > max_chars || l2 > max_chars {
            continue;
        }
        let mut pen = (l1 as i32 - l2 as i32).abs() * 2;
        // Widow / orphan single-word line prevention
        if k == 1 {
            pen += 350;
        }
        if slice.len() - k == 1 {
            pen += 350;
        }
        // Never leave a preposition/article/conjunction hanging at the end of Line 1
        if is_connector_word(&slice[k - 1].text) {
            pen += 450;
        }
        // Slight penalty for splitting `Noun | de/da/do Noun` across lines
        if is_genitive_word(&slice[k].text) && !slice[k - 1].text.ends_with([',', ';', ':', '.', '!', '?']) {
            pen += 30;
        }
        // Bonus for splitting at natural punctuation
        if slice[k - 1].text.ends_with([',', ';', ':', '.', '!', '?', '—']) {
            pen -= 35;
        }
        if best.as_ref().is_none_or(|(_, bp)| pen < *bp) {
            best = Some((vec![join_slice(&slice[..k]), join_slice(&slice[k..])], pen));
        }
    }
    best
}

fn smart_caption_groups(words: &[SeqWord], rules: &CaptionRules) -> Vec<(std::ops::Range<usize>, Vec<String>)> {
    let max_chars = rules.max_chars.max(1);
    let max_lines = rules.lines.max(1);
    let mut out = Vec::new();
    let mut a = 0;
    while a < words.len() {
        // Find the furthest word index before a hard break or capacity limit
        let mut limit = a + 1;
        for i in (a + 1)..words.len() {
            let w = &words[i];
            let prev = &words[i - 1];
            let hard_break = w.speaker != prev.speaker || w.start - prev.end >= rules.break_pause || w.end - words[a].start > rules.max_duration;
            if hard_break {
                break;
            }
            if best_balanced_lines(&words[a..=i], max_chars, max_lines).is_none() {
                break;
            }
            limit = i + 1;
            // Stop at sentence-ending punctuation once the block has reasonable content
            if words[i].text.ends_with(['.', '?', '!']) && slice_char_len(&words[a..=i]) >= max_chars / 2 {
                break;
            }
        }

        if limit == a + 1 {
            out.push((a..a + 1, vec![words[a].text.clone()]));
            a += 1;
            continue;
        }

        // Find how far until the next hard break (to avoid leaving a 1-word orphan in the next block)
        let mut next_hard = limit;
        while next_hard < words.len() {
            let w = &words[next_hard];
            let prev = &words[next_hard - 1];
            if w.speaker != prev.speaker || w.start - prev.end >= rules.break_pause || prev.text.ends_with(['.', '?', '!']) {
                break;
            }
            next_hard += 1;
        }

        let mut best_b = limit;
        let mut best_lines = best_balanced_lines(&words[a..limit], max_chars, max_lines).map(|(l, _)| l).unwrap_or_else(|| vec![join_slice(&words[a..limit])]);
        let mut best_score = i32::MAX;

        for b in (a + 1)..=limit {
            let Some((lines, line_pen)) = best_balanced_lines(&words[a..b], max_chars, max_lines) else {
                continue;
            };
            let mut score = line_pen + ((limit - b) as i32) * 28;
            let last_w = &words[b - 1];
            if b < words.len() {
                let next_w = &words[b];
                let continues = next_w.speaker == last_w.speaker && next_w.start - last_w.end < rules.break_pause;
                if continues {
                    if is_connector_word(&last_w.text) {
                        score += 600;
                    }
                    if is_genitive_word(&next_w.text) && !last_w.text.ends_with([',', ';', ':', '.', '!', '?']) {
                        score += 120;
                    }
                    if next_hard > b && next_hard - b == 1 {
                        score += 400;
                    }
                }
            }
            if last_w.text.ends_with(['.', '!', '?']) {
                score -= 80;
            } else if last_w.text.ends_with([',', ';', ':']) {
                score -= 45;
            }
            if score < best_score {
                best_score = score;
                best_b = b;
                best_lines = lines;
            }
        }

        out.push((a..best_b, best_lines));
        a = best_b;
    }
    out
}

/// A caption block made from words.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionBlock {
    pub start: Tick,
    pub end: Tick,
    /// Lines joined by `\n`.
    pub text: String,
    pub speaker: Option<String>,
    /// The word index range it shows.
    pub words: std::ops::Range<usize>,
}

/// Lay words out as caption blocks: words fill lines of at most `max_chars` (a longer single word
/// gets a line of its own) and blocks of `lines` lines; a new block starts at a speaker change,
/// at a pause of `break_pause`, after `max_duration`, or after sentence-ending punctuation once the
/// block is past half full. Times are snapped to frames, blocks never overlap and keep
/// `gap_frames` between them.
pub fn caption_blocks(words: &[SeqWord], rules: &CaptionRules, rate: FrameRate) -> Vec<CaptionBlock> {
    let max_chars = rules.max_chars.max(1);
    let max_lines = rules.lines.max(1);
    let cap_chars = max_chars * max_lines;
    let formatted_groups: Vec<(std::ops::Range<usize>, Vec<String>)> = if rules.smart_balance {
        smart_caption_groups(words, rules)
    } else {
        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        let mut a = 0;
        let mut lines: Vec<usize> = vec![0];
        for i in 0..words.len() {
            let w = &words[i];
            let len = w.text.chars().count();
            if i > a {
                let prev = &words[i - 1];
                let cur = *lines.last().unwrap_or(&0);
                let fits_line = cur + 1 + len <= max_chars;
                let fits = fits_line || lines.len() < max_lines;
                let used: usize = lines.iter().sum::<usize>() + lines.len() - 1;
                let sentence_end = prev.text.ends_with(['.', '?', '!']) && used * 2 >= cap_chars;
                let brk = !fits
                    || w.speaker != prev.speaker
                    || w.start - prev.end >= rules.break_pause
                    || w.end - words[a].start > rules.max_duration
                    || sentence_end;
                if brk {
                    groups.push(a..i);
                    a = i;
                    lines = vec![len];
                    continue;
                }
                if fits_line {
                    if let Some(l) = lines.last_mut() {
                        *l += 1 + len;
                    }
                } else {
                    lines.push(len);
                }
            } else {
                lines = vec![len];
            }
        }
        if a < words.len() {
            groups.push(a..words.len());
        }
        groups
            .into_iter()
            .map(|g| {
                let mut text_lines: Vec<String> = vec![String::new()];
                for w in &words[g.clone()] {
                    let Some(l) = text_lines.last_mut() else { break };
                    if l.is_empty() {
                        l.push_str(&w.text);
                    } else if l.chars().count() + 1 + w.text.chars().count() <= max_chars {
                        l.push(' ');
                        l.push_str(&w.text);
                    } else {
                        text_lines.push(w.text.clone());
                    }
                }
                (g, text_lines)
            })
            .collect()
    };
    finalize_caption_blocks(words, formatted_groups, rules, rate)
}

/// Build [`CaptionBlock`]s from AI-segmented `(start_word, end_word_inclusive, lines)` groups,
/// enforcing `rules.max_chars` and `rules.lines` if any AI-returned group slightly exceeds the
/// character limit.
pub fn caption_blocks_from_ai_groups(words: &[SeqWord], ai_groups: &[(usize, usize, Vec<String>)], rules: &CaptionRules, rate: FrameRate) -> Vec<CaptionBlock> {
    if words.is_empty() || ai_groups.is_empty() {
        return caption_blocks(words, rules, rate);
    }
    let max_chars = rules.max_chars.max(1);
    let max_lines = rules.lines.max(1);
    let mut formatted_groups: Vec<(std::ops::Range<usize>, Vec<String>)> = Vec::new();
    for (start_w, end_w, lines) in ai_groups {
        let a = (*start_w).min(words.len().saturating_sub(1));
        let b = (*end_w + 1).clamp(a + 1, words.len());
        let valid_lines = !lines.is_empty() && lines.len() <= max_lines && lines.iter().all(|l| l.chars().count() <= max_chars);
        if valid_lines {
            formatted_groups.push((a..b, lines.clone()));
        } else if let Some((rebalanced, _)) = best_balanced_lines(&words[a..b], max_chars, max_lines) {
            formatted_groups.push((a..b, rebalanced));
        } else {
            for (sub_r, sub_lines) in smart_caption_groups(&words[a..b], rules) {
                formatted_groups.push(((a + sub_r.start)..(a + sub_r.end), sub_lines));
            }
        }
    }
    finalize_caption_blocks(words, formatted_groups, rules, rate)
}

fn finalize_caption_blocks(
    words: &[SeqWord],
    formatted_groups: Vec<(std::ops::Range<usize>, Vec<String>)>,
    rules: &CaptionRules,
    rate: FrameRate,
) -> Vec<CaptionBlock> {
    // text, frame-snapped times
    let fd = rate.frame_duration();
    let gap = Tick(fd.0 * rules.gap_frames.max(0));
    let mut out: Vec<CaptionBlock> = Vec::new();
    for (g, text_lines) in formatted_groups {
        if g.is_empty() || g.end > words.len() {
            continue;
        }
        let start = rate.snap(words[g.start].start);
        let mut end = rate.snap(words[g.end - 1].end);
        if end < words[g.end - 1].end {
            end += fd;
        }
        out.push(CaptionBlock { start, end: end.max(start + fd), text: text_lines.join("\n"), speaker: words[g.start].speaker.clone(), words: g });
    }
    // no overlaps, minimum duration (into the silence after a block, never over the next one)
    for i in 0..out.len() {
        if i > 0 {
            let min_start = out[i - 1].end + gap;
            if out[i].start < min_start {
                out[i].start = min_start;
                out[i].end = out[i].end.max(min_start + fd);
            }
        }
        let next = out.get(i + 1).map(|n| n.start);
        let b = &mut out[i];
        if b.end - b.start < rules.min_duration {
            let want = b.start + rules.min_duration;
            let mut e = rate.snap(want);
            if e < want {
                e += fd;
            }
            b.end = e;
        }
        if let Some(n) = next {
            b.end = b.end.min(n - gap).max(b.start + fd);
        }
    }
    out
}

/// Caption blocks as captions (ids from `ctx`).
pub fn blocks_to_captions(blocks: &[CaptionBlock], ctx: &mut EditCtx) -> Vec<Caption> {
    blocks
        .iter()
        .map(|b| Caption {
            id: ClipId(ctx.alloc()),
            start: b.start,
            duration: b.end - b.start,
            text: b.text.clone(),
            speaker: b.speaker.clone(),
            cue_id: None,
            settings: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests;

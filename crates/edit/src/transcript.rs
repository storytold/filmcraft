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

/// Lift every range on every unlocked track, leaving gaps (Delete with Lift chosen).
pub fn lift_ranges(seq: &mut Sequence, ranges: Vec<TimeRange>, ctx: &mut EditCtx) -> Tick {
    let tracks: Vec<TrackId> = seq.all_tracks().filter(|t| !t.locked).map(|t| t.id).collect();
    let mut total = Tick::ZERO;
    for r in merge_ranges(ranges) {
        if r.duration <= Tick::ZERO {
            continue;
        }
        crate::lift(seq, &tracks, r, ctx);
        total += r.duration;
    }
    total
}

// ---------------------------------------------------------------------------------------------
// Voice and pauses
// ---------------------------------------------------------------------------------------------

/// Where the sequence's transcribed clips have voice, in sequence time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeqVoice {
    /// Voiced ranges, sorted and merged. From each transcript's waveform voice map; for a
    /// transcript without one (imported, or older), its words.
    pub spans: Vec<TimeRange>,
    /// The ranges a transcribed clip covers (sorted, merged), with the empty stretches of the
    /// timeline between them: the only places a pause can be. An untranscribed clip is never read
    /// as silence, so its dialogue can't be deleted as a pause.
    pub covered: Vec<TimeRange>,
}

/// `r` minus every range in `cut` (any order).
fn subtract(r: TimeRange, cut: &[TimeRange]) -> Vec<TimeRange> {
    let mut out = vec![r];
    for c in cut {
        let mut next = Vec::with_capacity(out.len() + 1);
        for p in out {
            if !p.overlaps(c) {
                next.push(p);
                continue;
            }
            if c.start > p.start {
                next.push(TimeRange::from_bounds(p.start, c.start));
            }
            if c.end() < p.end() {
                next.push(TimeRange::from_bounds(c.end(), p.end()));
            }
        }
        out = next;
    }
    out.retain(|p| p.duration > Tick::ZERO);
    out
}

/// The voice of the sequence (see [`SeqVoice`]); clips are read like [`sequence_words`] (top
/// track first, a lower track's clip only where no higher transcribed clip plays).
pub fn sequence_voice(seq: &Sequence, transcripts: &Transcripts) -> SeqVoice {
    let mut spans = Vec::new();
    let mut covered = Vec::new();
    let mut claimed: Vec<TimeRange> = Vec::new();
    for track in &seq.audio_tracks {
        let mut mine = Vec::new();
        for it in &track.items {
            if !it.enabled || it.reverse || it.frame_hold.is_some() || it.speed <= 0.0 {
                continue;
            }
            let Some(tr) = transcripts.get(&it.item) else { continue };
            mine.push(it.range());
            let visible = subtract(it.range(), &claimed);
            if visible.is_empty() {
                continue;
            }
            let speed = it.speed;
            let to_tl = |m: Tick| it.start + ticks((m - it.source_in).0 as f64 / speed);
            let media_end = it.source_in + ticks(it.duration.0 as f64 * speed);
            let media: Vec<(Tick, Tick)> =
                if tr.voice.is_empty() { tr.words.iter().map(|w| (w.start, w.end.max(w.start))).collect() } else { tr.voice.clone() };
            for (a, b) in media {
                if b <= it.source_in || a >= media_end {
                    continue;
                }
                let r = TimeRange::from_bounds(to_tl(a.max(it.source_in)), to_tl(b.min(media_end)));
                for v in &visible {
                    let (s, e) = (r.start.max(v.start), r.end().min(v.end()));
                    if e > s {
                        spans.push(TimeRange::from_bounds(s, e));
                    }
                }
            }
            covered.extend(visible);
        }
        claimed.extend(mine);
    }
    // an empty stretch of the timeline (no clip on any track) between two transcribed ranges is
    // silence too: it joins the silence around it, so Delete closes it with the pause
    let covered = merge_ranges(covered);
    let mut used: Vec<TimeRange> = seq.all_tracks().flat_map(|t| t.items.iter().map(|i| i.range())).collect();
    used.sort_by_key(|r| r.start);
    let mut gaps = Vec::new();
    for w in covered.windows(2) {
        let g = TimeRange::from_bounds(w[0].end(), w[1].start);
        let lo = used.partition_point(|u| u.end() <= g.start);
        if g.duration > Tick::ZERO && !used[lo..].iter().take_while(|u| u.start < g.end()).any(|u| u.overlaps(&g)) {
            gaps.push(g);
        }
    }
    SeqVoice { spans: merge_ranges(spans), covered: merge_ranges(covered.into_iter().chain(gaps).collect()) }
}

/// A pause: silence between voice inside the transcribed part of the sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Pause {
    /// From the end of the voice before it to the start of the voice after it.
    pub range: TimeRange,
    /// The last word before it and the first word after it (`None` at the start or the end).
    pub before: Option<usize>,
    pub after: Option<usize>,
    /// Voice on that side. False where the pause runs to the edge of the transcribed range: a
    /// take's lead-in before its first word, or its tail after the last.
    pub voiced_before: bool,
    pub voiced_after: bool,
}

/// Pauses of at least `min`: silences between the voiced spans of `voice` within its covered
/// ranges, and never inside a recognised word, whatever the waveform says there (a stop closure,
/// a quiet syllable or word ending below the gate): a pause is cut, a word is speech.
pub fn find_voice_pauses(words: &[SeqWord], voice: &SeqVoice, min: Tick) -> Vec<Pause> {
    let mid = |w: &SeqWord| Tick(w.start.0 + (w.end.0 - w.start.0) / 2);
    // everything that is speech, merged once (sorted): the pauses are what the covered ranges
    // leave of it, found in one sweep however long the sequence is
    let mut speech: Vec<TimeRange> = voice.spans.clone();
    speech.extend(words.iter().map(SeqWord::range).filter(|r| r.duration > Tick::ZERO));
    let speech = merge_ranges(speech);
    let mut out = Vec::new();
    for c in &voice.covered {
        let lo = speech.partition_point(|v| v.end() <= c.start);
        let mut gaps = Vec::new();
        let mut at = c.start;
        for v in speech[lo..].iter().take_while(|v| v.start < c.end()) {
            if v.start > at {
                gaps.push(TimeRange::from_bounds(at, v.start));
            }
            at = at.max(v.end());
        }
        if at < c.end() {
            gaps.push(TimeRange::from_bounds(at, c.end()));
        }
        for g in gaps {
            if g.duration < min.max(Tick(1)) {
                continue;
            }
            let m = Tick(g.start.0 + g.duration.0 / 2);
            let after_i = words.partition_point(|w| mid(w) < m);
            out.push(Pause {
                range: g,
                before: after_i.checked_sub(1),
                after: (after_i < words.len()).then_some(after_i),
                voiced_before: g.start > c.start,
                voiced_after: g.end() < c.end(),
            });
        }
    }
    out.sort_by_key(|p| p.range.start);
    out
}

/// What Delete removes for a pause: the silence less `keep_after` after the voice before it and
/// `keep_before` before the voice after it (a tight YouTube pause cut keeps about 30 ms and 35 ms), rounded
/// inward to frames. `None` when less than a frame would go.
pub fn pause_cut(p: &Pause, keep_after: Tick, keep_before: Tick, rate: FrameRate) -> Option<TimeRange> {
    let s = p.range.start + if p.voiced_before { keep_after.max(Tick::ZERO) } else { Tick::ZERO };
    let e = p.range.end() - if p.voiced_after { keep_before.max(Tick::ZERO) } else { Tick::ZERO };
    let (s, e) = (rate.snap_edit(s), rate.snap_frame(e));
    (e > s).then(|| TimeRange::from_bounds(s, e))
}

/// Voice with no word on it (a stutter or restart the recogniser tidied away, a laugh) of at least
/// `min`. The Text panel shows these, and they are never pauses.
pub fn unlabelled_speech(words: &[SeqWord], voice: &SeqVoice, min: Tick) -> Vec<TimeRange> {
    let pad = Tick(TICKS_PER_SECOND / 12);
    let near: Vec<TimeRange> = words.iter().map(|w| TimeRange::from_bounds(w.start - pad, w.end.max(w.start) + pad)).collect();
    let mut out = Vec::new();
    for v in &voice.spans {
        let lo = near.partition_point(|n| n.end() <= v.start);
        let hits: Vec<TimeRange> = near[lo..].iter().take_while(|n| n.start < v.end()).copied().collect();
        out.extend(subtract(*v, &hits).into_iter().filter(|r| r.duration >= min));
    }
    out
}

/// One entry of the Text panel's transcript, in time order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Word(usize),
    Pause(usize),
    /// Index into the unlabelled speech list.
    Speech(usize),
}

/// Words, pauses and unlabelled speech in time order (a pause sits between the words around it).
pub fn layout(words: &[SeqWord], pauses: &[Pause], speech: &[TimeRange]) -> Vec<Token> {
    let mut v: Vec<(Tick, u8, Token)> = Vec::with_capacity(words.len() + pauses.len() + speech.len());
    let mid = |a: Tick, b: Tick| Tick(a.0 + (b.0 - a.0) / 2);
    v.extend(words.iter().enumerate().map(|(i, w)| (mid(w.start, w.end), 1, Token::Word(i))));
    v.extend(pauses.iter().enumerate().map(|(i, p)| (mid(p.range.start, p.range.end()), 0, Token::Pause(i))));
    v.extend(speech.iter().enumerate().map(|(i, r)| (mid(r.start, r.end()), 2, Token::Speech(i))));
    v.sort_by_key(|(t, k, _)| (*t, *k));
    v.into_iter().map(|(_, _, t)| t).collect()
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
        }
    }
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
            let brk =
                !fits || w.speaker != prev.speaker || w.start - prev.end >= rules.break_pause || w.end - words[a].start > rules.max_duration || sentence_end;
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
    // text, frame-snapped times
    let fd = rate.frame_duration();
    let gap = Tick(fd.0 * rules.gap_frames.max(0));
    let mut out: Vec<CaptionBlock> = Vec::new();
    for g in groups {
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

//! Project-wide, source-time transcript search. It never edits the document or media.
use std::collections::VecDeque;

use filmcraft_project::transcript::normalize_word;
use filmcraft_project::{ItemId, ItemKind, Project};
use filmcraft_time::Tick;
use serde::Serialize;
use serde_json::{Value, json};

use crate::commands::{bad, str_p};
use crate::{Result, Session};

const MAX_WORDS: usize = 1_000_000;
const MAX_ITEMS: usize = 100_000;

#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub item: ItemId,
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub start: Tick,
    pub end: Tick,
    pub text: String,
    pub context: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub matches: Vec<Hit>,
    pub transcribed_sources: usize,
    pub untranscribed_sources: usize,
    pub scanned_words: usize,
    /// More matches or sources may exist beyond the result/work limits.
    pub incomplete: bool,
}

fn terms(query: &str) -> std::result::Result<Vec<String>, String> {
    if query.len() > 1024 {
        return Err("search query must be at most 1024 bytes".into());
    }
    let words: Vec<_> = query.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
    if words.len() > 32 {
        return Err("search query must contain at most 32 words".into());
    }
    Ok(words)
}

fn excerpt(words: &[filmcraft_project::Word]) -> String {
    // Bound both allocation and display work for imported transcripts with enormous words.
    let mut out = String::new();
    for word in words.iter().take(40) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.text.chars().take(128));
        if out.len() >= 1024 {
            break;
        }
    }
    out
}

/// Literal word phrases, Unicode lowercase, ignoring punctuation at word edges (as the transcript
/// editor does). `keep_going` makes background readers cancellable; a cancelled report is incomplete.
pub fn search(project: &Project, query: &str, limit: usize, mut keep_going: impl FnMut() -> bool) -> std::result::Result<Report, String> {
    if !(1..=500).contains(&limit) {
        return Err("search limit must be between 1 and 500".into());
    }
    let needle = terms(query)?;
    let mut report = Report::default();
    for item in project.items.values().take(MAX_ITEMS) {
        if matches!(&item.kind, ItemKind::Media(m) if m.info.has_audio()) {
            if project.transcripts.contains_key(&item.id) {
                report.transcribed_sources += 1;
            } else {
                report.untranscribed_sources += 1;
            }
        }
    }
    report.incomplete = project.items.len() > MAX_ITEMS;
    if needle.is_empty() {
        return Ok(report);
    }
    'sources: for item in project.items.values().take(MAX_ITEMS) {
        let ItemKind::Media(media) = &item.kind else { continue };
        let Some(transcript) = project.transcripts.get(&item.id) else { continue };
        let mut window = VecDeque::with_capacity(needle.len());
        for (index, word) in transcript.words.iter().enumerate() {
            if report.scanned_words >= MAX_WORDS || !keep_going() {
                report.incomplete = true;
                break 'sources;
            }
            report.scanned_words += 1;
            // Do not normalize unbounded hostile strings.
            if word.text.len() > 8192 {
                window.clear();
                report.incomplete = true;
                continue;
            }
            window.push_back(word.normalized());
            if window.len() > needle.len() {
                window.pop_front();
            }
            if window.len() != needle.len() || !window.iter().eq(needle.iter()) {
                continue;
            }
            let from = index + 1 - needle.len();
            let Some(first) = transcript.words.get(from) else { continue };
            if first.start.0 < 0 || word.end <= first.start || word.end > media.duration() {
                continue;
            }
            if report.matches.len() == limit {
                report.incomplete = true;
                break 'sources;
            }
            let matched = &transcript.words[from..=index];
            let context = &transcript.words[from.saturating_sub(3)..index.saturating_add(4).min(transcript.words.len())];
            report.matches.push(Hit {
                item: item.id,
                name: item.name.chars().take(256).collect(),
                from,
                to: index,
                start: first.start,
                end: word.end,
                text: excerpt(matched),
                context: excerpt(context),
            });
        }
    }
    Ok(report)
}

pub(crate) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let query = str_p(p, "query").ok_or_else(|| bad("transcript.search", "`query` must be a string"))?;
    let limit = match p.get("limit") {
        None => 100,
        Some(v) => {
            v.as_u64().filter(|n| (1..=500).contains(n)).ok_or_else(|| bad("transcript.search", "`limit` must be an integer between 1 and 500"))? as usize
        }
    };
    let report = search(&s.project, query, limit, || true).map_err(|e| bad("transcript.search", e))?;
    serde_json::to_value(report).map_err(|e| bad("transcript.search", e.to_string()))
}

/// Validate a hit against the current source transcript, then seek without setting project marks.
pub(crate) fn open(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.openSearchResult";
    let item = p.get("item").and_then(Value::as_u64).map(ItemId).ok_or_else(|| bad(cmd, "`item` is required"))?;
    let index = |key| p.get(key).and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or_else(|| bad(cmd, format!("`{key}` must be a word index")));
    let (from, to) = (index("from")?, index("to")?);
    let words = &s.project.transcripts.get(&item).ok_or_else(|| bad(cmd, "source transcript is gone"))?.words;
    let range = words.get(from..=to).filter(|w| !w.is_empty()).ok_or_else(|| bad(cmd, "search result is out of range"))?;
    let (start, end) = (range[0].start, range[range.len() - 1].end);
    let view = crate::clip_ops::source_view(s, item).ok_or_else(|| bad(cmd, "source is gone"))?;
    if start < view.start || end > view.end || end <= start {
        return Err(bad(cmd, "search result is outside the source"));
    }
    s.execute("source.open", json!({"item":item.0}))?;
    s.execute("source.setPlayhead", json!({"time":start.0}))?;
    Ok(json!({"item":item.0,"start":start.0,"end":end.0}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn session() -> (Session, ItemId, ItemId) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clips = &s.active_sequence().unwrap().audio_tracks[0].items;
        let (a, b) = (clips[0].item, clips[1].item);
        for (item, text) in [(a, ["Before", "HÉLLO,", "world!", "hello", "world", "after"]), (b, ["Other", "hello", "world", "clip", "hello", "world"])] {
            let words: Vec<_> = text.iter().enumerate().map(|(i, w)| json!({"text":w,"start": i as i64 * 100,"end":i as i64 * 100 + 90})).collect();
            s.execute("transcript.set", json!({"item":item.0,"transcript":{"words":words}})).unwrap();
        }
        s.state.active_sequence = None;
        (s, a, b)
    }

    #[test]
    fn project_search_finds_unused_sources_unicode_context_and_exact_ranges() {
        let (mut s, a, b) = session();
        let snapshot = s.project.clone();
        let r = s.execute("transcript.search", json!({"scope":"project","query":"hello world"})).unwrap();
        assert_eq!(r["matches"].as_array().unwrap().len(), 3);
        assert_eq!(r["matches"][0]["item"], a.0);
        assert_eq!(r["matches"][1]["item"], b.0);
        assert_eq!(r["matches"][0]["start"], 300);
        assert!(r["matches"][0]["context"].as_str().unwrap().contains("Before"));
        let r = s.execute("transcript.search", json!({"scope":"project","query":"héllo world"})).unwrap();
        assert_eq!(r["matches"][0]["start"], 100);
        assert_eq!(r["matches"][0]["end"], 290);
        assert!(Arc::ptr_eq(&snapshot, &s.project));
        assert!(s.execute("transcript.search", json!({"query":"hello"})).unwrap()["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hostile_parameters_limits_and_cancellation_are_bounded() {
        let (mut s, _, _) = session();
        let snapshot = s.project.clone();
        for p in [
            json!({"scope":false,"query":"hello"}),
            json!({"scope":"project","query":7}),
            json!({"scope":"project","query":"hello","limit":0}),
            json!({"scope":"project","query":"hello","limit":1.5}),
            json!({"scope":"project","query":"x".repeat(1025)}),
        ] {
            assert!(s.execute("transcript.search", p).is_err());
        }
        let r = search(&s.project, "hello world", 1, || true).unwrap();
        assert_eq!(r.matches.len(), 1);
        assert!(r.incomplete);
        let r = search(&s.project, "hello", 100, || false).unwrap();
        assert_eq!(r.scanned_words, 0);
        assert!(r.incomplete);
        assert!(search(&s.project, " ", 100, || true).unwrap().matches.is_empty());
        assert!(Arc::ptr_eq(&snapshot, &s.project));
    }

    #[test]
    fn opening_hit_seeks_the_source_without_marks_or_document_changes() {
        let (mut s, a, _) = session();
        let snapshot = s.project.clone();
        let result = s.execute("transcript.openSearchResult", json!({"item":a.0,"from":1,"to":2})).unwrap();
        assert_eq!(result["start"], 100);
        assert_eq!(s.state.source_item, Some(a));
        assert!(Arc::ptr_eq(&snapshot, &s.project));
        for p in [json!({"item":a.0,"from":3,"to":1}), json!({"item":a.0,"from":0,"to":u64::MAX}), json!({"item":999999,"from":0,"to":0})] {
            assert!(s.execute("transcript.openSearchResult", p).is_err());
            assert_eq!(s.state.source_item, Some(a));
            assert!(Arc::ptr_eq(&snapshot, &s.project));
        }
    }

    #[test]
    fn edits_undo_and_roundtrip_are_seen_by_new_searches() {
        let (mut s, a, _) = session();
        let before = search(&s.project, "héllo", 100, || true).unwrap();
        assert_eq!(before.matches.len(), 1);
        s.execute("transcript.delete", json!({"items":[a.0]})).unwrap();
        assert!(search(&s.project, "héllo", 100, || true).unwrap().matches.is_empty());
        s.undo();
        assert_eq!(search(&s.project, "héllo", 100, || true).unwrap().matches.len(), 1);
        let encoded = serde_json::to_vec(&*s.project).unwrap();
        let reopened: Project = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(search(&reopened, "hello world", 100, || true).unwrap().matches.len(), 3);
    }

    #[test]
    #[ignore = "manual bounded search throughput measurement"]
    fn project_search_throughput() {
        let (mut s, a, _) = session();
        let transcript = Arc::make_mut(Arc::make_mut(&mut s.project).transcripts.get_mut(&a).unwrap());
        transcript.words = (0..200_000).map(|i| filmcraft_project::Word::new("interview", Tick(i * 100), Tick(i * 100 + 90))).collect();
        let start = web_time::Instant::now();
        let r = search(&s.project, "unmatched phrase", 100, || true).unwrap();
        eprintln!("search_words={} elapsed_ms={:.2}", r.scanned_words, start.elapsed().as_secs_f64() * 1000.0);
        assert!(r.matches.is_empty());
    }
}

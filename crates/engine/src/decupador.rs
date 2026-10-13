//! Decupador Inteligente (`decupador.*`): the studio's decoupage workflow inside the editor.
//!
//! The decupador decides *what* to keep (its plans are written from the transcript and the rules
//! of its `knowledge_base/`); these commands do the editor's side of it:
//!
//! - `decupador.applyPlan` builds a sequence per cut of a `premiere_cuts.json` (the flattened plan
//!   the decupador compiles for an editor): every segment straight from the original camera file,
//!   with the external recorder's audio linked under it when the plan has one (Áudio Duplo). It
//!   replaces the Premiere UXP panel. The plan is turned into an OpenTimelineIO document and goes
//!   through the same import as any OTIO file.
//! - `decupador.candidates` / `decupador.markCandidates` list (or mark on the timeline) the
//!   mechanical candidates of the decupador's step A over the sequence transcript: silences of at
//!   least `minSilence` seconds, PT-BR filler words, and low-confidence spans where the recogniser
//!   was unsure (stammers, "eeee"). They are candidates, never cuts: a pause can be rhetorical and
//!   "é" is also a verb, so the decision stays with whoever writes the plan.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use filmcraft_edit::transcript::SeqWord;
use filmcraft_interchange::Format;
use filmcraft_project::{Label, Marker, MarkerId, MarkerKind, SequenceSettings};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p};
use crate::{EngineError, Result, Session};

/// The decupador's PT-BR filler words (`src/decoupage.py::DEFAULT_MULETAS`). Matched by case but
/// with accents significant: "é" (filler or verb) is not the conjunction "e".
pub const MULETAS: &[&str] = &["né", "então", "tipo", "é", "aí", "sabe", "na verdade", "meio que", "assim", "tipo assim"];

/// Silence threshold of the decupador (`--min-silence`, seconds).
pub const MIN_SILENCE: f64 = 0.8;
/// Words under this probability are low-confidence candidates.
pub const MIN_PROB: f64 = 0.6;
/// Low-confidence words closer than this (seconds) form one span.
pub const MAX_GAP: f64 = 0.6;

/// Name prefix of the markers `decupador.markCandidates` adds (and replaces on the next run).
pub const MARKER_PREFIX: &str = "Decupador · ";

// ---------------------------------------------------------------------------------------------
// premiere_cuts.json → OTIO
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Plan {
    #[serde(default)]
    source_video: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    cuts: Vec<Cut>,
}

#[derive(Deserialize)]
struct Cut {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    output_filename: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    #[serde(default)]
    source_video: Option<String>,
    #[serde(default)]
    source_audio: Option<String>,
    #[serde(default)]
    audio_start: Option<f64>,
}

impl Cut {
    fn name(&self, slug: Option<&str>) -> String {
        let base = self
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| self.output_filename.as_deref().map(|f| Path::new(f).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.id.clone());
        match slug {
            Some(s) if !s.is_empty() => format!("{base} — {s}"),
            _ => base,
        }
    }
}

/// The decupador project folder of a plan at `<root>/analysis/<slug>/premiere_cuts.json`.
fn plan_root(plan: &Path) -> Option<PathBuf> {
    Some(plan.parent()?.parent()?.parent()?.to_path_buf())
}

fn resolve(root: &Path, rel: &str) -> String {
    let p = Path::new(rel);
    if p.is_absolute() { rel.to_string() } else { root.join(p).to_string_lossy().to_string() }
}

/// A placed segment, in sequence frames and source ticks.
#[derive(Clone, Debug, PartialEq)]
struct Placed {
    video: String,
    audio: String,
    /// The audio is a separate file (external recorder) rather than the camera's own track.
    external_audio: bool,
    video_in: i64,
    frames: i64,
    audio_in: Tick,
}

/// Snap the plan's segments to frames: the video In to the nearest frame, the duration to whole
/// frames, and the audio In moved by the same amount so picture and sound stay in sync.
fn place(cut: &Cut, plan_video: Option<&str>, root: &Path, rate: FrameRate) -> std::result::Result<Vec<Placed>, String> {
    let mut out = Vec::new();
    for (i, s) in cut.segments.iter().enumerate() {
        let video = s.source_video.as_deref().or(plan_video).ok_or_else(|| format!("segment {} of `{}` has no source_video", i + 1, cut.id))?;
        if !(s.end > s.start) || s.start < 0.0 {
            return Err(format!("segment {} of `{}` has an empty or negative range", i + 1, cut.id));
        }
        let video_in = rate.frame_at(rate.snap_nearest(Tick::from_seconds_f64(s.start)));
        let video_out = rate.frame_at(rate.snap_nearest(Tick::from_seconds_f64(s.end)));
        let frames = video_out - video_in;
        if frames <= 0 {
            continue; // shorter than a frame
        }
        let shift = rate.tick_of(video_in) - Tick::from_seconds_f64(s.start);
        let (audio, external, audio_in) = match &s.source_audio {
            Some(a) => (resolve(root, a), true, Tick::from_seconds_f64(s.audio_start.unwrap_or(s.start)) + shift),
            None => (resolve(root, video), false, rate.tick_of(video_in)),
        };
        out.push(Placed { video: resolve(root, video), audio, external_audio: external, video_in, frames, audio_in: audio_in.max(Tick::ZERO) });
    }
    Ok(out)
}

fn rt_frames(f: i64, rate: FrameRate) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "rate": rate.as_f64(), "value": f as f64})
}

fn rt_ticks(t: Tick, rate: FrameRate) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "rate": rate.as_f64(), "value": t.0 as f64 * rate.num as f64 / (rate.den as f64 * TICKS_PER_SECOND as f64)})
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string())
}

fn clip(path: &str, start: Value, duration: Value, meta: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Clip.1",
        "name": file_name(path),
        "source_range": {"OTIO_SCHEMA": "TimeRange.1", "start_time": start, "duration": duration},
        "media_reference": {"OTIO_SCHEMA": "ExternalReference.1", "name": file_name(path), "target_url": path, "available_range": null, "metadata": {}},
        "effects": [], "markers": [], "enabled": true,
        "metadata": {"filmcraft": meta},
    })
}

/// One OTIO timeline: V1 with the picture, A1 with the sound (linked pairs, one per segment).
fn timeline(name: &str, placed: &[Placed], rate: FrameRate, settings: &SequenceSettings) -> Value {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for (i, p) in placed.iter().enumerate() {
        let dur = rate.tick_of(p.frames);
        video.push(clip(&p.video, rt_frames(p.video_in, rate), rt_frames(p.frames, rate), json!({"link": i + 1})));
        audio.push(clip(
            &p.audio,
            rt_ticks(p.audio_in, rate),
            rt_frames(p.frames, rate),
            json!({"link": i + 1, "source_in_ticks": p.audio_in.0, "duration_ticks": dur.0}),
        ));
    }
    let track = |name: &str, kind: &str, children: Vec<Value>| json!({"OTIO_SCHEMA": "Track.1", "name": name, "kind": kind, "children": children, "source_range": null, "effects": [], "markers": [], "enabled": true, "metadata": {}});
    json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": name,
        "global_start_time": null,
        "metadata": {"filmcraft": {"settings": settings}},
        "tracks": {
            "OTIO_SCHEMA": "Stack.1", "name": "tracks", "source_range": null, "effects": [], "markers": [], "enabled": true, "metadata": {},
            "children": [track("Video 1", "Video", video), track("Audio 1", "Audio", audio)],
        },
    })
}

/// Sequence settings matching the camera file (size, rate, pixel aspect) when it can be probed.
fn camera_settings(s: &Session, path: &str) -> Option<SequenceSettings> {
    let src = s.media.open_file(path, &*s.services).ok()?;
    let v = src.info().video.clone()?;
    Some(SequenceSettings {
        width: v.width,
        height: v.height,
        frame_rate: v.frame_rate,
        par: v.par,
        preset: format!("Decupador {}x{} {}", v.width, v.height, v.frame_rate.label()),
        ..SequenceSettings::default()
    })
}

fn apply_plan(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "decupador.applyPlan";
    let path = str_p(p, "path").ok_or_else(|| bad(CMD, "`path` (the premiere_cuts.json) is required"))?.to_string();
    let text = std::fs::read_to_string(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let plan: Plan = serde_json::from_str(&text).map_err(|e| bad(CMD, format!("{path} is not a decupador plan: {e}")))?;
    let root = match str_p(p, "root") {
        Some(r) => PathBuf::from(r),
        None => plan_root(Path::new(&path)).ok_or_else(|| bad(CMD, "pass `root`: the plan is not inside <decupador>/analysis/<slug>/"))?,
    };
    let wanted = str_p(p, "cut");
    let cuts: Vec<&Cut> = plan.cuts.iter().filter(|c| wanted.is_none_or(|w| c.id == w)).collect();
    if cuts.is_empty() {
        let ids: Vec<&str> = plan.cuts.iter().map(|c| c.id.as_str()).collect();
        return Err(bad(CMD, match wanted {
            Some(w) => format!("no cut `{w}` in the plan (cuts: {})", ids.join(", ")),
            None => "the plan has no cuts".into(),
        }));
    }
    let mut timelines = Vec::new();
    let mut summary = Vec::new();
    let mut missing = Vec::new();
    for cut in cuts {
        let first = cut.segments.iter().find_map(|g| g.source_video.as_deref()).or(plan.source_video.as_deref());
        let first = first.map(|f| resolve(&root, f));
        let settings = first.as_deref().and_then(|f| camera_settings(s, f)).unwrap_or_default();
        let rate = settings.frame_rate;
        let placed = place(cut, plan.source_video.as_deref(), &root, rate).map_err(|e| bad(CMD, e))?;
        if placed.is_empty() {
            continue;
        }
        for f in placed.iter().flat_map(|x| [&x.video, &x.audio]) {
            if !Path::new(f).exists() && !missing.contains(f) {
                missing.push(f.clone());
            }
        }
        let name = cut.name(plan.slug.as_deref());
        let frames: i64 = placed.iter().map(|x| x.frames).sum();
        summary.push(json!({
            "cut": cut.id, "name": name, "segments": placed.len(),
            "seconds": rate.tick_of(frames).seconds(),
            "externalAudio": placed.iter().any(|x| x.external_audio),
        }));
        timelines.push(timeline(&name, &placed, rate, &settings));
    }
    if timelines.is_empty() {
        return Err(bad(CMD, "no segment of the plan is at least one frame long"));
    }
    let doc = json!({"OTIO_SCHEMA": "SerializableCollection.1", "name": plan.slug.clone().unwrap_or_else(|| "Decupador".into()), "children": timelines, "metadata": {}});
    let fake = root.join(format!("{}.otio", plan.slug.as_deref().unwrap_or("decupador"))).to_string_lossy().to_string();
    let bytes = serde_json::to_vec(&doc).map_err(|e| EngineError::Other(e.to_string()))?;
    let mut r = crate::interchange::import(s, &fake, &bytes, Format::Otio)?;
    r["cuts"] = json!(summary);
    r["missingFiles"] = json!(missing);
    Ok(r)
}

// ---------------------------------------------------------------------------------------------
// Step A candidates over the sequence transcript
// ---------------------------------------------------------------------------------------------

/// A candidate for the decoupage, in sequence time.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub kind: &'static str,
    pub start: Tick,
    pub end: Tick,
    /// What it is: the filler, the words around a silence, the unsure words.
    pub text: String,
    /// Seconds of silence, or the lowest probability of a span.
    pub value: f64,
}

/// Lower case, punctuation trimmed, accents kept (`src/decoupage.py::_normalize`).
fn norm(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

fn secs(t: Tick) -> f64 {
    t.seconds()
}

pub fn candidates(words: &[SeqWord], min_silence: f64, min_prob: f64, max_gap: f64, muletas: &[String]) -> Vec<Candidate> {
    let mut out = Vec::new();
    // silences: gaps between consecutive words
    for w in words.windows(2) {
        let gap = secs(w[1].start) - secs(w[0].end);
        if gap >= min_silence {
            out.push(Candidate { kind: "silence", start: w[0].end, end: w[1].start, text: format!("{} … {}", w[0].text, w[1].text), value: (gap * 100.0).round() / 100.0 });
        }
    }
    // fillers: words and phrases, longest first so "tipo assim" wins over "tipo"
    let normed: Vec<String> = words.iter().map(|w| norm(&w.text)).collect();
    let mut phrases: Vec<Vec<String>> = muletas.iter().map(|m| m.split_whitespace().map(norm).filter(|x| !x.is_empty()).collect::<Vec<_>>()).filter(|v| !v.is_empty()).collect();
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut i = 0;
    while i < normed.len() {
        match phrases.iter().find(|ph| i + ph.len() <= normed.len() && normed[i..i + ph.len()] == ph[..]) {
            Some(ph) => {
                let j = i + ph.len() - 1;
                out.push(Candidate { kind: "filler", start: words[i].start, end: words[j].end, text: ph.join(" "), value: 0.0 });
                i = j + 1;
            }
            None => i += 1,
        }
    }
    // low confidence: unsure words joined across gaps up to max_gap
    let mut span: Vec<&SeqWord> = Vec::new();
    let flush = |span: &mut Vec<&SeqWord>, out: &mut Vec<Candidate>| {
        if let (Some(a), Some(b)) = (span.first(), span.last()) {
            let low = span.iter().map(|w| f64::from(w.confidence)).fold(1.0, f64::min);
            out.push(Candidate { kind: "lowConfidence", start: a.start, end: b.end, text: span.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "), value: (low * 1000.0).round() / 1000.0 });
        }
        span.clear();
    };
    for w in words {
        if f64::from(w.confidence) < min_prob {
            if span.last().is_some_and(|l| secs(w.start) - secs(l.end) > max_gap) {
                flush(&mut span, &mut out);
            }
            span.push(w);
        } else {
            flush(&mut span, &mut out);
        }
    }
    flush(&mut span, &mut out);
    out.sort_by_key(|c| (c.start, c.end));
    out
}

fn params(p: &Value) -> (f64, f64, f64, Vec<String>) {
    let muletas = match p.get("muletas").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => MULETAS.iter().map(|m| m.to_string()).collect(),
    };
    (f64_p(p, "minSilence").unwrap_or(MIN_SILENCE), f64_p(p, "minProb").unwrap_or(MIN_PROB), f64_p(p, "maxGap").unwrap_or(MAX_GAP), muletas)
}

fn sequence_candidates(s: &Session, p: &Value) -> Vec<Candidate> {
    let (min_silence, min_prob, max_gap, muletas) = params(p);
    candidates(&crate::transcript::sequence_words(s), min_silence, min_prob, max_gap, &muletas)
}

fn candidate_json(c: &Candidate) -> Value {
    json!({"kind": c.kind, "start": c.start.0, "end": c.end.0, "startSeconds": secs(c.start), "endSeconds": secs(c.end), "text": c.text, "value": c.value})
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let cs = sequence_candidates(s, p);
    let count = |k: &str| cs.iter().filter(|c| c.kind == k).count();
    Ok(json!({
        "silences": count("silence"), "fillers": count("filler"), "lowConfidence": count("lowConfidence"),
        "candidates": cs.iter().map(candidate_json).collect::<Vec<_>>(),
    }))
}

fn marker_of(c: &Candidate) -> (String, String, Label) {
    match c.kind {
        "silence" => (format!("{MARKER_PREFIX}Silêncio {:.1}s", c.value), c.text.clone(), Label::Rose),
        "filler" => (format!("{MARKER_PREFIX}Muleta “{}”", c.text), String::new(), Label::Yellow),
        _ => (format!("{MARKER_PREFIX}Baixa confiança ({:.2})", c.value), c.text.clone(), Label::Mango),
    }
}

fn mark(s: &mut Session, p: &Value) -> Result<Value> {
    let cs = sequence_candidates(s, p);
    let replace = bool_p(p, "replace").unwrap_or(true);
    let n = cs.len();
    let removed = s.edit_sequence("Mark Decoupage Candidates", |q, ctx, _| {
        let before = q.markers.len();
        if replace {
            q.markers.retain(|m| !m.name.starts_with(MARKER_PREFIX));
        }
        let removed = before - q.markers.len();
        for c in &cs {
            let (name, comment, color) = marker_of(c);
            q.markers.push(Marker { id: MarkerId(ctx.alloc()), start: c.start, duration: (c.end - c.start).max(Tick::ZERO), name, comment, kind: MarkerKind::Comment, color });
        }
        q.markers.sort_by_key(|m| m.start);
        Ok(removed)
    })?;
    let count = |k: &str| cs.iter().filter(|c| c.kind == k).count();
    Ok(json!({"markers": n, "replaced": removed, "silences": count("silence"), "fillers": count("filler"), "lowConfidence": count("lowConfidence")}))
}

fn has_words(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if crate::transcript::sequence_words(s).is_empty() { Err("the sequence has no transcript (Transcribe first)".into()) } else { Ok(()) }
}

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: fn(&Session) -> std::result::Result<(), String>, run: fn(&mut Session, &Value) -> Result<Value>, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("decupador.applyPlan", "Apply Decoupage Plan…", &["Sequence", "Decupador Inteligente"], r#"{"path":str,"cut":str?,"root":str?}"#, always, apply_plan, true),
        spec(
            "decupador.markCandidates",
            "Mark Decoupage Candidates",
            &["Sequence", "Decupador Inteligente"],
            r#"{"minSilence":f?,"minProb":f?,"maxGap":f?,"muletas":[str]?,"replace":bool?}"#,
            has_words,
            mark,
            true,
        ),
        spec("decupador.candidates", "List Decoupage Candidates", &[], r#"{"minSilence":f?,"minProb":f?,"maxGap":f?,"muletas":[str]?}"#, has_words, list, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ClipId, ItemId};

    fn w(text: &str, a: f64, b: f64, conf: f32) -> SeqWord {
        SeqWord { text: text.into(), start: Tick::from_seconds_f64(a), end: Tick::from_seconds_f64(b), clip: ClipId(1), item: ItemId(1), index: 0, track: 0, speaker: None, confidence: conf }
    }

    fn muletas() -> Vec<String> {
        MULETAS.iter().map(|m| m.to_string()).collect()
    }

    #[test]
    fn finds_silences_fillers_and_unsure_spans() {
        let words = vec![
            w("Então,", 0.0, 0.3, 0.9),
            w("isso", 0.4, 0.6, 0.9),
            w("é", 0.6, 0.7, 0.9),
            w("importante.", 0.7, 1.2, 0.9),
            w("Tipo", 2.5, 2.7, 0.9),
            w("assim,", 2.7, 3.0, 0.9),
            w("eee", 3.1, 3.5, 0.3),
            w("hum", 3.6, 3.8, 0.5),
            w("e", 3.9, 4.0, 0.9),
        ];
        let c = candidates(&words, 0.8, 0.6, 0.6, &muletas());
        let kinds: Vec<(&str, &str)> = c.iter().map(|c| (c.kind, c.text.as_str())).collect();
        assert_eq!(kinds, vec![("filler", "então"), ("filler", "é"), ("silence", "importante. … Tipo"), ("filler", "tipo assim"), ("lowConfidence", "eee hum")]);
        assert_eq!(c[2].value, 1.3);
        assert_eq!(c[4].value, 0.3);
        // the conjunction "e" is not the filler "é"
        assert!(!c.iter().any(|c| c.start == Tick::from_seconds_f64(3.9)));
    }

    #[test]
    fn unsure_words_far_apart_are_separate_spans() {
        let words = vec![w("a", 0.0, 0.2, 0.1), w("b", 1.5, 1.7, 0.1)];
        let c = candidates(&words, 5.0, 0.6, 0.6, &[]);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn plan_segments_snap_to_frames_and_keep_sync() {
        let plan: Plan = serde_json::from_str(
            r#"{"slug":"x","cuts":[{"id":"c","segments":[
                {"start":30.976,"end":34.196,"source_video":"videos/T1.MOV","audio_start":35.26,"source_audio":"audio/T1.m4a"},
                {"start":1.0,"end":1.01,"source_video":"videos/T1.MOV"},
                {"start":2.0,"end":3.0}
            ]}]}"#,
        )
        .unwrap();
        let rate = FrameRate::new(30, 1);
        let root = Path::new("/r");
        assert!(place(&plan.cuts[0], None, root, rate).is_err(), "the third segment has no video");
        let placed = place(&plan.cuts[0], Some("videos/T1.MOV"), root, rate).unwrap();
        assert_eq!(placed.len(), 2, "the sub-frame segment is dropped");
        let a = &placed[0];
        assert_eq!((a.video.as_str(), a.audio.as_str(), a.external_audio), ("/r/videos/T1.MOV", "/r/audio/T1.m4a", true));
        assert_eq!((a.video_in, a.frames), (929, 97)); // 30.976 s → frame 929, 34.196 s → 1026
        // the video In moved by 929/30 − 30.976 s; the audio In moves by the same amount
        let expect = Tick::from_seconds_f64(35.26) + (rate.tick_of(929) - Tick::from_seconds_f64(30.976));
        assert_eq!(a.audio_in, expect);
        let b = &placed[1];
        assert!(!b.external_audio);
        assert_eq!(b.audio_in, rate.tick_of(b.video_in));
    }

    #[test]
    fn plan_root_is_three_levels_up() {
        assert_eq!(plan_root(Path::new("/d/analysis/slug/premiere_cuts.json")), Some(PathBuf::from("/d")));
    }

    #[test]
    fn timeline_links_picture_and_sound() {
        let rate = FrameRate::new(30, 1);
        let p = Placed { video: "/v.mov".into(), audio: "/a.m4a".into(), external_audio: true, video_in: 10, frames: 30, audio_in: Tick::from_seconds_f64(2.5) };
        let t = timeline("T", &[p], rate, &SequenceSettings::default());
        let kids = &t["tracks"]["children"];
        assert_eq!(kids[0]["kind"], "Video");
        assert_eq!(kids[1]["kind"], "Audio");
        assert_eq!(kids[0]["children"][0]["metadata"]["filmcraft"]["link"], 1);
        assert_eq!(kids[1]["children"][0]["metadata"]["filmcraft"]["link"], 1);
        assert_eq!(kids[1]["children"][0]["media_reference"]["target_url"], "/a.m4a");
    }
}

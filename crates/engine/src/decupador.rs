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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use filmcraft_audio_dsp::sync::{SyncOptions, find_offset};
use filmcraft_edit::transcript::SeqWord;
use filmcraft_interchange::Format;
use filmcraft_project::{
    ClipId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, MediaRef, Sequence, SequenceSettings, TrackItem, TrackKind, Transcript,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

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
        if s.end.is_nan() || s.start.is_nan() || s.end <= s.start || s.start < 0.0 {
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
    let (plan, path): (Plan, Option<String>) = match p.get("plan").filter(|v| !v.is_null()) {
        Some(v) => (serde_json::from_value(v.clone()).map_err(|e| bad(CMD, format!("`plan` is not a decupador plan: {e}")))?, None),
        None => {
            let path = str_p(p, "path").ok_or_else(|| bad(CMD, "pass `plan`, or `path` to a plan file"))?.to_string();
            let text = std::fs::read_to_string(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            (serde_json::from_str(&text).map_err(|e| bad(CMD, format!("{path} is not a decupador plan: {e}")))?, Some(path))
        }
    };
    // A plan that names no source file is in session time: seconds of the session sequence.
    if plan.source_video.is_none() && plan.cuts.iter().all(|c| c.segments.iter().all(|g| g.source_video.is_none())) {
        return apply_session(s, &plan, p);
    }
    let path = path.ok_or_else(|| bad(CMD, "a plan that names source files must be a file (`path`), so its paths can be resolved"))?;
    let root = match str_p(p, "root") {
        Some(r) => PathBuf::from(r),
        None => plan_root(Path::new(&path)).ok_or_else(|| bad(CMD, "pass `root`: the plan is not inside <decupador>/analysis/<slug>/"))?,
    };
    let wanted = str_p(p, "cut");
    let cuts: Vec<&Cut> = plan.cuts.iter().filter(|c| wanted.is_none_or(|w| c.id == w)).collect();
    if cuts.is_empty() {
        let ids: Vec<&str> = plan.cuts.iter().map(|c| c.id.as_str()).collect();
        return Err(bad(
            CMD,
            match wanted {
                Some(w) => format!("no cut `{w}` in the plan (cuts: {})", ids.join(", ")),
                None => "the plan has no cuts".into(),
            },
        ));
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
    let mut r = crate::interchange::import(s, &fake, &bytes, Format::Otio, None)?;
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
            out.push(Candidate {
                kind: "silence",
                start: w[0].end,
                end: w[1].start,
                text: format!("{} … {}", w[0].text, w[1].text),
                value: (gap * 100.0).round() / 100.0,
            });
        }
    }
    // fillers: words and phrases, longest first so "tipo assim" wins over "tipo"
    let normed: Vec<String> = words.iter().map(|w| norm(&w.text)).collect();
    let mut phrases: Vec<Vec<String>> =
        muletas.iter().map(|m| m.split_whitespace().map(norm).filter(|x| !x.is_empty()).collect::<Vec<_>>()).filter(|v| !v.is_empty()).collect();
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
            out.push(Candidate {
                kind: "lowConfidence",
                start: a.start,
                end: b.end,
                text: span.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
                value: (low * 1000.0).round() / 1000.0,
            });
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
            q.markers.push(Marker {
                id: MarkerId(ctx.alloc()),
                url: String::new(),
                author: String::new(),
                custom_color: None,
                start: c.start,
                duration: (c.end - c.start).max(Tick::ZERO),
                name,
                comment,
                kind: MarkerKind::Comment,
                color,
            });
        }
        q.markers.sort_by_key(|m| m.start);
        Ok(removed)
    })?;
    let count = |k: &str| cs.iter().filter(|c| c.kind == k).count();
    Ok(json!({"markers": n, "replaced": removed, "silences": count("silence"), "fillers": count("filler"), "lowConfidence": count("lowConfidence")}))
}

// ---------------------------------------------------------------------------------------------
// Session: every take in recording order, transcribed as one recording
// ---------------------------------------------------------------------------------------------

/// Seconds from 1904-01-01 (the ISO-BMFF epoch) to 1970-01-01.
const MP4_EPOCH: i64 = 2_082_844_800;

/// When an ISO-BMFF file (MOV, MP4, M4A) says it was recorded (`mvhd` creation time), in Unix
/// seconds. The file's own creation date is when it was copied, not when it was recorded.
pub fn recorded_at(path: &Path) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut pos = 0u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 8];
        f.read_exact(&mut h).ok()?;
        let (mut size, mut header) = (u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]])), 8u64);
        if size == 1 {
            let mut l = [0u8; 8];
            f.read_exact(&mut l).ok()?;
            (size, header) = (u64::from_be_bytes(l), 16);
        } else if size == 0 {
            size = len - pos;
        }
        if size < header {
            return None;
        }
        if &h[4..8] == b"moov" {
            let mut moov = vec![0u8; (size - header).min(64 << 20) as usize];
            f.read_exact(&mut moov).ok()?;
            return mvhd_creation(&moov);
        }
        pos += size;
    }
    None
}

fn mvhd_creation(moov: &[u8]) -> Option<i64> {
    let mut i = 0usize;
    while i + 8 <= moov.len() {
        let size = u32::from_be_bytes(moov.get(i..i + 4)?.try_into().ok()?) as usize;
        if moov.get(i + 4..i + 8)? == b"mvhd" {
            let b = moov.get(i + 8..)?;
            let secs = match *b.first()? {
                1 => i64::try_from(u64::from_be_bytes(b.get(4..12)?.try_into().ok()?)).ok()?,
                _ => i64::from(u32::from_be_bytes(b.get(4..8)?.try_into().ok()?)),
            };
            return (secs > 0).then_some(secs - MP4_EPOCH);
        }
        if size < 8 {
            return None;
        }
        i += size;
    }
    None
}

fn file_created(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.created().ok()?;
    t.duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// A recording of the session.
#[derive(Clone, Debug, PartialEq)]
struct Take {
    item: ItemId,
    name: String,
    /// Recording time (Unix seconds): the file's metadata, else its creation date.
    when: Option<i64>,
    video: bool,
    audio: bool,
}

fn take_of(s: &Session, id: ItemId) -> Option<Take> {
    let it = s.project.item(id)?;
    let ItemKind::Media(m) = &it.kind else { return None };
    let path = match &m.media {
        MediaRef::File { path } => Some(PathBuf::from(path)),
        _ => None,
    };
    let when = path.as_deref().and_then(|p| recorded_at(p).or_else(|| file_created(p)));
    Some(Take { item: id, name: it.name.clone(), when, video: it.has_video(), audio: it.has_audio() })
}

/// Recording order: the recording time, then the name.
fn in_order(takes: &mut [Take]) {
    takes.sort_by(|a, b| (a.when.unwrap_or(i64::MAX), &a.name).cmp(&(b.when.unwrap_or(i64::MAX), &b.name)));
}

/// Split the transcript of recordings laid end to end (`spans`: where each starts, how long it
/// is) back into one transcript per recording, in that recording's own media time. A word
/// belongs to the recording that holds its midpoint.
fn split_words(t: &Transcript, spans: &[(Tick, Tick)]) -> Vec<Transcript> {
    let mut out: Vec<Transcript> =
        spans.iter().map(|_| Transcript { language: t.language.clone(), source: t.source.clone(), speakers: t.speakers.clone(), words: Vec::new() }).collect();
    for w in &t.words {
        let mid = Tick((w.start.0 + w.end.0) / 2);
        let Some(k) = spans.iter().position(|&(st, len)| mid >= st && mid < st + len) else { continue };
        let (st, len) = spans[k];
        let mut nw = w.clone();
        nw.start = (w.start - st).max(Tick::ZERO);
        nw.end = (w.end - st).min(len);
        if nw.end > nw.start {
            out[k].words.push(nw);
        }
    }
    out
}

/// Where a camera recording starts inside one of the external recordings: (which, seconds,
/// confidence), the best match by waveform cross-correlation.
fn best_sync(recorders: &[Vec<f32>], camera: &[f32]) -> Option<(usize, f64, f32)> {
    let sr = filmcraft_speech::SAMPLE_RATE;
    recorders
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            find_offset(a, camera, sr, &SyncOptions::default())
                .filter(|r| !r.inverted || r.confidence > 0.0)
                .map(|r| (i, r.lag_fine / f64::from(sr), r.confidence))
        })
        .max_by(|x, y| x.2.total_cmp(&y.2))
}

fn mmss(t: f64) -> String {
    let t = t.max(0.0);
    format!("{:02}:{:04.1}", (t / 60.0).floor() as u64, t % 60.0)
}

fn session_items(s: &Session, p: &Value) -> Vec<ItemId> {
    let given: Vec<ItemId> = p.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect()).unwrap_or_default();
    let ids = if !given.is_empty() {
        given
    } else if !s.state.project_selection.is_empty() {
        s.state.project_selection.clone()
    } else {
        s.project.items.keys().copied().collect()
    };
    ids.into_iter().filter(|i| matches!(s.project.item(*i).map(|x| &x.kind), Some(ItemKind::Media(_)))).collect()
}

/// `decupador.transcribeSession`: the decupador's multi-take engine inside the editor. The takes
/// go in recording order and their audio is transcribed as ONE recording (the recogniser keeps
/// the context across takes, which take-by-take transcription loses); the words then go back to
/// each file, in its own media time. With separate recorder files (Áudio Duplo) the recorder is
/// what is transcribed, and every camera file is placed where its audio matches the recorder's.
/// The result is a session sequence: every take in order, the reference all session plans
/// (`decupador.applyPlan` without source files) are written against.
fn transcribe_session(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "decupador.transcribeSession";
    let mut takes: Vec<Take> = session_items(s, p).into_iter().filter_map(|i| take_of(s, i)).filter(|t| t.audio || t.video).collect();
    in_order(&mut takes);
    let recorders: Vec<Take> = takes.iter().filter(|t| t.audio && !t.video).cloned().collect();
    let cameras: Vec<Take> = takes.iter().filter(|t| t.video).cloned().collect();
    let dual = !recorders.is_empty() && !cameras.is_empty();
    let heard: Vec<Take> = if recorders.is_empty() { cameras.iter().filter(|t| t.audio).cloned().collect() } else { recorders.clone() };
    if heard.is_empty() {
        return Err(bad(CMD, "no take with sound to transcribe (import the takes, or select them in the Project panel)"));
    }
    // one recording: every take's sound end to end
    let mut audio: Vec<Vec<f32>> = Vec::new();
    let mut spans: Vec<(Tick, Tick)> = Vec::new();
    let mut all: Vec<f32> = Vec::new();
    let mut skipped = Vec::new();
    let mut kept: Vec<Take> = Vec::new();
    for t in &heard {
        let Some(a) = crate::transcript::item_audio(s, t.item) else {
            skipped.push(t.name.clone());
            continue;
        };
        spans.push((filmcraft_speech::sample_tick(all.len() as i64), filmcraft_speech::sample_tick(a.len() as i64)));
        all.extend_from_slice(&a);
        audio.push(a);
        kept.push(t.clone());
    }
    if kept.is_empty() {
        return Err(EngineError::Other("none of the takes has audio that can be read".into()));
    }
    let tr = crate::transcript::transcriber(s, p)?;
    let opts = filmcraft_speech::Options {
        language: str_p(p, "language").filter(|l| !l.is_empty() && *l != "auto").map(str::to_string),
        diarize: false,
        max_speakers: 1,
    };
    let mut whole = tr.transcribe(&all, &opts, &mut |_, _| true).map_err(|e| EngineError::Other(e.to_string()))?;
    whole.normalize();
    let per_take = split_words(&whole, &spans);
    // the cameras: synced to the recorder (Áudio Duplo), or the transcribed takes themselves
    let mut synced = Vec::new();
    let mut placed_video: Vec<(ItemId, Tick)> = Vec::new();
    if dual {
        for c in &cameras {
            let hit = crate::transcript::item_audio(s, c.item).and_then(|cam| best_sync(&audio, &cam));
            match hit {
                Some((k, secs, confidence)) => {
                    let at = spans[k].0 + Tick::from_seconds_f64(secs);
                    placed_video.push((c.item, at));
                    synced.push(json!({"camera": c.name, "recorder": kept[k].name, "seconds": (secs * 1000.0).round() / 1000.0, "confidence": confidence}));
                }
                None => synced.push(json!({"camera": c.name, "recorder": null, "error": "no sound to match (import the camera file with its own audio)"})),
            }
        }
    }
    let first_video = cameras.first().and_then(|c| match s.project.item(c.item).map(|i| &i.kind) {
        Some(ItemKind::Media(m)) => match &m.media {
            MediaRef::File { path } => Some(path.clone()),
            _ => None,
        },
        _ => None,
    });
    let settings = first_video.as_deref().and_then(|f| camera_settings(s, f)).unwrap_or_default();
    let rate = settings.frame_rate;
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("Sessão — {}", s.project.name));
    let sizes: Vec<(ItemId, (u32, u32))> = cameras
        .iter()
        .map(|c| c.item)
        .filter_map(|i| s.project.resolve_media(i).and_then(|(_, m, _)| m.info.video.as_ref().map(|v| (i, (v.width, v.height)))))
        .collect();
    let words: usize = per_take.iter().map(|t| t.words.len()).sum();
    let linked_camera_audio = !dual;
    let seq = s.edit("Transcribe Session", |pr, _| {
        for (t, tx) in kept.iter().zip(per_take) {
            pr.transcripts.insert(t.item, Arc::new(tx));
        }
        // A1: the transcribed sound, end to end
        let mut a_items = Vec::new();
        let mut prev = Tick::ZERO;
        let mut v_tracks: Vec<Vec<TrackItem>> = Vec::new();
        for (t, &(st, len)) in kept.iter().zip(&spans) {
            let at = rate.snap_nearest(st).max(prev);
            let mut ti = pr.make_track_item(t.item, TrackKind::Audio, at, TimeRange::new(Tick::ZERO, len), rate).ok_or_else(|| bad(CMD, "bad audio take"))?;
            if linked_camera_audio && t.video {
                // the camera's own sound: its picture sits right above it, linked
                let l = pr.alloc_id();
                ti.link = Some(l);
                let mut v =
                    pr.make_track_item(t.item, TrackKind::Video, at, TimeRange::new(Tick::ZERO, len), rate).ok_or_else(|| bad(CMD, "bad video take"))?;
                v.duration = ti.duration;
                v.link = Some(l);
                if let Some((_, size)) = sizes.iter().find(|(i, _)| *i == t.item) {
                    for e in &mut v.effects {
                        filmcraft_project::resolve_auto_points(e, (settings.width, settings.height), *size);
                    }
                }
                if v_tracks.is_empty() {
                    v_tracks.push(Vec::new());
                }
                v_tracks[0].push(v);
            }
            prev = ti.start + ti.duration;
            a_items.push(ti);
        }
        // Áudio Duplo: the pictures where their sound matches the recorder (V2 and up where two
        // cameras overlap)
        for &(item, exact) in &placed_video {
            let at = rate.snap_nearest(exact.max(Tick::ZERO));
            let source_in = (at - exact).max(Tick::ZERO);
            let len = s_len(pr, item).unwrap_or(Tick::ZERO);
            if len <= source_in {
                continue;
            }
            let mut ti =
                pr.make_track_item(item, TrackKind::Video, at, TimeRange::new(source_in, len - source_in), rate).ok_or_else(|| bad(CMD, "bad video take"))?;
            if let Some((_, size)) = sizes.iter().find(|(i, _)| *i == item) {
                for e in &mut ti.effects {
                    filmcraft_project::resolve_auto_points(e, (settings.width, settings.height), *size);
                }
            }
            let track = match v_tracks.iter().position(|t| t.last().is_none_or(|l| l.start + l.duration <= ti.start)) {
                Some(k) => k,
                None => {
                    v_tracks.push(Vec::new());
                    v_tracks.len() - 1
                }
            };
            v_tracks[track].push(ti);
        }
        let sid = pr.new_sequence(&name, settings.clone(), v_tracks.len().max(1), 1, None);
        let q = pr.sequence_mut(sid).ok_or(EngineError::NoSequence)?;
        for (t, items) in q.video_tracks.iter_mut().zip(v_tracks) {
            t.items = items;
            t.sort();
        }
        if let Some(t) = q.audio_tracks.first_mut() {
            t.items = a_items;
            t.sort();
        }
        Ok(sid)
    })?;
    s.state.active_sequence = Some(seq);
    if !s.state.open_sequences.contains(&seq) {
        s.state.open_sequences.push(seq);
    }
    // the script: the one asked for, else one found next to the takes (never an error here)
    let script = match str_p(p, "script") {
        Some(path) => Some(align_script(s, &json!({"path": path})).unwrap_or_else(|e| json!({"script": path, "error": e.to_string()}))),
        None => align_script(s, &json!({})).ok(),
    };
    let order: Vec<Value> = kept
        .iter()
        .zip(&spans)
        .map(|(t, &(st, len))| json!({"item": t.item.0, "name": t.name, "recorded": t.when, "sessionStart": st.seconds(), "seconds": len.seconds()}))
        .collect();
    Ok(json!({
        "sequence": seq.0, "name": name, "mode": if dual { "dualAudio" } else { "camera" },
        "language": whole.language, "source": whole.source, "words": words,
        "order": order, "cameras": synced, "skipped": skipped,
        "script": script.map(|v| json!({"path": v["script"], "lines": v["lines"].as_array().map_or(0, Vec::len), "markers": v["markers"], "error": v.get("error")})),
    }))
}

fn s_len(p: &filmcraft_project::Project, item: ItemId) -> Option<Tick> {
    match &p.item(item)?.kind {
        ItemKind::Media(m) => Some(m.duration()),
        _ => None,
    }
}

/// `decupador.context`: what the decupador reads to write a plan, from the session sequence (the
/// active one, or `sequence`): the takes and where they sit, the transcript as timed lines in
/// session time, every word with its confidence, and the step A candidates.
fn context(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(id) = crate::commands::u64_p(p, "sequence") {
        if s.project.sequence(ItemId(id)).is_none() {
            return Err(EngineError::NoSequence);
        }
        s.state.active_sequence = Some(ItemId(id));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut takes = Vec::new();
    for (kind, tracks) in [("V", &q.video_tracks), ("A", &q.audio_tracks)] {
        for (n, t) in tracks.iter().enumerate() {
            for it in &t.items {
                takes.push(json!({"track": format!("{kind}{}", n + 1), "item": it.item.0, "name": it.name, "start": it.start.seconds(), "end": (it.start + it.duration).seconds(), "sourceIn": it.source_in.seconds()}));
            }
        }
    }
    let duration = q.duration().seconds();
    let script = script_markers(q);
    let name = s.state.active_sequence.and_then(|i| s.project.item(i)).map(|i| i.name.clone()).unwrap_or_default();
    let words = crate::transcript::sequence_words(s);
    let gap = f64_p(p, "paragraphGap").unwrap_or(1.5);
    let mut lines: Vec<String> = Vec::new();
    let mut cur: Vec<&SeqWord> = Vec::new();
    let flush = |cur: &mut Vec<&SeqWord>, lines: &mut Vec<String>| {
        if let (Some(a), Some(b)) = (cur.first(), cur.last()) {
            lines.push(format!(
                "[{}–{}] {}",
                mmss(a.start.seconds()),
                mmss(b.end.seconds()),
                cur.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
            ));
        }
        cur.clear();
    };
    for w in &words {
        if let Some(l) = cur.last()
            && (w.start.seconds() - l.end.seconds() > gap || w.item != l.item)
        {
            if w.item != l.item {
                flush(&mut cur, &mut lines);
                lines.push(format!("— {} —", w_take_name(s, w.item)));
            } else {
                flush(&mut cur, &mut lines);
            }
        }
        if cur.is_empty() && lines.is_empty() {
            lines.push(format!("— {} —", w_take_name(s, w.item)));
        }
        cur.push(w);
    }
    flush(&mut cur, &mut lines);
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    let cs = sequence_candidates(s, p);
    Ok(json!({
        "sequence": name, "sequenceId": s.state.active_sequence.map(|i| i.0), "duration": duration, "rate": q.settings.frame_rate.as_f64(), "takes": takes,
        "text": lines.join("\n"),
        "words": words.iter().map(|w| json!([r2(w.start.seconds()), r2(w.end.seconds()), w.text, r2(f64::from(w.confidence))])).collect::<Vec<_>>(),
        "candidates": cs.iter().map(candidate_json).collect::<Vec<_>>(),
        "script": script,
    }))
}

fn w_take_name(s: &Session, item: ItemId) -> String {
    s.project.item(item).map(|i| i.name.clone()).unwrap_or_default()
}

/// A session plan: copy each cut's ranges of the session sequence, every track, into a new
/// sequence, one range after the other. Pictures and sounds come from whatever the session holds
/// at those times (camera and recorder stay in sync because they sit in sync there).
fn apply_session(s: &mut Session, plan: &Plan, p: &Value) -> Result<Value> {
    const CMD: &str = "decupador.applyPlan";
    let src_id = match crate::commands::u64_p(p, "sequence") {
        Some(i) => ItemId(i),
        None => s.state.active_sequence.ok_or(EngineError::NoSequence)?,
    };
    let src = s.project.sequence(src_id).ok_or(EngineError::NoSequence)?.clone();
    let rate = src.settings.frame_rate;
    let wanted = str_p(p, "cut");
    let cuts: Vec<&Cut> = plan.cuts.iter().filter(|c| wanted.is_none_or(|w| c.id == w)).collect();
    if cuts.is_empty() {
        return Err(bad(CMD, "the plan has no such cut"));
    }
    let mut todo = Vec::new();
    for cut in cuts {
        let ranges: Vec<(Tick, Tick)> = cut
            .segments
            .iter()
            .map(|g| (rate.snap_nearest(Tick::from_seconds_f64(g.start.max(0.0))), rate.snap_nearest(Tick::from_seconds_f64(g.end))))
            .filter(|(a, b)| b > a)
            .collect();
        if !ranges.is_empty() {
            todo.push((cut.name(plan.slug.as_deref()), cut.id.clone(), ranges));
        }
    }
    if todo.is_empty() {
        return Err(bad(CMD, "no segment of the plan is at least one frame long"));
    }
    let made = s.edit("Apply Decoupage Plan", |pr, _| {
        let mut made = Vec::new();
        for (name, id, ranges) in &todo {
            let sid = extract(pr, &src, ranges, name);
            let secs: f64 = ranges.iter().map(|(a, b)| (*b - *a).seconds()).sum();
            made.push(json!({"sequence": sid.0, "name": name, "cut": id, "segments": ranges.len(), "seconds": secs}));
        }
        Ok(made)
    })?;
    if let Some(first) = made.first().and_then(|m| m["sequence"].as_u64()).map(ItemId) {
        s.state.active_sequence = Some(first);
        if !s.state.open_sequences.contains(&first) {
            s.state.open_sequences.push(first);
        }
    }
    Ok(json!({"sequences": made}))
}

/// Bring pictures down to the lowest track where they overlap nothing: in the session two
/// cameras can overlap by a moment (one stacked on V2), but a cut rarely keeps both.
fn pack(tracks: Vec<Vec<TrackItem>>) -> Vec<Vec<TrackItem>> {
    let mut out: Vec<Vec<TrackItem>> = vec![Vec::new(); tracks.len()];
    let free = |t: &[TrackItem], it: &TrackItem| t.iter().all(|o| o.start + o.duration <= it.start || it.start + it.duration <= o.start);
    for it in tracks.into_iter().flatten() {
        let k = (0..out.len()).find(|&k| free(&out[k], &it)).unwrap_or(out.len() - 1);
        out[k].push(it);
    }
    while out.len() > 1 && out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

/// A new sequence holding `ranges` of `src`, back to back, on the same tracks.
fn extract(pr: &mut filmcraft_project::Project, src: &Sequence, ranges: &[(Tick, Tick)], name: &str) -> ItemId {
    let mut links: HashMap<(u64, usize), u64> = HashMap::new();
    let mut cut_tracks = |pr: &mut filmcraft_project::Project, tracks: &[filmcraft_project::Track]| -> Vec<Vec<TrackItem>> {
        let mut out = vec![Vec::new(); tracks.len()];
        let mut cursor = Tick::ZERO;
        for (r, &(rs, re)) in ranges.iter().enumerate() {
            for (k, t) in tracks.iter().enumerate() {
                for it in &t.items {
                    let (s0, e0) = (it.start.max(rs), (it.start + it.duration).min(re));
                    if e0 <= s0 {
                        continue;
                    }
                    let mut ti = it.clone();
                    ti.id = ClipId(pr.alloc_id());
                    ti.start = cursor + (s0 - rs);
                    ti.duration = e0 - s0;
                    ti.source_in = it.source_in + Tick(((s0 - it.start).0 as f64 * it.speed).round() as i64);
                    ti.markers.clear();
                    // a range's picture and sound move together: the session's own links, else
                    // one link per range (camera and recorder sit unlinked in the session)
                    let key = (it.link.unwrap_or(u64::MAX), r);
                    ti.link = Some(*links.entry(key).or_insert_with(|| pr.alloc_id()));
                    out[k].push(ti);
                }
            }
            cursor += re - rs;
        }
        out
    };
    let video = pack(cut_tracks(pr, &src.video_tracks));
    let audio = cut_tracks(pr, &src.audio_tracks);
    let sid = pr.new_sequence(name, src.settings.clone(), video.len().max(1), audio.len().max(1), None);
    if let Some(q) = pr.sequence_mut(sid) {
        for (t, items) in q.video_tracks.iter_mut().zip(video) {
            t.items = items;
            t.sort();
        }
        for (t, items) in q.audio_tracks.iter_mut().zip(audio) {
            t.items = items;
            t.sort();
        }
    }
    sid
}

// ---------------------------------------------------------------------------------------------
// Script (roteiro): every line of the script lined up with its attempts in the session
// ---------------------------------------------------------------------------------------------

/// Name prefix of the markers `decupador.alignScript` adds (and replaces on the next run).
pub const SCRIPT_PREFIX: &str = "Roteiro · ";

/// A script line ("fala") as the speaker should say it.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
}

/// Clean the blocks a script reader returns: keep only the quoted speech when a block quotes it
/// (a PDF export glues the page header to the first line), drop page footers and headers.
pub fn script_lines(blocks: &[(String, String)]) -> Vec<Line> {
    let mut out = Vec::new();
    for (_, text) in blocks {
        let t = text.replace('\u{0}', " ");
        if t.contains('→') {
            continue; // a footer ("18 de setembro de 2026 08:00 → 11:30")
        }
        let quoted: Vec<&str> = t.split(['"', '“', '”']).skip(1).step_by(2).map(str::trim).filter(|q| !q.is_empty()).collect();
        let speech = if quoted.is_empty() { t.trim().to_string() } else { quoted.join(" ") };
        let speech = speech.split_whitespace().collect::<Vec<_>>().join(" ");
        if speech.split_whitespace().filter(|w| w.chars().any(char::is_alphabetic)).count() >= 3 {
            out.push(Line { text: speech });
        }
    }
    out
}

/// Plain-text and Markdown scripts without the decupador: paragraphs (or `#` headings' bodies).
pub fn blocks_of_text(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            if !cur.trim().is_empty() {
                out.push((String::new(), cur.trim().to_string()));
            }
            cur.clear();
        } else {
            cur.push(' ');
            cur.push_str(l);
        }
    }
    if !cur.trim().is_empty() {
        out.push((String::new(), cur.trim().to_string()));
    }
    out
}

/// A word reduced for comparing speech with the script: lower case, no punctuation, no accents,
/// and the spoken short forms written out ("pra" = "para").
fn token(w: &str) -> String {
    let t: String = w
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect();
    match t.as_str() {
        "pra" | "pro" => "para".into(),
        // plural and singular count as the same word ("negócios" / "negócio")
        _ if t.chars().count() > 3 && t.ends_with('s') => t[..t.len() - 1].to_string(),
        _ => t,
    }
}

/// One attempt at a script line: session word indices `[from, to]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Attempt {
    pub line: usize,
    pub from: usize,
    pub to: usize,
    /// Share of the line's words that were said.
    pub coverage: f64,
    /// Share of the attempt's words that are the line's.
    pub precision: f64,
    /// The first and last word of the line it reached.
    pub reached: (usize, usize),
    /// The last word trails off ("sua...") or closes a sentence ("assunto.").
    pub trails_off: bool,
    pub ends_sentence: bool,
}

impl Attempt {
    /// It starts at the line's beginning and gets to its end: the line's last words, or a full
    /// stop when the speaker said it in their own words. Trailing off ("…") is never complete.
    pub fn complete(&self, line_len: usize) -> bool {
        self.reached.0 <= 1 && !self.trails_off && (self.reached.1 + 1 >= line_len || self.ends_sentence)
    }
}

/// Longest common subsequence of `a` (script) and `b` (speech): the matched pairs.
fn lcs(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if !a[i].is_empty() && a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if !a[i].is_empty() && a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The best attempts of every line inside the uncovered words `free` (session word indices).
fn candidates_in(lines: &[Vec<String>], speech: &[String], free: &[bool]) -> Vec<Attempt> {
    let mut out = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let n = line.len();
        if n == 0 {
            continue;
        }
        let window = (n * 3 / 2).max(n + 4);
        for i in 0..speech.len() {
            if !free[i] || !line.contains(&speech[i]) {
                continue;
            }
            // an attempt ends where the speaker starts the line over ("O verdadeiro sucesso…
            // pera aí. O verdadeiro sucesso…")
            let restarts = |e: usize| {
                let at = |o: usize, l: usize| speech.get(e + o).is_some_and(|w| line.get(l) == Some(w));
                n >= 3 && ((at(0, 0) && at(1, 1)) || (at(0, 1) && at(1, 2)))
            };
            let mut end = i;
            while end < speech.len() && end < i + window && free[end] && !(end > i + 2 && restarts(end)) {
                end += 1;
            }
            let pairs = lcs(line, &speech[i..end]);
            let (Some(&(b0, s0)), Some(&(b1, s1))) = (pairs.first(), pairs.last()) else { continue };
            let matched = pairs.len();
            let span = s1 - s0 + 1;
            let (coverage, precision) = (matched as f64 / n as f64, matched as f64 / span as f64);
            // the line, said fully or in the speaker's own words
            let whole = coverage >= 0.45 && precision >= 0.5 && matched >= n.min(3);
            // a false start: the line's first words, then the speaker stops
            let start = matched >= 4.min(n) && b0 <= 1 && precision >= 0.7;
            if whole || start {
                out.push(Attempt { line: k, from: i + s0, to: i + s1, coverage, precision, reached: (b0, b1), trails_off: false, ends_sentence: false });
            }
        }
    }
    out
}

/// Line up the script with the session: every attempt at every line, best matches first, no
/// two attempts sharing a word. A second pass looks for false starts in what is left.
pub fn align(script: &[Line], words: &[SeqWord]) -> Vec<Attempt> {
    let lines: Vec<Vec<String>> = script.iter().map(|l| l.text.split_whitespace().map(token).filter(|t| !t.is_empty()).collect()).collect();
    let speech: Vec<String> = words.iter().map(|w| token(&w.text)).collect();
    let mut free = vec![true; speech.len()];
    let mut picked: Vec<Attempt> = Vec::new();
    for _ in 0..3 {
        let mut cands = candidates_in(&lines, &speech, &free);
        if cands.is_empty() {
            break;
        }
        cands.sort_by(|a, b| (b.coverage * b.precision).total_cmp(&(a.coverage * a.precision)).then(a.from.cmp(&b.from)));
        let before = picked.len();
        for c in cands {
            if (c.from..=c.to).all(|i| free[i]) {
                for f in &mut free[c.from..=c.to] {
                    *f = false;
                }
                picked.push(c);
            }
        }
        if picked.len() == before {
            break;
        }
    }
    // an attempt ends where the sentence does: "destrinchar melhor esse assunto." is one attempt
    // even though "melhor esse assunto" is not in the script
    let ends = |w: &SeqWord| w.text.ends_with(['.', '?', '!']);
    for a in &mut picked {
        let mut k = 0;
        while k < 4 && !ends(&words[a.to]) && a.to + 1 < words.len() && free[a.to + 1] && (words[a.to + 1].start - words[a.to].end).seconds() < 0.6 {
            a.to += 1;
            free[a.to] = false;
            k += 1;
        }
    }
    for a in &mut picked {
        let last = words[a.to].text.trim_end();
        a.trails_off = last.ends_with("...") || last.ends_with('…');
        a.ends_sentence = !a.trails_off && last.ends_with(['.', '?', '!']);
    }
    picked.sort_by_key(|a| (a.from, a.line));
    picked
}

const LINE_COLORS: [Label; 8] = [Label::Iris, Label::Caribbean, Label::Lavender, Label::Cerulean, Label::Forest, Label::Mango, Label::Purple, Label::Teal];

fn text_of(words: &[SeqWord], from: usize, to: usize) -> String {
    words[from..=to].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
}

/// The script file next to the session's media: a `.pdf`, `.md` or `.txt` named like a script
/// ("roteiro", "script") in a folder of one of the session's files (or the folder above it).
fn find_script(s: &Session, q: &Sequence) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for t in q.video_tracks.iter().chain(&q.audio_tracks) {
        for it in &t.items {
            if let Some(ItemKind::Media(m)) = s.project.item(it.item).map(|i| &i.kind)
                && let MediaRef::File { path } = &m.media
                && let Some(d) = Path::new(path).parent()
            {
                for d in [Some(d), d.parent()].into_iter().flatten() {
                    if !dirs.iter().any(|x| x == d) {
                        dirs.push(d.to_path_buf());
                    }
                }
            }
        }
    }
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                !name.starts_with("._") && (name.contains("roteiro") || name.contains("script")) && matches!(ext.as_str(), "pdf" | "md" | "txt")
            })
            .collect();
        found.sort();
        if let Some(f) = found.into_iter().next() {
            return Some(f);
        }
    }
    None
}

fn read_script_file(path: &Path) -> Result<Vec<(String, String)>> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(cfg) = crate::transcript::decupador_config() {
        return filmcraft_speech::decupador::Decupador::new(cfg).read_script(path).map_err(|e| EngineError::Other(e.to_string()));
    }
    if ext == "pdf" {
        return Err(EngineError::Other("reading a PDF script needs the Decupador Inteligente (decupador.json); use a .txt or .md".into()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| EngineError::Other(format!("{}: {e}", path.display())))?;
    Ok(blocks_of_text(&text))
}

/// `decupador.alignScript {path?, sequence?}`: read the script (`path`, else the one found next to
/// the session's files), line it up with the session transcript and mark it on the session:
/// every attempt at every line ("Roteiro · Fala 6 — tentativa 3/4", incomplete ones say so), the
/// speech that is in no line ("Roteiro · Fora do roteiro": slate, direction, chat) and the lines
/// nobody said. The decision of which attempt to keep stays with whoever writes the plan.
fn align_script(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "decupador.alignScript";
    if let Some(id) = crate::commands::u64_p(p, "sequence") {
        if s.project.sequence(ItemId(id)).is_none() {
            return Err(EngineError::NoSequence);
        }
        s.state.active_sequence = Some(ItemId(id));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let path = match str_p(p, "path") {
        Some(x) => PathBuf::from(x),
        None => find_script(s, &q).ok_or_else(|| bad(CMD, "no script found next to the session's files: pass `path` (PDF, Markdown or text)"))?,
    };
    let lines = script_lines(&read_script_file(&path)?);
    if lines.is_empty() {
        return Err(bad(CMD, format!("{} has no lines of speech", path.display())));
    }
    let words = crate::transcript::sequence_words(s);
    if words.is_empty() {
        return Err(bad(CMD, "the sequence has no transcript (Transcribe Session first)"));
    }
    let attempts = align(&lines, &words);
    // speech in no line: runs of at least three words or 1.5 s
    let mut covered = vec![false; words.len()];
    for a in &attempts {
        for c in &mut covered[a.from..=a.to] {
            *c = true;
        }
    }
    let mut off: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if covered[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < words.len() && !covered[j + 1] && (words[j + 1].start - words[j].end).seconds() < 2.0 {
            j += 1;
        }
        if j - i + 1 >= 3 || (words[j].end - words[i].start).seconds() >= 1.5 {
            off.push((i, j));
        }
        i = j + 1;
    }
    let mut markers: Vec<(Tick, Tick, String, String, Label)> = Vec::new();
    let mut report = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let mine: Vec<&Attempt> = attempts.iter().filter(|a| a.line == k).collect();
        let color = LINE_COLORS[k % LINE_COLORS.len()];
        if mine.is_empty() {
            markers.push((Tick::ZERO, Tick::ZERO, format!("{SCRIPT_PREFIX}Fala {} — não encontrada", k + 1), format!("Roteiro: {}", line.text), Label::Tan));
        }
        let mut list = Vec::new();
        for (n, a) in mine.iter().enumerate() {
            let complete = a.complete(line.text.split_whitespace().map(token).filter(|t| !t.is_empty()).count());
            let said = text_of(&words, a.from, a.to);
            let name = format!("{SCRIPT_PREFIX}Fala {} — tentativa {}/{}{}", k + 1, n + 1, mine.len(), if complete { "" } else { " (incompleta)" });
            let comment = format!("Roteiro: {}\nFalado: {}\nCobertura {:.0}%", line.text, said, a.coverage * 100.0);
            markers.push((words[a.from].start, words[a.to].end, name, comment, color));
            list.push(json!({"start": words[a.from].start.seconds(), "end": words[a.to].end.seconds(), "complete": complete, "coverage": (a.coverage * 100.0).round() / 100.0, "text": said}));
        }
        report.push(json!({"line": k + 1, "script": line.text, "attempts": list}));
    }
    for &(a, b) in &off {
        let said = text_of(&words, a, b);
        let short: String = said.chars().take(200).collect();
        markers.push((words[a].start, words[b].end, format!("{SCRIPT_PREFIX}Fora do roteiro"), short, Label::Brown));
    }
    let n = markers.len();
    s.edit_sequence("Align Script", |q, ctx, _| {
        q.markers.retain(|m| !m.name.starts_with(SCRIPT_PREFIX));
        for (start, end, name, comment, color) in markers {
            q.markers.push(Marker {
                id: MarkerId(ctx.alloc()),
                url: String::new(),
                author: String::new(),
                custom_color: None,
                start,
                duration: (end - start).max(Tick::ZERO),
                name,
                comment,
                kind: MarkerKind::Comment,
                color,
            });
        }
        q.markers.sort_by_key(|m| m.start);
        Ok(())
    })?;
    Ok(json!({
        "script": path.to_string_lossy(), "lines": report, "markers": n,
        "offScript": off.iter().map(|&(a, b)| json!({"start": words[a].start.seconds(), "end": words[b].end.seconds(), "text": text_of(&words, a, b)})).collect::<Vec<_>>(),
    }))
}

/// The script as marked on the session (`Roteiro · …` markers), for `decupador.context`.
fn script_markers(q: &Sequence) -> Vec<Value> {
    q.markers
        .iter()
        .filter(|m| m.name.starts_with(SCRIPT_PREFIX))
        .map(|m| json!({"name": m.name.trim_start_matches(SCRIPT_PREFIX), "start": m.start.seconds(), "end": (m.start + m.duration).seconds(), "note": m.comment}))
        .collect()
}

fn has_words(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if crate::transcript::sequence_words(s).is_empty() { Err("the sequence has no transcript (Transcribe first)".into()) } else { Ok(()) }
}

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "decupador.applyPlan",
            "Apply Decoupage Plan…",
            &["Sequence", "Decupador Inteligente"],
            r#"{"plan":{cuts:[{id,title?,segments:[{start,end}]}]}?,"path":str?,"cut":str?,"sequence":id?,"root":str?}"#,
            always,
            apply_plan,
            true,
        ),
        spec(
            "decupador.markCandidates",
            "Mark Decoupage Candidates",
            &["Sequence", "Decupador Inteligente"],
            r#"{"minSilence":f?,"minProb":f?,"maxGap":f?,"muletas":[str]?,"replace":bool?}"#,
            has_words,
            mark,
            true,
        ),
        spec(
            "decupador.transcribeSession",
            "Transcribe Session…",
            &["Sequence", "Decupador Inteligente"],
            r#"{"items":[id]?,"name":str?,"language":str?,"model":str?,"script":str?}"#,
            crate::transcript::can_transcribe,
            transcribe_session,
            true,
        ),
        spec(
            "decupador.alignScript",
            "Align Script…",
            &["Sequence", "Decupador Inteligente"],
            r#"{"path":str?,"sequence":id?}"#,
            has_words,
            align_script,
            true,
        ),
        spec("decupador.context", "Decoupage Context", &[], r#"{"sequence":id?,"paragraphGap":f?}"#, has_seq, context, false),
        spec(
            "decupador.candidates",
            "List Decoupage Candidates",
            &[],
            r#"{"minSilence":f?,"minProb":f?,"maxGap":f?,"muletas":[str]?}"#,
            has_words,
            list,
            false,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ClipId, ItemId};

    fn w(text: &str, a: f64, b: f64, conf: f32) -> SeqWord {
        SeqWord {
            text: text.into(),
            start: Tick::from_seconds_f64(a),
            end: Tick::from_seconds_f64(b),
            clip: ClipId(1),
            item: ItemId(1),
            index: 0,
            track: 0,
            speaker: None,
            confidence: conf,
        }
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
        assert_eq!(
            kinds,
            vec![("filler", "então"), ("filler", "é"), ("silence", "importante. … Tipo"), ("filler", "tipo assim"), ("lowConfidence", "eee hum")]
        );
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
    fn recording_time_comes_from_mvhd() {
        // moov > mvhd, version 0 (32-bit times) and version 1 (64-bit times)
        let mvhd = |v1: bool, t: u64| {
            let mut b = Vec::new();
            let body: Vec<u8> = if v1 {
                [vec![1, 0, 0, 0], t.to_be_bytes().to_vec(), vec![0; 20]].concat()
            } else {
                [vec![0, 0, 0, 0], (t as u32).to_be_bytes().to_vec(), vec![0; 20]].concat()
            };
            b.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
            b.extend_from_slice(b"mvhd");
            b.extend_from_slice(&body);
            b
        };
        let t = 1_789_000_000 + MP4_EPOCH as u64;
        assert_eq!(mvhd_creation(&mvhd(false, t)), Some(1_789_000_000));
        assert_eq!(mvhd_creation(&mvhd(true, t)), Some(1_789_000_000));
        assert_eq!(mvhd_creation(&mvhd(false, 0)), None, "no date");
        // a file: ftyp, a big mdat, then moov
        let dir = std::env::temp_dir().join(format!("fc-decupador-mvhd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.mov");
        let moov_body = mvhd(false, t);
        let mut f = Vec::new();
        f.extend_from_slice(&16u32.to_be_bytes());
        f.extend_from_slice(b"ftypqt  \0\0\0\0");
        f.extend_from_slice(&(8 + 1000u32).to_be_bytes());
        f.extend_from_slice(b"mdat");
        f.extend_from_slice(&[0u8; 1000]);
        f.extend_from_slice(&((8 + moov_body.len()) as u32).to_be_bytes());
        f.extend_from_slice(b"moov");
        f.extend_from_slice(&moov_body);
        std::fs::write(&p, &f).unwrap();
        assert_eq!(recorded_at(&p), Some(1_789_000_000));
        std::fs::write(&p, b"not a movie").unwrap();
        assert_eq!(recorded_at(&p), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn takes_go_in_recording_order_then_by_name() {
        let t = |i: u64, name: &str, when: Option<i64>| Take { item: ItemId(i), name: name.into(), when, video: true, audio: true };
        let mut v = vec![t(1, "Take 3", Some(300)), t(2, "B", None), t(3, "Take 1", Some(100)), t(4, "A", None), t(5, "Take 2", Some(200))];
        in_order(&mut v);
        let names: Vec<&str> = v.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Take 1", "Take 2", "Take 3", "A", "B"]);
    }

    #[test]
    fn words_go_back_to_their_take() {
        let sec = Tick::from_seconds_f64;
        let mut t = Transcript { language: "pt".into(), source: "x".into(), ..Default::default() };
        for (w, a, b) in [("um", 0.5, 0.9), ("dois", 9.6, 10.2), ("três", 10.6, 11.0), ("fora", 31.0, 31.5)] {
            t.words.push(filmcraft_project::Word::new(w, sec(a), sec(b)));
        }
        // takes of 10 s and 20 s end to end
        let out = split_words(&t, &[(Tick::ZERO, sec(10.0)), (sec(10.0), sec(20.0))]);
        let texts = |k: usize| out[k].words.iter().map(|w| w.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(0), ["um", "dois"], "\"dois\" has its midpoint in the first take");
        assert_eq!(texts(1), ["três"], "words past the last take are dropped");
        assert_eq!(out[0].words[1].end, sec(10.0), "clamped to its take");
        assert_eq!(out[1].words[0].start, sec(0.6), "in the take's own media time");
        assert_eq!(out[1].language, "pt");
    }

    #[test]
    fn session_transcription_and_session_plans_on_the_demo() {
        use filmcraft_speech::FixedTranscriber;
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let takes: Vec<u64> =
            s.project.items.values().filter(|i| matches!(i.kind, ItemKind::Media(_)) && i.has_video() && i.has_audio()).map(|i| i.id.0).collect();
        assert!(takes.len() >= 2, "the demo has clips with sound");
        let mut t = Transcript { language: "pt".into(), ..Default::default() };
        for k in 0..120 {
            let a = f64::from(k) * 0.5 + 0.1;
            t.words.push(filmcraft_project::Word::new(format!("w{k}"), Tick::from_seconds_f64(a), Tick::from_seconds_f64(a + 0.3)));
        }
        s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
        let r = s.execute("decupador.transcribeSession", json!({"items": takes})).unwrap();
        assert_eq!(r["mode"], "camera");
        let seq = ItemId(r["sequence"].as_u64().unwrap());
        assert_eq!(s.state.active_sequence, Some(seq));
        let q = s.project.sequence(seq).unwrap().clone();
        let a1 = &q.audio_tracks[0].items;
        let v1 = &q.video_tracks[0].items;
        assert_eq!(a1.len(), r["order"].as_array().unwrap().len());
        assert_eq!(v1.len(), a1.len());
        for w in a1.windows(2) {
            assert!(w[1].start >= w[0].start + w[0].duration, "takes end to end, in order");
        }
        assert!(v1.iter().zip(a1).all(|(v, a)| v.start == a.start && v.link.is_some() && v.link == a.link), "camera picture and sound linked");
        // the words went back to their takes, inside each take
        let n: usize = a1.iter().map(|it| s.project.transcripts.get(&it.item).map_or(0, |t| t.words.len())).sum();
        assert_eq!(n as u64, r["words"].as_u64().unwrap());
        assert!(n > 0);
        for it in a1 {
            if let Some(t) = s.project.transcripts.get(&it.item) {
                assert!(t.words.iter().all(|w| w.end <= it.duration + it.source_in + Tick::from_seconds_f64(0.05)));
            }
        }
        // the context lists the takes and the timed lines
        let c = s.execute("decupador.context", json!({})).unwrap();
        assert!(c["text"].as_str().unwrap().starts_with("— "), "{}", c["text"]);
        assert_eq!(c["takes"].as_array().unwrap().len(), a1.len() + v1.len());
        // a plan in session seconds: two ranges, back to back, every track
        let rate = q.settings.frame_rate;
        let r = s.execute("decupador.applyPlan", json!({"sequence": seq.0, "plan": {"cuts": [{"id": "final", "title": "Final", "segments": [{"start": 1.0, "end": 3.0}, {"start": 5.0, "end": 6.0}]}]}})).unwrap();
        let out = ItemId(r["sequences"][0]["sequence"].as_u64().unwrap());
        let o = s.project.sequence(out).unwrap();
        assert_eq!(o.duration(), rate.snap_nearest(Tick::from_seconds_f64(2.0)) + rate.snap_nearest(Tick::from_seconds_f64(1.0)) - Tick::ZERO);
        let first = &o.video_tracks[0].items[0];
        let src = v1.iter().find(|v| v.start <= Tick::from_seconds_f64(1.0) && v.start + v.duration > Tick::from_seconds_f64(1.0)).unwrap();
        assert_eq!(first.item, src.item);
        assert_eq!(first.source_in, src.source_in + (rate.snap_nearest(Tick::from_seconds_f64(1.0)) - src.start));
        let a_first = &o.audio_tracks[0].items[0];
        assert_eq!((a_first.start, a_first.link.is_some()), (Tick::ZERO, true));
        assert_eq!(a_first.link, first.link, "still linked after the copy");
        assert_ne!(first.id, src.id, "new clips");
    }

    #[test]
    fn pictures_move_down_when_nothing_overlaps() {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let base = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        let at = |a: f64, b: f64| {
            let mut c = base.clone();
            c.start = Tick::from_seconds_f64(a);
            c.duration = Tick::from_seconds_f64(b - a);
            c
        };
        let out = pack(vec![vec![at(0.0, 2.0)], vec![at(2.0, 3.0), at(2.5, 4.0)]]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 2, "the V2 clip that overlaps nothing comes down");
        assert_eq!(out[1].len(), 1, "the overlapping one stays above");
        assert_eq!(pack(vec![vec![at(0.0, 1.0)], vec![at(1.0, 2.0)]]).len(), 1, "empty tracks go");
    }

    #[test]
    fn script_blocks_lose_page_headers_and_footers() {
        let blocks: Vec<(String, String)> = [
            "Miopia de Marca Date Tags Roteiro: Miopia de Marca \"Sua marca já tentou virar outra coisa pra vender mais?\"",
            "A dualidade de separar o negócio de nosso propósito, acaba nos desviando.",
            "“Segue aqui pra gente destrinchar isso.”",
            "18 de setembro de 2026 08\u{0}00 →  11\u{0}30 Miopia de Marca 1",
            "ok",
        ]
        .iter()
        .map(|t| (String::new(), t.to_string()))
        .collect();
        let lines: Vec<String> = script_lines(&blocks).into_iter().map(|l| l.text).collect();
        assert_eq!(
            lines,
            [
                "Sua marca já tentou virar outra coisa pra vender mais?",
                "A dualidade de separar o negócio de nosso propósito, acaba nos desviando.",
                "Segue aqui pra gente destrinchar isso."
            ]
        );
        assert_eq!(
            blocks_of_text("# Abertura\nLinha um\ncontinua\n\nLinha dois\n").into_iter().map(|b| b.1).collect::<Vec<_>>(),
            ["Linha um continua", "Linha dois"]
        );
    }

    #[test]
    fn the_script_lines_up_with_attempts_and_false_starts() {
        let script =
            vec![Line { text: "E se, no fim, nenhuma dessas roupas for a sua marca?".into() }, Line { text: "Segue aqui pra gente destrinchar isso.".into() }];
        let said = "Luz, ação. E se no fim nenhuma dessas roupas for a sua... pera aí. E se no fim nenhuma dessas roupas for a sua marca? Bate a palma. Segue aqui para a gente destrinchar melhor esse assunto. Fechou.";
        let words: Vec<SeqWord> = said.split_whitespace().enumerate().map(|(i, t)| w(t, i as f64 * 0.4, i as f64 * 0.4 + 0.3, 0.9)).collect();
        let a = align(&script, &words);
        let text = |x: &Attempt| text_of(&words, x.from, x.to);
        let line0: Vec<&Attempt> = a.iter().filter(|x| x.line == 0).collect();
        assert_eq!(line0.len(), 2, "{a:?}");
        assert_eq!(text(line0[0]), "E se no fim nenhuma dessas roupas for a sua...");
        assert!(!line0[0].complete(11), "stopped before \"marca\"");
        assert_eq!(text(line0[1]), "E se no fim nenhuma dessas roupas for a sua marca?");
        assert!(line0[1].complete(11));
        let line1: Vec<&Attempt> = a.iter().filter(|x| x.line == 1).collect();
        assert_eq!(text(line1[0]), "Segue aqui para a gente destrinchar melhor esse assunto.", "to the end of the sentence, \"para\" = \"pra\"");
        assert!(line1[0].complete(6));
        // slate, the "pera aí" and the chat are in no line
        assert!(!a.iter().any(|x| text(x).contains("Luz") || text(x).contains("Fechou") || text(x).contains("palma")));
    }

    #[test]
    fn a_restarted_line_is_two_attempts() {
        let script = vec![Line { text: "O verdadeiro sucesso de uma marca não pede pra trocar de roupa".into() }];
        let said = "O verdadeiro sucesso de uma marca não pede O verdadeiro sucesso de uma marca não pede pra trocar de roupa";
        let words: Vec<SeqWord> = said.split_whitespace().enumerate().map(|(i, t)| w(t, i as f64 * 0.3, i as f64 * 0.3 + 0.2, 0.9)).collect();
        let a = align(&script, &words);
        assert_eq!(a.len(), 2, "{a:?}");
        assert_eq!((a[0].from, a[0].to, a[1].from), (0, 7, 8));
        assert!(!a[0].complete(12) && a[1].complete(12));
    }

    #[test]
    fn plan_root_is_three_levels_up() {
        assert_eq!(plan_root(Path::new("/d/analysis/slug/premiere_cuts.json")), Some(PathBuf::from("/d")));
    }

    #[test]
    fn timeline_links_picture_and_sound() {
        let rate = FrameRate::new(30, 1);
        let p =
            Placed { video: "/v.mov".into(), audio: "/a.m4a".into(), external_audio: true, video_in: 10, frames: 30, audio_in: Tick::from_seconds_f64(2.5) };
        let t = timeline("T", &[p], rate, &SequenceSettings::default());
        let kids = &t["tracks"]["children"];
        assert_eq!(kids[0]["kind"], "Video");
        assert_eq!(kids[1]["kind"], "Audio");
        assert_eq!(kids[0]["children"][0]["metadata"]["filmcraft"]["link"], 1);
        assert_eq!(kids[1]["children"][0]["metadata"]["filmcraft"]["link"], 1);
        assert_eq!(kids[1]["children"][0]["media_reference"]["target_url"], "/a.m4a");
    }
}

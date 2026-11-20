//! Read-only loudness measurements over a sequence, strip output, or individual clips.

use std::collections::{BTreeMap, BTreeSet};

use filmcraft_audio_dsp::LoudnessMeter;
use filmcraft_audio_dsp::loudness::LoudnessSummary;
use filmcraft_project::{ClipId, Project, Sequence, TrackId, TrackItem};
use filmcraft_render::SourceProvider;
use filmcraft_time::TimeRange;
use serde_json::{Value, json};

use crate::commands::{bad, track_p_in};
use crate::{EngineError, Result, Session, export_tools};

const CMD: &str = "audio.measureLoudness";

pub(crate) fn measure(s: &Session, p: &Value) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad(CMD, "expected a parameter object"));
    }
    if p.get("sequence").is_some_and(|v| !v.is_null() && !v.is_u64()) {
        return Err(bad(CMD, "sequence must be an integer id"));
    }
    let id = export_tools::sequence_param(s, p, CMD)?;
    let seq = s.project.sequence(id).ok_or(EngineError::NoSequence)?;
    let of = match p.get("of") {
        None | Some(Value::Null) => "mix",
        Some(v) => v.as_str().ok_or_else(|| bad(CMD, "of must be mix, track or clips"))?,
    };
    // Export normally defaults to In/Out. A quality check should cover the whole sequence unless
    // the caller asks for a smaller range.
    let bounds = json!({ "startSeconds": p.get("startSeconds"), "endSeconds": p.get("endSeconds") });
    let mode = match p.get("range") {
        None | Some(Value::Null) if !bounds["startSeconds"].is_null() || !bounds["endSeconds"].is_null() => "custom",
        None | Some(Value::Null) => "entire",
        Some(v) => v.as_str().ok_or_else(|| bad(CMD, "range must be entire, inOut, workArea or custom"))?,
    };
    let (range, _) = export_tools::range_param(s, &s.project, id, Some(&json!(mode)), &bounds, CMD)?;
    let range = range.ok_or_else(|| bad(CMD, "give a start and an end for a custom range"))?;
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone()).own_sources().recording();
    let mut result = match of {
        "mix" => {
            let summary = filmcraft_export::measure_mix_loudness(s.project.clone(), id, range, &provider).map_err(|e| bad(CMD, e.to_string()))?;
            report(Some(range), Some(summary))
        }
        "track" => {
            let target = p.get("track").filter(|v| !v.is_null()).ok_or_else(|| bad(CMD, "give an audio track id or name like A1"))?;
            let track = track_p_in(seq, target, "track", CMD)?;
            if !seq.audio_tracks.iter().any(|t| t.id == track) {
                return Err(bad(CMD, "track must name an audio track"));
            }
            let summary = track_summary(&s.project, seq, track, range, &provider);
            let mut result = report(Some(range), summary);
            result["track"] = json!(track.0);
            result
        }
        "clips" => {
            let selection = if s.state.active_sequence == Some(id) { s.state.selection.as_slice() } else { &[] };
            let targets = clip_targets(seq, p, selection)?;
            let whole = mode.eq_ignore_ascii_case("entire");
            let sr = seq.settings.sample_rate;
            let clips: Vec<_> = targets
                .into_iter()
                .map(|clip| {
                    let measured = if whole { Some(clip.range()) } else { overlap(clip, range) };
                    let summary = measured.and_then(|r| crate::essential_sound::clip_meter(clip, r, sr, &provider)).map(|m| m.summary());
                    let mut result = report(measured, summary);
                    result["clip"] = json!(clip.id.0);
                    result
                })
                .collect();
            json!({ "clips": clips })
        }
        _ => return Err(bad(CMD, "of must be mix, track or clips")),
    };
    result["of"] = json!(of);
    result["offlineMedia"] = json!(provider.offline_used());
    Ok(result)
}

fn clip_targets<'a>(seq: &'a Sequence, p: &Value, selection: &[ClipId]) -> Result<Vec<&'a TrackItem>> {
    let items: BTreeMap<_, _> = seq.audio_tracks.iter().flat_map(|t| &t.items).map(|clip| (clip.id, clip)).collect();
    let clips: Vec<&TrackItem> = match p.get("clips").filter(|v| !v.is_null()) {
        None => selection.iter().filter_map(|id| items.get(id).copied()).collect(),
        Some(v) => {
            let list = v.as_array().ok_or_else(|| bad(CMD, "clips must be an array of audio clip ids"))?;
            let mut seen = BTreeSet::new();
            let mut found = Vec::new();
            for v in list {
                let id = ClipId(v.as_u64().ok_or_else(|| bad(CMD, "clips must contain integer audio clip ids"))?);
                if seen.insert(id) {
                    found.push(*items.get(&id).ok_or_else(|| bad(CMD, format!("no audio clip {} in this sequence", id.0)))?);
                }
            }
            found
        }
    };
    if clips.is_empty() {
        return Err(bad(CMD, "give or select one or more audio clips"));
    }
    Ok(clips)
}

fn overlap(clip: &TrackItem, range: TimeRange) -> Option<TimeRange> {
    let start = clip.start.max(range.start);
    let end = clip.end().min(range.end());
    (end > start).then(|| TimeRange::from_bounds(start, end))
}

fn track_summary(project: &Project, seq: &Sequence, track: TrackId, range: TimeRange, sources: &dyn SourceProvider) -> Option<LoudnessSummary> {
    let sr = i64::from(filmcraft_render::audio::bounded_sample_rate(seq.settings.sample_rate));
    let mut pos = range.start.to_units_floor(sr);
    let end = range.end().to_units_floor(sr);
    let mut meter = None;
    while pos < end {
        let frames = (end - pos).min(sr);
        let audio = filmcraft_render::mixer::mix_track(project, seq, track, pos, frames as usize, sources)?;
        let meter = meter.get_or_insert_with(|| LoudnessMeter::new(f64::from(audio.sample_rate), audio.channels.len()));
        let channels: Vec<&[f32]> = audio.channels.iter().map(Vec::as_slice).collect();
        meter.process(&channels);
        pos += frames;
    }
    meter.map(|m| m.summary())
}

fn report(range: Option<TimeRange>, summary: Option<LoudnessSummary>) -> Value {
    // JSON encodes the meter's non-finite readings as null. LRA is also unavailable with silence.
    json!({
        "start": range.map(|r| r.start.0),
        "end": range.map(|r| r.end().0),
        "seconds": range.map_or(0.0, |r| r.duration.seconds()),
        "integratedLufs": summary.map(|m| m.integrated_lufs),
        "loudnessRangeLu": summary.filter(|m| m.sample_peak_dbfs.is_finite()).map(|m| m.loudness_range_lu),
        "maxMomentaryLufs": summary.map(|m| m.max_momentary_lufs),
        "maxShortTermLufs": summary.map(|m| m.max_short_term_lufs),
        "samplePeakDbfs": summary.map(|m| m.sample_peak_dbfs),
        "truePeakDbtp": summary.map(|m| m.true_peak_dbtp),
    })
}

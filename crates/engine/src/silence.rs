//! `sequence.removeSilence` — remove the silent parts of the voice track, as one undoable edit.
//!
//! The word-level tools (`transcript.removePauses`, …) need a transcript; this command does not.
//! It measures the audio itself, 10 ms at a time, with the same energy VAD the transcriber uses
//! ([`filmcraft_speech::vad`]), finds every run of frames below the clip's own threshold that is
//! at least `minSeconds` long — keeping `keepSeconds` of air at each side — and cuts them with
//! the ordinary [`filmcraft_edit::transcript::ripple_delete_ranges`] edit: the ranges are
//! frame-snapped and ripple-deleted on every unlocked track, so video and audio are cut
//! together and stay in sync.
//!
//! By default the threshold is derived from the clip (floor at the 10th percentile of levels,
//! speech at the 95th, 30 % of the way up, at least 6 dB above the floor); `noiseDb` sets an
//! absolute dBFS RMS floor instead.

use filmcraft_speech::vad::{frame_db, threshold};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, f64_p, has_seq};
use crate::{Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal, named: crate::commands::NamedKeys::NONE }
}

fn from_frames_of_tenths(t: usize) -> Tick {
    Tick::from_seconds_f64(t as f64 * 0.01)
}

/// Silence runs `(start, end)` in frames of 10 ms (media time), keeping `keep` frames of air at
/// each side of the cut. Runs shorter than `min_run` frames are ignored. A clip with no frame
/// above the threshold contributes no cuts.
fn silence_cuts(db: &[f32], thr: f32, min_run: usize, keep: usize) -> Vec<(usize, usize)> {
    if db.iter().all(|&v| v < thr) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < db.len() {
        if db[i] >= thr {
            i += 1;
            continue;
        }
        let start = i;
        while i < db.len() && db[i] < thr {
            i += 1;
        }
        let end = i;
        let a = start + keep;
        let b = end.saturating_sub(keep);
        if end - start >= min_run && b > a {
            out.push((a, b));
        }
    }
    out
}

/// The threshold for this clip: the VAD threshold from the word aligner, or — when it fails to
/// separate noise from signal (e.g. a constant-tone clip where floor == speech) — 12 dB below the
/// clip's peak, so digital silence still reads as silence. An all-silent clip yields −90 dBFS.
fn effective_threshold(db: &[f32]) -> f32 {
    let vad = threshold(db);
    if db.iter().any(|&d| d >= vad) {
        vad
    } else {
        let peak = db.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if peak > -100.0 { peak - 12.0 } else { -90.0 }
    }
}

pub fn remove_silence(s: &mut Session, p: &Value) -> Result<Value> {
    let seq = s.active_sequence().ok_or_else(|| bad("sequence.removeSilence", "no sequence is open"))?;
    let rate = seq.settings.frame_rate;
    const SPEECH_SR: u32 = filmcraft_speech::SAMPLE_RATE; // 16 kHz (the VAD's frame grid)
    let min_run = (f64_p(p, "minSeconds").unwrap_or(1.0).max(0.0) * 100.0).round() as usize;
    let keep = (f64_p(p, "keepSeconds").unwrap_or(0.15).max(0.0) * 100.0).round() as usize;
    let noise_db = f64_p(p, "noiseDb");

    let to_tl = |it: &filmcraft_project::TrackItem, m: Tick| -> Tick { it.start + Tick(((m.0 - it.source_in.0) as f64 / it.speed).round() as i64) };

    // Targets: the selection, or every enabled audio clip with forward playback.
    let selection: Vec<filmcraft_project::ClipId> = s.state.selection.clone();
    let items: Vec<&filmcraft_project::TrackItem> = seq
        .audio_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .filter(|it| it.enabled && it.speed > 0.0 && !it.reverse && it.frame_hold.is_none())
        .filter(|it| selection.is_empty() || selection.contains(&it.id))
        .collect();
    if items.is_empty() {
        return Err(bad("sequence.removeSilence", "no audio clips to analyse (selection empty or clips unusable)"));
    }

    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mut cuts: Vec<TimeRange> = Vec::new();
    let mut skipped = Vec::new();
    for it in items {
        let Some(src) = filmcraft_render::SourceProvider::source(&provider, it.item) else {
            skipped.push(json!({"clip": it.id.0, "reason": "no media"}));
            continue;
        };
        if !src.info().has_audio() {
            skipped.push(json!({"clip": it.id.0, "reason": "no audio stream"}));
            continue;
        }
        let s0 = it.source_in.to_units_floor(SPEECH_SR as i64);
        let len = ((it.duration.to_units_floor(SPEECH_SR as i64) as f64) * it.speed.max(1e-6)).ceil() as i64;
        let mut db = Vec::new();
        let mut pos = s0;
        while pos < s0 + len {
            let n = ((s0 + len - pos).min(SPEECH_SR as i64)) as usize;
            match src.audio_stream(it.audio_stream, pos, n, SPEECH_SR) {
                Ok(b) => {
                    let frames = b.frames();
                    let n_ch = b.channels.len().max(1) as f32;
                    let mut mono = vec![0f32; frames];
                    for ch in &b.channels {
                        for (i, v) in ch.iter().enumerate() {
                            mono[i] += *v;
                        }
                    }
                    for v in &mut mono {
                        *v /= n_ch;
                    }
                    db.extend(frame_db(&mono));
                }
                Err(_) => {}
            }
            pos += n as i64;
        }
        if db.is_empty() {
            skipped.push(json!({"clip": it.id.0, "reason": "no samples"}));
            continue;
        }
        let thr = noise_db.map(|d| d as f32).unwrap_or_else(|| effective_threshold(&db));
        for (a, b) in silence_cuts(&db, thr, min_run, keep) {
            let ma = from_frames_of_tenths(a) + it.source_in;
            let mb = from_frames_of_tenths(b) + it.source_in;
            let a = to_tl(it, ma).max(it.start);
            let b = to_tl(it, mb).min(it.start + it.duration);
            if b > a {
                cuts.push(TimeRange::from_bounds(a, b));
            }
        }
    }

    let mut ranges = filmcraft_edit::transcript::merge_ranges(cuts);
    let snap_up = |t: Tick| {
        let s = rate.snap(t);
        if s < t { s + rate.frame_duration() } else { s }
    };
    ranges = ranges
        .into_iter()
        .filter_map(|r| {
            let s = snap_up(r.start);
            let e = rate.snap(r.end());
            if e > s { Some(TimeRange::from_bounds(s, e)) } else { None }
        })
        .collect();
    let n = ranges.len();
    if n == 0 {
        return Ok(json!({"removed": 0, "ticks": 0, "seconds": 0.0, "skipped": skipped}));
    }
    let total = s.edit_sequence("Remove Silence", |q, ctx, _| Ok(filmcraft_edit::transcript::ripple_delete_ranges(q, ranges, ctx)))?;
    Ok(json!({
        "removed": n,
        "ticks": total.0,
        "seconds": total.0 as f64 / TICKS_PER_SECOND as f64,
        "skipped": skipped,
    }))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![spec(
        "sequence.removeSilence",
        "Remove Silence…",
        &["Sequence", "Transcript"],
        r#"{"minSeconds":f?,"keepSeconds":f?,"noiseDb":f?}"#,
        has_seq,
        remove_silence,
        true,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A 1 s clip: 0.4 s of 440 Hz tone, 1.5 s of digital silence, then 0.4 s of tone.
    fn tone_silence_tone(path: &std::path::Path) {
        let rate = 48000u32;
        let mut samples: Vec<f32> = Vec::new();
        let mut push = |secs: f64, tone: bool| {
            let n = (secs * rate as f64) as usize;
            for i in 0..n {
                let v = if tone { 0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() } else { 0.0 };
                samples.push(v);
            }
        };
        push(0.4, true);
        push(1.5, false);
        push(0.4, true);
        filmcraft_testkit::wav::write_f32(path, &[&samples], rate).unwrap();
    }

    #[test]
    fn remove_silence_cuts_only_the_silent_middle() {
        let dir = filmcraft_testkit::fixtures_dir("engine/silence");
        let path = dir.join("tone_sil_tone.wav");
        tone_silence_tone(&path);
        let mut s = Session::default();
        s.execute("file.newSequence", json!({"name": "silence", "width": 320, "height": 240, "fps": 25.0, "sampleRate": 48000})).unwrap();
        let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
        let item = r["items"].as_array().unwrap()[0].as_u64().unwrap();
        s.execute("timeline.place", json!({"item": item, "track": "A1", "seconds": 0.0})).unwrap();
        let before = s.execute("sequence.inspect", json!({})).unwrap();
        let n_before = before["audio"].as_array().unwrap().iter().flat_map(|t| t["items"].as_array().unwrap()).count();
        assert_eq!(n_before, 1);

        let r = s.execute("sequence.removeSilence", json!({"minSeconds": 1.0, "keepSeconds": 0.1})).unwrap();
        assert_eq!(r["removed"], 1);
        let after = s.execute("sequence.inspect", json!({})).unwrap();
        let items: Vec<&Value> = after["audio"].as_array().unwrap().iter().flat_map(|t| t["items"].as_array().unwrap()).collect();
        assert_eq!(items.len(), 2);
        let d0 = items[0]["duration"].as_u64().unwrap_or(0);
        let d1 = items[1]["duration"].as_u64().unwrap_or(0);
        let total = d0 + d1;
        // both tones survive; ~4.5 s of the 2.3 s clip was silence (only the kept air remains)
        assert!(total <= (1.1_f64 * TICKS_PER_SECOND as f64) as u64);
        assert_eq!(after["duration"].as_u64().unwrap(), total);
    }
}

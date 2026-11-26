//! `audio.levels`: how loud a sequence's mix is over time, for agents that can't listen.
//!
//! A transcript tells an agent what is said, but not where music, applause, laughter, room tone or
//! silence are. This read-only report renders the mix through the mixer graph (as playback and
//! export do) and returns its level in fixed windows, the quiet stretches and the loudest moments,
//! so an agent can find pauses without speech recognition, cut on the music's loud parts or check
//! that nothing went silent after an edit. Levels are RMS and sample peak in dBFS over all channels
//! of the stereo fold; they are not loudness (LUFS) readings.

use filmcraft_project::ItemId;
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, f64_p, u64_p};
use crate::{EngineError, Result, Session};

const ID: &str = "audio.levels";
/// Floor reported for digital silence.
const FLOOR_DB: f64 = -120.0;
/// Windows reported at most; a longer range gets wider windows.
const MAX_WINDOWS: usize = 4000;
/// Longest range analysed in one call (4 h).
const MAX_SECONDS: f64 = 4.0 * 3600.0;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Audio Levels Over Time",
        menu: &[],
        shortcut: None,
        params: r#"{"sequence":id?,"startSeconds":f?,"endSeconds":f?,"windowSeconds":f=0.5,"quietDb":f=-45,"minQuietSeconds":f=1,"loudest":n=5}"#,
        enabled: always,
        run: levels,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

fn db(x: f64) -> f64 {
    if x > 0.0 { (20.0 * x.log10()).max(FLOOR_DB) } else { FLOOR_DB }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Runs of windows below `quiet_db` lasting at least `min_windows`, as (first, last exclusive).
pub(crate) fn quiet_runs(rms: &[f64], quiet_db: f64, min_windows: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &v) in rms.iter().chain(std::iter::once(&f64::INFINITY)).enumerate() {
        match (v < quiet_db, start) {
            (true, None) => start = Some(i),
            (false, Some(a)) => {
                if i - a >= min_windows.max(1) {
                    out.push((a, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// The rate the mixer renders `q` at (it bounds a damaged sequence rate), so requests in samples
/// cover the times they are meant to.
pub(crate) fn mix_rate(s: &Session, q: &filmcraft_project::Sequence, provider: &dyn filmcraft_render::SourceProvider) -> u32 {
    filmcraft_render::audio::mix_sequence(&s.project, q, 0, 1, provider).sample_rate.max(1)
}

fn levels(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = match u64_p(p, "sequence") {
        Some(id) => ItemId(id),
        None => s.state.active_sequence.ok_or(EngineError::NoSequence)?,
    };
    let q = s.project.sequence(seq_id).ok_or_else(|| bad(ID, format!("{} is not a sequence", seq_id.0)))?;
    let positive = |k: &str, d: f64| -> Result<f64> {
        let v = f64_p(p, k).unwrap_or(d);
        if v.is_finite() && v > 0.0 { Ok(v) } else { Err(bad(ID, format!("{k} must be a positive number"))) }
    };
    let duration = q.duration().seconds();
    let start = f64_p(p, "startSeconds").unwrap_or(0.0);
    let end = f64_p(p, "endSeconds").unwrap_or(duration).min(duration);
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err(bad(ID, "the range is empty: startSeconds must be ≥ 0 and before endSeconds (and the sequence end)"));
    }
    if end - start > MAX_SECONDS {
        return Err(bad(ID, "the range is longer than 4 hours; measure it in parts"));
    }
    let mut window = positive("windowSeconds", 0.5)?.clamp(0.05, 60.0);
    let mut widened = false;
    if (end - start) / window > MAX_WINDOWS as f64 {
        window = (end - start) / MAX_WINDOWS as f64;
        widened = true;
    }
    let quiet_db = f64_p(p, "quietDb").unwrap_or(-45.0);
    if !quiet_db.is_finite() {
        return Err(bad(ID, "quietDb must be a number"));
    }
    let min_quiet = positive("minQuietSeconds", 1.0)?;
    let loudest_n = u64_p(p, "loudest").unwrap_or(5).min(100) as usize;

    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let sr = mix_rate(s, q, &provider);
    let first = Tick::from_seconds_f64(start).to_units_floor(i64::from(sr));
    let last = Tick::from_seconds_f64(end).to_units_floor(i64::from(sr));
    let per = ((window * f64::from(sr)).round() as i64).max(1);
    // On the web a clip's audio can still be loading: the mix leaves it out, which would read as
    // silence. Report that instead of measuring it (as file.exportFrame does, #228).
    let _ = filmcraft_media::pending::take();
    let (mut rms, mut peak) = (Vec::new(), Vec::new());
    // a u64 count: hours of multichannel audio at high rates overflow a 32-bit usize (web)
    let (mut sum_sq, mut n_all, mut max_peak) = (0f64, 0u64, FLOOR_DB);
    // rendered in blocks of at most a second, the size playback asks for, so a long window takes
    // the same render path (time stretching has a span limit) as what is heard
    let block = i64::from(sr).max(1);
    let mut pos = first;
    while pos < last {
        let n = (last - pos).min(per);
        let (mut w_sq, mut w_n, mut w_pk) = (0f64, 0u64, 0f32);
        let mut at = pos;
        while at < pos + n {
            let m = (pos + n - at).min(block) as usize;
            let b = filmcraft_render::audio::mix_sequence(&s.project, q, at, m, &provider);
            for c in &b.channels {
                w_sq += c.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>();
                w_n += c.len() as u64;
                w_pk = c.iter().fold(w_pk, |m, x| m.max(x.abs()));
            }
            at += m as i64;
        }
        let (r, pk) = (if w_n > 0 { db((w_sq / w_n as f64).sqrt()) } else { FLOOR_DB }, db(f64::from(w_pk)));
        sum_sq += w_sq;
        n_all += w_n;
        max_peak = max_peak.max(pk);
        rms.push(r);
        peak.push(pk);
        pos += n;
    }
    if filmcraft_media::pending::take() {
        return Err(EngineError::Other("the audio is still loading; measure again in a moment".into()));
    }
    // the windows as measured: whole samples from the sample the range starts on
    let window = per as f64 / f64::from(sr);
    let first_seconds = first as f64 / f64::from(sr);
    let t_of = |i: usize| first_seconds + i as f64 * window;
    // lengths in samples, so a stretch of exactly minQuietSeconds isn't lost to rounding
    let secs = |samples: i64| samples as f64 / f64::from(sr);
    let min_samples = ((min_quiet * f64::from(sr)).round() as i64).max(1);
    // ceiling division that can't overflow, for any minimum (a huge one simply finds nothing)
    let min_windows = (min_samples - 1) / per + 1;
    // a run that reaches the end of the range is clipped to it, and then must still be long enough
    let quiet: Vec<Value> = quiet_runs(&rms, quiet_db, min_windows as usize)
        .into_iter()
        .map(|(a, b)| (first + a as i64 * per, (first + b as i64 * per).min(last)))
        .filter(|(a, b)| b - a >= min_samples)
        .map(|(a, b)| json!({"startSeconds": secs(a), "endSeconds": secs(b), "seconds": secs(b - a)}))
        .collect();
    let mut order: Vec<usize> = (0..rms.len()).collect();
    order.sort_by(|a, b| rms[*b].total_cmp(&rms[*a]));
    let loudest: Vec<Value> =
        order.into_iter().take(loudest_n).map(|i| json!({"startSeconds": t_of(i), "rmsDb": round1(rms[i]), "peakDb": round1(peak[i])})).collect();
    let overall = if n_all > 0 { db((sum_sq / n_all as f64).sqrt()) } else { FLOOR_DB };
    let mut warnings = Vec::new();
    if widened {
        warnings.push(format!("the range has more than {MAX_WINDOWS} windows: windowSeconds was widened to {window:.3}"));
    }
    Ok(json!({
        "sequence": seq_id.0,
        "startSeconds": first_seconds,
        "endSeconds": end,
        "windowSeconds": window,
        "rmsDb": rms.iter().map(|v| round1(*v)).collect::<Vec<_>>(),
        "peakDb": peak.iter().map(|v| round1(*v)).collect::<Vec<_>>(),
        "overallRmsDb": round1(overall),
        "maxPeakDb": round1(max_peak),
        "quietDb": quiet_db,
        "quiet": quiet,
        "loudest": loudest,
        "warnings": warnings,
        "notes": "rmsDb[i] and peakDb[i] cover startSeconds + i × windowSeconds. dBFS of the mix (stereo fold, after track and master faders); -120 is silence. Not LUFS.",
    }))
}

#[cfg(test)]
mod tests;

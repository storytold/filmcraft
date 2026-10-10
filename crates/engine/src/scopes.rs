//! `scopes.read`: Lumetri Scopes as numbers (M8.9), so agents can check colour without looking.
//! It renders the active sequence at the playhead (or `time`), computes the scopes with
//! `filmcraft-scopes` and returns levels in percent (per channel and per column range), histogram
//! bins, and the densest vectorscope spots with their angle and saturation.

use filmcraft_scopes::{self as scopes, ColorSpace, ParadeType, Params, ScopeKind, Signal, WaveformType, summary};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, has_seq, str_p, time_p, u64_p};
use crate::{Result, Session};

/// The scope signal of the active sequence at `t` (`scale` = render scale): R'G'B' code values
/// of the monitor picture, or PQ code values of the working-space render for HDR scopes.
pub fn scope_signal(s: &Session, t: Tick, scale: f32, hdr: bool) -> Result<Option<Signal>> {
    let Some(seq) = s.state.active_sequence else { return Ok(None) };
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    if hdr {
        let opts = filmcraft_render::RenderOptions { scale, working_output: true, ..Default::default() };
        let img = filmcraft_render::render_sequence(&s.project, seq, t, opts, &provider).map_err(crate::EngineError::Other)?;
        return Ok(Some(Signal::from_rgba_f32_with(img.w, img.h, &img.px, scopes::MAX_W, scopes::MAX_H, scopes::linear_to_pq)));
    }
    let opts = filmcraft_render::RenderOptions { scale, captions: true, ..Default::default() };
    let img = filmcraft_render::render_sequence(&s.project, seq, t, opts, &provider).map_err(crate::EngineError::Other)?;
    Ok(Some(Signal::from_rgba8(img.w, img.h, &img.over_black_rgba8())))
}

fn levels_json(w: &scopes::Waveform, columns: usize) -> Value {
    let mut o = serde_json::Map::new();
    for g in &w.traces {
        o.insert(g.name.to_string(), json!(summary::trace_columns(w, g, columns)));
    }
    json!({"lo": (w.lo as f64 * 100.0).round(), "hi": (w.hi as f64 * 100.0).round(), "columns": columns, "traces": o})
}

fn vectorscope_json(v: &scopes::Vectorscope, peaks: usize) -> Value {
    json!({
        "samples": v.samples,
        "mean": [(v.mean[0] as f64 * 10_000.0).round() / 100.0, (v.mean[1] as f64 * 10_000.0).round() / 100.0],
        "meanAngleDeg": (scopes::angle_deg(v.mean[0], v.mean[1]) as f64 * 100.0).round() / 100.0,
        "peaks": summary::peaks(v, peaks, 0.0),
    })
}

fn scopes_read(s: &mut Session, p: &Value) -> Result<Value> {
    let seq = s.active_sequence().ok_or(crate::EngineError::NoSequence)?;
    let hdr_seq = seq.settings.color.working.is_hdr();
    let rate = seq.settings.frame_rate;
    let space = match str_p(p, "colorSpace") {
        Some(c) => ColorSpace::from_name(c).ok_or_else(|| bad("scopes.read", format!("unknown colorSpace `{c}` (auto, 601, 709, 2100)")))?,
        None => ColorSpace::Auto,
    }
    .resolve(hdr_seq);
    let hdr = space == ColorSpace::Rec2100 && hdr_seq;
    let t = rate.snap(time_p(s, p, "").unwrap_or(s.playhead()));
    let scale = p.get("scale").and_then(Value::as_f64).unwrap_or(0.5).clamp(1.0 / 32.0, 1.0) as f32;
    let params = Params { matrix: space.matrix(), clamp: bool_p(p, "clamp").unwrap_or(true), ..Params::default() };
    let wt = match str_p(p, "waveformType") {
        Some(n) => WaveformType::from_name(n).ok_or_else(|| bad("scopes.read", format!("unknown waveformType `{n}`")))?,
        None => WaveformType::Rgb,
    };
    let pt = match str_p(p, "paradeType") {
        Some(n) => ParadeType::from_name(n).ok_or_else(|| bad("scopes.read", format!("unknown paradeType `{n}`")))?,
        None => ParadeType::Rgb,
    };
    let kinds: Vec<ScopeKind> = match p.get("scopes").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .map(|v| v.as_str().and_then(ScopeKind::from_name).ok_or_else(|| bad("scopes.read", format!("unknown scope {v}"))))
            .collect::<Result<_>>()?,
        None => ScopeKind::ALL.to_vec(),
    };
    let columns = u64_p(p, "columns").unwrap_or(8).clamp(1, 512) as usize;
    let npeaks = u64_p(p, "peaks").unwrap_or(8).clamp(1, 64) as usize;
    let bins = bool_p(p, "bins").unwrap_or(true);
    let sig = scope_signal(s, t, scale, hdr)?.ok_or(crate::EngineError::NoSequence)?;
    let mut out = json!({
        "time": t.0,
        "frame": rate.frame_at(t),
        "samples": [sig.w, sig.h],
        "colorSpace": space.label(),
        "matrix": format!("{:?}", space.matrix()),
        "clamp": params.clamp,
        "unit": if hdr { "pq%" } else { "%" },
        "stats": summary::channel_stats(&sig, params.matrix),
    });
    for k in kinds {
        let v = match scopes::compute(k, &sig, &params, wt, pt) {
            scopes::Computed::Waveform(w) => {
                let mut v = levels_json(&w, columns);
                v["type"] = json!(wt);
                v
            }
            scopes::Computed::Parade(w) => {
                let mut v = levels_json(&w, columns);
                v["type"] = json!(pt);
                v
            }
            scopes::Computed::Histogram(h) => {
                let peak = |b: &[u32]| b.iter().enumerate().max_by_key(|(i, c)| (**c, usize::MAX - i)).map(|(i, _)| i).unwrap_or(0);
                let mut v = json!({
                    "samples": h.samples,
                    "below": h.below, "above": h.above,
                    "peakBin": {"r": peak(&h.r), "g": peak(&h.g), "b": peak(&h.b), "y": peak(&h.y)},
                });
                if bins {
                    v["r"] = json!(h.r);
                    v["g"] = json!(h.g);
                    v["b"] = json!(h.b);
                    v["y"] = json!(h.y);
                }
                v
            }
            scopes::Computed::Vectorscope(vs) => vectorscope_json(&vs, npeaks),
        };
        out[k.name()] = v;
    }
    Ok(out)
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "scopes.read",
        label: "Read Lumetri Scopes",
        menu: &[],
        shortcut: None,
        params: r#"{"scopes":["waveform","parade","histogram","vectorscopeYuv","vectorscopeHls"]?,"waveformType":"rgb|luma|yc|ycNoChroma"?,"paradeType":"rgb|yuv|rgbWhite"?,"colorSpace":"auto|601|709|2100"?,"clamp":bool=true,"columns":n=8,"peaks":n=8,"bins":bool=true,"scale":0.5,"time":ticks?|"frame"|"seconds"|"timecode"}"#,
        enabled: has_seq,
        run: scopes_read,
        journal: false,
    }]
}

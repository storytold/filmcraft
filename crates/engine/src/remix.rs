//! Clip ▸ Remix: retime a music clip to a target duration by cutting it at musically similar beat
//! boundaries (Premiere's Remix; analysis and planning in [`filmcraft_audio_dsp::remix`], playback
//! in [`filmcraft_render::remix`]).
//!
//! | command | menu | what it does |
//! |---|---|---|
//! | `clip.remix.enable` | Clip ▸ Remix ▸ Enable Remix | analyses the selected audio clip and makes it a remix clip at its current duration |
//! | `clip.remix.properties` | Clip ▸ Remix ▸ Remix Properties… | without parameters returns the settings; with `duration`/`seconds`/`frame`/`timecode`, `segments`, `variations` re-plans |
//! | `clip.remix.revert` | Clip ▸ Remix ▸ Revert Remix | restores the clip as it was before the remix |
//! | `clip.remix` | — | agents: enable (if needed) and remix `clip` (default: the selected audio clip) to a target |
//!
//! The remix keeps the clip's start and source In, plays the intro from the beginning and ends on
//! the source's end; only the duration changes. The clip's linked partners are not changed. When
//! the remixed clip would run into the next clip on its track the command is refused (nothing is
//! trimmed or moved); remixing shorter always fits. Speed changes and reverse do not apply to a
//! remixed clip (enabling resets them to 100 %).
//!
//! The analysis of a clip (beats, beat self-similarity) is cached per media range and rate, so
//! changing the target or the sliders re-plans instantly. Everything is deterministic.

use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_audio_dsp::remix::{self as dsp, Analysis, Params};
use filmcraft_project::{ClipId, ItemId, TrackKind};
use filmcraft_render::remix::{Remix, TickPiece};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, f64_p, has_seq, u64_p};
use crate::{EngineError, Result, Session};

type Key = (u64, i64, i64, u32);

fn cache() -> &'static Mutex<Vec<(Key, Arc<Analysis>)>> {
    static C: OnceLock<Mutex<Vec<(Key, Arc<Analysis>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The audio clip a menu command works on: the first selected clip on an audio track.
fn selected_audio(s: &Session) -> Option<ClipId> {
    let seq = s.active_sequence()?;
    s.state.selection.iter().copied().find(|c| seq.find_item(*c).and_then(|(t, _)| seq.track(t)).is_some_and(|t| t.kind == TrackKind::Audio))
}

fn check_clip(s: &Session, c: ClipId) -> std::result::Result<(), String> {
    let seq = s.active_sequence().ok_or("no sequence is open")?;
    let (t, it) = seq.find_item(c).ok_or("no such clip")?;
    if seq.track(t).is_none_or(|t| t.kind != TrackKind::Audio) {
        return Err("Remix works on audio clips".into());
    }
    let item = s.project.item(it.item).ok_or("the clip has no media")?;
    if !matches!(item.kind, filmcraft_project::ItemKind::Media(_)) || !item.has_audio() {
        return Err("the clip has no audio media to remix".into());
    }
    Ok(())
}

fn can_enable(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let c = selected_audio(s).ok_or("select an audio clip")?;
    check_clip(s, c)
}

fn is_remixed(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let c = selected_audio(s).ok_or("select an audio clip")?;
    let seq = s.active_sequence().ok_or("no sequence is open")?;
    if seq.find_item(c).is_some_and(|(_, i)| filmcraft_render::remix::is_remixed(i)) { Ok(()) } else { Err("the selected clip is not remixed".into()) }
}

fn target_clip(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    let c = match u64_p(p, "clip") {
        Some(c) => ClipId(c),
        None => selected_audio(s).ok_or_else(|| bad(cmd, "select an audio clip or pass `clip`"))?,
    };
    check_clip(s, c).map_err(|e| bad(cmd, e))?;
    Ok(c)
}

/// Target duration from `duration` (ticks), `seconds`, `frame` or `timecode` (a duration).
fn duration_p(s: &Session, p: &Value) -> Option<Tick> {
    if let Some(d) = p.get("duration").and_then(Value::as_i64) {
        return Some(Tick(d));
    }
    let rate = s.sequence_rate();
    if let Some(sec) = f64_p(p, "seconds") {
        return Some(Tick::from_seconds_f64(sec));
    }
    if let Some(f) = p.get("frame").or_else(|| p.get("frames")).and_then(Value::as_i64) {
        return Some(rate.tick_of(f));
    }
    if let Some(tc) = p.get("timecode").and_then(Value::as_str) {
        let df = s.active_sequence().map(|q| q.settings.drop_frame).unwrap_or(false);
        return filmcraft_time::parse_timecode(tc, rate, df, 0).ok().map(|f| rate.tick_of(f));
    }
    None
}

/// Analysis of `[source_in, source_in + dur)` of the clip's media at the sequence rate (cached).
fn analysis(s: &Session, item: ItemId, source_in: Tick, dur: Tick) -> Result<Arc<Analysis>> {
    let sr = s.active_sequence().map(|q| q.settings.sample_rate).unwrap_or(48_000).max(1);
    let m0 = source_in.to_units_floor(sr as i64);
    let len = (source_in + dur).to_units_floor(sr as i64) - m0;
    let key = (item.0, m0, len, sr);
    if let Some((_, a)) = cache().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) {
        return Ok(a.clone());
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let src = filmcraft_render::SourceProvider::source(&provider, item).ok_or_else(|| EngineError::Other("the clip's media is offline".into()))?;
    let buf = src.audio(m0, len.max(0) as usize, sr).map_err(|e| EngineError::Other(format!("reading the clip's audio: {e}")))?;
    let refs: Vec<&[f32]> = buf.channels.iter().map(Vec::as_slice).collect();
    let a = Arc::new(dsp::analyze(&refs, sr).map_err(|e| EngineError::Other(format!("cannot remix: {e}")))?);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.push((key, a.clone()));
    if c.len() > 8 {
        c.remove(0);
    }
    Ok(a)
}

/// The remix of `c` to `target` with the given sliders: (new duration, state).
fn make_plan(s: &Session, c: ClipId, target: Tick, segments: f64, variations: f64) -> Result<(Tick, Remix, Arc<Analysis>)> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sr = seq.settings.sample_rate.max(1) as i64;
    let (_, it) = seq.find_item(c).ok_or_else(|| EngineError::Other("no such clip".into()))?;
    let old = filmcraft_render::remix::Remix::of(it);
    let original = old.as_ref().map(|r| r.original).unwrap_or(it.duration);
    let a = analysis(s, it.item, it.source_in, original)?;
    let params = Params { segments: segments.clamp(0.0, 100.0), variations: variations.clamp(0.0, 100.0) };
    let plan = dsp::plan(&a, target.to_units_floor(sr), params).map_err(|e| EngineError::Other(format!("cannot remix: {e}")))?;
    let pieces = plan.pieces.iter().map(|p| TickPiece { src: it.source_in + Tick::from_units(p.src, sr), len: Tick::from_units(p.len, sr) }).collect();
    let rm = Remix { target, segments: params.segments, variations: params.variations, original, xfade: Tick::from_units(plan.xfade, sr), pieces };
    Ok((Tick::from_units(plan.len(), sr), rm, a))
}

/// Apply `rm` to clip `c` with duration `dur` (one undo step); refuses overlaps.
fn apply(s: &mut Session, label: &str, c: ClipId, dur: Tick, rm: Option<Remix>) -> Result<()> {
    s.edit_sequence(label, move |q, _, _| {
        let (tid, it) = q.find_item(c).ok_or_else(|| EngineError::Other("no such clip".into()))?;
        let start = it.start;
        let tr = q.track(tid).ok_or_else(|| EngineError::Other("no such track".into()))?;
        if let Some(next) = tr.items.iter().filter(|i| i.id != c && i.start >= start).map(|i| i.start).min()
            && start + dur > next
        {
            return Err(EngineError::Other(format!("not enough room: the remixed clip ({:.2} s) would overlap the next clip on {}", dur.seconds(), tr.name)));
        }
        let (_, it) = q.find_item_mut(c).ok_or_else(|| EngineError::Other("no such clip".into()))?;
        it.duration = dur;
        match rm {
            Some(r) => {
                it.speed = 1.0;
                it.reverse = false;
                r.store(it);
            }
            None => {
                filmcraft_render::remix::take(it);
            }
        }
        if let Some(t) = q.track_mut(tid) {
            t.sort();
        }
        Ok(())
    })
}

fn report(s: &Session, c: ClipId, a: Option<&Analysis>) -> Value {
    let Some(seq) = s.active_sequence() else { return Value::Null };
    let Some((_, it)) = seq.find_item(c) else { return Value::Null };
    let rm = Remix::of(it);
    let mut v = json!({
        "clip": c.0,
        "remixed": rm.is_some(),
        "duration": it.duration.0,
        "seconds": it.duration.seconds(),
    });
    if let Some(r) = rm {
        let cuts: Vec<Value> = r.pieces.windows(2).map(|w| json!({"out": (w[0].src + w[0].len).0, "in": w[1].src.0})).collect();
        v["target"] = json!(r.target.0);
        v["targetSeconds"] = json!(r.target.seconds());
        v["originalSeconds"] = json!(r.original.seconds());
        v["segments"] = json!(r.segments);
        v["variations"] = json!(r.variations);
        v["pieces"] = Value::Array(r.pieces.iter().map(|p| json!({"src": p.src.0, "len": p.len.0})).collect());
        v["cuts"] = Value::Array(cuts);
        v["errorSeconds"] = json!((it.duration - r.target).seconds());
    }
    if let Some(a) = a {
        v["bpm"] = json!(a.bpm);
        v["beats"] = json!(a.beats.len());
    }
    v
}

fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    let c = target_clip(s, p, "clip.remix.enable")?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(c).ok_or_else(|| bad("clip.remix.enable", "no such clip"))?;
    if filmcraft_render::remix::is_remixed(it) {
        return Ok(report(s, c, None));
    }
    let dur = it.duration;
    let (d, rm, a) = make_plan(s, c, dur, 50.0, 50.0)?;
    apply(s, "Enable Remix", c, d, Some(rm))?;
    Ok(report(s, c, Some(&a)))
}

fn remix(s: &mut Session, p: &Value, cmd: &str, label: &str) -> Result<Value> {
    let c = target_clip(s, p, cmd)?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
    let cur = Remix::of(it);
    let target = duration_p(s, p);
    let segments = f64_p(p, "segments");
    let variations = f64_p(p, "variations");
    if target.is_none() && segments.is_none() && variations.is_none() {
        if cmd == "clip.remix" {
            return Err(bad(cmd, "need a target `duration` (ticks), `seconds`, `frame` or `timecode`"));
        }
        return Ok(report(s, c, None));
    }
    let target = target.or(cur.as_ref().map(|r| r.target)).unwrap_or(it.duration);
    if target.0 <= 0 {
        return Err(bad(cmd, "the target duration must be positive"));
    }
    let segments = segments.or(cur.as_ref().map(|r| r.segments)).unwrap_or(50.0);
    let variations = variations.or(cur.as_ref().map(|r| r.variations)).unwrap_or(50.0);
    let (d, rm, a) = make_plan(s, c, target, segments, variations)?;
    apply(s, label, c, d, Some(rm))?;
    Ok(report(s, c, Some(&a)))
}

fn revert(s: &mut Session, p: &Value) -> Result<Value> {
    let c = target_clip(s, p, "clip.remix.revert")?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(c).ok_or_else(|| bad("clip.remix.revert", "no such clip"))?;
    let rm = Remix::of(it).ok_or_else(|| bad("clip.remix.revert", "the clip is not remixed"))?;
    apply(s, "Revert Remix", c, rm.original, None)?;
    Ok(report(s, c, None))
}

pub fn commands() -> Vec<CommandSpec> {
    const PARAMS: &str = r#"{"clip":id?,"duration":ticks?,"seconds":f64?,"frame":n?,"timecode":str?,"segments":0..100?,"variations":0..100?}"#;
    vec![
        CommandSpec {
            id: "clip.remix.enable",
            label: "Enable Remix",
            menu: &["Clip", "Remix"],
            shortcut: None,
            params: r#"{"clip":id?}"#,
            enabled: can_enable,
            run: enable,
            journal: true,
        },
        CommandSpec {
            id: "clip.remix.properties",
            label: "Remix Properties…",
            menu: &["Clip", "Remix"],
            shortcut: None,
            params: PARAMS,
            enabled: is_remixed,
            run: |s, p| remix(s, p, "clip.remix.properties", "Remix Properties"),
            journal: true,
        },
        CommandSpec {
            id: "clip.remix.revert",
            label: "Revert Remix",
            menu: &["Clip", "Remix"],
            shortcut: None,
            params: r#"{"clip":id?}"#,
            enabled: is_remixed,
            run: revert,
            journal: true,
        },
        CommandSpec {
            id: "clip.remix",
            label: "Remix",
            menu: &[],
            shortcut: None,
            params: PARAMS,
            enabled: has_seq,
            run: |s, p| remix(s, p, "clip.remix", "Remix"),
            journal: true,
        },
    ]
}

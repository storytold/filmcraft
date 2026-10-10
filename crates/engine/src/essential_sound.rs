//! Essential Sound commands (`essentialSound.*`).
//!
//! Clips get an audio type (Dialogue / Music / SFX / Ambience) and per-type settings
//! ([`filmcraft_project::essential`]). Every change goes through one undoable edit that updates the
//! settings and reconciles the clip's essential effects, clip gain, Volume and Panner
//! ([`filmcraft_project::essential::apply`]), so what the panel does is ordinary clip data that the
//! mixer, playback and export render without special cases.
//!
//! | command | does |
//! |---|---|
//! | `essentialSound.inspect` | types, settings and essential effects of the selected audio clips; presets; targets |
//! | `essentialSound.setType` / `clearType` | assign / clear the audio type (clearing removes its effects and gain) |
//! | `essentialSound.set` | one setting by dotted key (`repair.noise.amount`), or several (`values`) |
//! | `essentialSound.applyPreset` / `savePreset` / `deletePreset` | built-in and user presets |
//! | `essentialSound.autoMatch` | measure integrated loudness (BS.1770, mono clips as one channel) and set the match gain to the target |
//! | `essentialSound.generateDucking` | Volume keyframes that duck Music / Ambience under the trigger types |

use serde_json::{Value, json};

use filmcraft_audio_dsp::LoudnessMeter;
use filmcraft_project::essential::{self as es, AudioType, EssentialSound, Preset, Section};
use filmcraft_project::{ClipId, ParamValue, Project, TrackItem};
use filmcraft_render::SourceProvider;
use filmcraft_time::Tick;

use crate::autosave::UserSoundPreset;
use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, with_links};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// The audio clips a command acts on: `clips` (or `clip`), else the selection, with linked partners.
pub fn targets(s: &Session, p: &Value) -> Vec<ClipId> {
    let sel: Vec<ClipId> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => match p.get("clip").and_then(Value::as_u64) {
            Some(c) => vec![ClipId(c)],
            None => s.state.selection.clone(),
        },
    };
    let Some(seq) = s.active_sequence() else { return Vec::new() };
    let all = with_links(s, &sel);
    seq.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| all.contains(&i.id)).map(|i| i.id).collect()
}

fn mt_of(it: &TrackItem, t: Tick) -> Tick {
    it.source_time_at(t.clamp(it.start, (it.end() - Tick(1)).max(it.start)))
}

/// Change the settings of `clips` with `f` and reconcile their effects, as one undo step (or merged
/// into the previous step with the same `merge` key while a control is dragged).
fn update(
    s: &mut Session,
    cmd: &str,
    label: &str,
    merge: Option<String>,
    clips: &[ClipId],
    f: impl Fn(&TrackItem, &mut Option<EssentialSound>) -> std::result::Result<(), String>,
) -> Result<usize> {
    if clips.is_empty() {
        return Err(bad(cmd, "select one or more audio clips"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let ph = s.playhead();
    let body = |p: &mut Project, _: &mut crate::EditorState| -> Result<usize> {
        let seq = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let mut n = 0;
        for it in seq.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|i| clips.contains(&i.id)) {
            let old = it.essential.clone();
            let mut next = old.clone();
            f(it, &mut next).map_err(|e| bad(cmd, e))?;
            if next != old {
                it.essential = next;
                let mt = mt_of(it, ph);
                es::apply(it, old.as_ref(), mt);
                n += 1;
            }
        }
        Ok(n)
    };
    match merge {
        Some(k) => s.edit_merged(label, &k, body),
        None => s.edit(label, body),
    }
}

/// Built-in presets followed by the user's.
pub fn presets(s: &Session) -> Vec<Preset> {
    let mut v = es::builtin_presets();
    v.extend(s.prefs.essential_sound.user_presets.iter().map(|u| Preset {
        name: u.name.clone(),
        kind: u.settings.kind,
        settings: u.settings.clone(),
        builtin: false,
    }));
    v
}

fn type_p(p: &Value, cmd: &str) -> Result<Option<AudioType>> {
    match str_p(p, "type") {
        None => Ok(None),
        Some(t) => AudioType::parse(t).map(Some).ok_or_else(|| bad(cmd, format!("unknown audio type `{t}` (dialogue, music, sfx, ambience)"))),
    }
}

fn clip_json(s: &Session, it: &TrackItem, track: &str) -> Value {
    let t = s.playhead();
    let level = it.effect("volume").map(|e| e.f64_at("level", mt_of(it, t))).unwrap_or(0.0);
    json!({
        "clip": it.id.0,
        "name": it.name,
        "track": track,
        "type": it.essential.as_ref().map(|e| e.kind.label()),
        "settings": it.essential.as_ref().map(EssentialSound::to_value),
        "gainDb": it.gain_db,
        "volumeDb": level,
        "volumeAnimated": it.effect("volume").and_then(|e| e.param("level")).is_some_and(|p| p.is_animated()),
        "effects": it.effects.iter().enumerate().filter(|(_, e)| e.essential).map(|(i, e)| json!({"index": i, "effect": e.effect, "enabled": e.enabled})).collect::<Vec<_>>(),
    })
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = targets(s, p);
    let mut out = Vec::new();
    let mut kinds = Vec::new();
    if let Some(seq) = s.active_sequence() {
        for (k, t) in seq.audio_tracks.iter().enumerate() {
            for it in t.items.iter().filter(|i| clips.contains(&i.id)) {
                out.push(clip_json(s, it, &format!("A{}", k + 1)));
                kinds.push(it.essential.as_ref().map(|e| e.kind));
            }
        }
    }
    kinds.dedup();
    let kind = match kinds.as_slice() {
        [Some(k)] => json!(k.label()),
        [None] | [] => Value::Null,
        _ => json!("mixed"),
    };
    let ps = presets(s);
    let by_type = |t: AudioType| ps.iter().filter(|p| p.kind == t).map(|p| json!({"name": p.name, "builtin": p.builtin})).collect::<Vec<_>>();
    Ok(json!({
        "clips": out,
        "type": kind,
        "presets": AudioType::ALL.iter().map(|t| (t.label().to_string(), json!(by_type(*t)))).collect::<serde_json::Map<_, _>>(),
        "sections": AudioType::ALL.iter().map(|t| (t.label().to_string(), json!(t.sections().iter().map(|s| s.label()).collect::<Vec<_>>()))).collect::<serde_json::Map<_, _>>(),
        "targets": AudioType::ALL.iter().map(|t| (t.label().to_string(), json!(s.prefs.audio.loudness_target(*t)))).collect::<serde_json::Map<_, _>>(),
        "eqPresets": es::EQ_PRESETS.iter().map(|p| p.name).collect::<Vec<_>>(),
        "reverbPresets": es::REVERB_PRESETS.iter().map(|p| p.name).collect::<Vec<_>>(),
    }))
}

fn set_type(s: &mut Session, p: &Value) -> Result<Value> {
    let t = type_p(p, "essentialSound.setType")?.ok_or_else(|| bad("essentialSound.setType", "need `type`"))?;
    let clips = targets(s, p);
    let n = update(s, "essentialSound.setType", &format!("Set Audio Type to {}", t.label()), None, &clips, |_, e| {
        if e.as_ref().is_none_or(|x| x.kind != t) {
            *e = Some(EssentialSound::new(t));
        }
        Ok(())
    })?;
    Ok(json!({"clips": n, "type": t.label()}))
}

fn clear_type(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = targets(s, p);
    let n = update(s, "essentialSound.clearType", "Clear Audio Type", None, &clips, |_, e| {
        *e = None;
        Ok(())
    })?;
    Ok(json!({"clips": n}))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let mut kv: Vec<(String, Value)> = Vec::new();
    if let Some(k) = str_p(p, "key") {
        kv.push((k.to_string(), p.get("value").cloned().ok_or_else(|| bad("essentialSound.set", "need `value`"))?));
    }
    if let Some(m) = p.get("values").and_then(Value::as_object) {
        kv.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if kv.is_empty() {
        return Err(bad("essentialSound.set", "need `key` + `value` or `values`"));
    }
    let clips = targets(s, p);
    if bool_p(p, "begin").unwrap_or(false) {
        s.history.merge_key = None;
    }
    let keys: Vec<&str> = kv.iter().map(|(k, _)| k.as_str()).collect();
    let merge = format!("essentialSound:{clips:?}:{}", keys.join(","));
    let n = update(s, "essentialSound.set", "Essential Sound", Some(merge), &clips, |it, e| {
        let st = e.as_mut().ok_or_else(|| format!("clip `{}` has no audio type; set one first", it.name))?;
        for (k, v) in &kv {
            st.set(k, v)?;
        }
        Ok(())
    })?;
    Ok(json!({"clips": n}))
}

fn apply_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "essentialSound.applyPreset";
    let name = str_p(p, "preset").ok_or_else(|| bad(cmd, "need `preset`"))?.to_string();
    let forced = type_p(p, cmd)?;
    let ps = presets(s);
    let clips = targets(s, p);
    let n = update(s, cmd, &format!("Apply Sound Preset {name}"), None, &clips, |it, e| {
        let kind = forced.or(e.as_ref().map(|x| x.kind)).ok_or_else(|| format!("clip `{}` has no audio type; pass `type`", it.name))?;
        let pr =
            ps.iter().rev().find(|x| x.kind == kind && x.name.eq_ignore_ascii_case(&name)).ok_or_else(|| format!("no {} preset `{name}`", kind.label()))?;
        *e = Some(es::with_preset(e.as_ref(), pr));
        Ok(())
    })?;
    Ok(json!({"clips": n, "preset": name}))
}

fn save_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "essentialSound.savePreset";
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "need `name`"))?.to_string();
    let clips = targets(s, p);
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut st =
        clips.iter().find_map(|c| seq.find_item(*c).and_then(|(_, i)| i.essential.clone())).ok_or_else(|| bad(cmd, "select a clip with an audio type"))?;
    if es::builtin_presets().iter().any(|b| b.kind == st.kind && b.name.eq_ignore_ascii_case(&name)) {
        return Err(bad(cmd, format!("`{name}` is a built-in preset name")));
    }
    st.preset = name.clone();
    st.loudness = es::Loudness { enabled: st.loudness.enabled, ..Default::default() };
    st.volume = Default::default();
    st.mute = false;
    let kind = st.kind;
    let mut prefs = s.prefs.clone();
    prefs.essential_sound.user_presets.retain(|u| !(u.settings.kind == kind && u.name.eq_ignore_ascii_case(&name)));
    prefs.essential_sound.user_presets.push(UserSoundPreset { name: name.clone(), settings: st });
    s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    // the clips now show the saved preset
    let _ = update(s, cmd, "Save Sound Preset", None, &clips, |_, e| {
        if let Some(x) = e.as_mut().filter(|x| x.kind == kind) {
            x.preset = name.clone();
        }
        Ok(())
    });
    Ok(json!({"name": name, "type": kind.label()}))
}

fn delete_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "essentialSound.deletePreset";
    let name = str_p(p, "name").or_else(|| str_p(p, "preset")).ok_or_else(|| bad(cmd, "need `name`"))?.to_string();
    let kind = type_p(p, cmd)?;
    let mut prefs = s.prefs.clone();
    let before = prefs.essential_sound.user_presets.len();
    prefs.essential_sound.user_presets.retain(|u| !(u.name.eq_ignore_ascii_case(&name) && kind.is_none_or(|k| k == u.settings.kind)));
    if prefs.essential_sound.user_presets.len() == before {
        return Err(bad(cmd, format!("no user preset `{name}` (built-in presets cannot be deleted)")));
    }
    s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    Ok(json!({"deleted": name}))
}

// ------------------------------------------------------------------------------------- loudness

/// Integrated loudness (LUFS) of a clip's own signal (clip gain + clip effects) over its whole
/// timeline range, or `None` when it is silent / has no audio.
///
/// The clip is measured with the channels it has: a stereo (or wider) source as the two channels
/// it plays, a mono source or a mono pick as one channel (its two identical sides folded to one),
/// the way BS.1770 and ffmpeg's `ebur128` measure a mono programme. Metering the dual-mono pair the
/// clip plays would read 3 LU louder than the file and match it 3 dB too quietly (#296).
pub fn clip_loudness(item: &TrackItem, sr: u32, sources: &dyn SourceProvider) -> Option<f64> {
    let a0 = item.start.to_units_floor(sr as i64);
    let a1 = item.end().to_units_floor(sr as i64);
    let mono = filmcraft_render::audio::clip_is_mono(item, sources)?;
    let mut meter = LoudnessMeter::new(sr as f64, if mono { 1 } else { 2 });
    let mut pos = a0;
    while pos < a1 {
        let n = (a1 - pos).min(sr as i64) as usize;
        let [l, r] = filmcraft_render::audio::clip_signal(item, pos, n, sr, sources)?;
        if mono {
            let m: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
            meter.process(&[&m]);
        } else {
            meter.process(&[&l, &r]);
        }
        pos += n as i64;
    }
    Some(meter.integrated()).filter(|v| v.is_finite())
}

fn auto_match(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "essentialSound.autoMatch";
    let clips = targets(s, p);
    let target_p = f64_p(p, "target");
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sr = seq.settings.sample_rate.max(1);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    // measure first (read-only), then write every match in one undo step
    let mut matches: Vec<(ClipId, f64, f64, f64)> = Vec::new(); // clip, measured (without match gain), target, new match gain
    let mut report = Vec::new();
    for it in seq.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| clips.contains(&i.id)) {
        let Some(st) = it.essential.as_ref() else {
            report.push(json!({"clip": it.id.0, "skipped": "no audio type"}));
            continue;
        };
        let target = target_p.unwrap_or_else(|| s.prefs.audio.loudness_target(st.kind)).clamp(-60.0, 0.0);
        let Some(l) = clip_loudness(it, sr, &provider) else {
            report.push(json!({"clip": it.id.0, "skipped": "silent"}));
            continue;
        };
        // the clip gain (including any earlier match gain) applies linearly after the effects
        let current = st.loudness_gain_db();
        let measured = l - current;
        let gain = filmcraft_audio_dsp::normalize_gain_db(measured, target);
        matches.push((it.id, measured, target, gain));
        report.push(json!({"clip": it.id.0, "measuredLufs": measured, "targetLufs": target, "gainDb": gain}));
    }
    if matches.is_empty() {
        return Err(bad(cmd, "no clips with an audio type and audible audio in the selection"));
    }
    let ids: Vec<ClipId> = matches.iter().map(|m| m.0).collect();
    update(s, cmd, "Auto-Match Loudness", None, &ids, |it, e| {
        if let (Some(st), Some(m)) = (e.as_mut(), matches.iter().find(|m| m.0 == it.id)) {
            st.loudness.enabled = true;
            st.loudness.measured_lufs = Some(m.1);
            st.loudness.target_lufs = Some(m.2);
            st.loudness.gain_db = m.3;
        }
        Ok(())
    })?;
    Ok(json!({"clips": report}))
}

// ------------------------------------------------------------------------------------- ducking

/// Analysis hop and smoothing window of the ducking detector.
const DUCK_HOP_S: f64 = 0.01;
const DUCK_WINDOW_HOPS: usize = 5;
/// Ignore trigger blips shorter than this; bridge pauses between words shorter than this.
const DUCK_MIN_ON_S: f32 = 0.1;
const DUCK_BRIDGE_S: f32 = 0.25;

/// Level envelope (dBFS, 10 ms hops, 50 ms centred window) of the summed trigger clips over
/// timeline samples `[a0, a1)`.
fn trigger_envelope(triggers: &[&TrackItem], a0: i64, a1: i64, sr: u32, sources: &dyn SourceProvider) -> Vec<f32> {
    let hop = ((sr as f64 * DUCK_HOP_S).round() as usize).max(1);
    let hops = ((a1 - a0).max(0) as usize).div_ceil(hop);
    let mut power = vec![0.0f64; hops];
    let chunk = hop * 100;
    let mut pos = a0;
    while pos < a1 {
        let n = ((a1 - pos) as usize).min(chunk);
        let mut sum = [vec![0.0f32; n], vec![0.0f32; n]];
        for t in triggers {
            if let Some(b) = filmcraft_render::audio::clip_signal(t, pos, n, sr, sources) {
                for c in 0..2 {
                    for (d, x) in sum[c].iter_mut().zip(&b[c]) {
                        *d += x;
                    }
                }
            }
        }
        let base = ((pos - a0) as usize) / hop;
        for (k, i) in (0..n).step_by(hop).enumerate() {
            let e = (i + hop).min(n);
            let pw: f64 = (i..e).map(|j| 0.5 * ((sum[0][j] as f64).powi(2) + (sum[1][j] as f64).powi(2))).sum::<f64>() / (e - i) as f64;
            if let Some(p) = power.get_mut(base + k) {
                *p = pw;
            }
        }
        pos += n as i64;
    }
    let h = DUCK_WINDOW_HOPS / 2;
    (0..hops)
        .map(|k| {
            let (lo, hi) = (k.saturating_sub(h), (k + h + 1).min(hops));
            let m = power[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
            (10.0 * m.max(1e-12).log10()).max(-120.0) as f32
        })
        .collect()
}

fn generate_ducking(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "essentialSound.generateDucking";
    let clips = targets(s, p);
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sr = seq.settings.sample_rate.max(1);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let all: Vec<&TrackItem> = seq.audio_tracks.iter().flat_map(|t| t.items.iter()).collect();
    let mut plans: Vec<(ClipId, Vec<(Tick, f64)>, f64)> = Vec::new();
    let mut report = Vec::new();
    for it in all.iter().copied().filter(|i| clips.contains(&i.id)) {
        let Some(st) = it.essential.as_ref().filter(|e| e.kind.has(Section::Ducking)) else { continue };
        let d = &st.ducking;
        let triggers: Vec<&TrackItem> = all
            .iter()
            .copied()
            .filter(|o| o.id != it.id && o.enabled && o.range().overlaps(&it.range()))
            .filter(|o| match o.essential.as_ref() {
                Some(e) => !e.mute && d.against.contains(&e.kind),
                None => d.against_untyped,
            })
            .collect();
        let (a0, a1) = (it.start.to_units_floor(sr as i64), it.end().to_units_floor(sr as i64));
        let env = trigger_envelope(&triggers, a0, a1, sr, &provider);
        let thr = filmcraft_audio_dsp::ducking::sensitivity_threshold_db(d.sensitivity as f32);
        let regions = filmcraft_audio_dsp::ducking::activity(&env, DUCK_HOP_S as f32, thr, DUCK_MIN_ON_S, DUCK_BRIDGE_S);
        let start_s = a0 as f64 / sr as f64;
        let end_s = a1 as f64 / sr as f64;
        let abs: Vec<(f64, f64)> = regions.iter().map(|(x, y)| (start_s + x, start_s + y)).collect();
        let lvl = it.effect("volume").and_then(|e| e.param("level"));
        let base = match lvl {
            Some(p) if p.is_animated() => p.keyframes.iter().filter_map(|k| k.value.as_f64()).fold(f64::NEG_INFINITY, f64::max),
            Some(p) => p.value.as_f64().unwrap_or(0.0),
            None => 0.0,
        };
        let kfs = filmcraft_audio_dsp::ducking::duck_keyframes(&abs, start_s, end_s, base, d.reduce_db, d.fade_s);
        let ticks: Vec<(Tick, f64)> = kfs.iter().map(|(t, v)| (Tick::from_seconds_f64(*t).clamp(it.start, it.end()), *v)).collect();
        report.push(json!({
            "clip": it.id.0,
            "triggers": triggers.iter().map(|t| t.id.0).collect::<Vec<_>>(),
            "regions": abs.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
            "keyframes": ticks.iter().map(|(t, v)| json!({"time": t.0, "seconds": t.seconds(), "levelDb": v})).collect::<Vec<_>>(),
        }));
        plans.push((it.id, ticks, base));
    }
    if plans.is_empty() {
        return Err(bad(cmd, "select Music or Ambience clips (ducking applies to those types)"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    s.edit("Generate Ducking Keyframes", |pr, _| {
        let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        for (c, ticks, base) in &plans {
            let Some((_, it)) = seq.find_item_mut(*c) else { continue };
            if let Some(e) = it.essential.as_mut() {
                e.ducking.enabled = true;
            }
            let at: Vec<(Tick, f64)> = ticks.iter().map(|(t, v)| (mt_of(it, *t), *v)).collect();
            let Some(par) = it.effect_mut("volume").and_then(|e| e.param_mut("level")) else { continue };
            par.keyframes.clear();
            par.value = ParamValue::Float(*base);
            for (mt, v) in at {
                par.put_keyframe(mt, ParamValue::Float(v));
            }
        }
        Ok(())
    })?;
    Ok(json!({"clips": report}))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("essentialSound.inspect", "Inspect Essential Sound", r#"{"clips":[id]?}"#, always, inspect, false),
        spec("essentialSound.setType", "Set Audio Type", r#"{"clips":[id]?,"type":"dialogue|music|sfx|ambience"}"#, has_seq, set_type, true),
        spec("essentialSound.clearType", "Clear Audio Type", r#"{"clips":[id]?}"#, has_seq, clear_type, true),
        spec(
            "essentialSound.set",
            "Essential Sound Setting",
            r#"{"clips":[id]?,"key":"repair.noise.on|repair.noise.amount|repair.humHz|clarity.eqPreset|creative.reverbPreset|ducking.reduceDb|volume.levelDb|mute|…","value":any,"values":{key:value}?,"begin":bool?}"#,
            has_seq,
            set,
            true,
        ),
        spec(
            "essentialSound.applyPreset",
            "Apply Sound Preset",
            r#"{"clips":[id]?,"preset":str,"type":"dialogue|music|sfx|ambience"?}"#,
            has_seq,
            apply_preset,
            true,
        ),
        spec("essentialSound.savePreset", "Save Sound Preset", r#"{"clips":[id]?,"name":str}"#, has_seq, save_preset, true),
        spec("essentialSound.deletePreset", "Delete Sound Preset", r#"{"name":str,"type":"dialogue|music|sfx|ambience"?}"#, always, delete_preset, true),
        spec("essentialSound.autoMatch", "Auto-Match Loudness", r#"{"clips":[id]?,"target":lufs?}"#, has_seq, auto_match, true),
        spec("essentialSound.generateDucking", "Generate Ducking Keyframes", r#"{"clips":[id]?}"#, has_seq, generate_ducking, true),
    ]
}

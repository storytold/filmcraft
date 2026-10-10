//! Essential Sound: types, settings → clip effects, presets, loudness auto-match, ducking, repair
//! metrics through the render path, render-vs-export identity.

use std::sync::Arc;

use filmcraft_audio_dsp::LoudnessMeter;
use filmcraft_project::{AudioType, ClipId, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::*;

const SR: u32 = 48_000;

/// Deterministic xorshift noise in [-1, 1).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// A speech-like signal: a voiced source (gliding f0 ≈ 110–160 Hz, harmonics falling 6 dB/oct up
/// to 4 kHz) under a syllable envelope (~4.5 syllables/s), only where `active(t)`.
fn speech(secs: f64, amp: f32, active: impl Fn(f64) -> bool) -> Vec<f32> {
    let n = (secs * SR as f64) as usize;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            if !active(t) {
                return 0.0;
            }
            let f0 = 135.0 + 25.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
            phase += f0 / SR as f64;
            let mut v = 0.0;
            let mut k = 1;
            while k as f64 * f0 < 4000.0 {
                v += (2.0 * std::f64::consts::PI * phase * k as f64).sin() / k as f64;
                k += 1;
            }
            let syl = 0.6 - 0.4 * (2.0 * std::f64::consts::PI * 4.5 * t).cos();
            (v * syl * 0.5) as f32 * amp
        })
        .collect()
}

fn tone(secs: f64, f: f64, amp: f32) -> Vec<f32> {
    (0..(secs * SR as f64) as usize).map(|i| (amp as f64 * (2.0 * std::f64::consts::PI * f * i as f64 / SR as f64).sin()) as f32).collect()
}

fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

/// A session with one sequence and each signal on its own audio track (A1, A2…) at `start` seconds.
fn session_with(tracks: &[(Vec<f32>, f64)]) -> (Session, Vec<ClipId>) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "es", "audio": tracks.len().max(2), "video": 1})).unwrap();
    let mut clips = Vec::new();
    for (k, (x, start)) in tracks.iter().enumerate() {
        let inter: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
        let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&inter, SR).into();
        let item = crate::commands::import_bytes(&mut s, &format!("/sig{k}.wav"), bytes, None).unwrap();
        let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": format!("A{}", k + 1), "seconds": start})).unwrap();
        clips.push(ClipId(r["clips"][0].as_u64().unwrap()));
    }
    s.execute("edit.deselectAll", json!({})).unwrap();
    (s, clips)
}

fn item(s: &Session, c: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
}

fn es_ids(s: &Session, c: ClipId) -> Vec<String> {
    item(s, c).effects.iter().filter(|e| e.essential).map(|e| e.effect.clone()).collect()
}

/// The full sequence mix for `[a, a + n)` samples, in requests of `chunk` samples.
fn mix(s: &Session, a: i64, n: usize, chunk: usize) -> [Vec<f32>; 2] {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let seq = s.active_sequence().unwrap();
    let mut out = [Vec::new(), Vec::new()];
    let mut pos = a;
    while pos < a + n as i64 {
        let m = chunk.min((a + n as i64 - pos) as usize);
        let b = filmcraft_render::audio::mix_sequence(&s.project, seq, pos, m, &provider);
        out[0].extend_from_slice(&b.channels[0]);
        out[1].extend_from_slice(&b.channels[1]);
        pos += m as i64;
    }
    out
}

fn lufs(x: &[Vec<f32>; 2]) -> f64 {
    let mut m = LoudnessMeter::new(SR as f64, 2);
    m.process(&[&x[0], &x[1]]);
    m.integrated()
}

/// The clip's own processed signal (clip gain + effects) over its whole range.
fn clip_out(s: &Session, c: ClipId) -> [Vec<f32>; 2] {
    let it = item(s, c);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let a0 = it.start.to_units_floor(SR as i64);
    let n = (it.end().to_units_floor(SR as i64) - a0) as usize;
    let mut out = [Vec::new(), Vec::new()];
    let mut pos = a0;
    while pos < a0 + n as i64 {
        let m = 4800.min((a0 + n as i64 - pos) as usize);
        let b = filmcraft_render::audio::clip_signal(&it, pos, m, SR, &provider).unwrap();
        out[0].extend_from_slice(&b[0]);
        out[1].extend_from_slice(&b[1]);
        pos += m as i64;
    }
    out
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// Amplitude of the `f` component (single-bin DFT).
fn tone_amp(x: &[f32], f: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * f * i as f64 / SR as f64;
        re += *v as f64 * w.cos();
        im += *v as f64 * w.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn secs(a: f64, b: f64) -> std::ops::Range<usize> {
    (a * SR as f64) as usize..(b * SR as f64) as usize
}

#[test]
fn commands_are_registered() {
    for id in [
        "essentialSound.inspect",
        "essentialSound.setType",
        "essentialSound.clearType",
        "essentialSound.set",
        "essentialSound.applyPreset",
        "essentialSound.savePreset",
        "essentialSound.deletePreset",
        "essentialSound.autoMatch",
        "essentialSound.generateDucking",
    ] {
        let c = commands::find(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(!c.label.is_empty() && !c.params.is_empty());
    }
    assert!(!Session::default().is_enabled("essentialSound.setType"), "needs a sequence");
}

#[test]
fn type_settings_effects_undo_redo_and_clear() {
    let (mut s, c) = session_with(&[(speech(2.0, 0.3, |_| true), 0.0)]);
    let c = c[0];
    assert!(s.execute("essentialSound.set", json!({"clips": [c.0], "key": "repair.noise.on", "value": true})).is_err(), "untyped");
    s.execute("timeline.select", json!({"clips": [c.0]})).unwrap();
    s.execute("essentialSound.setType", json!({"type": "dialogue"})).unwrap();
    assert_eq!(item(&s, c).essential.as_ref().unwrap().kind, AudioType::Dialogue);
    s.execute(
        "essentialSound.set",
        json!({"values": {"repair.noise.on": true, "repair.noise.amount": 7.0, "clarity.eq.on": true, "clarity.eqPreset": "Telephone"}}),
    )
    .unwrap();
    assert_eq!(es_ids(&s, c), ["denoise", "parametric_eq"]);
    let it = item(&s, c);
    assert_eq!(it.effect("denoise").unwrap().f64_at("amount", Tick::ZERO), 70.0);
    assert_eq!(it.effect("parametric_eq").unwrap().f64_at("low_gain", Tick::ZERO), -9.0, "Telephone at amount 5 of 10");
    // a drag (same key, no begin) is one undo step
    for v in [6.0, 5.0, 4.0] {
        s.execute("essentialSound.set", json!({"key": "repair.noise.amount", "value": v})).unwrap();
    }
    assert_eq!(item(&s, c).effect("denoise").unwrap().f64_at("amount", Tick::ZERO), 40.0);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(item(&s, c).effect("denoise").unwrap().f64_at("amount", Tick::ZERO), 70.0);
    s.execute("edit.redo", json!({})).unwrap();
    // inspect
    let v = s.execute("essentialSound.inspect", json!({})).unwrap();
    assert_eq!(v["type"], "Dialogue");
    assert_eq!(v["clips"][0]["settings"]["repair"]["noise"]["amount"], 4.0);
    assert_eq!(v["clips"][0]["effects"].as_array().unwrap().len(), 2);
    assert!(v["presets"]["Dialogue"].as_array().unwrap().len() > 3);
    assert_eq!(v["targets"]["Dialogue"], -23.0);
    // type change resets, clear removes everything
    s.execute("essentialSound.setType", json!({"type": "music"})).unwrap();
    assert!(es_ids(&s, c).is_empty());
    s.execute("essentialSound.clearType", json!({})).unwrap();
    assert!(item(&s, c).essential.is_none());
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(es_ids(&s, c), ["denoise", "parametric_eq"]);
    // bad input
    assert!(s.execute("essentialSound.set", json!({"key": "repair.nope", "value": 1})).is_err());
    assert!(s.execute("essentialSound.setType", json!({"type": "podcast"})).is_err());
}

#[test]
fn presets_apply_save_and_delete() {
    let (mut s, c) = session_with(&[(speech(2.0, 0.3, |_| true), 0.0)]);
    let c = c[0];
    s.execute("essentialSound.applyPreset", json!({"clips": [c.0], "type": "dialogue", "preset": "Podcast Voice"})).unwrap();
    assert_eq!(es_ids(&s, c), ["highpass", "deesser", "dynamics", "parametric_eq", "speech_enhance"]);
    assert_eq!(item(&s, c).essential.unwrap().preset, "Podcast Voice");
    s.execute("essentialSound.set", json!({"clips": [c.0], "key": "creative.reverb.on", "value": true})).unwrap();
    assert!(item(&s, c).essential.unwrap().preset.is_empty());
    s.execute("essentialSound.savePreset", json!({"clips": [c.0], "name": "My Voice"})).unwrap();
    assert_eq!(item(&s, c).essential.unwrap().preset, "My Voice");
    assert!(s.execute("essentialSound.savePreset", json!({"clips": [c.0], "name": "(Default)"})).is_err());
    s.execute("essentialSound.applyPreset", json!({"clips": [c.0], "preset": "(Default)"})).unwrap();
    assert!(es_ids(&s, c).is_empty());
    s.execute("essentialSound.applyPreset", json!({"clips": [c.0], "preset": "My Voice"})).unwrap();
    assert!(es_ids(&s, c).contains(&"studio_reverb".to_string()));
    let v = s.execute("essentialSound.inspect", json!({"clips": [c.0]})).unwrap();
    assert!(v["presets"]["Dialogue"].as_array().unwrap().iter().any(|p| p["name"] == "My Voice" && p["builtin"] == false));
    s.execute("essentialSound.deletePreset", json!({"name": "My Voice"})).unwrap();
    assert!(s.execute("essentialSound.deletePreset", json!({"name": "Podcast Voice"})).is_err());
    assert!(s.execute("essentialSound.applyPreset", json!({"clips": [c.0], "preset": "My Voice"})).is_err());
}

#[test]
fn auto_match_hits_the_target_loudness() {
    // quiet and loud speech, one with a repair/clarity chain, plus a preference target
    let quiet = speech(12.0, 0.05, |t| (t % 3.0) < 2.2);
    let loud = speech(12.0, 0.9, |t| (t % 2.0) < 1.5);
    for (sig, chain, target) in [(quiet.clone(), false, None), (loud, false, None), (quiet, true, Some(-16.0))] {
        let (mut s, c) = session_with(&[(sig, 0.0)]);
        let c = c[0];
        s.execute("essentialSound.setType", json!({"clips": [c.0], "type": "dialogue"})).unwrap();
        if chain {
            s.execute(
                "essentialSound.set",
                json!({"clips": [c.0], "values": {"repair.rumble.on": true, "repair.noise.on": true, "clarity.dynamics.on": true, "clarity.eq.on": true}}),
            )
            .unwrap();
        }
        if let Some(t) = target {
            s.execute("prefs.set", json!({"key": "audio.dialogueTargetLufs", "value": t})).unwrap();
        }
        let want = target.unwrap_or(-23.0);
        let r = s.execute("essentialSound.autoMatch", json!({"clips": [c.0]})).unwrap();
        let got = lufs(&mix(&s, 0, 12 * SR as usize, 48_000));
        println!("auto-match: measured {:.2} LUFS → mix {got:.3} LUFS (target {want}), chain {chain}", r["clips"][0]["measuredLufs"].as_f64().unwrap());
        assert!((got - want).abs() <= 0.5, "mix {got} LUFS, target {want}");
        let st = item(&s, c).essential.unwrap();
        assert_eq!(st.loudness.target_lufs, Some(want));
        assert!((item(&s, c).gain_db - st.loudness.gain_db).abs() < 1e-9, "match gain is clip gain");
        // matching again is stable; switching Loudness off removes the gain
        s.execute("essentialSound.autoMatch", json!({"clips": [c.0]})).unwrap();
        assert!((lufs(&mix(&s, 0, 12 * SR as usize, 48_000)) - want).abs() <= 0.5);
        s.execute("essentialSound.set", json!({"clips": [c.0], "key": "loudness.enabled", "value": false})).unwrap();
        assert!(item(&s, c).gain_db.abs() < 1e-9);
    }
}

/// #296: a mono file measured ~3 LU too loud. The same speech imported as a 1-channel WAV and as
/// a dual-mono stereo WAV must measure 3.01 LU apart (the stereo pair carries twice the energy),
/// the mono clip must agree with a one-channel BS.1770 meter over the file (what ffmpeg's
/// `ebur128` reports), and a mono pick (Modify ▸ Audio Channels) of the stereo file counts as mono.
#[test]
fn auto_match_measures_a_mono_clip_as_one_channel() {
    let sig = speech(12.0, 0.3, |t| (t % 3.0) < 2.2);
    let (mut s, c) = session_with(&[(sig.clone(), 0.0)]);
    let stereo = c[0];
    let mono_wav: Arc<[u8]> = filmcraft_media::wav::write_wav16(&sig, 1, SR).into();
    let mono_item = crate::commands::import_bytes(&mut s, "/mono.wav", mono_wav, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": mono_item.0, "audioTrack": "A2", "seconds": 0.0})).unwrap();
    let mono = ClipId(r["clips"][0].as_u64().unwrap());
    for c in [stereo, mono] {
        s.execute("essentialSound.setType", json!({"clips": [c.0], "type": "dialogue"})).unwrap();
    }
    let measured = |s: &mut Session, c: ClipId| {
        let r = s.execute("essentialSound.autoMatch", json!({"clips": [c.0], "target": -23.0})).unwrap();
        r["clips"][0]["measuredLufs"].as_f64().unwrap()
    };
    let (two, one) = (measured(&mut s, stereo), measured(&mut s, mono));
    let mut file = LoudnessMeter::new(SR as f64, 1);
    file.process(&[&sig]);
    let want = file.integrated();
    println!("stereo (dual mono) {two:.3} LUFS, mono {one:.3} LUFS, mono file {want:.3} LUFS");
    assert!((one - want).abs() <= 0.1, "mono clip {one} LUFS, the file reads {want} LUFS");
    assert!((two - one - 3.0103).abs() <= 0.1, "dual mono {two} LUFS should read 3.01 LU above mono {one} LUFS");
    let st = item(&s, mono).essential.unwrap();
    assert!((st.loudness.gain_db - (-23.0 - want)).abs() <= 0.1, "match gain {} dB for {want} LUFS", st.loudness.gain_db);
    // one picked channel of the stereo file plays on both sides: measured as mono too
    s.execute("clip.audioChannels", json!({"clips": [stereo.0], "channels": [0]})).unwrap();
    let picked = measured(&mut s, stereo);
    assert!((picked - want).abs() <= 0.1, "mono pick {picked} LUFS, the file reads {want} LUFS");
}

#[test]
fn ducking_generates_volume_keyframes_under_dialogue() {
    // dialogue in [2, 4] and [6, 7.5] s; music for 10 s
    let dlg = speech(10.0, 0.4, |t| (2.0..4.0).contains(&t) || (6.0..7.5).contains(&t));
    let music = add(&tone(10.0, 220.0, 0.2), &tone(10.0, 330.0, 0.1));
    let (mut s, c) = session_with(&[(dlg, 0.0), (music, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    s.execute("essentialSound.setType", json!({"clips": [c[1].0], "type": "music"})).unwrap();
    s.execute("essentialSound.set", json!({"clips": [c[1].0], "values": {"ducking.enabled": true, "ducking.reduceDb": -15.0, "ducking.fadeS": 0.8}})).unwrap();
    let r = s.execute("essentialSound.generateDucking", json!({"clips": [c[1].0]})).unwrap();
    let kf = r["clips"][0]["keyframes"].as_array().unwrap();
    let got: Vec<(f64, f64)> = kf.iter().map(|k| (k["seconds"].as_f64().unwrap(), k["levelDb"].as_f64().unwrap())).collect();
    println!("ducking keyframes: {got:?}");
    let want = [(1.2, 0.0), (2.0, -15.0), (4.0, -15.0), (4.8, 0.0), (5.2, 0.0), (6.0, -15.0), (7.5, -15.0), (8.3, 0.0)];
    assert_eq!(got.len(), want.len(), "{got:?}");
    for ((t, v), (wt, wv)) in got.iter().zip(want) {
        assert!((t - wt).abs() <= 0.06, "keyframe at {t} s, expected {wt} s");
        assert!((v - wv).abs() < 1e-9);
    }
    // the keyframes are on the music clip's Volume level, and the mix follows them
    let it = item(&s, c[1]);
    let lvl = it.effect("volume").unwrap().param("level").unwrap();
    assert_eq!(lvl.keyframes.len(), 8);
    assert_eq!(lvl.f64_at(it.source_time_at(Tick::from_seconds_f64(3.0))), -15.0);
    assert_eq!(lvl.f64_at(it.source_time_at(Tick::from_seconds_f64(5.0))), 0.0);
    // listen to the music alone
    s.execute("timeline.select", json!({"clips": [c[0].0]})).unwrap();
    s.execute("clip.enable", json!({"clips": [c[0].0]})).unwrap();
    let m = mix(&s, 0, 10 * SR as usize, 48_000);
    let during = tone_amp(&m[0][secs(2.5, 3.5)], 220.0);
    let between = tone_amp(&m[0][secs(0.2, 1.0)], 220.0);
    println!("music 220 Hz: {:.2} dB ducked vs open", db(during / between));
    assert!((db(during / between) + 15.0).abs() < 0.3);
    // regenerate after changing the depth: replaces the keyframes, no stacking
    s.execute("clip.enable", json!({"clips": [c[0].0]})).unwrap();
    s.execute("essentialSound.set", json!({"clips": [c[1].0], "key": "ducking.reduceDb", "value": -6.0})).unwrap();
    s.execute("essentialSound.generateDucking", json!({"clips": [c[1].0]})).unwrap();
    let lvl = item(&s, c[1]).effects.iter().find(|e| e.effect == "volume").unwrap().params["level"].clone();
    assert_eq!(lvl.keyframes.len(), 8);
    assert!(lvl.keyframes.iter().all(|k| matches!(k.value.as_f64(), Some(v) if v == 0.0 || v == -6.0)));
    // undo restores the previous keyframes
    s.execute("edit.undo", json!({})).unwrap();
    let lvl = item(&s, c[1]).effects.iter().find(|e| e.effect == "volume").unwrap().params["level"].clone();
    assert!(lvl.keyframes.iter().any(|k| k.value.as_f64() == Some(-15.0)));
    // nothing to duck against → no keyframes
    assert!(s.execute("essentialSound.generateDucking", json!({"clips": [c[0].0]})).is_err(), "dialogue does not duck");
}

/// Run `sig` through one Dialogue repair setting and return (input, output) over 1…len−0.5 s.
fn repair(sig: Vec<f32>, values: Value) -> (Vec<f32>, Vec<f32>) {
    let (mut s, c) = session_with(&[(sig.clone(), 0.0)]);
    let c = c[0];
    s.execute("essentialSound.setType", json!({"clips": [c.0], "type": "dialogue"})).unwrap();
    s.execute("essentialSound.set", json!({"clips": [c.0], "values": values})).unwrap();
    let out = clip_out(&s, c);
    (sig, out[0].clone())
}

#[test]
fn repair_stages_improve_their_metrics() {
    let n_s = 6.0;
    let voice = speech(n_s, 0.3, |t| (t % 1.0) < 0.6);
    // DeHum: 60 Hz hum + harmonics
    let hum = add(&add(&tone(n_s, 60.0, 0.05), &tone(n_s, 180.0, 0.03)), &voice);
    let (x, y) = repair(hum, json!({"repair.dehum.on": true, "repair.dehum.amount": 5.0, "repair.humHz": 60}));
    let r = secs(2.0, 5.5);
    let (h0, h1) = (tone_amp(&x[r.clone()], 60.0), tone_amp(&y[r.clone()], 60.0));
    println!("DeHum: 60 Hz {:.1} dB", db(h1 / h0));
    assert!(db(h1 / h0) < -20.0);
    // Reduce Rumble: 25 Hz rumble
    let rum = add(&tone(n_s, 25.0, 0.1), &voice);
    let (x, y) = repair(rum, json!({"repair.rumble.on": true, "repair.rumble.amount": 5.0}));
    let (r0, r1) = (tone_amp(&x[r.clone()], 25.0), tone_amp(&y[r.clone()], 25.0));
    println!("Reduce Rumble: 25 Hz {:.1} dB", db(r1 / r0));
    assert!(db(r1 / r0) < -15.0);
    // Reduce Noise: white noise floor, measured in the pauses (latency-aligned by the render path)
    let mut g = Rng(0x1234_5678);
    let noise: Vec<f32> = (0..voice.len()).map(|_| g.next() * 0.01).collect();
    let (x, y) = repair(add(&voice, &noise), json!({"repair.noise.on": true, "repair.noise.amount": 6.0}));
    let pause = secs(4.7, 4.95);
    let nr = db(rms(&y[pause.clone()]) / rms(&x[pause.clone()]));
    let speech_change = db(rms(&y[secs(4.1, 4.5)]) / rms(&x[secs(4.1, 4.5)]));
    println!("Reduce Noise: floor {nr:.1} dB, speech {speech_change:.2} dB");
    assert!(nr < -6.0);
    assert!(speech_change.abs() < 2.0);
}

#[test]
fn deess_and_dereverb_improve_their_metrics() {
    let n_s = 6.0;
    let voice = speech(n_s, 0.3, |t| (t % 1.0) < 0.6);
    // sibilants: 5–10 kHz noise bursts (2nd-difference of white noise ≈ high-passed) at the
    // start of every syllable group
    let mut g = Rng(99);
    let mut prev = (0.0f32, 0.0f32);
    let sib: Vec<f32> = (0..voice.len())
        .map(|i| {
            let w = g.next();
            let hp = w - 2.0 * prev.0 + prev.1;
            prev = (w, prev.0);
            let t = i as f64 / SR as f64;
            if (t % 1.0) < 0.12 { hp * 0.12 } else { 0.0 }
        })
        .collect();
    let (x, y) = repair(add(&voice, &sib), json!({"repair.deess.on": true, "repair.deess.amount": 8.0}));
    let hi = |v: &[f32]| {
        // energy above ~5 kHz: first difference twice (a crude high-pass), RMS
        let d: Vec<f32> = v.windows(3).map(|w| w[2] - 2.0 * w[1] + w[0]).collect();
        rms(&d)
    };
    let burst = secs(4.0, 4.1);
    let red = db(hi(&y[burst.clone()]) / hi(&x[burst.clone()]));
    let body = db(tone_amp(&y[secs(4.2, 4.55)], 135.0).max(1e-9) / tone_amp(&x[secs(4.2, 4.55)], 135.0).max(1e-9));
    println!("DeEss: sibilance {red:.1} dB, voice body {body:.2} dB");
    assert!(red < -4.0, "sibilance only {red} dB");
    // Reduce Reverb: dry bursts convolved with an exponentially decaying noise tail (RT60 0.8 s)
    let dry = speech(n_s, 0.3, |t| (t % 0.5) < 0.1);
    let mut g = Rng(7);
    let ir_len = (0.8 * SR as f64) as usize;
    let ir: Vec<f32> = (0..ir_len).map(|i| if i == 0 { 1.0 } else { g.next() * 0.08 * (-6.9 * i as f64 / ir_len as f64).exp() as f32 }).collect();
    // sparse convolution (decimated IR taps are enough for a diffuse tail)
    let mut wet = dry.clone();
    for (k, h) in ir.iter().enumerate().skip(1).step_by(7) {
        for i in k..dry.len() {
            wet[i] += dry[i - k] * h * 7.0f32.sqrt();
        }
    }
    let (x, y) = repair(wet, json!({"repair.reverb.on": true, "repair.reverb.amount": 10.0}));
    let ratio = |v: &[f32]| {
        // tail energy (gaps 0.2–0.45 s after each burst) relative to burst energy, over 3…5.5 s
        let (mut b, mut t) = (0.0, 0.0);
        for k in 6..11 {
            let s0 = k as f64 * 0.5;
            b += rms(&v[secs(s0 + 0.01, s0 + 0.09)]).powi(2);
            t += rms(&v[secs(s0 + 0.2, s0 + 0.45)]).powi(2);
        }
        10.0 * (t / b).log10()
    };
    let (rx, ry) = (ratio(&x), ratio(&y));
    println!("Reduce Reverb: tail/burst {rx:.1} dB → {ry:.1} dB");
    assert!(ry < rx - 3.0);
}

#[test]
fn rendered_mix_is_identical_however_requests_are_cut_and_matches_export() {
    let mut g = Rng(42);
    let voice: Vec<f32> = speech(4.0, 0.3, |t| (t % 1.0) < 0.6).iter().map(|v| v + g.next() * 0.005).collect();
    let (mut s, c) = session_with(&[(voice, 0.5)]);
    s.execute("essentialSound.applyPreset", json!({"clips": [c[0].0], "type": "dialogue", "preset": "Podcast Voice"})).unwrap();
    s.execute(
        "essentialSound.set",
        json!({"clips": [c[0].0], "values": {"repair.noise.on": true, "repair.reverb.on": true, "repair.dehum.on": true, "creative.reverb.on": true}}),
    )
    .unwrap();
    s.execute("essentialSound.autoMatch", json!({"clips": [c[0].0]})).unwrap();
    let n = 5 * SR as usize;
    let whole = mix(&s, 0, n, n);
    for chunk in [512usize, 1000, 4096 + 7] {
        let cut = mix(&s, 0, n, chunk);
        let d = whole[0].iter().chain(&whole[1]).zip(cut[0].iter().chain(&cut[1])).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(d <= 1e-6, "chunk {chunk}: max diff {d}");
    }
    assert!(rms(&whole[0]) > 0.01);
    // export (WAV, 16-bit) equals the mix
    let dir = std::env::temp_dir().join(format!("fc-es-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("es.wav");
    s.execute("file.exportMedia", json!({"path": path.to_string_lossy(), "format": "wav", "wait": true})).unwrap();
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_media::open_bytes("es.wav", bytes, &[]).unwrap();
    let back = src.audio(0, n, SR).unwrap();
    let d = (0..n).map(|i| (back.channels[0][i] - whole[0][i]).abs().max((back.channels[1][i] - whole[1][i]).abs())).fold(0.0f32, f32::max);
    println!("export vs render: max diff {d:e}");
    assert!(d <= 2.0 / 32768.0, "export differs from render by {d}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `cargo test --release -p filmcraft-engine essential_sound_tests::perf -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_full_dialogue_chain_realtime_factor() {
    let secs_len = 60.0;
    let mut g = Rng(5);
    let voice: Vec<f32> = speech(secs_len, 0.3, |t| (t % 1.0) < 0.6).iter().map(|v| v + g.next() * 0.005).collect();
    let (mut s, c) = session_with(&[(voice, 0.0)]);
    s.execute("essentialSound.setType", json!({"clips": [c[0].0], "type": "dialogue"})).unwrap();
    s.execute(
        "essentialSound.set",
        json!({"clips": [c[0].0], "values": {
            "repair.noise.on": true, "repair.rumble.on": true, "repair.dehum.on": true, "repair.deess.on": true, "repair.reverb.on": true,
            "clarity.dynamics.on": true, "clarity.eq.on": true, "clarity.enhance.on": true, "creative.reverb.on": true}}),
    )
    .unwrap();
    assert_eq!(es_ids(&s, c[0]).len(), 9);
    let t0 = std::time::Instant::now();
    let out = clip_out(&s, c[0]);
    let el = t0.elapsed().as_secs_f64();
    assert!(rms(&out[0]) > 0.0);
    println!("full Dialogue chain (9 effects), 60 s stereo 48 kHz: {el:.2} s → {:.1}× realtime", secs_len / el);
}

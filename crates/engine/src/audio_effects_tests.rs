//! The Premiere audio-effect set through engine commands: listed with folders by
//! `effects.list`, applicable as clip effects (`effects.apply`) and track effects
//! (`mixer.addInsert`), parameters settable by id, and the sequence still renders.

use super::*;
use filmcraft_project::ParamValue;
use filmcraft_project::effect::PREMIERE_AUDIO_EFFECTS;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn a1_clip(s: &Session) -> u64 {
    s.active_sequence().unwrap().audio_tracks[0].items[0].id.0
}

fn render(s: &Session, frames: usize) -> filmcraft_frame::AudioBuffer {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), 0, frames, &provider)
}

#[test]
fn effects_list_has_every_audio_effect_in_its_folder() {
    let mut s = demo();
    let list = s.execute("effects.list", json!({})).unwrap();
    let arr = list.as_array().unwrap();
    for id in PREMIERE_AUDIO_EFFECTS {
        let e = arr.iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("{id} not listed"));
        assert_eq!(e["kind"], "Audio", "{id}");
        assert_eq!(e["category"][0], "Audio Effects", "{id}");
    }
    let folder = |id: &str| arr.iter().find(|e| e["id"] == id).unwrap()["category"][1].clone();
    assert_eq!(folder("tube_compressor"), "Amplitude and Compression");
    assert_eq!(folder("chorus_flanger"), "Modulation");
    assert_eq!(folder("binauralizer"), "Special");
    assert!(folder("volume_a").is_null(), "loose item");
}

#[test]
fn every_audio_effect_applies_to_clips_and_tracks_and_renders() {
    let mut s = demo();
    let clip = a1_clip(&s);
    for id in PREMIERE_AUDIO_EFFECTS {
        let n0 = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.effects.len();
        s.execute("effects.apply", json!({"clips": [clip], "effect": id})).unwrap_or_else(|e| panic!("{id}: {e}"));
        let it = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone();
        assert_eq!(it.effects.len(), n0 + 1, "{id}");
        assert_eq!(it.effects.last().unwrap().effect, id);
        assert!(filmcraft_render::audio_fx::supported(id), "{id}");
        s.execute("mixer.addInsert", json!({"strip": "A1", "effect": id})).unwrap_or_else(|e| panic!("{id}: {e}"));
        let b = render(&s, 2400);
        assert_eq!(b.frames(), 2400);
        assert!(b.channels.iter().flatten().all(|v| v.is_finite()), "{id}: non-finite mix");
        s.execute("mixer.removeInsert", json!({"strip": "A1", "slot": 0})).unwrap();
        s.execute("effects.remove", json!({"clip": clip, "index": n0})).unwrap();
    }
}

#[test]
fn parameters_are_set_by_id_and_change_the_sound() {
    let mut s = demo();
    let clip = a1_clip(&s);
    let before = render(&s, 4800);
    let peak = |b: &filmcraft_frame::AudioBuffer| b.channels.iter().flatten().fold(0.0f32, |a, v| a.max(v.abs()));
    assert!(peak(&before) > 1e-3, "the demo has audio on A1");
    // Mute (a loose item) silences the clip.
    s.execute("effects.apply", json!({"clips": [clip], "effect": "Mute"})).unwrap();
    let it = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone();
    let idx = it.effects.iter().position(|e| e.effect == "mute").unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": idx, "param": "mute", "value": true})).unwrap();
    let it = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone();
    assert_eq!(it.effects[idx].param("mute").unwrap().value, ParamValue::Bool(true));
    // the clip's own signal (other tracks of the demo keep playing)
    let it = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let a0 = it.start.to_units_floor(48000);
    let sig = filmcraft_render::audio::clip_signal(&it, a0, 4800, 48000, &provider).unwrap();
    let pk = sig.iter().flatten().fold(0.0f32, |a, v| a.max(v.abs()));
    assert!(pk < 1e-6, "muted clip peak {pk}");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    // A track-level Graphic Equalizer, its master gain set through mixer.setInsert (A1 soloed so
    // only it is heard).
    s.execute("mixer.setStrip", json!({"strip": "A1", "solo": true})).unwrap();
    let before = render(&s, 4800);
    s.execute("mixer.addInsert", json!({"strip": "A1", "effect": "graphic_eq_30"})).unwrap();
    s.execute("mixer.setInsert", json!({"strip": "A1", "slot": 0, "params": {"gain": -12.0}})).unwrap();
    let quieter = render(&s, 4800);
    let rms = |b: &filmcraft_frame::AudioBuffer| {
        (b.channels.iter().flatten().map(|v| (*v as f64).powi(2)).sum::<f64>() / (b.frames() * b.channel_count()) as f64).sqrt()
    };
    let d = 20.0 * (rms(&quieter) / rms(&before)).log10();
    assert!((d + 12.0).abs() < 1.0, "master gain −12 dB → {d}");
}

#[test]
fn studio_reverb_track_insert_changes_the_rendered_audio() {
    let mut s = demo();
    let before = render(&s, 4800);
    s.execute("mixer.addInsert", json!({"strip": "A1", "effect": "studio_reverb"})).unwrap();
    s.execute("mixer.setInsert", json!({"strip": "A1", "slot": 0, "params": {"dry": 0.0, "wet": 100.0}})).unwrap();
    let after = render(&s, 4800);

    let difference =
        before.channels.iter().flatten().zip(after.channels.iter().flatten()).map(|(before, after)| f64::from((*before - *after).abs())).sum::<f64>();
    assert!(difference > 1.0, "the track reverb should change the audible mix, difference={difference}");
}

#[test]
fn shared_names_resolve_by_track_kind() {
    let mut s = demo();
    let clip = a1_clip(&s);
    for (name, id) in [("Invert", "invert_a"), ("Volume", "volume_a"), ("Channel Volume", "channel_volume_a"), ("Balance", "balance_a")] {
        s.execute("effects.apply", json!({"clips": [clip], "effect": name})).unwrap();
        let it = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone();
        assert_eq!(it.effects.last().unwrap().effect, id, "{name}");
    }
    // On a video clip "Invert" is still the video effect.
    let v = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    s.execute("effects.apply", json!({"clips": [v], "effect": "Invert"})).unwrap();
    let it = s.active_sequence().unwrap().find_item(ClipId(v)).unwrap().1.clone();
    assert!(it.effects.iter().any(|e| e.effect == "invert"));
}

/// #696: applying an audio effect to the video side of an A/V pair lands on the linked audio.
#[test]
fn audio_effect_on_video_clip_applies_to_linked_audio() {
    let mut s = demo();
    let seq = s.active_sequence().unwrap();
    let v = seq.video_tracks[0].items[0].clone();
    let a = seq.audio_tracks[0].items.iter().find(|i| i.link == v.link).expect("linked audio").clone();
    let n0 = a.effects.len();
    s.execute("effects.apply", json!({"clips": [v.id.0], "effect": "parametric_eq"})).unwrap();
    let a = s.active_sequence().unwrap().find_item(a.id).unwrap().1.clone();
    let v = s.active_sequence().unwrap().find_item(v.id).unwrap().1.clone();
    assert_eq!(a.effects.len(), n0 + 1);
    assert_eq!(a.effects.last().unwrap().effect, "parametric_eq");
    assert!(v.effects.iter().all(|e| e.effect != "parametric_eq"), "EQ stays off the picture clip");
}

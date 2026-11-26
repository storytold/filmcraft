//! `sequence.review` has to find the mistakes an edit introduces: a jump in the same shot, black
//! left by a lifted clip, a cut inside a word.

use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;

fn review(s: &mut Session, p: Value) -> Value {
    s.execute("sequence.review", p).unwrap()
}

fn kinds(r: &Value) -> Vec<String> {
    r["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap().to_string()).collect()
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    // sync lock off, so the demo's music bed doesn't block ripple edits
    let tracks: Vec<u64> = s.active_sequence().unwrap().all_tracks().map(|t| t.id.0).collect();
    for t in tracks {
        s.execute("timeline.setTrack", json!({"track": t, "syncLock": false})).unwrap();
    }
    s
}

#[test]
fn removing_the_middle_of_a_shot_is_reported_as_a_jump_cut_with_the_change() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let clean = review(&mut s, json!({}));
    assert!(!kinds(&clean).contains(&"jumpCut".to_string()), "{clean}");
    let shot = s.execute("sequence.pacing", json!({})).unwrap()["longest"].clone();
    let clip = shot["clip"].as_u64().unwrap();
    let (a, b) = (shot["start"].as_i64().unwrap(), shot["end"].as_i64().unwrap());
    let t1 = rate.tick_of(rate.frame_at(Tick(a + (b - a) / 3)));
    let t2 = rate.tick_of(rate.frame_at(Tick(a + 2 * (b - a) / 3)));
    s.execute("timeline.razor", json!({"time": t2.0, "clip": clip})).unwrap();
    s.execute("timeline.razor", json!({"time": t1.0, "clip": clip})).unwrap();
    let middle = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| t.items.iter()).find(|c| c.start == t1).unwrap().id.0;
    s.execute("edit.rippleDelete", json!({"clips": [middle]})).unwrap();

    let r = review(&mut s, json!({"againstUndo": 1}));
    let jump = r["issues"].as_array().unwrap().iter().find(|i| i["kind"] == "jumpCut").unwrap_or_else(|| panic!("no jump cut: {r}"));
    assert_eq!(jump["time"].as_i64(), Some(t1.0), "the jump is where the pieces now meet");
    let num = |side: &str, k: &str| r[side][k].as_f64().unwrap_or_else(|| panic!("{side}.{k} missing: {r}"));
    assert!(num("after", "pictureSeconds") < num("before", "pictureSeconds"), "{r}");
    assert!(num("after", "shotCount") > num("before", "shotCount"), "{r}");
}

#[test]
fn a_lifted_clip_leaves_a_black_gap_error() {
    let mut s = demo();
    assert_eq!(review(&mut s, json!({}))["errors"], 0, "the demo has no errors to begin with");
    // a V1 clip with nothing above it, followed by more picture
    let gap_clip = {
        let q = s.active_sequence().unwrap();
        let clips = crate::pacing::picture_clips(&s.project, q);
        let segs = crate::pacing::segments(&s.project, q);
        segs[..segs.len() - 1].iter().filter_map(|g| g.clip).find(|c| clips.iter().all(|(i, x)| x.id != *c || *i == 0)).unwrap().0
    };
    s.execute("edit.clear", json!({"clips": [gap_clip]})).unwrap();
    let r = review(&mut s, json!({}));
    assert!(r["errors"].as_u64().unwrap() > 0, "{r}");
    assert!(kinds(&r).contains(&"gap".to_string()), "{r}");
}

#[test]
fn a_razor_inside_a_word_is_a_mid_word_cut_with_a_boundary_to_trim_to() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let (clip, item, start, src_in) = {
        let it = &s.active_sequence().unwrap().audio_tracks[0].items[0];
        (it.id.0, it.item.0, it.start, it.source_in)
    };
    let sec = |x: f64| Tick::from_seconds_f64(x);
    // "hello" from 1.0 s to 1.6 s into the clip
    let (w0, w1) = (src_in + sec(1.0), src_in + sec(1.6));
    s.execute(
        "transcript.set",
        json!({"item": item, "transcript": {"language": "en", "speakers": [{"name": "A"}], "words": [{"text": "hello", "start": w0.0, "end": w1.0}]}}),
    )
    .unwrap();
    assert!(!kinds(&review(&mut s, json!({}))).contains(&"midWord".to_string()));
    let cut = rate.tick_of(rate.frame_at(start + sec(1.3)));
    s.execute("timeline.razor", json!({"time": cut.0, "clip": clip})).unwrap();
    // a razor alone plays on unchanged: no word is cut yet
    assert!(!kinds(&review(&mut s, json!({}))).contains(&"midWord".to_string()), "a through edit cuts no word");
    // lifting what follows the cut leaves the first piece ending inside the word
    let after_cut = s.active_sequence().unwrap().audio_tracks[0].items.iter().find(|c| c.start == cut).unwrap().id.0;
    s.execute("edit.clear", json!({"clips": [after_cut]})).unwrap();
    let r = review(&mut s, json!({}));
    let mid: Vec<&Value> = r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "midWord").collect();
    assert!(!mid.is_empty(), "{r}");
    assert!(mid.iter().all(|i| i["seconds"].as_f64() == Some(cut.seconds())), "{r}");
    // the suggested trim lands on a word edge, in sequence time
    let edges = [(start + sec(1.0)).seconds(), (start + sec(1.6)).seconds()];
    for i in &mid {
        let fix = i["fix"].as_str().unwrap();
        assert!(edges.iter().any(|e| fix.contains(&format!("{e:.3}"))), "{fix} names neither word edge {edges:?}");
    }
}

#[test]
fn media_made_offline_is_reported_once_and_only_on_enabled_tracks() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    let path = tmp_dir("review-offline").join("shot.mov");
    make_movie(&path, filmcraft_media::DemoScene::Forest, 160, 90, 24);
    let (mut s, items, _) = session_with(&[&path]);
    assert!(!kinds(&review(&mut s, json!({}))).contains(&"offlineMedia".to_string()));
    s.execute("media.makeOffline", json!({"items": [items[0].0]})).unwrap();
    let r = review(&mut s, json!({}));
    let offline: Vec<&Value> = r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "offlineMedia").collect();
    assert_eq!(offline.len(), 1, "one issue per offline item, not per clip or track: {r}");
    assert_eq!(offline[0]["severity"], "error");
    // a disabled track doesn't render, so its offline clips don't matter
    let tracks: Vec<u64> = s.active_sequence().unwrap().all_tracks().map(|t| t.id.0).collect();
    for t in tracks {
        s.execute("timeline.setTrack", json!({"track": t, "enabled": false})).unwrap();
    }
    let r = review(&mut s, json!({}));
    assert!(!kinds(&r).contains(&"offlineMedia".to_string()), "{r}");
}

/// A click is judged from the edited clips' own sound: quiet clips cut under loud music on
/// another track are not a pop, the same cuts with loud clips are.
#[test]
fn audio_pops_come_from_the_edited_sound_not_other_tracks() {
    let mut s = demo();
    let (a1, edges): (Vec<u64>, Vec<i64>) = {
        let q = s.active_sequence().unwrap();
        let t = &q.audio_tracks[0];
        let end = q.duration();
        (t.items.iter().map(|c| c.id.0).collect(), t.items.iter().flat_map(|c| [c.start, c.end()]).filter(|x| x.0 > 0 && *x < end).map(|x| x.0).collect())
    };
    let pops_on_a1 =
        |r: &Value| r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "audioPop" && edges.contains(&i["time"].as_i64().unwrap())).count();
    let loud = review(&mut s, json!({}));
    assert!(pops_on_a1(&loud) > 0, "the demo's hard cuts on A1 are loud: {loud}");
    s.execute("clip.audioGain", json!({"clips": a1, "mode": "set", "db": -96.0})).unwrap();
    let quiet = review(&mut s, json!({}));
    assert_eq!(pops_on_a1(&quiet), 0, "silent A1 clips under the music bed: {quiet}");
}

/// What a clip plays at a cut counts, not its raw media: a Volume effect at -96 dB or Essential
/// Sound ▸ Mute silences it, so its cuts are not pops.
#[test]
fn volume_and_essential_mute_silence_a_clips_cuts() {
    for silence in ["volume", "mute"] {
        let mut s = demo();
        let (seq, edges) = {
            let q = s.active_sequence().unwrap();
            let end = q.duration();
            (
                s.state.active_sequence.unwrap(),
                q.audio_tracks[0].items.iter().flat_map(|c| [c.start, c.end()]).filter(|x| x.0 > 0 && *x < end).map(|x| x.0).collect::<Vec<_>>(),
            )
        };
        let pops_on_a1 =
            |r: &Value| r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "audioPop" && edges.contains(&i["time"].as_i64().unwrap())).count();
        assert!(pops_on_a1(&review(&mut s, json!({}))) > 0, "the demo's A1 cuts are loud");
        if silence == "volume" {
            let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap();
            for c in &mut q.audio_tracks[0].items {
                let fx = c.effects.iter_mut().find(|e| e.effect == "volume").expect("audio clips carry Volume");
                fx.params.get_mut("level").unwrap().value = filmcraft_project::ParamValue::Float(-96.0);
            }
        } else {
            let clips: Vec<u64> = s.active_sequence().unwrap().audio_tracks[0].items.iter().map(|c| c.id.0).collect();
            s.execute("essentialSound.setType", json!({"clips": clips, "type": "dialogue"})).unwrap();
            s.execute("essentialSound.set", json!({"clips": clips, "key": "mute", "value": true})).unwrap();
        }
        let r = review(&mut s, json!({}));
        assert_eq!(pops_on_a1(&r), 0, "{silence}: {r}");
    }
}

/// Volume keyframes are keyed in media time: a trimmed clip's edge reads the level at its own
/// media time, not at the start of its media.
#[test]
fn volume_keyframes_are_read_at_the_edges_media_time() {
    use filmcraft_project::{Interpolation, Keyframe, ParamValue};
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let (edges, ids): (Vec<i64>, Vec<u64>) = {
        let q = s.active_sequence().unwrap();
        let end = q.duration();
        let items = &q.audio_tracks[0].items;
        (items.iter().flat_map(|c| [c.start, c.end()]).filter(|x| x.0 > 0 && *x < end).map(|x| x.0).collect(), items.iter().map(|c| c.id.0).collect())
    };
    let pops = |r: &Value| r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "audioPop" && edges.contains(&i["time"].as_i64().unwrap())).count();
    assert!(pops(&review(&mut s, json!({}))) > 0);
    // Volume at 0 dB before the clip's media In point, -96 dB from it: silent wherever the clip plays
    let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap();
    for c in q.audio_tracks[0].items.iter_mut().filter(|c| ids.contains(&c.id.0)) {
        let at = c.source_in;
        let fx = c.effects.iter_mut().find(|e| e.effect == "volume").unwrap();
        let level = fx.params.get_mut("level").unwrap();
        let key = |time: Tick, v: f64| {
            let mut k = Keyframe::new(time, ParamValue::Float(v));
            k.interp = Interpolation::Hold;
            k
        };
        level.keyframes = vec![key(Tick::ZERO, 0.0), key(at, -96.0)];
    }
    let r = review(&mut s, json!({}));
    assert_eq!(pops(&r), 0, "{r}");
}

/// The track's fader is part of what a cut sounds like: A1 pulled to -96 dB has no audible pops.
#[test]
fn a_tracks_volume_counts_for_pops() {
    let mut s = demo();
    let (a1, edges) = {
        let q = s.active_sequence().unwrap();
        let end = q.duration();
        let t = &q.audio_tracks[0];
        (t.id.0, t.items.iter().flat_map(|c| [c.start, c.end()]).filter(|x| x.0 > 0 && *x < end).map(|x| x.0).collect::<Vec<_>>())
    };
    let pops = |r: &Value| r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "audioPop" && edges.contains(&i["time"].as_i64().unwrap())).count();
    assert!(pops(&review(&mut s, json!({}))) > 0);
    s.execute("timeline.setTrack", json!({"track": a1, "volumeDb": -96.0})).unwrap();
    let r = review(&mut s, json!({}));
    assert_eq!(pops(&r), 0, "{r}");
}

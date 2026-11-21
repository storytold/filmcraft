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
    assert!(r["after"]["pictureSeconds"].as_f64() < r["before"]["pictureSeconds"].as_f64(), "{r}");
    assert!(r["after"]["shotCount"].as_u64() > r["before"]["shotCount"].as_u64(), "{r}");
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
    assert!(r["errors"].as_u64() > Some(0), "{r}");
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

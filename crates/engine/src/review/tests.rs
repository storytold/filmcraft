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
        let segs = crate::pacing::segments(&s.project, q);
        segs[..segs.len() - 1].iter().filter_map(|g| g.clip).find(|c| q.video_tracks[0].item(*c).is_some()).unwrap().0
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

/// The black that overlapping clips leave (#5167) is a gap in the review too: it lies before the
/// end of the clip that is hidden there.
#[test]
fn black_left_by_overlapping_clips_is_a_gap() {
    use filmcraft_project::{ClipId, ItemId, TrackKind};
    use filmcraft_time::TimeRange;
    let mut s = demo();
    let sec = Tick::from_seconds_f64;
    let item = |name: &str| s.project.items.iter().find(|(_, i)| i.name == name).map(|(id, _)| *id).unwrap();
    let (city, desert): (ItemId, ItemId) = (item("City_Night_Drive.mp4"), item("Desert_Dunes.mp4"));
    let seq = s.state.active_sequence.unwrap();
    let rate = s.sequence_rate();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let mut clip = |item: ItemId, at: f64, len: f64| {
        let mut c = p.make_track_item(item, TrackKind::Video, sec(at), TimeRange::new(Tick::ZERO, sec(len)), rate).unwrap();
        c.id = ClipId(p.alloc_id());
        c
    };
    let (a, b) = (clip(city, 0.0, 10.0), clip(desert, 3.0, 2.0));
    let b_end = b.end();
    let q = p.sequence_mut(seq).unwrap();
    for t in &mut q.video_tracks {
        t.items.clear();
    }
    q.video_tracks[0].items = vec![a, b];
    let r = review(&mut s, json!({}));
    let gap = r["issues"].as_array().unwrap().iter().find(|i| i["kind"] == "gap").unwrap_or_else(|| panic!("no gap: {r}"));
    assert_eq!(gap["time"].as_i64(), Some(b_end.0), "{r}");

/// Offline footage inside a nest is reported in the parent, where the nest shows it, and only
/// where it shows (#5166).
#[test]
fn offline_footage_inside_a_nest_is_reported_where_the_nest_shows_it() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_project::{ClipId, TrackKind};
    use filmcraft_time::TimeRange;
    let dir = tmp_dir("review-nested-offline");
    let (forest, ocean) = (dir.join("forest.mov"), dir.join("ocean.mov"));
    make_movie(&forest, filmcraft_media::DemoScene::Forest, 160, 90, 24);
    make_movie(&ocean, filmcraft_media::DemoScene::OceanSunset, 160, 90, 24);
    let (mut s, items, seq) = session_with(&[&forest, &ocean]);
    let rate = s.sequence_rate();
    let sec = Tick::from_seconds_f64;
    // child: forest 0–1 s, then ocean 1–2 s; two parents: one shows the whole child, one only its ocean part
    let (child, whole, ocean_only) = {
        let p = std::sync::Arc::make_mut(&mut s.project);
        let settings = p.sequence(seq).unwrap().settings.clone();
        let child = p.new_sequence("child", settings.clone(), 1, 1, None);
        let mut a = p.make_track_item(items[0], TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, sec(1.0)), rate).unwrap();
        a.id = ClipId(p.alloc_id());
        let mut b = p.make_track_item(items[1], TrackKind::Video, a.end(), TimeRange::new(Tick::ZERO, sec(1.0)), rate).unwrap();
        b.id = ClipId(p.alloc_id());
        let ocean_starts = b.start;
        p.sequence_mut(child).unwrap().video_tracks[0].items = vec![a, b];
        let child_len = p.sequence(child).unwrap().duration();
        let mut parent = |src: TimeRange| {
            let q = p.new_sequence("parent", settings.clone(), 1, 1, None);
            let mut n = p.make_track_item(child, TrackKind::Video, Tick::ZERO, src, rate).unwrap();
            n.id = ClipId(p.alloc_id());
            p.sequence_mut(q).unwrap().video_tracks[0].items = vec![n];
            q
        };
        let whole = parent(TimeRange::new(Tick::ZERO, child_len));
        let ocean_only = parent(TimeRange::from_bounds(ocean_starts, child_len));
        (child, whole, ocean_only)
    };
    s.execute("media.makeOffline", json!({"items": [items[0].0]})).unwrap();
    let offline = |s: &mut Session, q: filmcraft_project::ItemId| {
        let r = review(s, json!({"sequence": q.0}));
        r["issues"].as_array().unwrap().iter().filter(|i| i["kind"] == "offlineMedia").cloned().collect::<Vec<_>>()
    };
    assert_eq!(offline(&mut s, child).len(), 1, "the child itself");
    let found = offline(&mut s, whole);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0]["detail"].as_str().unwrap().contains("inside the nested sequence"), "{found:?}");
    assert_eq!(found[0]["seconds"].as_f64(), Some(0.0), "where it first shows in the parent");
    assert!(offline(&mut s, ocean_only).is_empty(), "the nest never shows the offline part");
}

/// Nests of nests multiply: a sequence that nests the same child many times, at every level,
/// is checked once per part shown and in bounded time.
#[test]
fn repeated_nests_are_walked_once() {
    use filmcraft_project::{ClipId, TrackKind};
    use filmcraft_time::TimeRange;
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    let rate = s.sequence_rate();
    let top = {
        let p = std::sync::Arc::make_mut(&mut s.project);
        let settings = p.sequence(seq).unwrap().settings.clone();
        // seven levels, each holding the level below 40 times on 40 tracks: 40^7 nest clips if each were walked
        let mut below = seq;
        for _ in 0..7 {
            let q = p.new_sequence("level", settings.clone(), 40, 0, None);
            let len = p.sequence(below).unwrap().duration();
            for k in 0..40 {
                let mut n = p.make_track_item(below, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, len), rate).unwrap();
                n.id = ClipId(p.alloc_id());
                p.sequence_mut(q).unwrap().video_tracks[k].items = vec![n];
            }
            below = q;
        }
        below
    };
    let t0 = std::time::Instant::now();
    let r = review(&mut s, json!({"sequence": top.0}));
    assert!(t0.elapsed().as_secs() < 10, "the check is bounded: {:?}", t0.elapsed());
    assert_eq!(r["errors"], 0, "the demo footage is online: {r}");
}

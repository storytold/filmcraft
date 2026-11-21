//! `media.analyze` has to describe footage nobody has put on a timeline: shots that cover it,
//! measures that tell a night street from a sunlit desert, and the words spoken in each shot,
//! without changing the project.

use serde_json::{Value, json};

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn item_named(s: &Session, name: &str) -> u64 {
    s.project.items.iter().find(|(_, i)| i.name == name).map(|(id, _)| id.0).unwrap()
}

fn index(s: &mut Session, item: u64) -> Value {
    s.execute("media.index", json!({"item": item})).unwrap()
}

#[test]
fn footage_is_indexed_into_shots_that_cover_it_and_measures_that_tell_scenes_apart() {
    let mut s = demo();
    let (city, desert, score) = (item_named(&s, "City_Night_Drive.mp4"), item_named(&s, "Desert_Dunes.mp4"), item_named(&s, "Ambient_Score.wav"));
    assert!(s.execute("media.index", json!({"item": city})).is_err(), "nothing before the analysis");
    let project = s.project.clone();
    let undo = s.history.undo.len();
    s.execute("media.analyze", json!({"items": [city, desert, score], "fps": 4, "wait": true})).unwrap();
    assert!(std::sync::Arc::ptr_eq(&project, &s.project), "analysing doesn't touch the project");
    assert_eq!(s.history.undo.len(), undo);

    for item in [city, desert] {
        let ix = index(&mut s, item);
        let shots = ix["shots"].as_array().unwrap();
        assert!(!shots.is_empty(), "{ix}");
        assert_eq!(shots[0]["start"], 0);
        for w in shots.windows(2) {
            assert_eq!(w[0]["end"], w[1]["start"], "shots are contiguous");
        }
        let covered: f64 = shots.iter().map(|x| x["seconds"].as_f64().unwrap()).sum();
        assert!((covered - ix["durationSeconds"].as_f64().unwrap()).abs() < 1e-6, "{ix}");
        assert!(shots.iter().all(|x| (0.0..=100.0).contains(&x["score"].as_f64().unwrap())));
    }
    let mean = |ix: &Value| {
        let sh = ix["shots"].as_array().unwrap();
        sh.iter().map(|x| x["brightness"].as_f64().unwrap()).sum::<f64>() / sh.len() as f64
    };
    let (night, day) = (index(&mut s, city), index(&mut s, desert));
    assert!(mean(&night) < mean(&day), "night {} vs desert {}", mean(&night), mean(&day));
    // sound only: no shots, but levels
    let music = index(&mut s, score);
    assert_eq!(music["shotCount"], 0);
    assert!(!music["audio"]["rmsDb"].as_array().unwrap().is_empty(), "{music}");
}

#[test]
fn each_shot_carries_the_words_spoken_in_it() {
    let mut s = demo();
    let sunset = item_named(&s, "Ocean_Sunset.mp4");
    let t = |x: f64| filmcraft_time::Tick::from_seconds_f64(x).0;
    s.execute(
        "transcript.set",
        json!({"item": sunset, "transcript": {"language": "en", "speakers": [], "words": [
            {"text": "golden", "start": t(0.5), "end": t(0.9)}, {"text": "hour", "start": t(1.0), "end": t(1.3)}
        ]}}),
    )
    .unwrap();
    s.execute("media.analyze", json!({"items": [sunset], "fps": 4, "wait": true})).unwrap();
    let ix = index(&mut s, sunset);
    assert_eq!(ix["hasTranscript"], true);
    let text: Vec<&str> = ix["shots"].as_array().unwrap().iter().filter_map(|x| x["text"].as_str()).collect();
    assert!(text.join(" ").contains("golden hour"), "{ix}");
}

#[test]
fn a_flat_picture_measures_darker_and_softer_than_a_detailed_one() {
    let (w, h) = (32usize, 32usize);
    let flat: Vec<u8> = [10u8, 10, 10, 255].repeat(w * h);
    let checker: Vec<u8> = (0..w * h).flat_map(|i| if (i % w + i / w) % 2 == 0 { [250u8, 250, 250, 255] } else { [20, 20, 20, 255] }).collect();
    let (a, b) = (super::measure_frame(&flat, w, h), super::measure_frame(&checker, w, h));
    assert!(a.brightness < b.brightness && a.sharpness < b.sharpness, "{a:?} {b:?}");
    assert!(a.crushed == 0.0 && b.blown == 0.0, "luma 10 isn't crushed, 250 isn't blown: {a:?} {b:?}");
}

/// A night shot with lights is dark, not black; a slow pan is not a freeze; a picture that stops
/// inside a moving shot is.
#[test]
fn night_shots_are_dark_not_black_and_slow_pans_are_not_frozen() {
    let shot = |brightness: f32, crushed: f32, motion: f32, still_seconds: f64| super::Shot {
        start: filmcraft_time::Tick::ZERO,
        end: filmcraft_time::Tick::from_seconds_f64(5.0),
        brightness,
        crushed,
        blown: 0.0,
        sharpness: 0.1,
        motion,
        still_seconds,
    };
    let flags = |s: super::Shot| super::assess(&s, 0.1).0;
    let night = flags(shot(0.06, 0.5, 0.007, 0.0));
    assert!(night.contains(&"dark") && !night.contains(&"black"), "{night:?}");
    assert!(flags(shot(0.01, 0.99, 0.0, 5.0)).contains(&"black"));
    let pan = flags(shot(0.6, 0.0, 0.004, 0.0));
    assert!(!pan.contains(&"frozen") && !pan.contains(&"static"), "{pan:?}");
    assert!(flags(shot(0.6, 0.0, 0.004, 1.5)).contains(&"frozen"));
    assert!(flags(shot(0.6, 0.0, 0.0005, 0.0)).contains(&"static"));
}

/// Headless sessions only poll when asked: media.index picks up a finished background analysis
/// itself, and the reply counts items, not frames.
#[test]
fn a_finished_background_analysis_is_readable_without_a_ui_poll() {
    use std::sync::atomic::Ordering;
    let mut s = demo();
    let (city, desert) = (item_named(&s, "City_Night_Drive.mp4"), item_named(&s, "Desert_Dunes.mp4"));
    let r = s.execute("media.analyze", json!({"items": [city, desert], "fps": 2})).unwrap();
    assert_eq!(r["items"], 2, "{r}");
    let job = r["job"].as_u64().unwrap();
    let t0 = std::time::Instant::now();
    while !s.jobs.iter().find(|j| j.id == job).is_some_and(|j| j.progress.finished.load(Ordering::Relaxed)) {
        assert!(t0.elapsed().as_secs() < 60, "the analysis didn't finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.execute("media.index", json!({"item": city})).is_ok());
}

/// An undo can hand an analysed item's id to other media: its old index must not come back.
#[test]
fn an_index_is_not_reused_for_other_media_under_the_same_id() {
    use crate::media_test_util::{make_movie, tmp_dir};
    let dir = tmp_dir("index-identity");
    let (a, b) = (dir.join("forest.mov"), dir.join("ocean.mov"));
    make_movie(&a, filmcraft_media::DemoScene::Forest, 160, 90, 24);
    make_movie(&b, filmcraft_media::DemoScene::OceanSunset, 160, 90, 24);
    let mut s = Session::default();
    let first = s.execute("file.import", json!({"paths": [a.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    s.execute("media.analyze", json!({"items": [first], "fps": 4, "wait": true})).unwrap();
    assert!(s.execute("media.index", json!({"item": first})).is_ok());
    s.execute("edit.undo", json!({})).unwrap();
    let second = s.execute("file.import", json!({"paths": [b.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    // whether or not the id was handed out again, the ocean clip has no index yet
    assert!(s.execute("media.index", json!({"item": second})).is_err(), "ids {first} and {second}");
}

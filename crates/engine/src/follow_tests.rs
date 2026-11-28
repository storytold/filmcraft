//! Tests of [`crate::follow`] in a session: Follow Speech during a voice-over take with the
//! scripted recogniser and the synthetic input. The prompter follows a read, manual commands win,
//! an ad-lib loses the narrator and the read finds them again, the take's log and transcript, the
//! option off, the reasons it is unavailable, and a take stopped while a recognition pass runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_speech::stream::{RollingConfig, RollingRecognizer, ScriptedRecognizer, ScriptedWord, StreamingRecognizer};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::follow::{RecognizerFactory, tokens};
use crate::{EngineError, Session};

const SCRIPT: &str = "Sales grew every quarter this year, and the second half carried most of it. The chart on screen shows each region, \
    starting with the north. Every region grew, but the north grew fastest because of the new stores we opened in spring.";

fn sec(x: f64) -> Tick {
    Tick::from_seconds_f64(x)
}

/// The demo project with one 30-second section holding [`SCRIPT`], from 0.
fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let rate = s.sequence_rate();
    let frames = rate.frame_at(Tick(30 * TICKS_PER_SECOND));
    s.execute("markers.add", json!({"time": 0, "name": "Numbers", "comment": SCRIPT, "durationFrames": frames})).unwrap();
    s
}

/// Install a scripted recogniser hearing `words` and turn Follow Speech on.
fn follow(s: &mut Session, words: Vec<ScriptedWord>) {
    s.voiceover.recognizer = Some(RecognizerFactory {
        id: "scripted".into(),
        make: Arc::new(move || Box::new(ScriptedRecognizer::new("scripted", words.clone())) as Box<dyn StreamingRecognizer>),
    });
    s.execute("audio.voiceover.settings", json!({"followSpeech": true})).unwrap();
}

/// `text` read at `per_word` seconds a word from `start`, each word committed `lag` after it ends.
fn reading(text: &str, start: f64, per_word: f64, lag: f64) -> Vec<ScriptedWord> {
    ScriptedRecognizer::reading(text, start, per_word, lag)
}

fn start(s: &mut Session) {
    s.execute("playhead.set", json!({"seconds": 0})).unwrap();
    s.execute("audio.voiceover.start", json!({"preroll": 0})).unwrap();
}

/// Play to `t` seconds in steps of a quarter second, as UI frames would.
fn play_to(s: &mut Session, from: f64, to: f64) {
    let mut t = from;
    while t < to - 1e-9 {
        t = (t + 0.25).min(to);
        s.set_playhead(sec(t));
        crate::voiceover::pump(s);
    }
}

fn inspect(s: &mut Session) -> Value {
    s.execute("prompter.inspect", json!({})).unwrap()
}

fn offset(s: &mut Session) -> usize {
    inspect(s)["section"]["script"]["offset"].as_u64().unwrap() as usize
}

fn state(s: &mut Session) -> String {
    s.execute("audio.voiceover.followState", json!({})).unwrap()["state"].as_str().unwrap().to_owned()
}

fn stop(s: &mut Session, at: f64) -> Value {
    let dir = std::env::temp_dir().join(format!("filmcraft-follow-{}-{at}", std::process::id()));
    let r = s.execute("audio.voiceover.stop", json!({"time": sec(at).0, "dir": dir.to_string_lossy()})).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    r
}

/// The character in the script where word `i` (counting from 0, as `split_whitespace`) starts.
fn char_of_word(i: usize) -> usize {
    tokens(SCRIPT, Some("en")).get(i).map_or(SCRIPT.chars().count(), |t| t.start)
}

#[test]
fn the_prompter_follows_a_read_faster_than_the_clock_and_holds_when_the_narrator_pauses() {
    let mut s = session();
    // the first 30 words in 10.5 s: the clock would put the narrator less than half as far
    let first: Vec<&str> = SCRIPT.split_whitespace().take(30).collect();
    follow(&mut s, reading(&first.join(" "), 0.5, 0.35, 1.5));
    start(&mut s);
    assert_eq!(state(&mut s), "listening");
    let mut last = 0;
    let mut t = 0.0;
    while t < 16.0 {
        play_to(&mut s, t, t + 1.0);
        t += 1.0;
        let o = offset(&mut s);
        assert!(o >= last, "the reading line never goes back on a clean read ({last} then {o} at {t}s)");
        last = o;
        // what the narrator had said by now, and what was committed: the line is between them
        let said = (((t - 0.5) / 0.35).floor().max(0.0) as usize).min(30);
        assert!(o <= char_of_word(said + 1), "not ahead of the narrator at {t}s: {o} vs {}", char_of_word(said + 1));
    }
    assert_eq!(state(&mut s), "following");
    let q = inspect(&mut s);
    assert_eq!(q["held"], true, "held at the narrator's place, not running with the clock: {q}");
    let clock = (16.0 / 30.0 * SCRIPT.chars().count() as f64) as usize;
    assert!(last > clock, "it follows the voice, ahead of the clock's {clock}: {last}");
    // the narrator is silent from 11 s: the prompter holds
    play_to(&mut s, 16.0, 20.0);
    assert_eq!(offset(&mut s), last, "silence holds the place");
    assert_eq!(state(&mut s), "following");
}

#[test]
fn manual_commands_win_and_tracking_continues_from_where_they_put_the_prompter() {
    let mut s = session();
    follow(&mut s, reading(SCRIPT, 0.5, 0.35, 1.5));
    start(&mut s);
    play_to(&mut s, 0.0, 8.0);
    let followed = offset(&mut s);
    assert!(followed > 0 && inspect(&mut s)["held"] == true);
    // back to the start of the section by hand
    s.execute("prompter.previous", json!({})).unwrap();
    let q = inspect(&mut s);
    assert!(q["held"] == false && q["section"]["script"]["offset"] == 0, "Previous wins and releases the hold: {q}");
    let at = q["playhead"].as_i64().unwrap();
    play_to(&mut s, 8.0, 10.0);
    let q = inspect(&mut s);
    assert!(q["held"] == false, "the tracker waits before moving it again: {q}");
    assert_eq!(q["promptTime"].as_i64(), Some(q["playhead"].as_i64().unwrap() - at), "the prompter runs with the clock from where Previous put it");
    // the narrator keeps reading; after the pause the tracker takes over again from there
    play_to(&mut s, 10.0, 14.0);
    let q = inspect(&mut s);
    assert!(q["held"] == true && offset(&mut s) > followed, "following again, ahead of where it was: {q}");
    let log = s.execute("prompter.takeLog", json!({})).unwrap()["inProgress"]["entries"].clone();
    let kinds: Vec<&str> = log.as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    let prev = kinds.iter().position(|k| *k == "previous").unwrap();
    assert!(kinds[..prev].contains(&"follow") && kinds[prev + 1..].contains(&"follow"), "automatic moves before and after the manual one: {kinds:?}");
}

#[test]
fn an_ad_lib_loses_the_narrator_and_reading_on_finds_them_again() {
    let mut s = session();
    let words: Vec<&str> = SCRIPT.split_whitespace().collect();
    let first = words[..15].join(" ");
    let ad_lib = "by the way if you want the details they are on the website linked below";
    let rest = words[15..].join(" ");
    let mut heard = reading(&first, 0.5, 0.35, 1.5);
    heard.extend(reading(ad_lib, 6.0, 0.35, 1.5));
    heard.extend(reading(&rest, 12.0, 0.35, 1.5));
    follow(&mut s, heard);
    start(&mut s);
    play_to(&mut s, 0.0, 7.5);
    let before = offset(&mut s);
    let prompt = inspect(&mut s)["promptTime"].clone();
    play_to(&mut s, 7.5, 13.0);
    assert_eq!(state(&mut s), "lost", "the ad-lib matches nothing nearby");
    let q = inspect(&mut s);
    assert!(q["promptTime"] == prompt && q["held"] == true, "the prompter holds its place: {q}");
    assert_eq!(offset(&mut s), before);
    play_to(&mut s, 13.0, 20.0);
    assert_eq!(state(&mut s), "following", "back on the script");
    assert!(offset(&mut s) > before);
}

#[test]
fn the_take_keeps_its_transcript_and_returns_it_when_it_stops() {
    let mut s = session();
    follow(&mut s, reading("Sales grew every quarter", 1.0, 0.5, 1.0));
    s.execute("playhead.set", json!({"seconds": 2})).unwrap();
    s.execute("audio.voiceover.start", json!({"preroll": 1})).unwrap();
    s.set_playhead(sec(1.0));
    play_to(&mut s, 1.0, 6.0);
    let live = s.execute("audio.voiceover.followState", json!({})).unwrap();
    assert_eq!(live["committed"], json!(["Sales", "grew", "every", "quarter"]), "the latest words: {live}");
    let r = stop(&mut s, 6.0);
    let words = r["transcript"]["words"].as_array().unwrap();
    let texts: Vec<&str> = words.iter().map(|w| w["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["Sales", "grew", "every", "quarter"]);
    // capture began at 1 s (a second of pre-roll), the record point is 2 s: the first word, spoken
    // 1 s into the capture, is at the record point
    let n = |w: &Value, k: &str| w[k].as_i64().unwrap();
    assert_eq!(n(&words[0], "start"), 0, "take times count from the record point");
    assert!(words.windows(2).all(|w| n(&w[0], "end") <= n(&w[1], "start")));
    assert_eq!(r["transcript"]["model"], "scripted");
    let after = s.execute("audio.voiceover.followState", json!({})).unwrap();
    assert_eq!(after["lastTranscript"], r["transcript"], "the query returns the last take's transcript");
    assert_eq!(after["state"], "ready", "no take, ready for the next");
}

#[test]
fn with_the_option_off_nothing_changes_from_before() {
    let mut s = session();
    s.voiceover.recognizer = Some(RecognizerFactory {
        id: "scripted".into(),
        make: Arc::new(|| Box::new(ScriptedRecognizer::new("scripted", reading(SCRIPT, 0.5, 0.35, 1.5))) as Box<dyn StreamingRecognizer>),
    });
    assert!(!s.prefs.voice_over.follow_speech, "off by default");
    start(&mut s);
    for t in [2.0, 6.0, 10.0] {
        play_to(&mut s, t - 2.0, t);
        let q = inspect(&mut s);
        assert!(q["promptTime"] == q["playhead"] && q["held"] == false, "the prompt time is the playhead: {q}");
    }
    assert_eq!(state(&mut s), "off");
    let r = stop(&mut s, 10.0);
    assert!(r["transcript"].is_null(), "no transcript");
    let kinds: Vec<&str> = r["promptLog"]["entries"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["start"], "the log only has where the take started");
    assert_eq!(r["samples"], 480_000, "the recording is the same");
}

#[test]
fn unavailable_follow_speech_says_why_and_never_looks_as_if_it_works() {
    let mut s = session();
    let why = crate::follow::availability(&s);
    if crate::transcript::speech_available() {
        // a build with speech: the reason is the model, when it is not installed here
        if let Err(why) = why {
            assert!(why.contains("not installed") && why.contains("transcript.downloadModel"), "{why}");
        }
        return;
    }
    let why = why.unwrap_err();
    assert!(why.contains("not in this build"), "{why}");
    match s.execute("audio.voiceover.followSpeech", json!({"on": true})) {
        Err(EngineError::Disabled(_, reason)) => assert!(reason.contains("not in this build"), "{reason}"),
        other => panic!("the command is disabled with the reason: {other:?}"),
    }
    assert!(s.execute("audio.voiceover.settings", json!({"followSpeech": true})).is_err(), "and the setting refuses it");
    assert!(!s.prefs.voice_over.follow_speech);
    let q = s.execute("audio.voiceover.followState", json!({})).unwrap();
    assert!(q["state"] == "off" && q["available"] == false && q["reason"].as_str().is_some_and(|r| r.contains("whisper")), "{q}");
    // a preference turned on elsewhere (another build) shows as unavailable, also during a take
    s.prefs.voice_over.follow_speech = true;
    assert_eq!(state(&mut s), "unavailable");
    start(&mut s);
    play_to(&mut s, 0.0, 2.0);
    let q = s.execute("audio.voiceover.followState", json!({})).unwrap();
    assert!(q["state"] == "unavailable" && q["reason"].is_string(), "{q}");
    assert_eq!(inspect(&mut s)["held"], false);
    let r = stop(&mut s, 2.0);
    assert!(r["placed"] == true && r["transcript"].is_null(), "the take is recorded as usual");
    // it can always be turned off
    s.execute("audio.voiceover.followSpeech", json!({"on": false})).unwrap();
    assert!(!s.prefs.voice_over.follow_speech);
}

#[test]
fn the_follow_command_turns_it_on_and_off_and_the_state_reports_it() {
    let mut s = session();
    follow(&mut s, Vec::new());
    assert_eq!(state(&mut s), "ready");
    let r = s.execute("audio.voiceover.followSpeech", json!({})).unwrap();
    assert!(r["state"] == "off" && !s.prefs.voice_over.follow_speech, "toggled off: {r}");
    let r = s.execute("audio.voiceover.followSpeech", json!({"on": true})).unwrap();
    assert!(r["state"] == "ready" && r["model"] == "scripted", "{r}");
}

/// A transcriber whose passes wait until the test lets them finish, and that says when it is gone.
struct Blocking {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

impl filmcraft_speech::Transcriber for Blocking {
    fn id(&self) -> String {
        "blocking".into()
    }
    fn transcribe(
        &self,
        _: &[f32],
        _: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<filmcraft_project::Transcript, filmcraft_speech::SpeechError> {
        self.entered.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if !progress(1.0, "") {
            return Err(filmcraft_speech::SpeechError::Cancelled);
        }
        Ok(filmcraft_project::Transcript::default())
    }
}

impl Drop for Blocking {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn a_take_stops_cleanly_while_a_recognition_pass_is_running() {
    let mut s = session();
    // speech-like bursts, so the rolling recogniser runs a pass
    let audio: Vec<f32> = (0..48_000 * 10).map(|i| if (i / 4_800) % 2 == 0 { 0.3 * ((i as f32) * 0.05).sin() } else { 0.0 }).collect();
    s.voiceover.input = Some(Box::new(crate::voiceover::SyntheticInput::new(crate::voiceover::Synthetic::Buffer(vec![audio]))));
    let (entered, release, dropped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let model: Arc<dyn filmcraft_speech::Transcriber> = Arc::new(Blocking { entered: entered.clone(), release: release.clone(), dropped: dropped.clone() });
    let slot = std::sync::Mutex::new(Some(model));
    s.voiceover.recognizer = Some(RecognizerFactory {
        id: "blocking".into(),
        make: Arc::new(move || {
            let m = slot.lock().unwrap().take().expect("one take");
            Box::new(RollingRecognizer::new("blocking", Box::new(move || Ok(m)), RollingConfig::default())) as Box<dyn StreamingRecognizer>
        }),
    });
    s.execute("audio.voiceover.settings", json!({"followSpeech": true})).unwrap();
    start(&mut s);
    let mut t: f64 = 0.0;
    while !entered.load(Ordering::SeqCst) {
        t += 0.25;
        assert!(t < 120.0, "a pass starts");
        s.set_playhead(sec(t.min(9.0)));
        crate::voiceover::pump(&mut s);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // stop while the pass is still running: the take is placed, the recogniser is let go
    let r = stop(&mut s, t.min(9.0));
    assert_eq!(r["placed"], true);
    assert!(s.voiceover.follow.is_none());
    release.store(true, Ordering::SeqCst);
    for _ in 0..1_000 {
        if dropped.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the worker ends after its pass and lets the model go");
}

#[test]
fn the_take_log_with_follow_entries_replays_exactly_and_rebuilds_the_prompt_time() {
    let mut s = session();
    follow(&mut s, reading(SCRIPT, 0.5, 0.35, 1.5));
    start(&mut s);
    let record = s.playhead().0;
    let mut observed = Vec::new();
    let mut t = 0.0;
    while t < 18.0 {
        play_to(&mut s, t, t + 0.25);
        t += 0.25;
        match (t * 4.0).round() as i64 {
            20 => drop(s.execute("prompter.faster", json!({})).unwrap()),
            36 => drop(s.execute("prompter.slower", json!({})).unwrap()),
            48 => drop(s.execute("prompter.toPlayhead", json!({}))),
            _ => {}
        }
        let q = inspect(&mut s);
        observed.push((q["playhead"].as_i64().unwrap(), q["promptTime"].as_i64().unwrap()));
    }
    let log = s.execute("prompter.takeLog", json!({})).unwrap()["inProgress"].clone();
    let entries = log["entries"].as_array().unwrap();
    let n = |e: &Value, k: &str| e[k].as_i64().unwrap();
    let kinds: Vec<&str> = entries.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert!(kinds.iter().filter(|k| **k == "follow").count() >= 3, "the tracker's moves are entries of their own: {kinds:?}");
    assert!(entries.iter().any(|e| e["kind"] == "follow" && n(e, "rateAfter") == 0), "holds are entries with rate 0");
    for w in entries.windows(2) {
        let ran = n(&w[0], "promptAfter") + (n(&w[1], "playhead") - n(&w[0], "playhead")) * n(&w[0], "rateAfter") / 100;
        assert_eq!(n(&w[1], "promptBefore"), ran, "the log replays across follow entries: {} then {}", w[0], w[1]);
        assert_eq!(n(&w[1], "rateBefore"), n(&w[0], "rateAfter"), "and the rates chain");
    }
    let rebuild = |take_time: i64| {
        let e = entries.iter().rev().find(|e| n(e, "takeTime") <= take_time).unwrap();
        n(e, "promptAfter") + (take_time - n(e, "takeTime")) * n(e, "rateAfter") / 100
    };
    for (playhead, prompt) in observed {
        assert_eq!(rebuild(playhead - record), prompt, "the log gives the prompt time at take time {}", playhead - record);
    }
    // the follow entries name the section, so a take made without a key press can be conformed
    assert!(entries.iter().filter(|e| e["kind"] == "follow").all(|e| e["marker"].is_u64() && e["paragraph"].is_u64()));
}

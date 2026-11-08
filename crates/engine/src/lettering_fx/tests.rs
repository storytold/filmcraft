use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

static N: AtomicUsize = AtomicUsize::new(0);

/// A fresh folder with awkward characters: a space, a leading dash in a child and Unicode.
fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fc lettering ação {tag} {} {}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn bank(root: &Path) -> String {
    let t = root.join("templates");
    for (dir, body) in [
        ("pop-amarelo", r#"{"id":"pop-amarelo","nome":"Pop Amarelo","descricao":"Com mola."}"#),
        ("blur-suave", r#"{"id":"blur-suave","nome":"Blur Suave"}"#),
        ("_comum", r#"{"id":"_comum","nome":"shared"}"#),
        ("-x", r#"{"id":"-x","nome":"dash"}"#),
        ("quebrado", "{ not json"),
        ("outro-id", r#"{"id":"alheio","nome":"mismatch"}"#),
    ] {
        std::fs::create_dir_all(t.join(dir)).unwrap();
        std::fs::write(t.join(dir).join("template.json"), body).unwrap();
    }
    std::fs::create_dir_all(t.join("sem-json")).unwrap();
    root.to_string_lossy().into_owned()
}

fn w(text: &str, start: f64, end: f64) -> FxWord {
    FxWord { text: text.into(), start, end, hero: false }
}

#[test]
fn phrases_break_on_punctuation_pause_word_count_and_letters() {
    let ws = vec![w("Tem", 0.0, 0.2), w("uma", 0.2, 0.4), w("frase.", 0.4, 0.8), w("Agora", 0.8, 1.0), w("pausa", 1.6, 1.9)];
    assert_eq!(phrases(&ws), vec![vec![0, 1, 2], vec![3], vec![4]]);
    let five: Vec<FxWord> = (0..5).map(|i| w("ab", i as f64 * 0.1, i as f64 * 0.1 + 0.1)).collect();
    assert_eq!(phrases(&five), vec![vec![0, 1, 2, 3], vec![4]]);
    let long = vec![w("anticonstitucionalissimamente", 0.0, 1.0), w("de", 1.0, 1.1), w("novo", 1.1, 1.2)];
    assert_eq!(phrases(&long), vec![vec![0], vec![1, 2]], "a long word stands alone, then the rest");
    assert!(phrases(&[]).is_empty());
    // a comma does not break
    let c = vec![w("vivalista,", 0.0, 0.5), w("deus", 0.6, 0.8)];
    assert_eq!(phrases(&c), vec![vec![0, 1]]);
}

#[test]
fn words_json_has_the_contract() {
    let mut ws = vec![w("Tem", 0.0, 0.2), w("uma.", 0.2, 0.5), w("Frase", 0.7, 1.0)];
    mark_heroes(&mut ws, &[2], "t").unwrap();
    let sp = Span { captions: vec![], start: Tick(0), duration: 1.0, width: 1920, height: 1080, fps: 24.0, words: ws };
    let v = words_json(&sp);
    assert_eq!((v["fps"].as_f64(), v["width"].as_u64(), v["height"].as_u64(), v["duration"].as_f64()), (Some(24.0), Some(1920), Some(1080), Some(1.0)));
    let p = v["phrases"].as_array().unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0]["start"], json!(0.0));
    assert_eq!(p[0]["end"], json!(0.5));
    assert_eq!(p[1]["words"][0]["text"], json!("Frase"));
    assert_eq!(p[1]["words"][0]["hero"], json!(true));
    assert_eq!(p[1]["words"][0]["pause_before"], json!(0.2), "the pause crosses the phrase break");
    assert_eq!(p[0]["words"][0]["pause_before"], json!(0.0));
}

#[test]
fn hero_out_of_range_is_an_error_not_a_panic() {
    let mut ws = vec![w("a", 0.0, 0.1)];
    assert!(mark_heroes(&mut ws, &[1], "t").is_err());
    assert!(mark_heroes(&mut ws, &[usize::MAX], "t").is_err());
    assert!(mark_heroes(&mut [], &[0], "t").is_err());
}

#[test]
fn menu_lists_only_readable_templates_with_a_safe_id() {
    let root = tmp("menu");
    let b = bank(&root);
    let s = Session::default();
    let t = templates(&s, &b).unwrap();
    assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["blur-suave", "pop-amarelo"]);
    assert_eq!(t[1].name, "Pop Amarelo");
    assert_eq!(t[1].description, "Com mola.");
    assert_eq!(t[0].description, "");
    assert!(templates(&s, &root.join("nada").to_string_lossy()).is_err(), "a missing bank is an error");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn safe_ids_refuse_paths_and_dashes() {
    for bad in ["", "../x", "a/b", "-x", ".hidden", "_comum", "a b", "ação", &"x".repeat(65)] {
        assert!(!safe_id(bad), "{bad:?}");
    }
    assert!(safe_id("pop-amarelo"));
}

/// A runner that records what it was asked and plays the attendant: it writes the `.mov` named in
/// the order it was handed.
struct Fake {
    seen: Mutex<Vec<ProcessSpec>>,
    outcome: std::result::Result<i32, ProcessError>,
    write_mov: bool,
}

impl ProcessRunner for Fake {
    fn run(&self, spec: &ProcessSpec, _cancel: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError> {
        self.seen.lock().unwrap().push(spec.clone());
        let code = self.outcome.clone()?;
        if self.write_mov {
            let order: Value = serde_json::from_slice(&std::fs::read(&spec.args[1]).unwrap()).unwrap();
            std::fs::write(order["saida"].as_str().unwrap(), b"mov").unwrap();
        }
        Ok(ProcessOutput { code: Some(code), stderr: "boom".into(), ..Default::default() })
    }
}

fn session_with_caption(runner: Option<Arc<dyn ProcessRunner>>, b: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"width": 1080, "height": 1920, "fps": 30})).unwrap();
    let cap =
        s.execute("captions.add", json!({"track": "C1", "text": "Nunca pague 10 mil por um logo", "seconds": 5.0, "durationSeconds": 3.0})).unwrap()["caption"]
            .as_u64()
            .unwrap();
    s.execute("captions.select", json!({"captions": [cap]})).unwrap();
    s.lettering_fx.runner = runner;
    s.lettering_fx.bank = Some(b.into());
    s
}

fn wait(s: &Session, job: u64) -> Value {
    for _ in 0..500 {
        let v = s.lettering_fx.jobs.iter().find(|j| j.id == job).unwrap().to_json();
        if v["status"] != "running" {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the job never finished");
}

#[test]
fn prepare_describes_the_span() {
    let root = tmp("prep");
    let mut s = session_with_caption(None, &bank(&root));
    let v = s.execute("lettering.fx.prepare", json!({"heroes": [1]})).unwrap();
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(1080), Some(1920)));
    assert_eq!(v["fps"].as_f64(), Some(30.0));
    assert!((v["startSeconds"].as_f64().unwrap() - 5.0).abs() < 0.01);
    let words = v["words"].as_array().unwrap();
    assert_eq!(words.len(), 7);
    assert_eq!(words[1]["hero"], json!(true));
    assert_eq!(words[0]["hero"], json!(false));
    assert!(words[0]["start"].as_f64().unwrap() >= 0.0, "times are relative to the span start");
    assert!(v["duration"].as_f64().unwrap() > 2.0);
    assert!(s.execute("lettering.fx.prepare", json!({"heroes": [99]})).is_err());
    assert!(s.execute("lettering.fx.prepare", json!({"heroes": "x"})).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn send_writes_the_order_and_runs_the_attendant_with_hostile_paths() {
    let root = tmp("send");
    let b = bank(&root);
    let fake = Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Ok(0), write_mov: true });
    let mut s = session_with_caption(Some(fake.clone()), &b);
    let out = root.join("-x saída ñ").to_string_lossy().into_owned();
    let v = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "heroes": [2], "out": out})).unwrap();
    let job = v["job"].as_u64().unwrap();
    assert_eq!(wait(&s, job)["status"], "done");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[0].program, "python3");
    assert_eq!(seen[0].args, vec![format!("{b}/templates/../atender.py").replace("/templates/..", ""), format!("{out}/pedido.json")]);
    let order: Value = serde_json::from_slice(&std::fs::read(format!("{out}/pedido.json")).unwrap()).unwrap();
    assert_eq!(order["template"], "pop-amarelo");
    assert_eq!(order["saida"], json!(format!("{out}/lettering.mov")));
    assert_eq!(order["palavras"], json!(format!("{out}/words.json")));
    let words: Value = serde_json::from_slice(&std::fs::read(format!("{out}/words.json")).unwrap()).unwrap();
    let heroes: Vec<bool> =
        words["phrases"].as_array().unwrap().iter().flat_map(|p| p["words"].as_array().unwrap().iter().map(|w| w["hero"].as_bool().unwrap())).collect();
    assert_eq!(heroes.iter().filter(|h| **h).count(), 1);
    assert!(heroes[2]);
    let st = s.execute("lettering.fx.status", json!({"job": job})).unwrap();
    assert_eq!(st["jobs"][0]["status"], "done");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn send_refuses_bad_orders_before_touching_disk_or_processes() {
    let root = tmp("bad");
    let b = bank(&root);
    let fake = Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Ok(0), write_mov: true });
    let mut s = session_with_caption(Some(fake.clone()), &b);
    for t in ["../pop-amarelo", "nao-existe", "_comum", "-x", "", "pop amarelo"] {
        assert!(s.execute("lettering.fx.send", json!({"template": t})).is_err(), "{t:?}");
    }
    assert!(s.execute("lettering.fx.send", json!({})).is_err());
    assert!(s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "heroes": [50]})).is_err());
    assert!(s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": "a\u{0}b"})).is_err());
    assert!(fake.seen.lock().unwrap().is_empty());
    s.lettering_fx.runner = None;
    assert!(s.execute("lettering.fx.send", json!({"template": "pop-amarelo"})).is_err(), "no runner: unsupported");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn failures_become_a_failed_job_not_a_crash() {
    let root = tmp("fail");
    let b = bank(&root);
    let cases: Vec<(Arc<Fake>, &str)> = vec![
        (Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Ok(3), write_mov: false }), "code 3"),
        (Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Ok(0), write_mov: false }), "no .mov"),
        (Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Err(ProcessError::NotFound("python3".into())), write_mov: false }), "not found"),
        (Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Err(ProcessError::Timeout(1)), write_mov: false }), "timed out"),
    ];
    for (i, (fake, want)) in cases.into_iter().enumerate() {
        let mut s = session_with_caption(Some(fake), &b);
        let out = root.join(format!("o{i}")).to_string_lossy().into_owned();
        let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": out})).unwrap()["job"].as_u64().unwrap();
        let v = wait(&s, job);
        assert_eq!(v["status"], "failed", "{want}");
        assert!(v["error"].as_str().unwrap().contains(want), "{v}");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_old_or_empty_mov_never_passes_for_a_new_one() {
    let root = tmp("old");
    let b = bank(&root);
    let out = root.join("o").to_string_lossy().into_owned();
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(format!("{out}/lettering.mov"), b"stale render").unwrap();
    let fake = Arc::new(Fake { seen: Mutex::new(Vec::new()), outcome: Ok(0), write_mov: false });
    let mut s = session_with_caption(Some(fake), &b);
    let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": out})).unwrap()["job"].as_u64().unwrap();
    let v = wait(&s, job);
    assert_eq!(v["status"], "failed");
    assert!(v["error"].as_str().unwrap().contains("empty"), "{v}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_panicking_runner_is_a_failed_job() {
    struct Boom;
    impl ProcessRunner for Boom {
        fn run(&self, _: &ProcessSpec, _: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError> {
            panic!("runner bug")
        }
    }
    let root = tmp("boom");
    let b = bank(&root);
    let mut s = session_with_caption(Some(Arc::new(Boom)), &b);
    let out = root.join("o").to_string_lossy().into_owned();
    let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": out})).unwrap()["job"].as_u64().unwrap();
    assert_eq!(wait(&s, job)["status"], "failed");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cancel_reaches_the_runner() {
    struct Waits;
    impl ProcessRunner for Waits {
        fn run(&self, _: &ProcessSpec, cancel: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError> {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(ProcessError::Cancelled)
        }
    }
    let root = tmp("cancel");
    let b = bank(&root);
    let mut s = session_with_caption(Some(Arc::new(Waits)), &b);
    let out = root.join("o").to_string_lossy().into_owned();
    let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": out})).unwrap()["job"].as_u64().unwrap();
    assert_eq!(s.execute("lettering.fx.status", json!({"job": job})).unwrap()["jobs"][0]["status"], "running");
    s.execute("lettering.fx.cancel", json!({"job": job})).unwrap();
    assert_eq!(wait(&s, job)["status"], "cancelled");
    assert!(s.execute("lettering.fx.cancel", json!({"job": 999})).is_err());
    assert!(s.execute("lettering.fx.cancel", json!({})).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn nothing_selected_and_no_captions_are_errors() {
    let mut s = Session::default();
    assert!(s.execute("lettering.fx.prepare", json!({})).is_err());
    s.execute("file.newSequence", json!({"width": 1080, "height": 1920, "fps": 30})).unwrap();
    assert!(s.execute("lettering.fx.prepare", json!({})).is_err());
}

#[test]
fn a_pinned_span_ignores_a_selection_that_changed_and_a_busy_folder_is_refused() {
    struct Waits;
    impl ProcessRunner for Waits {
        fn run(&self, _: &ProcessSpec, cancel: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError> {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(ProcessError::Cancelled)
        }
    }
    let root = tmp("pin");
    let b = bank(&root);
    let mut s = session_with_caption(Some(Arc::new(Waits)), &b);
    let v = s.execute("lettering.fx.prepare", json!({})).unwrap();
    let pinned = v["captions"].clone();
    assert_eq!(pinned.as_array().unwrap().len(), 1);
    let other =
        s.execute("captions.add", json!({"track": "C1", "text": "outra legenda bem diferente", "seconds": 20.0, "durationSeconds": 2.0})).unwrap()["caption"]
            .as_u64()
            .unwrap();
    s.execute("captions.select", json!({"captions": [other]})).unwrap();
    let again = s.execute("lettering.fx.prepare", json!({"captions": pinned})).unwrap();
    assert_eq!(again["words"], v["words"], "the pinned captions, not the new selection");
    assert!(s.execute("lettering.fx.prepare", json!({"captions": []})).is_err());
    assert!(s.execute("lettering.fx.prepare", json!({"captions": ["x"]})).is_err());
    let out = root.join("busy").to_string_lossy().into_owned();
    let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "captions": pinned, "out": out})).unwrap()["job"].as_u64().unwrap();
    assert!(s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "captions": pinned, "out": out})).is_err(), "same folder while running");
    s.execute("lettering.fx.cancel", json!({"job": job})).unwrap();
    assert_eq!(wait(&s, job)["status"], "cancelled");
    assert!(s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "captions": pinned, "out": out})).is_ok(), "free again once it ended");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_dash_bank_path_cannot_become_an_interpreter_option() {
    struct Spy(Mutex<Vec<ProcessSpec>>);
    impl ProcessRunner for Spy {
        fn run(&self, spec: &ProcessSpec, _: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError> {
            self.0.lock().unwrap().push(spec.clone());
            Err(ProcessError::Cancelled)
        }
    }
    let root = tmp("dash");
    let real = bank(&root);
    let spy = Arc::new(Spy(Mutex::new(Vec::new())));
    let mut s = session_with_caption(Some(spy.clone()), &real);
    // a relative bank whose first character is a dash: copy the bank into the working directory
    let rel = format!("-banco-{}", std::process::id());
    let here = std::env::current_dir().unwrap().join(&rel);
    std::fs::create_dir_all(here.join("templates/pop-amarelo")).unwrap();
    std::fs::write(here.join("templates/pop-amarelo/template.json"), r#"{"id":"pop-amarelo"}"#).unwrap();
    s.lettering_fx.bank = Some(rel.clone());
    let out = root.join("o").to_string_lossy().into_owned();
    let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": out})).unwrap()["job"].as_u64().unwrap();
    wait(&s, job);
    let seen = spy.0.lock().unwrap();
    assert_eq!(seen[0].args[0], format!("./{rel}/atender.py"));
    let _ = std::fs::remove_dir_all(here);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_pinned_caption_that_was_deleted_is_an_error_and_a_dash_out_is_not_an_option() {
    let root = tmp("gone");
    let b = bank(&root);
    let mut s = session_with_caption(None, &b);
    let pinned = s.execute("lettering.fx.prepare", json!({})).unwrap()["captions"].clone();
    s.execute("captions.delete", json!({"captions": pinned})).unwrap();
    let other =
        s.execute("captions.add", json!({"track": "C1", "text": "outra", "seconds": 1.0, "durationSeconds": 1.0})).unwrap()["caption"].as_u64().unwrap();
    let both = json!([pinned[0], other]);
    assert!(s.execute("lettering.fx.prepare", json!({"captions": both})).is_err(), "one of the pinned captions is gone");
    assert!(s.execute("lettering.fx.prepare", json!({"captions": pinned})).is_err());
}

#[test]
fn a_relative_out_that_starts_with_a_dash_is_prefixed_in_the_order_argument() {
    struct Spy(Mutex<Vec<ProcessSpec>>);
    impl ProcessRunner for Spy {
        fn run(&self, spec: &ProcessSpec, _: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError> {
            self.0.lock().unwrap().push(spec.clone());
            Err(ProcessError::Cancelled)
        }
    }
    let root = tmp("dashout");
    let b = bank(&root);
    let spy = Arc::new(Spy(Mutex::new(Vec::new())));
    let mut s = session_with_caption(Some(spy.clone()), &b);
    let rel = format!("-saida-{}", std::process::id());
    let job = s.execute("lettering.fx.send", json!({"template": "pop-amarelo", "out": rel})).unwrap()["job"].as_u64().unwrap();
    wait(&s, job);
    assert_eq!(spy.0.lock().unwrap()[0].args[1], format!("./{rel}/pedido.json"));
    let _ = std::fs::remove_dir_all(&rel);
    let _ = std::fs::remove_dir_all(root);
}

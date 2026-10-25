//! Background imports that cannot finish normally: a worker that ended before opening every
//! file, and a project replaced while its files are still opening.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::json;

use super::{Opened, Pending, poll};
use crate::Session;
use crate::commands::{Prefetched, open_concurrently};

fn write_wav(path: &std::path::Path, seconds: f32) -> String {
    std::fs::write(path, crate::voiceover::write_wav_f32(&vec![0.0; (48_000.0 * seconds) as usize], 48_000)).unwrap();
    path.to_string_lossy().into_owned()
}

/// A worker that died (a panic `guard_job` caught, a thread the OS would not start) leaves some
/// paths without a result. The import used to wait for them forever: it stayed "running", never
/// reported `ImportFinished` and never ran ingest. Now it adds what was opened and says what was not.
#[test]
fn an_import_whose_worker_ended_early_finishes_with_the_files_it_opened() {
    let dir = crate::media_test_util::tmp_dir("background-import-ended");
    let paths: Vec<String> = (0..3).map(|i| write_wav(&dir.join(format!("ended{i}.wav")), 0.5)).collect();
    let mut s = Session::default();
    // the worker opened the first and the last file, then died
    let opened: Opened = Arc::default();
    for p in [&paths[0], &paths[2]] {
        open_concurrently(&s.media, &*s.services, std::slice::from_ref(p), None, &|path: &str, r: Prefetched| {
            opened.lock().unwrap().entry(path.to_string()).or_default().push_back(r);
        });
    }
    let job = crate::Job { id: 1, label: "Import 3 files".into(), progress: Default::default(), result: Default::default() };
    *job.result.lock().unwrap() = Some(Err("internal error (see the crash log); the job was stopped".into()));
    job.progress.finished.store(true, Ordering::Relaxed);
    s.background_imports.push(Pending {
        job: 1,
        bin: None,
        paths: paths.clone(),
        next: 0,
        opened,
        items: Vec::new(),
        errors: Vec::new(),
        started: web_time::Instant::now(),
        progress: job.progress.clone(),
        result: job.result.clone(),
    });
    s.jobs.push(job);
    poll(&mut s);
    assert!(s.background_imports.is_empty(), "the import is finished, not left running");
    let items = s
        .events
        .iter()
        .find_map(|e| match e {
            crate::Event::ImportFinished { job: 1, items } => Some(items.clone()),
            _ => None,
        })
        .expect("the import reports that it finished");
    let names: Vec<String> = items.iter().map(|i| s.project.item(*i).unwrap().name.clone()).collect();
    assert_eq!(names, ["ended0.wav", "ended2.wav"], "the files that were opened are added, in order");
    assert!(s.log.entries.iter().any(|e| e.message.contains("1 of 3 files not imported")), "the missing file is reported");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path given twice is opened twice; the results used to be kept by path, so the second
/// occurrence was reported as not imported. The background import now adds what the synchronous
/// import adds.
#[test]
fn a_path_given_twice_imports_as_the_synchronous_import_does() {
    let dir = crate::media_test_util::tmp_dir("background-import-twice");
    let a = write_wav(&dir.join("twice-a.wav"), 0.5);
    let b = write_wav(&dir.join("twice-b.wav"), 0.75);
    let paths = [a.clone(), a, b];
    let names = |s: &Session| -> Vec<String> {
        let mut v: Vec<(u64, String)> = s.project.items.values().filter(|it| it.name.starts_with("twice-")).map(|it| (it.id.0, it.name.clone())).collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect()
    };
    let mut sync = Session::default();
    sync.execute("file.import", json!({ "paths": paths })).unwrap();
    let mut s = Session::default();
    s.execute("file.import", json!({ "paths": paths, "async": true })).unwrap();
    let started = Instant::now();
    while !s.background_imports.is_empty() && started.elapsed() < Duration::from_secs(30) {
        poll(&mut s);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.background_imports.is_empty(), "the import finished");
    assert_eq!(names(&s), names(&sync), "the same items, in the same order");
    assert!(!s.log.entries.iter().any(|e| e.message.contains("not imported")), "no occurrence counts as not opened");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An import in which no file could be opened used to report its job as a success in `jobs.list`
/// (the failures only showed in a toast).
#[test]
fn a_background_import_that_adds_nothing_fails_its_job() {
    let dir = crate::media_test_util::tmp_dir("background-import-all-failed");
    let paths: Vec<String> = (0..3).map(|i| dir.join(format!("not-there{i}.wav")).to_string_lossy().into_owned()).collect();
    let mut s = Session::default();
    let r = s.execute("file.import", json!({ "paths": paths, "async": true })).unwrap();
    let job = r["importJob"].as_u64().unwrap();
    let started = Instant::now();
    while !s.background_imports.is_empty() && started.elapsed() < Duration::from_secs(30) {
        poll(&mut s);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.background_imports.is_empty(), "the import finished");
    // the worker may still be writing its own result: it must not turn the failure into a success
    let progress = s.jobs.iter().find(|j| j.id == job).unwrap().progress.clone();
    while !progress.finished.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(20));
    let listed = s.execute("jobs.list", json!({})).unwrap();
    let entry = listed.as_array().and_then(|a| a.iter().find(|j| j["id"] == job)).cloned().unwrap_or_default();
    let err = entry["result"]["error"].as_str().unwrap_or_default();
    assert!(err.contains("not-there0.wav") && err.contains("not-there2.wav"), "jobs.list reports the failure: {entry}");
    assert!(progress.error.lock().unwrap().is_some(), "the Progress panel shows the failure: {entry}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Opening another project while a drop of many files is still opening used to add the rest of
/// the files to the other project, one undo step each.
#[test]
fn replacing_the_project_cancels_its_background_imports() {
    let dir = crate::media_test_util::tmp_dir("background-import-replaced");
    let paths: Vec<String> = (0..12).map(|i| write_wav(&dir.join(format!("replaced{i:02}.wav")), 0.5)).collect();
    for replace in [json!(["file.newProject", {"name": "Next"}]), json!(["file.openDemoProject", {}])] {
        let mut s = Session::default();
        let r = s.execute("file.import", json!({ "paths": paths, "async": true })).unwrap();
        let job = r["importJob"].as_u64().unwrap();
        let generation = s.project_generation;
        s.execute(replace[0].as_str().unwrap(), replace[1].clone()).unwrap();
        assert_eq!(s.project_generation, generation + 1, "{replace}: frontends can tell the project was replaced");
        assert!(s.background_imports.is_empty(), "{replace}: the import is dropped with its project");
        let progress = s.jobs.iter().find(|j| j.id == job).unwrap().progress.clone();
        assert!(progress.cancel.load(Ordering::Relaxed), "{replace}: its job is cancelled");
        let started = Instant::now();
        while !progress.finished.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(progress.finished.load(Ordering::Relaxed), "{replace}: the worker stops");
        for _ in 0..3 {
            poll(&mut s);
        }
        assert!(!s.project.items.values().any(|it| it.name.starts_with("replaced")), "{replace}: nothing reaches the new project");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

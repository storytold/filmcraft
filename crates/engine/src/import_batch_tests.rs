//! A card's worth of files imported in one command: they are opened several at a time (opening
//! is the slow part; some decoders take a tenth of a second) but added in the order given,
//! with one error per file that could not be imported and the rest imported anyway.

use serde_json::json;

use crate::Session;
use filmcraft_project::ItemId;

fn write_wav(path: &std::path::Path, seconds: f32) -> String {
    std::fs::write(path, crate::voiceover::write_wav_f32(&vec![0.0; (48_000.0 * seconds) as usize], 48_000)).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn a_batch_import_keeps_the_given_order_and_reports_each_failure() {
    let dir = crate::media_test_util::tmp_dir("import-batch");
    // different lengths, so each item can be matched to its file
    let mut paths: Vec<String> = (0..12).map(|i| write_wav(&dir.join(format!("clip{i:02}.wav")), 0.25 + i as f32 * 0.25)).collect();
    let missing = dir.join("not-there.wav").to_string_lossy().into_owned();
    paths.insert(5, missing.clone());
    let mut s = Session::default();
    let r = s.execute("file.import", json!({ "paths": paths })).unwrap();
    let items: Vec<u64> = r["items"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).collect();
    assert_eq!(items.len(), 12, "every readable file imported: {r}");
    let errors = r["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "one error, for the missing file: {r}");
    assert!(errors[0].as_str().unwrap().contains("not-there.wav"));
    let names: Vec<String> = items.iter().map(|i| s.project.item(ItemId(*i)).unwrap().name.clone()).collect();
    let expected: Vec<String> = (0..12).map(|i| format!("clip{i:02}.wav")).collect();
    assert_eq!(names, expected, "items in the order the paths were given");
    for (i, id) in items.iter().enumerate() {
        let d = s.project.item(ItemId(*id)).unwrap().duration().seconds();
        assert!((d - (0.25 + i as f64 * 0.25)).abs() < 0.01, "item {i} has its own file's duration ({d} s)");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_async_import_returns_at_once_and_adds_the_files_in_order_as_they_open() {
    // The Import dialog, the Media Browser and dropped files import this way: opening is the slow
    // part (some decoders take ~10 ms a clip, one at a time), and the command runs on the UI thread.
    let dir = crate::media_test_util::tmp_dir("import-batch-async");
    let mut paths: Vec<String> = (0..12).map(|i| write_wav(&dir.join(format!("clip{i:02}.wav")), 0.25 + i as f32 * 0.25)).collect();
    paths.insert(3, dir.join("not-there.wav").to_string_lossy().into_owned());
    let mut s = Session::default();
    let r = s.execute("file.import", json!({ "paths": paths, "async": true })).unwrap();
    assert_eq!(r["pending"], 13, "every media file opens in the background: {r}");
    assert!(r["items"].as_array().unwrap().is_empty(), "nothing is added before it is opened");
    let job = r["importJob"].as_u64().unwrap();
    assert!(s.jobs.iter().any(|j| j.id == job), "the import shows as a job");
    let started = std::time::Instant::now();
    while !s.background_imports.is_empty() && started.elapsed() < std::time::Duration::from_secs(30) {
        crate::background_import::poll(&mut s);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.background_imports.is_empty(), "the import finished");
    let names: Vec<String> = s
        .project
        .items
        .values()
        .filter(|it| it.name.starts_with("clip"))
        .map(|it| (it.id.0, it.name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect();
    let expected: Vec<String> = (0..12).map(|i| format!("clip{i:02}.wav")).collect();
    assert_eq!(names, expected, "added in the order the paths were given");
    assert!(s.log.entries.iter().any(|e| e.message.contains("not-there.wav")), "the missing file is reported");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "timing probe: how long a card's worth of imports takes, and where"]
fn probe_batch_import_time() {
    let dir = crate::media_test_util::tmp_dir("import-batch-probe");
    let paths: Vec<String> = (0..109).map(|i| write_wav(&dir.join(format!("clip{i:03}.wav")), 1.0)).collect();
    let mut s = Session::default();
    let t = std::time::Instant::now();
    s.execute("file.import", json!({ "paths": paths })).unwrap();
    println!("109 WAV imports: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = std::time::Instant::now();
    for p in &paths {
        let _ = crate::relink::identity_of(&*s.services, p).unwrap();
    }
    println!("109 identities alone: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let _ = std::fs::remove_dir_all(&dir);
}

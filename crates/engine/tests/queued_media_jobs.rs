//! A media job waiting for its turn holds no threads. Proxy jobs made their pool of one thread per
//! core and then waited in line inside it (#1831), so ten queued jobs on a 32-core machine held 320
//! idle threads. Its own test binary: the start hook it installs counts every background pool's
//! threads in the process.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use serde_json::{Value, json};

static STARTED: AtomicUsize = AtomicUsize::new(0);
static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Each background pool thread counts itself and waits for the gate: the first job's threads
/// hold it at the front of the line.
fn held() {
    STARTED.fetch_add(1, Ordering::SeqCst);
    let (open, cv) = &GATE;
    let mut open = open.lock().unwrap_or_else(PoisonError::into_inner);
    while !*open {
        open = cv.wait(open).unwrap_or_else(PoisonError::into_inner);
    }
}

fn open_gate() {
    let (open, cv) = &GATE;
    *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
    cv.notify_all();
}

/// Poll `jobs.list` until every job in `ids` has finished (or `limit` passes); their entries.
fn wait_for(s: &mut Session, ids: &[u64], limit: Duration) -> Vec<Value> {
    let t0 = Instant::now();
    loop {
        let jobs = s.execute("jobs.list", json!({})).unwrap();
        let mine: Vec<Value> = jobs.as_array().unwrap().iter().filter(|j| ids.iter().any(|id| j["id"] == json!(id))).cloned().collect();
        if (mine.len() == ids.len() && mine.iter().all(|j| j["finished"] == true)) || t0.elapsed() > limit {
            return mine;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn queued_proxy_jobs_start_no_threads_before_their_turn() {
    let dir = filmcraft_testkit::workspace_root().join("target").join("export-tests").join(format!("engine-queued-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let clip = dir.join("clip.mov").to_string_lossy().to_string();
    let r = s.execute("file.exportMedia", json!({"path": clip, "format": "mjpeg", "range": "custom", "startSeconds": 0, "endSeconds": 0.25})).unwrap();
    let made = wait_for(&mut s, &[r["job"].as_u64().unwrap()], Duration::from_secs(60));
    assert!(made.iter().all(|j| j["result"]["error"].is_null()), "{made:?}");

    filmcraft_export::register_background_thread_start(held);
    let r = s.execute("file.import", json!({"paths": [clip]})).unwrap();
    let item = r["items"][0].as_u64().unwrap();
    let jobs: Vec<u64> = (0..3)
        .map(|_| {
            let r = s.execute("media.createProxies", json!({"items": [item], "preset": "prores_proxy_half"})).unwrap();
            r["job"]["job"].as_u64().or_else(|| r["job"].as_u64()).unwrap_or_else(|| panic!("{r}"))
        })
        .collect();
    let t0 = Instant::now();
    while STARTED.load(Ordering::SeqCst) == 0 && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(500));
    let started = STARTED.load(Ordering::SeqCst);
    open_gate();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    assert!(started >= 1, "the first proxy job started no pool threads");
    assert!(started <= cores, "{started} pool threads for one running job and two waiting ({cores} cores)");
    let done = wait_for(&mut s, &jobs, Duration::from_secs(120));
    assert!(done.len() == 3 && done.iter().all(|j| j["finished"] == true && j["result"]["error"].is_null()), "{done:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

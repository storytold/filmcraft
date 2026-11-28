//! A proxy job cancelled while it waits in the media queue finishes without the background pool.
//! It went into the shared pool to record that it was cancelled, so with that pool busy (a long
//! export, or the only core of a one-core machine) it stayed "running", its reserved output files
//! on disk, until the pool was free. Its own test binary: it holds the process's shared
//! background pool.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use serde_json::{Value, json};

/// Holds every thread of the shared background pool ([`filmcraft_export::on_own_threads`]) until
/// dropped.
struct BackgroundPoolHeld(Arc<(Mutex<bool>, Condvar)>, Option<std::thread::JoinHandle<()>>);

impl BackgroundPoolHeld {
    fn new() -> Self {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let started = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(AtomicUsize::new(0));
        let (g, n, t) = (gate.clone(), started.clone(), threads.clone());
        let holder = std::thread::spawn(move || {
            filmcraft_export::on_own_threads(0, || {
                t.store(rayon::current_num_threads(), Ordering::SeqCst);
                rayon::broadcast(|_| {
                    n.fetch_add(1, Ordering::SeqCst);
                    let (open, cv) = &*g;
                    let mut open = open.lock().unwrap_or_else(PoisonError::into_inner);
                    while !*open {
                        open = cv.wait(open).unwrap_or_else(PoisonError::into_inner);
                    }
                });
            });
        });
        // made before the wait below, so a failed wait unwinds through its drop and lets them go
        let held = Self(gate, Some(holder));
        let t0 = Instant::now();
        while threads.load(Ordering::SeqCst) == 0 || started.load(Ordering::SeqCst) < threads.load(Ordering::SeqCst) {
            assert!(t0.elapsed() < Duration::from_secs(10), "the background pool's threads did not all start");
            std::thread::sleep(Duration::from_millis(1));
        }
        held
    }
}

impl Drop for BackgroundPoolHeld {
    fn drop(&mut self) {
        let (open, cv) = &*self.0;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
        if let Some(h) = self.1.take() {
            let _ = h.join();
        }
    }
}

/// Poll `jobs.list` until job `id` has finished (or `limit` passes); its entry.
fn wait_for(s: &mut Session, id: u64, limit: Duration) -> Value {
    let t0 = Instant::now();
    loop {
        let jobs = s.execute("jobs.list", json!({})).unwrap();
        let job = jobs.as_array().and_then(|a| a.iter().find(|j| j["id"] == id)).cloned().unwrap_or(Value::Null);
        if job["finished"] == true || t0.elapsed() > limit {
            return job;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every file under `dir`.
fn files(dir: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(e.path());
            } else {
                out.insert(e.path());
            }
        }
    }
    out
}

fn proxies(s: &mut Session, item: u64) -> u64 {
    let r = s.execute("media.createProxies", json!({"items": [item], "preset": "prores_proxy_half"})).unwrap();
    r["job"]["job"].as_u64().or_else(|| r["job"].as_u64()).unwrap_or_else(|| panic!("{r}"))
}

#[test]
fn a_proxy_job_cancelled_in_the_queue_finishes_while_the_background_pool_is_busy() {
    let dir = filmcraft_testkit::workspace_root().join("target").join("export-tests").join(format!("engine-cancelled-queued-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let clip = dir.join("clip.mov").to_string_lossy().to_string();
    let r = s.execute("file.exportMedia", json!({"path": clip, "format": "mjpeg", "range": "custom", "startSeconds": 0, "endSeconds": 0.25})).unwrap();
    let made = wait_for(&mut s, r["job"].as_u64().unwrap(), Duration::from_secs(60));
    assert!(made["result"]["error"].is_null(), "{made}");
    let item = s.execute("file.import", json!({"paths": [clip]})).unwrap()["items"][0].as_u64().unwrap();

    let held = BackgroundPoolHeld::new();
    // the first job takes its turn and waits for the held pool; the second waits behind it
    let first = proxies(&mut s, item);
    let before = files(&dir);
    let second = proxies(&mut s, item);
    let reserved: Vec<PathBuf> = files(&dir).difference(&before).cloned().collect();
    assert!(!reserved.is_empty(), "the queued job reserved no output");
    s.execute("jobs.cancel", json!({"job": second})).unwrap();
    let cancelled = wait_for(&mut s, second, Duration::from_secs(30));
    let left: Vec<&PathBuf> = reserved.iter().filter(|p| p.exists()).collect();
    drop(held);
    assert_eq!(cancelled["finished"], json!(true), "the cancelled job waited for the background pool: {cancelled}");
    assert!(cancelled["result"]["error"].as_str().is_some_and(|e| e.contains("cancelled")), "{cancelled}");
    assert!(left.is_empty(), "the cancelled job left its reserved outputs: {left:?}");

    let done = wait_for(&mut s, first, Duration::from_secs(120));
    assert!(done["finished"] == true && done["result"]["error"].is_null(), "{done}");
    let _ = std::fs::remove_dir_all(&dir);
}

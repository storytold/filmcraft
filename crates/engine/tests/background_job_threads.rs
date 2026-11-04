//! Background exports render on threads of their own. The monitors render their frames on the
//! global rayon pool: an export sharing it kept every thread busy, so the frames of a scrub or a
//! play during an export queued behind the export's work. With every thread of the global pool
//! held, an export started in the background must still finish.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use serde_json::{Value, json};

/// Holds every thread of the global rayon pool until dropped.
struct GlobalPoolHeld(Arc<(Mutex<bool>, Condvar)>);

impl GlobalPoolHeld {
    fn new() -> Self {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        // made first, so a failed start below unwinds through its drop and lets the threads go
        let held = Self(gate.clone());
        let started = Arc::new(AtomicUsize::new(0));
        let (g, n) = (gate.clone(), started.clone());
        rayon::spawn_broadcast(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            let (open, cv) = &*g;
            let mut open = open.lock().unwrap_or_else(PoisonError::into_inner);
            while !*open {
                open = cv.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
        });
        let t0 = Instant::now();
        while started.load(Ordering::SeqCst) < rayon::current_num_threads() {
            assert!(t0.elapsed() < Duration::from_secs(10), "the global pool's threads did not all start");
            std::thread::sleep(Duration::from_millis(1));
        }
        held
    }
}

impl Drop for GlobalPoolHeld {
    fn drop(&mut self) {
        let (open, cv) = &*self.0;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = filmcraft_testkit::workspace_root().join("target").join("export-tests").join(format!("engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

#[test]
fn a_background_export_does_not_wait_for_the_global_pool() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let dir = scratch("own-threads");
    let out = dir.join("Demo.mov").to_string_lossy().to_string();
    let held = GlobalPoolHeld::new();
    let r = s.execute("file.exportMedia", json!({"path": out, "format": "mjpeg", "range": "custom", "startSeconds": 0, "endSeconds": 0.25})).unwrap();
    let id = r["job"].as_u64().unwrap();
    let job = wait_for(&mut s, id, Duration::from_secs(60));
    drop(held);
    assert_eq!(job["finished"], json!(true), "the export waited for the global pool: {job}");
    assert!(job["result"]["error"].is_null(), "{job}");
    assert!(std::fs::metadata(&out).is_ok_and(|m| m.len() > 0), "no file written");
    let _ = std::fs::remove_dir_all(&dir);
}

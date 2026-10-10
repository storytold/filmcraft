//! The web build runs every job synchronously (`wait || cfg!(target_arch = "wasm32")`), so a job
//! body executes on wasm32 — where `std::time::Instant::now()` panics with "time not implemented
//! on this platform" and takes the whole app down (#230: Create Proxies and mask tracking both
//! did). `web_time::Instant` is `std::time::Instant` on native targets and `performance.now()`
//! on the web, so there is never a reason to name the std one in this crate.

use std::path::Path;

/// Every non-test source file of the crate, as (path, contents).
fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") || name == "media_test_util.rs" {
                continue;
            }
            out.push((p.display().to_string(), std::fs::read_to_string(&p).unwrap()));
        }
    }
    assert!(out.len() > 20, "found {} source files", out.len());
    out
}

#[test]
fn no_std_instant_in_code_the_web_build_runs() {
    let mut hits = Vec::new();
    for (path, text) in sources() {
        for (i, line) in text.lines().enumerate() {
            // `use std::time::{.., Instant, ..}` or a fully qualified `std::time::Instant`
            let names_std_instant = line.contains("std::time::Instant") || (line.contains("use std::time::") && line.contains("Instant"));
            if names_std_instant && !line.contains("web_time") && !line.trim_start().starts_with("//") {
                hits.push(format!("{path}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(hits.is_empty(), "std::time::Instant panics on wasm32; use web_time::Instant:\n{}", hits.join("\n"));
}

/// The clock the jobs use works here as well: a job's "Done in 0.0s" report needs an elapsed time.
#[test]
fn web_time_instant_measures_elapsed_time() {
    let t0 = web_time::Instant::now();
    let secs = t0.elapsed().as_secs_f64();
    assert!((0.0..60.0).contains(&secs), "{secs}");
}

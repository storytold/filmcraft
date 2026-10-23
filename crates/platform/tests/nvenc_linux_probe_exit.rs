//! Linux NVENC warm-probe shutdown stress: run each probe in a fresh process.
#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn warm_probe_survives_process_exit() {
    const CHILD: &str = "FILMCRAFT_NVENC_PROBE_EXIT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        filmcraft_platform::nvenc::warm_hevc_probe();
        // Give the detached thread time to enter the driver before process teardown.
        std::thread::sleep(std::time::Duration::from_millis(150));
        return;
    }
    if !filmcraft_platform::nvenc::available() {
        eprintln!("SKIPPED: no NVENC device");
        return;
    }
    let executable = std::env::current_exe().expect("test executable");
    for run in 0..20 {
        let mut child = Command::new(&executable)
            .args(["--exact", "warm_probe_survives_process_exit", "--nocapture"])
            .env(CHILD, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn probe process");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().expect("poll probe process") {
                assert!(status.success(), "probe process {run} failed: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("stop hung probe process");
                let _ = child.wait();
                panic!("probe process {run} hung during shutdown");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

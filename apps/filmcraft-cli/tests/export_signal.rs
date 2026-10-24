//! A signal during a blocking export cancels it: the encoder stops, its temporary output and the
//! unpublished movie are removed, and the CLI exits 130 (SIGINT) / 143 (SIGTERM). Previous output stays.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The temporary files an export writes beside its output (`.filmcraft-export-<pid>-<n>.tmp`).
fn temporaries(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(".filmcraft-export-")).collect()
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("wait for filmcraft-cli") {
            return Some(status);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts `export` of the demo project, sends `signal` once the encoder has its temporary output,
/// and checks the exit status, stderr, temporary cleanup and preservation of any previous output.
fn export_interrupted_by(signal: &str, status: i32, existing: bool) {
    let dir: PathBuf = std::env::temp_dir().join(format!("fc-cli-signal-{signal}-{existing}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    let out = dir.join("out.mp4");
    let previous = b"previous export must survive cancellation";
    if existing {
        std::fs::write(&out, previous).expect("write previous output");
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
        .args(["--demo", "export", out.to_str().expect("utf-8 path")])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn filmcraft-cli");

    let start = Instant::now();
    while temporaries(&dir).is_empty() {
        assert!(child.try_wait().expect("try_wait").is_none(), "the export finished before the signal");
        assert!(start.elapsed() < Duration::from_secs(60), "no temporary output after 60 s");
        std::thread::sleep(Duration::from_millis(10));
    }
    let sent = Command::new("kill").args([&format!("-{signal}"), &child.id().to_string()]).status().expect("run kill");
    assert!(sent.success(), "kill -{signal} failed");

    let exit = wait_for_exit(&mut child, Duration::from_secs(30));
    let Some(exit) = exit else {
        let _ = child.kill();
        panic!("filmcraft-cli did not exit after SIG{signal}");
    };
    let stderr = child.stderr.take().map(|mut e| {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut e, &mut s);
        s
    });
    assert_eq!(exit.code(), Some(status), "exit status after SIG{signal}; stderr: {stderr:?}");
    assert!(stderr.as_deref().unwrap_or_default().contains("export interrupted"), "stderr: {stderr:?}");
    assert_eq!(temporaries(&dir), Vec::<String>::new(), "temporary output left behind");
    if existing {
        assert_eq!(std::fs::read(&out).expect("previous output kept"), previous);
    } else {
        assert!(!out.exists(), "a cancelled export must not leave its output");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sigint_cancels_export_and_removes_temporary_output() {
    export_interrupted_by("INT", 130, false);
}

#[test]
fn sigterm_cancels_export_and_removes_temporary_output() {
    export_interrupted_by("TERM", 143, false);
}

#[test]
fn cancellation_keeps_the_previous_output() {
    export_interrupted_by("INT", 130, true);
}

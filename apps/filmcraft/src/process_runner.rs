//! Starts the external programs the engine asks for (the Lettering attendant). The only place in
//! the product that starts a process (AGENTS.md §0): the engine owns the [`ProcessRunner`] trait and
//! the desktop app installs this implementation.
//!
//! Rules: the program and every argument are handed to the OS as they are (`Command::arg`, never a
//! shell), stdin is closed, stdout and stderr are drained on their own threads and kept only up to
//! `max_output` bytes, and the run ends at the timeout or when `cancel` turns true (the program is
//! killed). On Unix the program leads its own process group so that what it started (the attendant
//! starts the animation app's command line) is stopped with it.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use filmcraft_engine::lettering_fx::{ProcessError, ProcessOutput, ProcessRunner, ProcessSpec};

pub struct OsRunner;

/// Keeps up to `cap` bytes of `r` and drains the rest, so the program never blocks on a full pipe.
fn drain(mut r: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut cut = false;
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                let take = n.min(room);
                if let Some(chunk) = buf.get(..take) {
                    kept.extend_from_slice(chunk);
                }
                cut |= take < n;
            }
        }
    }
    (kept, cut)
}

fn reader(r: Option<impl Read + Send + 'static>, cap: usize) -> Option<JoinHandle<(Vec<u8>, bool)>> {
    r.and_then(|r| std::thread::Builder::new().name("filmcraft-process-io".into()).spawn(move || drain(r, cap)).ok())
}

fn collect(h: Option<JoinHandle<(Vec<u8>, bool)>>) -> (String, bool) {
    let (bytes, cut) = h.and_then(|h| h.join().ok()).unwrap_or_default();
    (String::from_utf8_lossy(&bytes).into_owned(), cut)
}

/// Stops the program and what it started. Never fails loudly: the run is over either way.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // the program leads a process group of its own (see `run`); `kill -- -PID` stops the group
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{}", child.id()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        // no process groups here: `taskkill /T` stops the program and what it started
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID"])
            .arg(child.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl ProcessRunner for OsRunner {
    fn run(&self, spec: &ProcessSpec, cancel: &AtomicBool) -> Result<ProcessOutput, ProcessError> {
        if spec.program.is_empty() || spec.program.contains('\0') || spec.args.iter().any(|a| a.contains('\0')) {
            return Err(ProcessError::Io("the program or an argument contains a NUL".into()));
        }
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ProcessError::NotFound(spec.program.clone()),
            _ => ProcessError::Io(format!("cannot start {}: {e}", spec.program)),
        })?;
        let out = reader(child.stdout.take(), spec.max_output);
        let err = reader(child.stderr.take(), spec.max_output);
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {}
                Err(e) => {
                    kill_tree(&mut child);
                    return Err(ProcessError::Io(e.to_string()));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                kill_tree(&mut child);
                return Err(ProcessError::Cancelled);
            }
            if start.elapsed() >= spec.timeout {
                kill_tree(&mut child);
                return Err(ProcessError::Timeout(spec.timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let ((stdout, cut_out), (stderr, cut_err)) = (collect(out), collect(err));
        Ok(ProcessOutput { code: status.code(), stdout, stderr, truncated: cut_out || cut_err })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec(program: &str, args: &[&str], timeout: Duration) -> ProcessSpec {
        ProcessSpec { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), timeout, max_output: 1024 }
    }

    const SECS: Duration = Duration::from_secs(10);

    #[test]
    fn a_missing_program_is_not_found_not_a_panic() {
        let r = OsRunner.run(&spec("/nonexistent/filmcraft-no-such-program", &[], SECS), &AtomicBool::new(false));
        assert!(matches!(r, Err(ProcessError::NotFound(_))), "{r:?}");
        let r = OsRunner.run(&spec("", &[], SECS), &AtomicBool::new(false));
        assert!(matches!(r, Err(ProcessError::Io(_))), "{r:?}");
        let r = OsRunner.run(&spec("echo", &["a\0b"], SECS), &AtomicBool::new(false));
        assert!(matches!(r, Err(ProcessError::Io(_))), "{r:?}");
    }

    /// `sh -c 'printf %s "$1"' sh ARG` prints ARG exactly: spaces, a leading dash, Unicode and shell
    /// metacharacters arrive as one argument and are never interpreted.
    #[test]
    fn arguments_arrive_verbatim() {
        for arg in ["a b/c d", "-x", "--version", "ação ñ 日本語", "$(echo pwned); `id` | * ?", "'quoted' \"double\"", ""] {
            let o = OsRunner.run(&spec("sh", &["-c", "printf %s \"$1\"", "sh", arg], SECS), &AtomicBool::new(false)).unwrap();
            assert_eq!(o.code, Some(0));
            assert_eq!(o.stdout, arg);
        }
    }

    #[test]
    fn a_path_with_spaces_and_unicode_can_be_the_program() {
        let dir = std::env::temp_dir().join(format!("fc runner ação -x {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("-meu programa.sh");
        std::fs::write(&prog, "#!/bin/sh\nprintf ok\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let o = OsRunner.run(&spec(&prog.to_string_lossy(), &[], SECS), &AtomicBool::new(false)).unwrap();
        assert_eq!((o.code, o.stdout.as_str()), (Some(0), "ok"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exit_codes_and_stderr_are_reported() {
        let o = OsRunner.run(&spec("sh", &["-c", "echo oops >&2; exit 7"], SECS), &AtomicBool::new(false)).unwrap();
        assert_eq!(o.code, Some(7));
        assert_eq!(o.stderr.trim(), "oops");
    }

    #[test]
    fn long_output_is_truncated_and_does_not_block_the_program() {
        let o = OsRunner
            .run(&spec("sh", &["-c", "head -c 2000000 /dev/zero | tr '\\0' x; head -c 2000000 /dev/zero | tr '\\0' y >&2"], SECS), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(o.code, Some(0));
        assert_eq!((o.stdout.len(), o.stderr.len()), (1024, 1024));
        assert!(o.truncated);
    }

    #[test]
    fn output_that_is_not_utf8_is_kept_lossily() {
        let o = OsRunner.run(&spec("sh", &["-c", "printf '\\377\\376ok'"], SECS), &AtomicBool::new(false)).unwrap();
        assert!(o.stdout.ends_with("ok"));
    }

    #[test]
    fn the_timeout_kills_the_program() {
        let t = Instant::now();
        let r = OsRunner.run(&spec("sleep", &["30"], Duration::from_millis(150)), &AtomicBool::new(false));
        assert!(matches!(r, Err(ProcessError::Timeout(_))), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_timeout_kills_what_the_program_started() {
        let dir = std::env::temp_dir().join(format!("fc timeout {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("grandchild-survived");
        let script = format!("(sleep 1; touch '{}') & wait", marker.display());
        let r = OsRunner.run(&spec("sh", &["-c", &script], Duration::from_millis(200)), &AtomicBool::new(false));
        assert!(matches!(r, Err(ProcessError::Timeout(_))), "{r:?}");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "the grandchild must die with the program");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancel_kills_the_program_and_what_it_started() {
        let dir = std::env::temp_dir().join(format!("fc cancel {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("grandchild-survived");
        let script = format!("(sleep 1; touch '{}') & wait", marker.display());
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.store(true, Ordering::Relaxed);
        });
        let r = OsRunner.run(&spec("sh", &["-c", &script], SECS), &cancel);
        let _ = h.join();
        assert!(matches!(r, Err(ProcessError::Cancelled)), "{r:?}");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "the grandchild must die with the program");
        let _ = std::fs::remove_dir_all(dir);
    }
}

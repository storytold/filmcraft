//! The desktop binary's command-line paths that end before a window opens (`--version`, `--help`,
//! usage errors), run as a child process.

use std::process::{Command, Output, Stdio};

/// Runs `filmcraft` with `args`, stderr piped and stdout a pipe whose read end is closed before the
/// child starts, so its first write always finds no reader. Skipped (returns `None`) for a Windows release build: there the binary
/// has no console (`windows_subsystem = "windows"`) and shows `--version` in a message box too,
/// which would wait for a click.
fn run_with_closed_stdout(args: &[&str]) -> Option<Output> {
    if cfg!(all(windows, not(debug_assertions))) {
        eprintln!("SKIPPED (closed stdout, {args:?}): Windows release build shows a message box");
        return None;
    }
    // With the reader gone before the spawn, the child's write fails every time: Windows reports
    // "The pipe is being closed. (os error 232)", Unix "Broken pipe (os error 32)".
    let (reader, writer) = std::io::pipe().ok()?;
    drop(reader);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_filmcraft"));
    cmd.args(args).env_remove("FILMCRAFT_CONTROL_PORT").stdin(Stdio::null()).stdout(writer).stderr(Stdio::piped());
    let child = cmd.spawn().ok()?;
    // `cmd` still holds our copy of the write end; drop it so only the child has one.
    drop(cmd);
    child.wait_with_output().ok()
}

/// `filmcraft.exe --version` run from PowerShell with its output captured panicked in `print!`
/// ("thread 'main' panicked at library/std/src/io/stdio.rs: failed printing to stdout: The pipe is
/// being closed. (os error 232)") and exited 101 instead of 0. Writing to a pipe nobody reads must
/// lose the text and nothing else: the exit status stays and nothing panics.
#[test]
fn version_and_help_with_closed_stdout_exit_cleanly() {
    for args in [&["--version"][..], &["--help"], &["-h"]] {
        let Some(out) = run_with_closed_stdout(args) else { return };
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {:?}: {err}", out.status);
        assert!(!err.contains("panicked"), "{args:?} panicked: {err}");
    }
}

/// A usage error prints to stderr, so a closed stdout changes nothing for it: still exit 2, still
/// the message, no panic.
#[test]
fn usage_error_with_closed_stdout_keeps_its_exit_code() {
    let Some(out) = run_with_closed_stdout(&["--no-such-option"]) else { return };
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{:?}: {err}", out.status);
    assert!(!err.contains("panicked"), "panicked: {err}");
    assert!(err.contains("--no-such-option") && err.contains("Usage:"), "{err}");
}

/// With a reader, `--version` prints the version and nothing on stderr (the baseline the closed-pipe
/// tests weaken).
#[test]
fn version_prints_the_package_version() {
    if cfg!(all(windows, not(debug_assertions))) {
        eprintln!("SKIPPED (version_prints_the_package_version): Windows release build shows a message box");
        return;
    }
    let Ok(out) = Command::new(env!("CARGO_BIN_EXE_filmcraft")).arg("--version").env_remove("FILMCRAFT_CONTROL_PORT").output() else {
        panic!("could not spawn filmcraft")
    };
    assert_eq!(out.status.code(), Some(0), "{:?}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("filmcraft {}\n", env!("CARGO_PKG_VERSION")));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

//! Swap-at-relaunch with a `.prev` backup, and automatic rollback of a release that will not start.
//!
//! Lifecycle (all files live next to the executable, so every rename stays on one filesystem):
//!
//! 1. [`apply`] runs in the helper process after the app has exited. For each binary it copies the
//!    staged file to `<name>.new`, fsyncs it, then renames `<name>` → `<name>.prev` and
//!    `<name>.new` → `<name>` (atomic). If anything fails midway, every swap already done is undone.
//!    It then writes `<exe>.update_state` as *unconfirmed* with zero boots.
//! 2. [`on_boot`] runs first thing in every start. An unconfirmed state counts the boot. When two
//!    earlier boots of the new version never reached [`confirm`] (it crashed or hung twice), the
//!    third start restores every `.prev`, keeps the bad build as `.broken` and reports
//!    [`BootOutcome::RolledBack`].
//! 3. [`confirm`] is called once the new version has run healthily (30 s, or its UI is up). The
//!    `.prev` backups are removed [`PREV_KEEP_SECS`] later by [`cleanup_old_backups`].

use crate::extract::sync_dir;
use crate::{Result, UpdateError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Unconfirmed boots tolerated before the next start rolls back.
pub const MAX_UNCONFIRMED_BOOTS: u32 = 2;
/// How long `.prev` is kept after a confirmed update.
pub const PREV_KEEP_SECS: u64 = 7 * 24 * 3600;
/// Companion binary updated together with the app when it sits next to it.
pub const CLI_NAME: &str = "filmcraft-cli";

/// One binary to replace: the staged file and where it goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedFile {
    pub src: PathBuf,
    pub dst: PathBuf,
}

/// A fully downloaded, verified and smoke-tested update, ready for [`apply`]. Saved as
/// `staged.json` in the staging directory so the helper process can pick it up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    /// The new version.
    pub version: String,
    /// The version being replaced.
    pub from: String,
    pub files: Vec<StagedFile>,
}

impl Staged {
    /// File name of the manifest inside a staging directory.
    pub const MANIFEST: &'static str = "staged.json";

    pub fn save(&self, dir: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| UpdateError::Metadata(e.to_string()))?;
        write_atomic(&dir.join(Self::MANIFEST), &json)
    }

    pub fn load(dir: &Path) -> Result<Staged> {
        let bytes = std::fs::read(dir.join(Self::MANIFEST))?;
        serde_json::from_slice(&bytes).map_err(|e| UpdateError::Metadata(format!("{}: {e}", Self::MANIFEST)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Unconfirmed,
    Confirmed,
}

/// Contents of `<exe>.update_state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootState {
    pub status: Status,
    /// Starts of the new version that have not been confirmed yet.
    pub boots: u32,
    pub from: String,
    pub to: String,
    /// Every binary that was swapped (to restore them all on rollback).
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub confirmed_at: Option<u64>,
}

/// What [`on_boot`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootOutcome {
    /// No update in flight (or a confirmed one).
    Normal,
    /// First or second start of a freshly updated version, not yet confirmed.
    Unconfirmed { from: String, to: String },
    /// The new version failed to start twice and the previous one was restored.
    RolledBack { failed: String, restored: String },
}

/// `<exe>.update_state`.
pub fn state_path(exe: &Path) -> PathBuf {
    sibling(exe, ".update_state")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Write via a temp file and rename so a reader never sees half a file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = sibling(path, ".tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

fn read_state(exe: &Path) -> Option<BootState> {
    let bytes = std::fs::read(state_path(exe)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("ignoring unreadable update state: {e}");
            None
        }
    }
}

fn write_state(exe: &Path, state: &BootState) -> Result<()> {
    let json = serde_json::to_vec_pretty(state).map_err(|e| UpdateError::Metadata(e.to_string()))?;
    write_atomic(&state_path(exe), &json)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Copy `src` to `dst`, fsync it and make it executable.
fn copy_durable(src: &Path, dst: &Path) -> Result<()> {
    let _ = std::fs::remove_file(dst);
    std::fs::copy(src, dst)?;
    std::fs::File::open(dst)?.sync_all()?;
    make_executable(dst)
}

/// Only the app and its CLI, in the app's own directory, may be replaced.
fn validate(staged: &Staged, exe: &Path) -> Result<()> {
    let dir = exe.parent().ok_or_else(|| UpdateError::Unsupported("the executable has no directory".into()))?;
    let exe_name = exe.file_name().ok_or_else(|| UpdateError::Unsupported("the executable has no name".into()))?;
    if staged.files.is_empty() {
        return Err(UpdateError::Unsupported("nothing was staged".into()));
    }
    for f in &staged.files {
        let name = f.dst.file_name().ok_or_else(|| UpdateError::Unsupported("bad target path".into()))?;
        if f.dst.parent() != Some(dir) || !(name == exe_name || name == CLI_NAME) {
            return Err(UpdateError::Unsupported(format!("refusing to replace {}", f.dst.display())));
        }
        if !f.src.is_file() {
            return Err(UpdateError::Unsupported(format!("staged file {} is missing", f.src.display())));
        }
    }
    if !staged.files.iter().any(|f| f.dst == exe) {
        return Err(UpdateError::Unsupported("the staged update does not include the application".into()));
    }
    Ok(())
}

/// Replace the binaries of `staged`, keeping each old one as `<name>.prev`. Call it after the app
/// has exited. On failure the previous binaries are back in place and the error says why.
pub fn apply(staged: &Staged, exe: &Path) -> Result<()> {
    validate(staged, exe)?;
    if read_state(exe).is_some_and(|s| s.status == Status::Unconfirmed) {
        return Err(UpdateError::Unsupported("the previous update has not been confirmed yet".into()));
    }
    // Prepare every `<name>.new` first, so the swap itself is a handful of renames.
    let mut prepared: Vec<(PathBuf, PathBuf)> = Vec::new();
    for f in &staged.files {
        let new = sibling(&f.dst, ".new");
        if let Err(e) = copy_durable(&f.src, &new) {
            for (_, n) in &prepared {
                let _ = std::fs::remove_file(n);
            }
            let _ = std::fs::remove_file(&new);
            return Err(e);
        }
        prepared.push((f.dst.clone(), new));
    }
    let mut done: Vec<(PathBuf, bool)> = Vec::new();
    for (dst, new) in &prepared {
        let prev = sibling(dst, ".prev");
        let had_old = dst.exists();
        let step = (|| -> Result<()> {
            if had_old {
                std::fs::rename(dst, &prev)?;
            }
            if let Err(e) = std::fs::rename(new, dst) {
                if had_old {
                    let _ = std::fs::rename(&prev, dst);
                }
                return Err(e.into());
            }
            Ok(())
        })();
        match step {
            Ok(()) => done.push((dst.clone(), had_old)),
            Err(e) => {
                for (d, had) in done.iter().rev() {
                    if *had {
                        let _ = std::fs::rename(sibling(d, ".prev"), d);
                    } else {
                        let _ = std::fs::remove_file(d);
                    }
                }
                for (_, n) in &prepared {
                    let _ = std::fs::remove_file(n);
                }
                return Err(e);
            }
        }
    }
    if let Some(dir) = exe.parent() {
        sync_dir(dir);
    }
    let state = BootState {
        status: Status::Unconfirmed,
        boots: 0,
        from: staged.from.clone(),
        to: staged.version.clone(),
        files: staged.files.iter().map(|f| f.dst.clone()).collect(),
        confirmed_at: None,
    };
    write_state(exe, &state)
}

/// Call at the very start of every run. Counts unconfirmed boots and rolls back after two failures.
pub fn on_boot(exe: &Path) -> Result<BootOutcome> {
    let Some(mut state) = read_state(exe) else { return Ok(BootOutcome::Normal) };
    if state.status == Status::Confirmed {
        return Ok(BootOutcome::Normal);
    }
    if state.boots >= MAX_UNCONFIRMED_BOOTS {
        let outcome = BootOutcome::RolledBack { failed: state.to.clone(), restored: state.from.clone() };
        let result = rollback(&state, exe);
        // Whatever happened, stop retrying: a state we cannot act on must not loop forever.
        let _ = std::fs::remove_file(state_path(exe));
        result?;
        return Ok(outcome);
    }
    state.boots += 1;
    write_state(exe, &state)?;
    Ok(BootOutcome::Unconfirmed { from: state.from, to: state.to })
}

/// Restore every `.prev` over its binary; the failed build is kept as `.broken` for inspection.
fn rollback(state: &BootState, exe: &Path) -> Result<()> {
    let dir = exe.parent().ok_or_else(|| UpdateError::Unsupported("the executable has no directory".into()))?;
    let mut restored_main = false;
    for file in &state.files {
        if file.parent() != Some(dir) {
            continue; // a tampered state file must not make us rename things elsewhere
        }
        let prev = sibling(file, ".prev");
        if !prev.exists() {
            continue;
        }
        let broken = sibling(file, ".broken");
        let _ = std::fs::remove_file(&broken);
        if file.exists() {
            std::fs::rename(file, &broken)?;
        }
        if let Err(e) = std::fs::rename(&prev, file) {
            let _ = std::fs::rename(&broken, file);
            return Err(e.into());
        }
        restored_main |= file == exe;
    }
    sync_dir(dir);
    if restored_main { Ok(()) } else { Err(UpdateError::Unsupported("the previous version is no longer available to restore".into())) }
}

/// Mark the running new version as healthy.
pub fn confirm(exe: &Path) -> Result<()> {
    let Some(mut state) = read_state(exe) else { return Ok(()) };
    if state.status == Status::Confirmed {
        return Ok(());
    }
    state.status = Status::Confirmed;
    state.boots = 0;
    state.confirmed_at = Some(now_secs());
    write_state(exe, &state)
}

/// Delete `.prev` / `.broken` and the state file once a confirmed update is older than [`PREV_KEEP_SECS`].
pub fn cleanup_old_backups(exe: &Path) {
    let Some(state) = read_state(exe) else { return };
    let (Status::Confirmed, Some(at)) = (state.status, state.confirmed_at) else { return };
    if now_secs().saturating_sub(at) < PREV_KEEP_SECS {
        return;
    }
    for file in &state.files {
        let _ = std::fs::remove_file(sibling(file, ".prev"));
        let _ = std::fs::remove_file(sibling(file, ".broken"));
    }
    let _ = std::fs::remove_file(state_path(exe));
}

/// Wait until process `pid` has exited, up to `timeout`. Returns whether it has. On Linux replacing a
/// binary that is still running is harmless (the old inode stays alive), so callers continue anyway.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    if !Path::new("/proc/self").exists() {
        std::thread::sleep(timeout.min(Duration::from_secs(1)));
        return true;
    }
    let start = std::time::Instant::now();
    while proc_dir.exists() {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Start `exe` again with `args`, detached from the helper's terminal.
pub fn relaunch(exe: &Path, args: &[std::ffi::OsString]) -> Result<()> {
    std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        dir: tempfile::TempDir,
        exe: PathBuf,
        cli: PathBuf,
        staged: Staged,
    }

    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("filmcraft");
        let cli = dir.path().join(CLI_NAME);
        std::fs::write(&exe, b"app v1").unwrap();
        std::fs::write(&cli, b"cli v1").unwrap();
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("filmcraft"), b"app v2").unwrap();
        std::fs::write(stage.join(CLI_NAME), b"cli v2").unwrap();
        let staged = Staged {
            version: "0.7.3".into(),
            from: "0.7.2".into(),
            files: vec![StagedFile { src: stage.join("filmcraft"), dst: exe.clone() }, StagedFile { src: stage.join(CLI_NAME), dst: cli.clone() }],
        };
        Fx { dir, exe, cli, staged }
    }

    fn read(p: &Path) -> String {
        String::from_utf8(std::fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn apply_swaps_keeps_prev_and_marks_unconfirmed() {
        let fx = fixture();
        apply(&fx.staged, &fx.exe).unwrap();
        assert_eq!(read(&fx.exe), "app v2");
        assert_eq!(read(&fx.cli), "cli v2");
        assert_eq!(read(&sibling(&fx.exe, ".prev")), "app v1");
        assert_eq!(read(&sibling(&fx.cli, ".prev")), "cli v1");
        let s = read_state(&fx.exe).unwrap();
        assert_eq!((s.status, s.boots, s.from.as_str(), s.to.as_str()), (Status::Unconfirmed, 0, "0.7.2", "0.7.3"));
        assert!(!sibling(&fx.exe, ".new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&fx.exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn a_release_that_never_confirms_is_rolled_back_on_the_third_start() {
        let fx = fixture();
        apply(&fx.staged, &fx.exe).unwrap();
        assert_eq!(on_boot(&fx.exe).unwrap(), BootOutcome::Unconfirmed { from: "0.7.2".into(), to: "0.7.3".into() });
        assert_eq!(on_boot(&fx.exe).unwrap(), BootOutcome::Unconfirmed { from: "0.7.2".into(), to: "0.7.3".into() });
        assert_eq!(read(&fx.exe), "app v2", "two failed starts are tolerated");
        assert_eq!(on_boot(&fx.exe).unwrap(), BootOutcome::RolledBack { failed: "0.7.3".into(), restored: "0.7.2".into() });
        assert_eq!(read(&fx.exe), "app v1");
        assert_eq!(read(&fx.cli), "cli v1");
        assert_eq!(read(&sibling(&fx.exe, ".broken")), "app v2");
        assert!(!state_path(&fx.exe).exists());
        assert_eq!(on_boot(&fx.exe).unwrap(), BootOutcome::Normal, "and it does not loop");
    }

    #[test]
    fn a_confirmed_release_is_kept() {
        let fx = fixture();
        apply(&fx.staged, &fx.exe).unwrap();
        on_boot(&fx.exe).unwrap();
        confirm(&fx.exe).unwrap();
        for _ in 0..5 {
            assert_eq!(on_boot(&fx.exe).unwrap(), BootOutcome::Normal);
        }
        assert_eq!(read(&fx.exe), "app v2");
        assert!(sibling(&fx.exe, ".prev").exists(), "the backup stays for a week");
    }

    #[test]
    fn old_backups_are_removed_after_a_week_only() {
        let fx = fixture();
        apply(&fx.staged, &fx.exe).unwrap();
        confirm(&fx.exe).unwrap();
        cleanup_old_backups(&fx.exe);
        assert!(sibling(&fx.exe, ".prev").exists());
        let mut s = read_state(&fx.exe).unwrap();
        s.confirmed_at = Some(now_secs() - PREV_KEEP_SECS - 10);
        write_state(&fx.exe, &s).unwrap();
        cleanup_old_backups(&fx.exe);
        assert!(!sibling(&fx.exe, ".prev").exists() && !sibling(&fx.cli, ".prev").exists() && !state_path(&fx.exe).exists());
        assert_eq!(read(&fx.exe), "app v2");
    }

    #[test]
    fn failed_swap_restores_everything() {
        let fx = fixture();
        // The CLI's staged file vanishes between validation and use: the whole apply must fail cleanly.
        let mut staged = fx.staged.clone();
        staged.files[1].src = fx.dir.path().join("stage/missing");
        assert!(apply(&staged, &fx.exe).is_err());
        assert_eq!((read(&fx.exe).as_str(), read(&fx.cli).as_str()), ("app v1", "cli v1"));
        assert!(read_state(&fx.exe).is_none());
        // A mid-swap failure: make the CLI's `.prev` slot a non-empty directory so its rename fails.
        std::fs::create_dir_all(sibling(&fx.cli, ".prev").join("x")).unwrap();
        assert!(apply(&fx.staged, &fx.exe).is_err());
        assert_eq!((read(&fx.exe).as_str(), read(&fx.cli).as_str()), ("app v1", "cli v1"), "the app swap was undone");
        assert!(!sibling(&fx.exe, ".new").exists() && !sibling(&fx.cli, ".new").exists());
    }

    #[test]
    fn refuses_foreign_targets_and_stacked_updates() {
        let fx = fixture();
        let mut evil = fx.staged.clone();
        evil.files.push(StagedFile { src: fx.dir.path().join("stage/filmcraft"), dst: PathBuf::from("/etc/passwd") });
        assert!(matches!(apply(&evil, &fx.exe), Err(UpdateError::Unsupported(_))));
        let mut other_name = fx.staged.clone();
        other_name.files.push(StagedFile { src: fx.dir.path().join("stage/filmcraft"), dst: fx.dir.path().join("bash") });
        assert!(matches!(apply(&other_name, &fx.exe), Err(UpdateError::Unsupported(_))));
        apply(&fx.staged, &fx.exe).unwrap();
        assert!(matches!(apply(&fx.staged, &fx.exe), Err(UpdateError::Unsupported(_))), "no second update before the first is confirmed");
    }

    #[test]
    fn tampered_state_cannot_redirect_the_rollback() {
        let fx = fixture();
        apply(&fx.staged, &fx.exe).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        std::fs::write(sibling(&victim, ".prev"), b"planted").unwrap();
        let mut s = read_state(&fx.exe).unwrap();
        s.boots = MAX_UNCONFIRMED_BOOTS;
        s.files.push(victim.clone());
        write_state(&fx.exe, &s).unwrap();
        assert!(matches!(on_boot(&fx.exe).unwrap(), BootOutcome::RolledBack { .. }));
        assert_eq!(read(&victim), "keep");
    }

    #[test]
    fn corrupt_state_file_means_no_update_in_flight() {
        let fx = fixture();
        std::fs::write(state_path(&fx.exe), b"{ not json").unwrap();
        assert_eq!(on_boot(&fx.exe).unwrap(), BootOutcome::Normal);
    }

    #[test]
    fn staged_manifest_round_trips() {
        let fx = fixture();
        let d = fx.dir.path().join("stage");
        fx.staged.save(&d).unwrap();
        assert_eq!(Staged::load(&d).unwrap(), fx.staged);
    }

    #[test]
    fn wait_for_exit_notices_a_dead_pid() {
        // PID 0x7fff_fff0 is not a running process.
        assert!(wait_for_exit(0x7fff_fff0, Duration::from_secs(1)));
    }
}

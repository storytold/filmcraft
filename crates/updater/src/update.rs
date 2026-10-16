//! The update flow the app drives: check → stage (download, verify, extract, smoke-test) → apply.
//!
//! Nothing here replaces the running program. [`Updater::stage`] only fills the cache directory;
//! [`crate::swap::apply`] does the replacing, after the app has exited.

use crate::extract::{self, Limits};
use crate::install::{self, InstallKind, Target, UpdateLock};
use crate::release::Release;
use crate::swap::{CLI_NAME, Staged, StagedFile};
use crate::{Result, UpdateError, net};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// The installed version is the newest published one (or newer).
    UpToDate,
    /// A newer stable release exists and has a build for this machine.
    Available(Release),
    /// Checked too recently; nothing was requested.
    Throttled,
}

/// `~/.config/filmcraft/update_state.json`: when we last asked, so checks stay at most daily.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckState {
    /// Unix seconds of the last completed check.
    pub last_check: u64,
    /// Newest version seen at that check.
    pub latest_seen: Option<String>,
}

impl CheckState {
    /// Read the state; a missing or unreadable file means "never checked".
    pub fn load(path: &Path) -> CheckState {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(|e| UpdateError::Metadata(e.to_string()))?;
        crate::swap::write_atomic(path, &json)
    }

    /// Whether `interval` has passed since the last check. Never having checked, or a clock set
    /// back in time, counts as due.
    pub fn is_due(&self, now: u64, interval: Duration) -> bool {
        self.last_check == 0 || now < self.last_check || now - self.last_check >= interval.as_secs()
    }
}

/// Drives one update for one installation.
#[derive(Debug, Clone)]
pub struct Updater {
    /// The running version (`env!("CARGO_PKG_VERSION")` of the app).
    pub current: Version,
    /// The canonical path of the running executable, captured at startup (before any swap makes
    /// Linux report `… (deleted)`).
    pub exe: PathBuf,
    /// Where downloads and staged files go (`~/.cache/filmcraft/updates`).
    pub cache_dir: PathBuf,
}

impl Updater {
    /// Whether this installation may replace itself, or why it must not.
    pub fn install_kind(&self) -> InstallKind {
        install::detect(&self.exe, &|k| std::env::var(k).ok())
    }

    /// Ask GitHub for the newest release. With `state` = `Some((path, now_unix, interval))` the
    /// request is skipped when the last one was recent. Strictly newer versions only: a lower or
    /// equal tag (a rollback, a replay of an old page) is "up to date".
    pub fn check(&self, throttle: Option<(&Path, u64, Duration)>) -> Result<Availability> {
        let Some(target) = Target::current() else {
            return Err(UpdateError::Unsupported(
                "automatic updates are only available for Linux builds so far; download the installer from the releases page".into(),
            ));
        };
        let mut state = None;
        if let Some((path, now, interval)) = throttle {
            let s = CheckState::load(path);
            if !s.is_due(now, interval) {
                return Ok(Availability::Throttled);
            }
            state = Some((path, now, s));
        }
        let latest = net::latest_version()?;
        if let Some((path, now, mut s)) = state {
            s.last_check = now;
            s.latest_seen = Some(latest.to_string());
            if let Err(e) = s.save(path) {
                log::warn!("could not record the update check: {e}");
            }
        }
        if latest <= self.current {
            return Ok(Availability::UpToDate);
        }
        Ok(Availability::Available(net::fetch_release(&latest, &target)?))
    }

    /// Download `release`, verify it, extract it, prove the new binary starts here, and write
    /// `staged.json`. Returns the staging directory (pass it to the helper as `--staged`).
    ///
    /// Fails without touching the installation if the install is package-managed or read-only, the
    /// checksum is wrong, the archive is unsafe, or the new binary cannot run on this machine
    /// (for example, a glibc newer than this system's).
    pub fn stage(&self, release: &Release, progress: &mut dyn FnMut(u64, Option<u64>) -> bool) -> Result<PathBuf> {
        match self.install_kind() {
            InstallKind::SelfManaged => {}
            InstallKind::Managed(who) => return Err(UpdateError::Unsupported(format!("this copy is managed by {who}"))),
            InstallKind::ReadOnly => return Err(UpdateError::Unsupported("the install directory is not writable".into())),
        }
        if release.version <= self.current {
            return Err(UpdateError::Unsupported(format!("{} is not newer than the installed {}", release.version, self.current)));
        }
        let _lock = UpdateLock::acquire(&self.cache_dir)?;
        let work = self.cache_dir.join(format!("filmcraft-{}", release.version));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let result = self.stage_into(release, &work, progress);
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&work);
        }
        result.map(|()| work)
    }

    fn stage_into(&self, release: &Release, work: &Path, progress: &mut dyn FnMut(u64, Option<u64>) -> bool) -> Result<()> {
        let archive = work.join(&release.archive_name);
        net::download_release(release, &archive, progress)?;
        let unpacked = work.join("unpacked");
        extract::extract_tar_gz(&archive, &unpacked, Limits::default())?;
        let _ = std::fs::remove_file(&archive);
        let root = extract::release_root(&unpacked)?;
        let new_main = root.join("bin").join("filmcraft");
        if !new_main.is_file() {
            return Err(UpdateError::UnsafeArchive("the release has no bin/filmcraft".into()));
        }
        smoke_test(&new_main, &release.version)?;
        let mut files = vec![StagedFile { src: new_main, dst: self.exe.clone() }];
        let cli_dst = self.exe.with_file_name(CLI_NAME);
        let cli_src = root.join("bin").join(CLI_NAME);
        if cli_dst.is_file() && cli_src.is_file() {
            smoke_test(&cli_src, &release.version)?;
            files.push(StagedFile { src: cli_src, dst: cli_dst });
        }
        Staged { version: release.version.to_string(), from: self.current.to_string(), files }.save(work)
    }
}

/// Run `bin --version` (10 s limit) and require a clean exit that prints `version`. This catches a
/// wrong-architecture or too-new-glibc build *before* it replaces the working one.
pub fn smoke_test(bin: &Path, version: &Version) -> Result<()> {
    use std::io::Read;
    let cannot = |why: String| UpdateError::Unsupported(format!("the new version cannot run on this system: {why}"));
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| cannot(e.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(cannot("`--version` did not finish".into()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let mut out = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(4096).read_to_string(&mut out);
    }
    if !status.success() {
        return Err(cannot(format!("`--version` exited with {status}")));
    }
    if !out.contains(&version.to_string()) {
        return Err(cannot(format!("`--version` printed {:?}, expected {version}", out.trim())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_are_throttled_to_the_interval() {
        let s = CheckState { last_check: 1_000, latest_seen: None };
        let day = Duration::from_secs(86_400);
        assert!(!s.is_due(1_000 + 86_399, day));
        assert!(s.is_due(1_000 + 86_400, day));
        assert!(s.is_due(10, day), "a clock set backwards must not block checks forever");
        assert!(CheckState::default().is_due(5, day));
    }

    #[test]
    fn check_state_round_trips_and_survives_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/update_state.json");
        let s = CheckState { last_check: 42, latest_seen: Some("0.7.3".into()) };
        s.save(&p).unwrap();
        assert_eq!(CheckState::load(&p), s);
        std::fs::write(&p, b"\xff\xfe nonsense").unwrap();
        assert_eq!(CheckState::load(&p), CheckState::default());
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_accepts_only_a_binary_that_prints_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let v = Version::new(0, 7, 3);
        assert!(smoke_test(&script(dir.path(), "ok", "echo filmcraft 0.7.3"), &v).is_ok());
        assert!(smoke_test(&script(dir.path(), "old", "echo filmcraft 0.7.2"), &v).is_err());
        assert!(smoke_test(&script(dir.path(), "crash", "exit 139"), &v).is_err());
        assert!(smoke_test(&dir.path().join("missing"), &v).is_err());
        let not_exec = dir.path().join("data");
        std::fs::write(&not_exec, b"hello").unwrap();
        assert!(smoke_test(&not_exec, &v).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_does_not_hang_on_a_stuck_binary() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let r = smoke_test(&script(dir.path(), "hang", "sleep 60"), &Version::new(1, 0, 0));
        assert!(r.is_err());
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn staging_is_refused_for_package_managed_or_not_newer_installs() {
        let u = Updater { current: Version::new(0, 7, 2), exe: PathBuf::from("/usr/bin/filmcraft"), cache_dir: PathBuf::from("/nonexistent/cache") };
        let rel = Release {
            version: Version::new(0, 7, 3),
            archive_name: "x.tar.gz".into(),
            archive_url: crate::release::asset_url(&Version::new(0, 7, 3), "x.tar.gz"),
            sha256: "0".repeat(64),
        };
        assert!(matches!(u.stage(&rel, &mut |_, _| true), Err(UpdateError::Unsupported(_))));
        let dir = tempfile::tempdir().unwrap();
        let u2 = Updater { current: Version::new(0, 7, 3), exe: dir.path().join("filmcraft"), cache_dir: dir.path().join("cache") };
        assert!(matches!(u2.stage(&rel, &mut |_, _| true), Err(UpdateError::Unsupported(_))), "same version is not an update");
        assert!(!dir.path().join("cache").exists(), "and nothing is created");
    }
}

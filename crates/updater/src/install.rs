//! What kind of installation is running, and may we replace it?
//!
//! Package managers own their files: a `.deb` in `/usr/bin`, a Flatpak, a Nix store path. Replacing
//! those behind the manager's back breaks it, so for them we only *notify*. Only a self-managed
//! install (the release tarball's `install.sh` under `~/.local`, or a plain extracted tree) is
//! swapped in place, and only after a real write probe succeeds.

use std::path::{Path, PathBuf};

/// Whether this installation may update itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Owned by the user; the updater may swap the binaries.
    SelfManaged,
    /// Owned by something else (the text names it); tell the user, never touch the files.
    Managed(&'static str),
    /// The directory is not writable (a system prefix without root, a read-only mount).
    ReadOnly,
}

/// The release asset for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The `<arch>` in `filmcraft-<v>-linux-<arch>.tar.gz`.
    pub arch: &'static str,
}

impl Target {
    /// The target for the running machine. Only Linux tarballs can be applied so far; macOS and
    /// Windows installs return `None` and are told to use the installer.
    pub fn current() -> Option<Target> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            "riscv64" => "riscv64",
            _ => return None,
        };
        Some(Target { arch })
    }

    /// `filmcraft-<version>-linux-<arch>.tar.gz`, the name `docs/releasing.md` documents.
    pub fn archive_name(&self, version: &semver::Version) -> String {
        format!("filmcraft-{version}-linux-{}.tar.gz", self.arch)
    }
}

/// Classify the installation of `exe`. `env` is `std::env::var(..).ok()` in production; tests pass a stub.
pub fn detect(exe: &Path, env: &dyn Fn(&str) -> Option<String>) -> InstallKind {
    if env("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
        return InstallKind::Managed("Flatpak (update with `flatpak update`)");
    }
    if env("SNAP").is_some() {
        return InstallKind::Managed("Snap (update with `snap refresh`)");
    }
    if env("APPIMAGE").is_some() {
        return InstallKind::Managed("AppImage (update with AppImageUpdate)");
    }
    if exe.starts_with("/nix/store") {
        return InstallKind::Managed("Nix (update through your Nix configuration)");
    }
    for system in ["/usr/bin", "/usr/sbin", "/bin", "/sbin", "/opt"] {
        if exe.starts_with(system) {
            return InstallKind::Managed("a system package (update with your package manager)");
        }
    }
    match exe.parent() {
        Some(dir) if dir_is_writable(dir) => InstallKind::SelfManaged,
        _ => InstallKind::ReadOnly,
    }
}

/// Prove that `dir` is writable by creating and deleting a probe file. File mode bits lie
/// (read-only mounts, ACLs, immutable flags), so they are never consulted.
pub fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".filmcraft-write-test-{}.tmp", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// An exclusive lock directory entry so two FilmCraft windows never download or swap at once.
/// Dropping it releases the lock; a lock older than an hour is considered abandoned.
#[derive(Debug)]
pub struct UpdateLock {
    path: PathBuf,
}

impl UpdateLock {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

    /// Take the lock `dir/.lock`, creating `dir` if needed.
    pub fn acquire(dir: &Path) -> crate::Result<UpdateLock> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(".lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(UpdateLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale =
                        std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Self::STALE_AFTER);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    return Err(crate::UpdateError::Locked);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(crate::UpdateError::Locked)
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn package_and_sandbox_installs_are_managed() {
        assert!(matches!(detect(Path::new("/usr/bin/filmcraft"), &no_env), InstallKind::Managed(_)));
        assert!(matches!(detect(Path::new("/nix/store/abc-filmcraft/bin/filmcraft"), &no_env), InstallKind::Managed(_)));
        let flatpak = |k: &str| (k == "FLATPAK_ID").then(|| "ai.storyteller.filmcraft".to_string());
        assert!(matches!(detect(Path::new("/app/bin/filmcraft"), &flatpak), InstallKind::Managed(_)));
        let appimage = |k: &str| (k == "APPIMAGE").then(|| "/home/u/FilmCraft.AppImage".to_string());
        assert!(matches!(detect(Path::new("/tmp/.mount_x/usr/bin/filmcraft"), &appimage), InstallKind::Managed(_)));
    }

    #[test]
    fn user_directory_is_self_managed_and_missing_directory_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect(&dir.path().join("filmcraft"), &no_env), InstallKind::SelfManaged);
        assert_eq!(detect(&dir.path().join("nope/filmcraft"), &no_env), InstallKind::ReadOnly);
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none(), "the probe file is cleaned up");
    }

    #[test]
    fn archive_name_matches_release_assets() {
        let t = Target { arch: "x86_64" };
        assert_eq!(t.archive_name(&semver::Version::new(0, 7, 3)), "filmcraft-0.7.3-linux-x86_64.tar.gz");
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let a = UpdateLock::acquire(dir.path()).unwrap();
        assert!(matches!(UpdateLock::acquire(dir.path()), Err(crate::UpdateError::Locked)));
        drop(a);
        assert!(UpdateLock::acquire(dir.path()).is_ok());
    }
}

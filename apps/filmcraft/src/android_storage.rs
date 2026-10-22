//! Android's app-private storage: `Projects/`, `Exports/` and `config/` below the
//! `AndroidApp::internal_data_path` directory.
//!
//! Android has no system file picker in v1 (rfd has no backend there), so saves, exports and
//! settings default to private storage: no Storage Access Framework round-trip — invisible to the
//! Files app, and browsable through FilmCraft's own in-app browser instead
//! (`filmcraft-ui-egui`, which starts at [`Layout::base`]). That browser offers shared storage
//! ([`SHARED_ROOT`]) as a second root once the user grants "All files access". Pure path logic,
//! unit-tested on the host; the handle comes from `run_android` (`apps/filmcraft/src/lib.rs`).
//!
//! `run_inner` picks [`Layout::config`] as the data root and hands the whole layout to
//! `app_creator`, which sets the browser's root (`HostHooks::app_files_dir`) and the
//! default export folder from it — one split, read from two places.

// `layout` and `ensure` are called from the Android-only paths of `run_inner`; on a host build
// they have no caller and are exercised by the tests below instead.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Android's shared internal-storage root (`/storage/emulated/0`, the primary shared tree): the
/// in-app browser's *second* root beside [`Layout::base`], offered only on Android and usable
/// only once the user grants "All files access" (`MANAGE_EXTERNAL_STORAGE`) — the grant is what
/// decides whether it can be listed, so the path itself is always the same. Sourced from the
/// engine (`media_browser::SHARED_ROOT`), whose Media Browser and volumes use the same path.
pub const SHARED_ROOT: &str = match filmcraft_engine::media_browser::SHARED_ROOT {
    Some(p) => p,
    None => "/storage/emulated/0",
};

/// The storage folders below the app-private base directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The app-private root: what the in-app browser may reach (`HostHooks::app_files_dir`).
    pub base: PathBuf,
    /// Saved `.fcproj` projects (the Open Project prompt lists these).
    pub projects: PathBuf,
    /// Render output, frame stills and other saved files.
    pub exports: PathBuf,
    /// Settings, auto-save and crash-recovery state — the data root the session runs from.
    pub config: PathBuf,
}

/// Split the app-private `base` into its folders.
pub fn layout(base: &Path) -> Layout {
    Layout { base: base.to_path_buf(), projects: base.join("Projects"), exports: base.join("Exports"), config: base.join("config") }
}

/// Create the folders (`Err` names the first failure; the app then keeps that feature off
/// rather than crashing).
pub fn ensure(layout: &Layout) -> Result<(), String> {
    for dir in [&layout.projects, &layout.exports, &layout.config] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.to_string_lossy()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("filmcraft-android-storage-{tag}-{}", std::process::id()))
    }

    #[test]
    fn layout_splits_the_base_into_projects_exports_and_config() {
        let base = Path::new("/data/data/ai.storyteller.filmcraft/files");
        let l = layout(base);
        assert_eq!(l.base, base);
        assert_eq!(l.projects, base.join("Projects"));
        assert_eq!(l.exports, base.join("Exports"));
        assert_eq!(l.config, base.join("config"));
        // the data root is `base`'s `config` folder, so the split can be read back if needed
        assert_eq!(l.config.parent(), Some(base));
    }

    #[test]
    fn ensure_creates_every_folder() {
        let base = temp_base("ok");
        let _ = std::fs::remove_dir_all(&base);
        let l = layout(&base);
        ensure(&l).unwrap();
        assert!(l.projects.is_dir() && l.exports.is_dir() && l.config.is_dir());
        // Re-running over existing folders is fine.
        ensure(&l).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_reports_a_folder_it_could_not_create() {
        let base = temp_base("blocked");
        let _ = std::fs::remove_dir_all(&base);
        // a file where a folder has to go: create_dir_all must fail, not panic
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("Projects"), b"x").unwrap();
        let err = ensure(&layout(&base)).unwrap_err();
        assert!(err.contains("Projects"), "{err}");
        let _ = std::fs::remove_dir_all(&base);
    }
}

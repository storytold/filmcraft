//! The private data-directory hook a hosted (Nightly) start installs.
//!
//! This lives in its own test binary: the override is process-wide and must not leak into the
//! unit tests that exercise the per-user, portable and `FILMCRAFT_DATA_DIR` resolution. It is one
//! test on purpose — the hook can be selected once per process.

use filmcraft_engine::autosave;
use std::path::PathBuf;

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filmcraft-data-override-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_private_root_is_selected_once_and_wins_over_every_other_root() {
    // Before a selection, the ordinary resolution applies (never an empty path).
    assert!(autosave::data_dir_override().is_none());
    if let Some(path) = autosave::default_data_dir() {
        assert!(!path.as_os_str().is_empty());
    }
    // A relative or empty root selects nothing.
    assert!(autosave::set_data_dir_override(PathBuf::from("relative/profile")).is_err());
    assert!(autosave::set_data_dir_override(PathBuf::from("")).is_err());
    assert!(autosave::data_dir_override().is_none());

    let dir = temp("selected");
    // Nothing else resolves first: not the environment override, not a portable marker.
    std::fs::write(dir.join(autosave::PORTABLE_MARKER), "").unwrap();
    autosave::set_data_dir_override(dir.join("profile")).unwrap();
    assert_eq!(autosave::data_dir_override(), Some(dir.join("profile").as_path()));
    assert_eq!(autosave::default_data_dir(), Some(dir.join("profile")), "the private root wins over the portable marker");
    // A second selection is refused instead of silently taking over.
    assert!(autosave::set_data_dir_override(dir.join("second")).is_err());
    assert_eq!(autosave::default_data_dir(), Some(dir.join("profile")));
    let _ = std::fs::remove_dir_all(&dir);
}

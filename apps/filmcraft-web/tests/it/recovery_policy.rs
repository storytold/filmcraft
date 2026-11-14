//! The web recovery snapshot policy. It is pure, so it runs natively even though the rest of the
//! crate is wasm32-only.

#[path = "../../src/recovery_policy.rs"]
mod policy;

use policy::{Action, INTERVAL_S, Policy, StartupRecoveryMode, engine_opfs_write_allowed, recovery_display_name, startup_recovery_mode};

#[test]
fn missing_snapshot_after_read_error_does_not_hold_recovery() {
    assert_eq!(policy::recovery_read_fallback::<u8>(Ok(None)), policy::RecoveryReadFallback::Proceed);
}

#[test]
fn existing_raw_snapshot_after_read_error_is_held() {
    assert_eq!(policy::recovery_read_fallback(Ok(Some(vec![1, 2, 3]))), policy::RecoveryReadFallback::Hold(vec![1, 2, 3]));
}

#[test]
fn unreadable_raw_storage_keeps_recovery_unavailable() {
    assert_eq!(policy::recovery_read_fallback::<u8>(Err("storage failed".into())), policy::RecoveryReadFallback::Unavailable("storage failed".into()));
}

#[test]
fn recovery_metadata_name_is_bounded_without_changing_the_project() {
    assert_eq!(recovery_display_name("An ordinary project"), "An ordinary project");
    assert_eq!(recovery_display_name(""), "Recovered");
    assert_eq!(recovery_display_name(" \t\u{2003}"), "Recovered");
    assert_eq!(recovery_display_name(&"x".repeat(512)), "x".repeat(512));
    assert_eq!(recovery_display_name(&"é".repeat(257)), "Recovered");
    assert_eq!(recovery_display_name("folder\\name"), "Recovered");
    assert_eq!(recovery_display_name(&"x".repeat(65_536)), "Recovered");
    assert_eq!(recovery_display_name("folder/name"), "Recovered");
    assert_eq!(recovery_display_name("bad\nname"), "Recovered");
}

#[test]
fn skipped_dirty_snapshot_is_held_for_download_without_opening_it() {
    assert_eq!(startup_recovery_mode(true), StartupRecoveryMode::HoldForDownload);
    assert_eq!(startup_recovery_mode(false), StartupRecoveryMode::Open);
}

#[test]
fn engine_exports_cannot_replace_browser_recovery_files() {
    for path in [
        "/opfs/recovery/snapshot.fcproj",
        "/opfs/recovery/meta.json",
        "/opfs/media/take.wav",
        "/opfs//media/take.wav",
        "/opfs/projects/../media/take.wav",
        "/opfs//recovery/snapshot.fcproj",
        "/opfs/./recovery/snapshot.fcproj",
        "/opfs/projects/../recovery/snapshot.fcproj",
        "/opfs/",
    ] {
        assert!(!engine_opfs_write_allowed(path), "{path}");
    }
    assert!(engine_opfs_write_allowed("/opfs/projects/copy.fcproj"));
    assert!(engine_opfs_write_allowed("/exports/copy.fcproj"));
}

/// #197: a snapshot write that fails is retried after the interval without another edit.
#[test]
fn failed_snapshot_write_is_retried_without_another_edit() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 3, 0), Action::WriteSnapshot(3));
    p.written(3, false, 0.1);
    // not immediately (no spinning on a failing disk)…
    assert!(matches!(p.tick(1.0, 3, 0), Action::WakeAfter(_)));
    // …but once the interval has passed, with the revision unchanged
    assert_eq!(p.tick(0.1 + INTERVAL_S, 3, 0), Action::WriteSnapshot(3));
    p.written(3, true, 5.2);
    assert_eq!(p.tick(20.0, 3, 0), Action::Nothing);
}

/// At most two snapshots may wait for OPFS; later revisions wait for a completion.
#[test]
fn a_third_snapshot_waits_for_one_in_flight() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    assert_eq!(p.tick(INTERVAL_S, 2, 0), Action::WriteSnapshot(2));
    assert_eq!(p.tick(2.0 * INTERVAL_S, 3, 0), Action::WakeAfter(0.5));
    assert_eq!(p.tick(3.0 * INTERVAL_S, 3, 0), Action::WakeAfter(0.5));
    p.written(1, true, 3.0 * INTERVAL_S);
    assert_eq!(p.tick(3.0 * INTERVAL_S + 0.5, 3, 0), Action::WriteSnapshot(3));
}

/// A failure reported for an older revision doesn't make the newer one be written again.
#[test]
fn stale_failure_is_ignored() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    assert_eq!(p.tick(INTERVAL_S, 2, 0), Action::WriteSnapshot(2));
    p.written(1, false, INTERVAL_S);
    assert_eq!(p.tick(3.0 * INTERVAL_S, 2, 0), Action::Nothing);
}

/// #193: a clean session (started with `?norecover` / `?fresh`, so the snapshot left by an
/// earlier session was not reopened) never marks that snapshot clean.
#[test]
fn clean_start_leaves_an_unconsumed_snapshot_dirty() {
    let mut p = Policy::default();
    for t in 0..100 {
        assert_eq!(p.tick(t as f64, 0, 0), Action::Nothing);
    }
}

/// The snapshot this session wrote is retired once the project is saved.
#[test]
fn saving_marks_own_snapshot_clean() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    p.written(1, true, 0.1);
    assert_eq!(p.tick(1.0, 1, 1), Action::WriteClean);
    assert_eq!(p.tick(1.1, 1, 1), Action::Nothing);
    p.cleaned(true, 1.2);
    assert_eq!(p.tick(30.0, 1, 1), Action::Nothing);
}

/// A failed clean write is retried after the interval.
#[test]
fn failed_clean_write_is_retried() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    p.written(1, true, 0.1);
    assert_eq!(p.tick(1.0, 1, 1), Action::WriteClean);
    p.cleaned(false, 1.1);
    assert!(matches!(p.tick(2.0, 1, 1), Action::WakeAfter(_)));
    assert_eq!(p.tick(1.1 + INTERVAL_S, 1, 1), Action::WriteClean);
}

/// A save while the dirty-meta callback or snapshot is still running cannot enqueue a
/// clean marker ahead of that snapshot in the OPFS queue.
#[test]
fn saving_waits_for_pending_snapshot_before_marking_meta_clean() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    assert_eq!(p.tick(1.0, 1, 1), Action::Nothing);
    p.written(1, true, 1.1);
    assert_eq!(p.tick(1.2, 1, 1), Action::WriteClean);
}

/// An edit made while the clean write is in flight keeps the meta dirty after it lands.
#[test]
fn edit_during_clean_write_keeps_meta_owned() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    p.written(1, true, 0.1);
    assert_eq!(p.tick(10.0, 1, 1), Action::WriteClean);
    assert_eq!(p.tick(20.0, 2, 1), Action::WriteSnapshot(2));
    p.cleaned(true, 20.1);
    // Saving again must wait until the newer snapshot is on disk.
    assert_eq!(p.tick(30.0, 2, 2), Action::Nothing);
    p.written(2, true, 30.1);
    assert_eq!(p.tick(30.2, 2, 2), Action::WriteClean);
}

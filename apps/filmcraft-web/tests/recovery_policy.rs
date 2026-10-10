//! The web recovery snapshot policy. It is pure, so it runs natively even though the rest of the
//! crate is wasm32-only.

#[path = "../src/recovery_policy.rs"]
mod recovery_policy;

use recovery_policy::{Action, INTERVAL_S, Policy};

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

/// An edit made while the clean write is in flight keeps the meta dirty after it lands.
#[test]
fn edit_during_clean_write_keeps_meta_owned() {
    let mut p = Policy::default();
    assert_eq!(p.tick(0.0, 1, 0), Action::WriteSnapshot(1));
    assert_eq!(p.tick(10.0, 1, 1), Action::WriteClean);
    assert_eq!(p.tick(20.0, 2, 1), Action::WriteSnapshot(2));
    p.cleaned(true, 20.1);
    // saved again: the newer snapshot must be retired too
    assert_eq!(p.tick(30.0, 2, 2), Action::WriteClean);
}

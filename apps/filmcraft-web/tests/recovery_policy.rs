//! The web recovery snapshot policy. It is pure, so it runs natively even though the rest of the
//! crate is wasm32-only.

#[path = "../src/recovery_policy.rs"]
mod recovery_policy;

use recovery_policy::{
    Action, INTERVAL_S, Policy, StartupRecoveryMode, ValidationWarning, engine_opfs_write_allowed, recovery_display_name, should_show_warning,
    startup_recovery_mode,
};

#[test]
fn recovery_metadata_name_is_bounded_without_changing_the_project() {
    assert_eq!(recovery_display_name("An ordinary project"), "An ordinary project");
    assert_eq!(recovery_display_name(&"x".repeat(65_536)), "Recovered");
    assert_eq!(recovery_display_name("folder/name"), "Recovered");
    assert_eq!(recovery_display_name("bad\nname"), "Recovered");
}

#[test]
fn rejected_dimension_warning_stays_visible_until_a_later_revision_validates() {
    let mut warning = ValidationWarning::default();
    warning.reject(7, "invalid dimensions".into());
    warning.refresh(7, || -> Result<(), &str> { panic!("same revision should not revalidate") });
    assert_eq!(warning.message(), Some("invalid dimensions"));
    warning.refresh(8, || Err("still invalid"));
    assert_eq!(warning.message(), Some("still invalid"));
    warning.refresh(9, || Ok::<(), &str>(()));
    assert_eq!(warning.message(), None);
    warning.reject(10, "new invalid state".into());
    warning.clear();
    assert_eq!(warning.message(), None);
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

#[test]
fn recovery_warning_yields_to_command_errors_and_resumes_after_they_expire() {
    let warning = "Recovery paused for invalid dimensions";
    assert!(should_show_warning("", None));
    assert!(should_show_warning(warning, Some(warning)));
    assert!(!should_show_warning("Could not import media", Some(warning)));
    assert!(should_show_warning("", Some(warning)));
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

/// An invalid live revision cannot replace a valid browser recovery snapshot, and a repair resumes writes.
#[test]
fn invalid_dimensions_keep_last_browser_snapshot_until_repair() {
    use filmcraft_project::{ItemKind, Label, Project};
    use filmcraft_time::FrameRate;

    let mut project = Project::new("Browser recovery");
    let graphic = project.add_item("Oversized graphic", Label::Iris, ItemKind::Graphic { width: 64, height: 36, rate: FrameRate::FPS_24 }, None);
    let mut p = Policy::default();
    assert!(matches!(p.tick_checked(0.0, 1, 0, || filmcraft_format::validate_project_dimensions(&project)), Ok(Action::WriteSnapshot(1))));
    p.written(1, true, 0.1);

    if let Some(item) = project.item_mut(graphic)
        && let ItemKind::Graphic { width, height, .. } = &mut item.kind
    {
        (*width, *height) = (1_000_000, 1_000_000);
    }
    let error = p.tick_checked(INTERVAL_S, 2, 0, || filmcraft_format::validate_project_dimensions(&project));
    assert!(error.is_err_and(|e| e.to_string().contains("Oversized graphic")));
    assert!(matches!(
        p.tick_checked(INTERVAL_S + 1.0, 2, 0, || -> Result<(), filmcraft_format::FormatError> { panic!("same rejected revision was revalidated") }),
        Ok(Action::Nothing)
    ));

    if let Some(item) = project.item_mut(graphic)
        && let ItemKind::Graphic { width, height, .. } = &mut item.kind
    {
        (*width, *height) = (64, 36);
    }
    assert!(matches!(p.tick_checked(INTERVAL_S + 2.0, 3, 0, || filmcraft_format::validate_project_dimensions(&project)), Ok(Action::WriteSnapshot(3))));
    p.written(3, true, INTERVAL_S + 2.1);
    assert!(matches!(
        p.tick_checked(INTERVAL_S + 3.0, 3, 0, || -> Result<(), filmcraft_format::FormatError> { panic!("already journaled") }),
        Ok(Action::Nothing)
    ));
}

/// A structurally invalid live project must not retire this session's last valid snapshot.
#[test]
fn invalid_dimensions_cannot_mark_a_browser_snapshot_clean() {
    use filmcraft_project::{ItemKind, Label, Project};
    use filmcraft_time::FrameRate;

    let mut project = Project::new("Browser recovery");
    let graphic = project.add_item("Oversized graphic", Label::Iris, ItemKind::Graphic { width: 64, height: 36, rate: FrameRate::FPS_24 }, None);
    let mut policy = Policy::default();
    assert!(matches!(policy.tick_checked(0.0, 1, 0, || filmcraft_format::validate_project_dimensions(&project)), Ok(Action::WriteSnapshot(1))));
    policy.written(1, true, 0.1);

    if let Some(item) = project.item_mut(graphic)
        && let ItemKind::Graphic { width, height, .. } = &mut item.kind
    {
        (*width, *height) = (1_000_000, 1_000_000);
    }
    let rejected = policy.tick_checked(1.0, 1, 1, || filmcraft_format::validate_project_dimensions(&project));
    assert!(rejected.is_err_and(|error| error.to_string().contains("Oversized graphic")));
    assert!(matches!(
        policy.tick_checked(1.1, 1, 1, || -> Result<(), filmcraft_format::FormatError> { panic!("same rejected revision was revalidated") }),
        Ok(Action::Nothing)
    ));

    if let Some(item) = project.item_mut(graphic)
        && let ItemKind::Graphic { width, height, .. } = &mut item.kind
    {
        (*width, *height) = (64, 36);
    }
    assert!(matches!(policy.tick_checked(2.0, 2, 2, || filmcraft_format::validate_project_dimensions(&project)), Ok(Action::WriteClean)));
}

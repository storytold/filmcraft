//! The app id every FilmCraft OS window carries — the main window, and any extra window built
//! with [`builder`] — plus the immutable launch identity the desktop selects before any UI
//! service exists. Without an app id Wayland compositors' window rules and docks have nothing to
//! match, and a Nightly window would be indistinguishable from stable.

use std::sync::OnceLock;

/// Matches the `.desktop` file and hicolor icon name, so Wayland docks pick up the icon.
pub const APP_ID: &str = "ai.storyteller.filmcraft";

/// The nightly channel's window/Wayland app id. The package, bundle and Launcher identity is
/// [`NIGHTLY_PACKAGE_ID`] instead: two different strings, both are needed.
pub const NIGHTLY_V1_APP_ID: &str = "ai.storyteller.filmcraft.nightly-v1";

/// The Nightly package, bundle and Launcher id (also `FLATPAK_ID` inside its own sandbox).
pub const NIGHTLY_PACKAGE_ID: &str = "ai.storyteller.filmcraft.nightly";

/// Immutable startup identity. The desktop selects it before any UI/runtime service is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchIdentity {
    Stable,
    NightlyV1,
}

static IDENTITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static DISPLAY_VERSION: OnceLock<String> = OnceLock::new();

pub fn select_identity(identity: LaunchIdentity) -> Result<(), String> {
    let value = match identity {
        LaunchIdentity::Stable => 1,
        LaunchIdentity::NightlyV1 => 2,
    };
    IDENTITY
        .compare_exchange(0, value, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| "the window launch identity was already selected".into())
}

pub fn identity() -> LaunchIdentity {
    if IDENTITY.load(std::sync::atomic::Ordering::Acquire) == 2 { LaunchIdentity::NightlyV1 } else { LaunchIdentity::Stable }
}

pub const fn id_for(identity: LaunchIdentity) -> &'static str {
    match identity {
        LaunchIdentity::Stable => APP_ID,
        LaunchIdentity::NightlyV1 => NIGHTLY_V1_APP_ID,
    }
}

pub fn app_id() -> &'static str {
    id_for(identity())
}

pub const fn name_for(identity: LaunchIdentity) -> &'static str {
    match identity {
        LaunchIdentity::Stable => "FilmCraft",
        LaunchIdentity::NightlyV1 => "FilmCraft Nightly",
    }
}

pub fn name() -> &'static str {
    name_for(identity())
}

/// The version the UI shows (About): the exact compiled identity, announced by the desktop app.
/// Without one (unit tests, the web build) it is this crate's version.
pub fn set_display_version(version: String) -> Result<(), String> {
    DISPLAY_VERSION.set(version).map_err(|_| "the display version was already selected".into())
}

pub fn display_version() -> &'static str {
    DISPLAY_VERSION.get().map_or(env!("CARGO_PKG_VERSION"), String::as_str)
}

/// A builder for an extra OS window, carrying the selected app id.
pub fn builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default().with_app_id(app_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nightly_identity_is_distinct_from_stable() {
        assert_eq!(id_for(LaunchIdentity::Stable), APP_ID);
        assert_ne!(id_for(LaunchIdentity::NightlyV1), APP_ID);
        assert_eq!(id_for(LaunchIdentity::NightlyV1), NIGHTLY_V1_APP_ID);
        assert_ne!(name_for(LaunchIdentity::NightlyV1), name_for(LaunchIdentity::Stable));
    }

    #[test]
    fn the_package_identity_and_the_window_identity_are_two_different_strings() {
        // The bundle/package id the Launcher and macOS plist use, and the window/AUMID one.
        assert_eq!("ai.storyteller.filmcraft.nightly-v1", NIGHTLY_V1_APP_ID);
        assert_eq!("ai.storyteller.filmcraft.nightly", NIGHTLY_PACKAGE_ID);
        assert_ne!(NIGHTLY_PACKAGE_ID, NIGHTLY_V1_APP_ID);
        assert_ne!(NIGHTLY_PACKAGE_ID, APP_ID);
    }

    #[test]
    fn extra_windows_carry_the_selected_app_id() {
        assert_eq!(builder().app_id.as_deref(), Some(app_id()));
    }

    #[test]
    fn the_about_version_falls_back_to_the_crate_version_until_selected() {
        // Selection happens once per process, so this test only reads the default.
        assert!(display_version() == env!("CARGO_PKG_VERSION") || display_version().contains("nightly"));
    }
}

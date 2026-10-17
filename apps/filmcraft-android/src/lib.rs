//! Android entry point for FilmCraft.
//!
//! Android's `NativeActivity` loads this library and calls [`android_main`] on a dedicated
//! thread. It opens the shared app ([`filmcraft::run_android`], the same code the desktop binary
//! runs) and returns when the window closes, ending the activity's main-loop thread.
//!
//! This crate exists because that export must be unmangled, and on edition 2024 the only spelling
//! (`#[unsafe(no_mangle)]`) trips the workspace-forbidden `unsafe_code` lint — see `Cargo.toml`
//! for why the crate carries its own lints instead. It also holds `android_access` (Android
//! builds), this crate's one piece of real `unsafe` — the JNI to the activity's JVM that checks
//! and opens Android's "All files access" grant, named as an exception by AGENTS.md §0.3 — whose
//! safe closures are handed to the shared app as [`filmcraft::SharedStorageAccess`].
//!
//! Android limitations (v1): no system file picker and no system message dialogs (rfd has no
//! Android backend), so Save, Open Project, exports, folder picks and media import are served by
//! FilmCraft's own in-app browser: app-private storage always, shared storage
//! (`/storage/emulated/0`) too once the user grants "All files access" (`android_access`); links
//! and "open path" report why they cannot open (no `ACTION_VIEW` intent without JNI); and there is
//! no per-user data folder, so logs, auto-save and settings live in the app-private storage
//! directory the activity resolves.
//!
//! On every other target this crate is empty: only Android's loader ever looks for `android_main`.

#[cfg(target_os = "android")]
mod android_access;

/// NativeActivity's main-loop thread entry point (required; see the `android-activity` docs).
/// winit pumps the activity's lifecycle and input events inside [`filmcraft::run_android`], which
/// needs the activity's handle to create its event loop; when the activity is destroyed that
/// returns and this returns with it.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {
    // The "All files access" grant, checked and requested over JNI — the entry crate's unsafe
    // stays in `android_access`; `run_android` only ever sees the shared closures built here,
    // before the app moves (shared, because the graphics retry loop can build it twice).
    let shared = android_access::capture(&app).map(|a| filmcraft::SharedStorageAccess {
        granted: std::sync::Arc::new(move || a.shared_storage_granted()),
        ask: std::sync::Arc::new(move || a.ask_shared_storage_grant()),
    });
    if let Err(e) = filmcraft::run_android(app, shared) {
        log::error!("filmcraft exited with an error: {e}");
    }
}

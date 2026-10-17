//! CollabCraft browser bootstrap — regressions for the two bugs fixed in this PR.
//!
//! The fetch transport and cookie handling are wasm32-only, so these tests cover
//! only the pure parts that compile on any target.  The live paths (cookie cleared,
//! account badge populated) are confirmed by a manual smoke-test on the deployed build
//! described in the PR description.

#[path = "../src/cloud_paths.rs"]
mod cloud_paths;

/// Sign-out posts to `/auth/logout`, not the root `/logout`.
///
/// Regression: the original `post_logout` called
/// `Request::new_with_str_and_init("/logout", …)`.  On the CollabCraft host `/logout`
/// is an unknown route (404); the session cookie was never cleared and sign-out appeared
/// to succeed while the server still saw an authenticated session.
#[test]
fn logout_path_is_collabcraft_auth_logout() {
    assert_eq!(cloud_paths::LOGOUT_PATH, "/auth/logout", "sign-out must reach the CollabCraft auth namespace");
    assert!(cloud_paths::LOGOUT_PATH.starts_with("/auth/"), "CollabCraft auth routes all live under /auth/");
    assert_ne!(cloud_paths::LOGOUT_PATH, "/logout", "/logout is a 404 on the CollabCraft host");
}

/// The login redirect also stays inside the `/auth/` namespace.
///
/// Keeping both constants together prevents a future refactor from moving one
/// without the other and creating a cross-namespace mismatch.
#[test]
fn login_path_is_collabcraft_auth_login() {
    assert_eq!(cloud_paths::LOGIN_PATH, "/auth/login", "unauthenticated redirect must reach the CollabCraft login page");
    assert!(cloud_paths::LOGIN_PATH.starts_with("/auth/"), "login is in the same /auth/ namespace as logout");
}

/// The two auth paths must share the same namespace prefix.
///
/// Diverging prefixes would mean one route is served by a different handler and
/// the session cookie domain/path would not match.
#[test]
fn auth_paths_share_namespace() {
    let logout_ns = cloud_paths::LOGOUT_PATH.trim_start_matches('/').split('/').next().unwrap_or("");
    let login_ns = cloud_paths::LOGIN_PATH.trim_start_matches('/').split('/').next().unwrap_or("");
    assert_eq!(logout_ns, login_ns, "login ({}) and logout ({}) must share the same URL namespace segment", cloud_paths::LOGIN_PATH, cloud_paths::LOGOUT_PATH);
}

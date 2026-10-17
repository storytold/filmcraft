/// The CollabCraft endpoint that clears the session cookie on sign-out.
///
/// On a CollabCraft host the auth routes live under `/auth/`. Posting to the
/// root-level `/logout` results in a 404 and leaves the cookie in place, so
/// the app silently appears to have signed out while the backend still sees
/// an authenticated session.
pub const LOGOUT_PATH: &str = "/auth/logout";

/// The CollabCraft path that serves the sign-in page.
///
/// Used by `offer_login` to redirect unauthenticated sessions. Kept here so
/// both constants travel together and a reviewer can confirm they agree on
/// the auth namespace.
pub const LOGIN_PATH: &str = "/auth/login";

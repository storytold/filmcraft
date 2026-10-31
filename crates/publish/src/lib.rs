//! Publishing exported media to video platforms.
//!
//! - [`http`]: the small [`Transport`](http::Transport) every uploader talks through, so the
//!   protocol logic is tested without a network; (feature `net`) `http::UreqTransport`, the HTTPS
//!   client (pure-Rust TLS, the operating system's certificate verifier).
//! - [`youtube`]: YouTube Data API v3 uploads (`videos.insert`): the metadata checks YouTube applies,
//!   the resumable session, and a chunked upload that resumes after a dropped connection, retries
//!   server errors with exponential backoff and maps API errors to [`PublishError`].
//! - [`oauth`]: signing in with Google (OAuth 2.0 for installed apps with PKCE and a loopback
//!   redirect), the token exchange and refresh; (feature `net`) the loopback listener.
//!
//! The engine commands build on this crate; see storytold/filmcraft#2626.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod http;
pub mod oauth;
#[cfg(test)]
mod oauth_tests;
#[cfg(test)]
mod test_support;
pub mod youtube;
#[cfg(test)]
mod youtube_tests;

use std::time::Duration;

/// Why a publish step failed. Messages never contain tokens, secrets or upload session addresses.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("stopped")]
    Cancelled,
    /// The OAuth client file or settings are unusable.
    #[error("{0}")]
    Config(String),
    #[error("sign-in was cancelled in the browser")]
    SignInDenied,
    #[error("sign-in: {0}")]
    SignIn(String),
    #[error("{0}")]
    InvalidMetadata(String),
    #[error("the file is empty")]
    EmptyFile,
    #[error("the file changed while it was uploading")]
    FileChanged,
    /// The access token was refused: refresh it (or sign in again) and retry.
    #[error("the account needs to sign in again")]
    Unauthorized,
    #[error("this Google account has no YouTube channel: create one on youtube.com, then try again")]
    NoChannel,
    /// The upload session is gone (it expires about a week after it starts): start a new one.
    #[error("the upload session expired: start the upload again")]
    SessionExpired,
    #[error("the app's daily YouTube upload quota is used up: try again tomorrow")]
    QuotaExceeded,
    #[error("the channel has reached YouTube's daily upload limit: try again tomorrow")]
    UploadLimitExceeded,
    /// Retries ran out on server errors or a lost connection.
    #[error("network: {0}")]
    Network(String),
    /// The server took no new bytes over several attempts.
    #[error("the upload stopped making progress")]
    Stalled,
    #[error("the server answered {status}{}: {message}", reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default())]
    Api { status: u16, reason: Option<String>, message: String },
    /// The server's answer broke the protocol (a malformed header or body).
    #[error("unexpected server reply: {0}")]
    Protocol(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// What a running upload reports to its caller and asks of it.
pub trait Control {
    /// `sent` of `total` bytes are on the server. Return false to cancel.
    fn progress(&mut self, sent: u64, total: u64) -> bool;
    /// Wait `delay` before the next attempt. Return false to cancel instead.
    fn wait(&mut self, delay: Duration) -> bool;
}

/// A [`Control`] for a worker thread: progress goes to `on_progress`, waits sleep in short steps,
/// and both stop as soon as `cancel` is set (`jobs.cancel` sets it).
#[cfg(not(target_family = "wasm"))]
pub struct ThreadControl<'a, F: FnMut(u64, u64)> {
    pub cancel: &'a std::sync::atomic::AtomicBool,
    pub on_progress: F,
}

#[cfg(not(target_family = "wasm"))]
impl<F: FnMut(u64, u64)> Control for ThreadControl<'_, F> {
    fn progress(&mut self, sent: u64, total: u64) -> bool {
        (self.on_progress)(sent, total);
        !self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn wait(&mut self, delay: Duration) -> bool {
        const STEP: Duration = Duration::from_millis(100);
        let mut left = delay;
        while !left.is_zero() {
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return false;
            }
            let step = left.min(STEP);
            std::thread::sleep(step);
            left = left.saturating_sub(step);
        }
        !self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

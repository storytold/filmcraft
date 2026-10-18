//! Talking to CollabCraft from FilmCraft.
//!
//! [`service`] starts the worker that Open and Save run on. Both go through the same service
//! and share one saved session.
//!
//! FilmCraft holds no provider credential and speaks no provider protocol. Everything goes
//! through the CollabCraft service, which is the one place a provider is configured. This file
//! supplies the transport half of `filmcraft_ui_egui::cloud` and the two things a desktop
//! application has to do that a library cannot: open a browser, and decide how long to keep
//! polling.
//!
//! Every call in `craft_cloud_client` blocks. The pages therefore never make one. They state a
//! [`Job`], this thread runs it, and the answer goes back through the [`Post`] the job arrived
//! with. The egui context is woken afterwards, because a worker answering is not a local event
//! and the window would otherwise sit still until the mouse moved.
//!
//! # Browser transport
//!
//! The desktop app uses the device-authorization flow: the service issues a URL, the user
//! follows it in their browser, and this thread polls until the token arrives. No redirect URI
//! is involved, so there is no port to open and no local HTTP server to run.
//!
//! # Web / same-origin transport
//!
//! On the web build, the browser window is already signed in to the CollabCraft service through
//! its normal session cookie (same origin). No device flow or token storage is needed:
//! requests carry the session cookie automatically. The web host supplies a `CloudBackend`
//! that posts to `/api/cloud/…` (see `apps/filmcraft-web/src/cloud.rs`).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use craft_cloud_client::{Client, Error};
use filmcraft_ui_egui::cloud::{Account, CloudBackend, Event, Fault, Job, Post};

/// How long to keep polling a device-flow sign-in before giving up.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// Where the CollabCraft service is.
///
/// Read from the environment and compiled in nowhere, because the service is deployment-specific
/// and a desktop binary must not carry one. Without it there is no cloud worker and the
/// interface offers nothing cloud-shaped.
pub fn base_url() -> Option<String> {
    ["FILMCRAFT_CLOUD_URL", "CRAFT_CLOUD_URL", "COLLABCRAFT_URL"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .find(|value| !value.is_empty() && allows_service_url(value))
}

/// HTTPS everywhere, or plain HTTP only to the exact loopback hosts.
///
/// Prefix checks must not accept `localhost.evil.com`. Bearer tokens are attached
/// after this gate, so a mis-set `http://` service URL cannot leak the session.
fn allows_service_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or("");
    host == "localhost" || host == "127.0.0.1" || host == "[::1]"
}

fn allows_browser_url(url: &str) -> bool {
    allows_service_url(url)
}

/// A handle to the worker, as the pages see it.
///
/// Every job is answered on that thread. Nothing a page does can block a frame. One queue serves
/// them all, so a large project save holds up the jobs behind it. That is why the share dialog
/// has a client of its own.
pub struct Service {
    jobs: Sender<(Job, Post)>,
}

impl CloudBackend for Service {
    fn submit(&self, job: Job, post: Post) {
        if self.jobs.send((job, post)).is_err() {
            log::warn!("the cloud worker is gone; the job was dropped");
        }
    }
}

/// Starts the cloud worker, when a service is configured.
///
/// Returns `None` when no service URL is set, leaving the interface with nothing
/// cloud-shaped offered anywhere.
pub fn service(ctx: egui::Context) -> Option<Service> {
    let base = base_url()?;
    log::info!("cloud storage through {base}");
    let (jobs, receiver) = std::sync::mpsc::channel::<(Job, Post)>();
    let started = std::thread::Builder::new().name("collabcraft".to_owned()).spawn(move || {
        let token = load_token();
        let mut client = Client::new(&base);
        if let Some(token) = token {
            client = client.with_token(token);
        }
        let mut worker = Worker { client, base, ctx, answering: None };
        while let Ok((job, post)) = receiver.recv() {
            worker.answering = Some(post);
            worker.run(job);
        }
    });
    if let Err(e) = started {
        log::error!("the cloud worker did not start: {e}");
        return None;
    }
    Some(Service { jobs })
}

/// A client for the share dialog, built fresh for each of its requests.
///
/// Separate from the Open/Save worker so a large project upload does not hold up an
/// autocomplete or a notification. They share what matters: the service URL and the saved
/// session token, so a sign-in through either reaches both.
pub fn client() -> Option<Client> {
    let mut client = Client::new(base_url()?);
    if let Some(token) = load_token() {
        client = client.with_token(token);
    }
    Some(client)
}

struct Worker {
    client: Client,
    /// Kept beside the client so a rejected session can be replaced with a fresh one.
    base: String,
    ctx: egui::Context,
    /// Where the job in hand sends its answers.
    answering: Option<Post>,
}

impl Worker {
    fn post(&self, event: Event) {
        if let Some(post) = &self.answering {
            post.send(event);
        }
        self.ctx.request_repaint();
    }

    fn run(&mut self, job: Job) {
        match job {
            Job::Account => {
                let answer = self.call(|client| client.me()).map(|me| {
                    remember_account(&me.id, &me.display);
                    Account { display: me.display, capabilities: me.capabilities, provider_linked: me.provider_linked }
                });
                self.post(Event::Account(answer));
            }
            Job::LinkStart => match self.call(|client| client.link_start()) {
                Ok(start) => self.post(Event::Browser { url: start.login_url, handle: Some(start.handle) }),
                Err(fault) => self.post(Event::Linked(Err(fault))),
            },
            Job::Conversation(id) => {
                let answer = self.call(|client| client.conversation(&id));
                self.post(Event::Conversation(answer));
            }
            Job::SignOut => {
                if let Some(path) = token_path() {
                    let _ = std::fs::remove_file(path);
                }
                self.client = Client::new(&self.base);
            }
            Job::LinkPoll(handle) => {
                let answer = self.call(|client| client.link_poll(&handle));
                self.post(Event::Linked(answer));
            }
            Job::List(path) => {
                let entries = self.call(|client| client.list(&path));
                self.post(Event::Listed { path, entries });
            }
            Job::SharedWithMe => {
                let answer = self.call(|client| client.shared_with_me());
                self.post(Event::Shared(answer));
            }
            Job::Read { entry, into } => {
                let bytes = self.call(|client| client.read(&entry.id));
                self.post(Event::Read { entry, into, bytes });
            }
            // The etag the project was read at is the condition. A stored copy that has moved on
            // refuses the write, protecting another user's save from being overwritten.
            Job::Write { uid, entry, bytes } => {
                let answer = self.call(|client| client.write(&entry.id, &bytes, entry.etag.as_deref()));
                self.post(Event::Saved { uid, entry: answer });
            }
            Job::Create { uid, folder, name, bytes } => {
                let answer = self.call(|client| client.create(&folder, &name, &bytes));
                self.post(Event::Saved { uid, entry: answer });
            }
        }
    }

    /// Runs one call with a session behind it.
    ///
    /// A rejected token is worth exactly one retry: the saved one has been revoked or expired,
    /// and a fresh sign-in is the only recovery. A second rejection is reported as an error.
    fn call<T>(&mut self, work: impl Fn(&Client) -> craft_cloud_client::Result<T>) -> Result<T, Fault> {
        self.sign_in()?;
        match work(&self.client) {
            Ok(value) => Ok(value),
            Err(Error::Unauthenticated) => {
                log::info!("the saved cloud session was rejected; signing in again");
                forget_token();
                self.client = Client::new(&self.base);
                self.sign_in()?;
                work(&self.client).map_err(fault)
            }
            Err(e) => Err(fault(e)),
        }
    }

    /// Ensures there is a session token, signing in through the device flow when there is not.
    fn sign_in(&mut self) -> Result<(), Fault> {
        if self.client.is_signed_in() {
            return Ok(());
        }
        let start = self.client.sign_in_start().map_err(fault)?;
        if let Err(e) = open_browser(&start.verify_url) {
            log::warn!("could not open a browser: {e}");
        }
        self.post(Event::Browser { url: start.verify_url.clone(), handle: None });
        let interval = Duration::from_secs(start.interval.clamp(1, 10));
        #[allow(clippy::disallowed_methods)]
        let deadline = Instant::now() + SIGN_IN_WAIT.min(Duration::from_secs(start.expires_in));
        #[allow(clippy::disallowed_methods)]
        while Instant::now() < deadline {
            std::thread::sleep(interval);
            match self.client.sign_in_poll(&start.device_code) {
                Ok(Some(token)) => {
                    save_token(&token);
                    self.client = Client::new(&self.base).with_token(token);
                    self.post(Event::Linked(Ok(true)));
                    return Ok(());
                }
                Ok(None) => {}
                Err(e) => return Err(fault(e)),
            }
        }
        Err(Fault::Refused("the sign-in was not completed in time".to_owned()))
    }
}

/// Map a client error to one of the shapes the UI acts on differently.
///
/// A refused conditional write (HTTP 409/412 with code `conflict`) is the one case that must
/// never be retried and never be forced: it means somebody else's save is in the stored copy.
fn fault(e: Error) -> Fault {
    match e {
        Error::NotLinked => Fault::NotLinked,
        Error::Unauthenticated => Fault::Unauthenticated,
        Error::Unreachable(detail) => Fault::Unreachable(detail),
        Error::Refused { code, .. } if matches!(code.as_str(), "conflict" | "409" | "412") => Fault::Conflict,
        Error::Refused { code, detail } => Fault::Refused(if detail.trim().is_empty() { code } else { detail }),
        Error::Malformed(detail) => Fault::Refused(format!("the cloud service gave an answer FilmCraft could not read: {detail}")),
        Error::Unsendable(detail) => {
            log::error!("cloud request could not be encoded: {detail}");
            Fault::Refused("FilmCraft could not build that request".to_owned())
        }
    }
}

// ---------------------------------------------------------------- token persistence

fn token_path() -> Option<PathBuf> {
    // Store the session token beside the autosave data, not in a generic config dir.
    // This keeps all FilmCraft persistent data in one place.
    let dir = filmcraft_engine::autosave::default_data_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("collabcraft-session.json"))
}

/// Load the saved token, but only when it was stored for the same service
/// origin that is currently configured.
///
/// A token stored for `https://craft.example.com` is never returned when the
/// current service is `https://craft.other.com`. This prevents the credential
/// from a past deployment from silently reaching a different one.
fn load_token() -> Option<String> {
    let path = token_path()?;
    load_token_from(&path, &base_url().unwrap_or_default())
}

fn load_token_from(path: &Path, required_origin: &str) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    // An unbound legacy token is unsafe: after a URL change there is no way
    // to know which service issued it.
    let stored = value.get("service")?.as_str()?;
    if stored.trim().trim_end_matches('/') != required_origin.trim().trim_end_matches('/') {
        log::warn!("discarding stored token for {stored}: current service is {required_origin}");
        return None;
    }
    value.get("token")?.as_str().map(str::to_owned).filter(|t| !t.is_empty())
}

fn save_token(token: &str) {
    let service = base_url().unwrap_or_default();
    let value = serde_json::json!({"token": token, "service": service});
    if let Some(path) = token_path()
        && let Err(e) = write_session_to(&path, &value)
    {
        log::warn!("could not save the cloud session: {e}");
    }
}

fn remember_account(subject: &str, display: &str) {
    let Some(token) = load_token() else { return };
    let service = base_url().unwrap_or_default();
    let value = serde_json::json!({
        "token": token,
        "service": service,
        "subject": subject,
        "display": display,
    });
    if let Some(path) = token_path()
        && let Err(e) = write_session_to(&path, &value)
    {
        log::warn!("could not update the cloud account record: {e}");
    }
}

/// Write a session record atomically with mode 0600 (Unix) or no extra work
/// (Windows, where the per-user AppData directory is already owner-scoped).
///
/// Writes to a uniquely named temporary file in the same directory, sets
/// 0600 on Unix *before* renaming, then renames into place. The rename is
/// atomic on POSIX file systems; the file is never visible to other users
/// even for an instant.
fn write_session_to(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    // Unique per call so parallel tests (and overlapping writes) never share one
    // `.fc-session-{pid}.tmp` and trip create_new AlreadyExists / cross-cleanup NotFound.
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let text = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("token path has no parent directory"))?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!(".fc-session-{}-{seq}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true); // fail if the tmp file somehow already exists
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(&text)?;
        f.flush()?;
        drop(f);
        std::fs::rename(&tmp_path, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn forget_token() {
    if let Some(path) = token_path() {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------- browser

fn open_browser(url: &str) -> std::io::Result<()> {
    if !allows_browser_url(url) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only https URLs (or http://localhost) are opened",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        // `cmd` splits on `&` before anything else; percent-encode `"` for safety.
        use std::os::windows::process::CommandExt as _;
        let mut c = std::process::Command::new("cmd");
        c.raw_arg(format!("/C start \"\" \"{}\"", url.replace('"', "%22")));
        c.spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn().map(|_| ())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ())
    }
}

// ---------------------------------------------------------------- token tests

#[cfg(test)]
mod token_tests {
    use super::*;

    fn tmp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fc-cloud-test-{label}-{}.json", std::process::id()))
    }

    #[test]
    fn service_url_requires_https_or_loopback() {
        assert!(allows_service_url("https://craft.example.com"));
        assert!(allows_service_url("http://localhost"));
        assert!(allows_service_url("http://127.0.0.1:8080/api"));
        assert!(!allows_service_url("http://craft.example.com"));
        assert!(!allows_service_url("http://localhost.evil.com"));
        assert!(!allows_service_url("file:///etc/passwd"));
        assert!(!allows_browser_url("file:///tmp/x"));
    }

    #[test]
    fn roundtrip_same_origin() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let value = serde_json::json!({"token": "tok-abc", "service": "https://craft.example.com"});
        write_session_to(&path, &value).expect("write");
        let loaded = load_token_from(&path, "https://craft.example.com");
        assert_eq!(loaded.as_deref(), Some("tok-abc"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_different_origin() {
        let path = tmp_path("origin-mismatch");
        let _ = std::fs::remove_file(&path);
        let value = serde_json::json!({"token": "tok-xyz", "service": "https://craft.example.com"});
        write_session_to(&path, &value).expect("write");
        // A different origin should be rejected.
        let loaded = load_token_from(&path, "https://craft.other.com");
        assert!(loaded.is_none(), "should reject a token from a different origin");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_legacy_token_without_origin() {
        let path = tmp_path("empty-origin");
        let _ = std::fs::remove_file(&path);
        // Stored without service (legacy / missing).
        let value = serde_json::json!({"token": "tok-leg"});
        write_session_to(&path, &value).expect("write");
        let loaded = load_token_from(&path, "https://craft.example.com");
        assert!(loaded.is_none(), "an unbound legacy token must not be forwarded");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = tmp_path("mode");
        let _ = std::fs::remove_file(&path);
        let value = serde_json::json!({"token": "tok-mode", "service": "https://craft.example.com"});
        write_session_to(&path, &value).expect("write");
        let meta = std::fs::metadata(&path).expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-read-write only (0600), got {mode:03o}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_token_is_treated_as_missing() {
        let path = tmp_path("empty-token");
        let _ = std::fs::remove_file(&path);
        let value = serde_json::json!({"token": "", "service": "https://craft.example.com"});
        write_session_to(&path, &value).expect("write");
        let loaded = load_token_from(&path, "https://craft.example.com");
        assert!(loaded.is_none(), "empty token string should be treated as absent");
        let _ = std::fs::remove_file(&path);
    }
}

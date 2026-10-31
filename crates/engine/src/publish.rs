//! Publishing to YouTube (storytold/filmcraft#2626): the OAuth client, the signed-in account, and
//! uploads as background jobs. The protocol work is in `filmcraft-publish`.
//!
//! | Command | What it does |
//! |---|---|
//! | `publish.youtube.status` | whether this build can publish (feature `publish`), the client, whether an account is signed in |
//! | `publish.youtube.setClient` | read a *Desktop app* OAuth client file downloaded from Google Cloud and keep a copy |
//! | `publish.youtube.signIn` | a job: open Google's sign-in in the browser ([`crate::Event::OpenUrl`]) and wait for it to come back |
//! | `publish.youtube.signOut` | forget the account |
//! | `publish.youtube.upload` | a job: upload a file with its title, description, tags and privacy (private by default); with `afterJob`, once that export has written the file |
//!
//! Both files live in `<data dir>/publish/`: `youtube-client.json` (the client file as Google
//! downloads it) and `youtube-account.json` (the refresh token, owner-only on Unix). The access
//! token stays in memory. No command, job or log line shows a token or the client secret.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use filmcraft_export::Progress;
use filmcraft_publish::http::{AVAILABLE, Transport, default_transport};
use filmcraft_publish::oauth::{self, ClientConfig};
use filmcraft_publish::youtube::{UPLOAD_SCOPE, UploadSession, Uploader, VideoMetadata};
use filmcraft_publish::{Control, PublishError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, str_p};
use crate::narration::{new_job, spawn};
use crate::{EngineError, Result, Session};

const CLIENT_FILE: &str = "youtube-client.json";
const ACCOUNT_FILE: &str = "youtube-account.json";
const MAX_CLIENT_FILE: u64 = 64 * 1024;
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// The access token is refreshed this long before Google says it expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// The status of an upload job waiting for its export (`afterJob`). The status bar shows the
/// export instead of a job in this state.
pub const WAITING_FOR_EXPORT: &str = "Waiting for the export";
/// How long a finished export may take to store its report before the upload gives up.
const REPORT_GRACE: Duration = Duration::from_secs(10);

/// Publishing state shared by the session and its jobs.
#[derive(Default)]
pub struct State {
    access: Arc<Mutex<Option<Access>>>,
    /// Tests: where the files live, the client (with a local token endpoint) and the upload endpoint.
    pub(crate) dir: Option<PathBuf>,
    pub(crate) client: Option<ClientConfig>,
    pub(crate) upload_url: Option<String>,
}

impl State {
    /// Keep the client and account files in `dir` instead of `<data dir>/publish`.
    pub fn set_dir(&mut self, dir: impl Into<PathBuf>) {
        self.dir = Some(dir.into());
    }
}

/// An access token and when to stop using it.
struct Access {
    token: String,
    client_id: String,
    until: web_time::Instant,
}

/// `youtube-account.json`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    client_id: String,
    refresh_token: String,
}

fn dir(s: &Session) -> Option<PathBuf> {
    s.publish.dir.clone().or_else(|| crate::autosave::default_data_dir().map(|d| d.join("publish")))
}

fn load_client(s: &Session) -> std::result::Result<ClientConfig, String> {
    if let Some(client) = &s.publish.client {
        return Ok(client.clone());
    }
    let path = dir(s).ok_or("no data directory")?.join(CLIENT_FILE);
    let bytes = std::fs::read(&path).map_err(|_| "no YouTube client yet: choose the client file from Google Cloud (publish.youtube.setClient)".to_string())?;
    ClientConfig::from_google_json(&bytes).map_err(|e| e.to_string())
}

/// The account signed in with `client`, if any.
fn load_account(dir: &Path, client: &ClientConfig) -> Option<Account> {
    let bytes = std::fs::read(dir.join(ACCOUNT_FILE)).ok()?;
    serde_json::from_slice::<Account>(&bytes).ok().filter(|a| a.client_id == client.client_id && !a.refresh_token.is_empty())
}

/// Write `bytes` to `path` through a temporary file, readable only by the user on Unix (the
/// per-user data directory already is on Windows).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.part");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn forget_account(dir: &Path, access: &Mutex<Option<Access>>) {
    let _ = std::fs::remove_file(dir.join(ACCOUNT_FILE));
    *access.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

fn unavailable(cmd: &str) -> EngineError {
    bad(cmd, "publishing is not available in this build (built without the `publish` feature)")
}

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    let client = load_client(s).ok();
    let signed_in = match (&client, dir(s)) {
        (Some(client), Some(dir)) => load_account(&dir, client).is_some(),
        _ => false,
    };
    Ok(json!({
        "available": AVAILABLE,
        "client": client.map(|c| json!({"clientId": c.client_id})),
        "signedIn": signed_in,
    }))
}

fn set_client(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "publish.youtube.setClient";
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`: the client file downloaded from Google Cloud"))?;
    let size = std::fs::metadata(path).map_err(|e| bad(cmd, format!("can't read {path}: {e}")))?.len();
    if size > MAX_CLIENT_FILE {
        return Err(bad(cmd, "that file is too large to be an OAuth client file"));
    }
    let bytes = std::fs::read(path).map_err(|e| bad(cmd, format!("can't read {path}: {e}")))?;
    let client = ClientConfig::from_google_json(&bytes).map_err(|e| bad(cmd, e.to_string()))?;
    let dir = dir(s).ok_or_else(|| bad(cmd, "no data directory"))?;
    write_private(&dir.join(CLIENT_FILE), &bytes).map_err(|e| bad(cmd, format!("can't keep the client file: {e}")))?;
    // An account signed in through another client can't refresh through this one.
    if load_account(&dir, &client).is_none() {
        forget_account(&dir, &s.publish.access);
    }
    Ok(json!({"clientId": client.client_id}))
}

fn sign_out(s: &mut Session, _: &Value) -> Result<Value> {
    if let Some(dir) = dir(s) {
        forget_account(&dir, &s.publish.access);
    }
    Ok(json!({"signedIn": false}))
}

fn sign_in(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "publish.youtube.signIn";
    if !AVAILABLE {
        return Err(unavailable(cmd));
    }
    let client = load_client(s).map_err(|e| bad(cmd, e))?;
    let dir = dir(s).ok_or_else(|| bad(cmd, "no data directory"))?;
    let timeout = p
        .get("timeoutSeconds")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t > 0.0)
        .map_or(SIGN_IN_TIMEOUT, |t| Duration::from_secs_f64(t.min(3600.0)));
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    start_sign_in(s, cmd, client, dir, timeout, wait)
}

#[cfg(target_family = "wasm")]
fn start_sign_in(_: &mut Session, cmd: &str, _: ClientConfig, _: PathBuf, _: Duration, _: bool) -> Result<Value> {
    Err(bad(cmd, "signing in to YouTube needs the desktop app"))
}

#[cfg(not(target_family = "wasm"))]
fn start_sign_in(s: &mut Session, cmd: &str, client: ClientConfig, dir: PathBuf, timeout: Duration, wait: bool) -> Result<Value> {
    let (sign_in, loopback) = oauth::SignIn::start(&client, UPLOAD_SCOPE).map_err(|e| bad(cmd, e.to_string()))?;
    let url = sign_in.url.clone();
    s.events.push(crate::Event::OpenUrl(url.clone()));
    let job = new_job(s, "Signing in to YouTube".into());
    job.progress.total.store(1, Ordering::Relaxed);
    set_status(&job.progress, "Waiting for the browser");
    let (prog, res, access) = (job.progress.clone(), job.result.clone(), s.publish.access.clone());
    let id = spawn(s, job, wait, "filmcraft-youtube-sign-in", move || {
        let t0 = web_time::Instant::now();
        let r = (|| -> std::result::Result<(), PublishError> {
            let code = loopback.wait(&sign_in, &prog.cancel, timeout)?;
            set_status(&prog, "Signing in");
            let transport = default_transport().ok_or_else(|| PublishError::Config("this build can't publish".into()))?;
            let tokens = sign_in.exchange(&*transport, &client, &code)?;
            if !tokens.grants(UPLOAD_SCOPE) {
                return Err(PublishError::SignIn("YouTube uploads weren't allowed: sign in again and keep that box ticked".into()));
            }
            let refresh_token = tokens.refresh_token.clone().ok_or_else(|| PublishError::SignIn("Google sent no refresh token".into()))?;
            let account = Account { client_id: client.client_id.clone(), refresh_token };
            let bytes = serde_json::to_vec(&account).map_err(|e| PublishError::Protocol(e.to_string()))?;
            write_private(&dir.join(ACCOUNT_FILE), &bytes)?;
            remember(&access, &client, &tokens);
            Ok(())
        })();
        finish(&prog, &res, r.map(|()| ("Signed in to YouTube".into(), String::new())), t0, 0);
    })?;
    Ok(json!({"job": id, "url": url}))
}

fn upload(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "publish.youtube.upload";
    if !AVAILABLE {
        return Err(unavailable(cmd));
    }
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`: the video file to upload"))?.to_string();
    let meta: VideoMetadata = serde_json::from_value(p.clone()).map_err(|e| bad(cmd, e.to_string()))?;
    meta.validate().map_err(|e| bad(cmd, e.to_string()))?;
    // With `afterJob` the export writes the file later; it is checked once the export is done.
    let after = match p.get("afterJob").filter(|v| !v.is_null()) {
        Some(v) => {
            let id = v.as_u64().ok_or_else(|| bad(cmd, "`afterJob` is a job id"))?;
            Some(s.jobs.iter().find(|j| j.id == id).cloned().ok_or_else(|| bad(cmd, format!("no job {id}")))?)
        }
        None => {
            video_size(&path).map_err(|e| bad(cmd, e.to_string()))?;
            None
        }
    };
    let client = load_client(s).map_err(|e| bad(cmd, e))?;
    let dir = dir(s).ok_or_else(|| bad(cmd, "no data directory"))?;
    let account = load_account(&dir, &client).ok_or_else(|| bad(cmd, "sign in to YouTube first (publish.youtube.signIn)"))?;
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let job = new_job(s, format!("Uploading “{}” to YouTube", meta.title.trim()));
    set_status(&job.progress, if after.is_some() { WAITING_FOR_EXPORT } else { "Starting the upload" });
    let upload = Upload { path, meta, after, client, account, dir, upload_url: s.publish.upload_url.clone(), access: s.publish.access.clone() };
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let id = spawn(s, job, wait, "filmcraft-youtube-upload", move || {
        let t0 = web_time::Instant::now();
        let r = upload.run(&prog);
        if matches!(r, Err(PublishError::Unauthorized)) {
            // Google revoked or expired the refresh token: the user signs in again.
            forget_account(&upload.dir, &upload.access);
        }
        let bytes = prog.total.load(Ordering::Relaxed);
        finish(&prog, &res, r.map(|url| (format!("Uploaded to YouTube: {url}"), url)), t0, bytes);
    })?;
    Ok(json!({"job": id}))
}

/// The size of a file that can be uploaded.
fn video_size(path: &str) -> std::result::Result<u64, PublishError> {
    let file = std::fs::metadata(path).map_err(|e| PublishError::Config(format!("can't read {path}: {e}")))?;
    if !file.is_file() || file.len() == 0 {
        return Err(PublishError::Config(format!("{path} is not a video file")));
    }
    Ok(file.len())
}

/// One upload job's inputs.
struct Upload {
    path: String,
    meta: VideoMetadata,
    /// The export that writes the file.
    after: Option<crate::Job>,
    client: ClientConfig,
    account: Account,
    dir: PathBuf,
    upload_url: Option<String>,
    access: Arc<Mutex<Option<Access>>>,
}

impl Upload {
    /// The video's address. A refused access token is refreshed once.
    fn run(&self, prog: &Progress) -> std::result::Result<String, PublishError> {
        let path = self.wait_for_export(prog)?;
        let size = video_size(&path)?;
        prog.total.store(size, Ordering::Relaxed);
        set_status(prog, "Starting the upload");
        let transport = default_transport().ok_or_else(|| PublishError::Config("this build can't publish".into()))?;
        let transport = &*transport;
        let mut control = JobControl { prog };
        let mut token = self.access_token(transport, false)?;
        let mut refreshed = false;
        let session = loop {
            match self.uploader(transport, &token).start(&self.meta, size, content_type(&path), &mut control) {
                Err(PublishError::Unauthorized) if !refreshed => (token, refreshed) = (self.access_token(transport, true)?, true),
                r => break r?,
            }
        };
        let video = loop {
            match self.send(transport, &token, &path, &session, &mut control) {
                Err(PublishError::Unauthorized) if !refreshed => (token, refreshed) = (self.access_token(transport, true)?, true),
                r => break r?,
            }
        };
        Ok(video)
    }

    /// The file to upload: the export's own path once it has finished (the `path` param without
    /// `afterJob`). A failed or cancelled export uploads nothing.
    fn wait_for_export(&self, prog: &Progress) -> std::result::Result<String, PublishError> {
        let Some(export) = &self.after else { return Ok(self.path.clone()) };
        // The report, not `finished`, ends the export: an export marks itself finished a moment
        // before its job stores the report.
        let mut finished_at = None;
        let report = loop {
            if let Some(r) = export.result.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                break r;
            }
            if prog.cancel.load(Ordering::Relaxed) {
                return Err(PublishError::Cancelled);
            }
            if export.progress.finished.load(Ordering::Relaxed) && finished_at.get_or_insert_with(web_time::Instant::now).elapsed() > REPORT_GRACE {
                return Err(PublishError::Config("the export stopped, so nothing was uploaded".into()));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        match report {
            Ok(report) if !report.path.is_empty() => Ok(report.path),
            Ok(_) => Ok(self.path.clone()),
            // `filmcraft_export::ExportError::Cancelled`, as the export queue reads it too
            Err(e) if e == "cancelled" => Err(PublishError::Config("the export was cancelled, so nothing was uploaded".into())),
            Err(e) => Err(PublishError::Config(format!("the export failed ({e}), so nothing was uploaded"))),
        }
    }

    fn send(
        &self,
        transport: &dyn Transport,
        token: &str,
        path: &str,
        session: &UploadSession,
        control: &mut JobControl,
    ) -> std::result::Result<String, PublishError> {
        let mut file = std::fs::File::open(path)?;
        set_status(control.prog, "Uploading");
        Ok(self.uploader(transport, token).upload(session, &mut file, control)?.watch_url())
    }

    fn uploader<'a>(&'a self, transport: &'a dyn Transport, token: &'a str) -> Uploader<'a> {
        let up = Uploader::new(transport, token);
        match &self.upload_url {
            Some(url) => up.with_upload_url(url),
            None => up,
        }
    }

    /// The cached access token, or a new one from the refresh token.
    fn access_token(&self, transport: &dyn Transport, force: bool) -> std::result::Result<String, PublishError> {
        if !force {
            let cached = self.access.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(a) = cached.as_ref()
                && a.client_id == self.client.client_id
                && web_time::Instant::now() < a.until
            {
                return Ok(a.token.clone());
            }
        }
        let tokens = oauth::refresh(transport, &self.client, &self.account.refresh_token)?;
        if !tokens.grants(UPLOAD_SCOPE) {
            return Err(PublishError::Unauthorized);
        }
        remember(&self.access, &self.client, &tokens);
        Ok(tokens.access_token)
    }
}

fn remember(access: &Mutex<Option<Access>>, client: &ClientConfig, tokens: &oauth::Tokens) {
    let until = web_time::Instant::now() + tokens.expires_in.saturating_sub(EXPIRY_MARGIN);
    *access.lock().unwrap_or_else(PoisonError::into_inner) = Some(Access { token: tokens.access_token.clone(), client_id: client.client_id.clone(), until });
}

/// The upload's progress and retry waits, shown on its job; `jobs.cancel` stops it.
struct JobControl<'a> {
    prog: &'a Progress,
}

impl Control for JobControl<'_> {
    fn progress(&mut self, sent: u64, total: u64) -> bool {
        self.prog.total.store(total, Ordering::Relaxed);
        self.prog.done.store(sent, Ordering::Relaxed);
        !self.prog.cancel.load(Ordering::Relaxed)
    }

    fn wait(&mut self, delay: Duration) -> bool {
        set_status(self.prog, &format!("Connection problem: trying again in {} s", delay.as_secs().max(1)));
        let step = Duration::from_millis(100);
        let mut left = delay;
        while !left.is_zero() && !self.prog.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(left.min(step));
            left = left.saturating_sub(step);
        }
        set_status(self.prog, "Uploading");
        !self.prog.cancel.load(Ordering::Relaxed)
    }
}

/// The MIME type YouTube is told, from the file name.
fn content_type(path: &str) -> &'static str {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mpg" | "mpeg" => "video/mpeg",
        "ts" | "m2ts" | "mts" => "video/mp2t",
        "wmv" => "video/x-ms-wmv",
        "3gp" => "video/3gpp",
        _ => "application/octet-stream",
    }
}

fn set_status(prog: &Progress, status: &str) {
    *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = status.into();
}

/// End a job: `Ok((status, path))` (the path is the video's address for an upload).
fn finish(
    prog: &Progress,
    res: &Mutex<Option<std::result::Result<filmcraft_export::Report, String>>>,
    r: std::result::Result<(String, String), PublishError>,
    t0: web_time::Instant,
    bytes: u64,
) {
    let seconds = t0.elapsed().as_secs_f64();
    let r = match r {
        Ok((status, path)) => {
            set_status(prog, &status);
            prog.done.store(prog.total.load(Ordering::Relaxed), Ordering::Relaxed);
            Ok(filmcraft_export::Report { path, frames: 0, seconds, bytes, render_fps: 0.0, extra_files: Vec::new() })
        }
        Err(e) => {
            let message = e.to_string();
            set_status(prog, &message);
            *prog.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(message.clone());
            Err(message)
        }
    };
    prog.finished.store(true, Ordering::Relaxed);
    *res.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("publish.youtube.status", "YouTube Publishing Status", "{}", status),
        spec("publish.youtube.setClient", "Set YouTube OAuth Client", r#"{"path":str}"#, set_client),
        spec("publish.youtube.signIn", "Sign In to YouTube", r#"{"timeoutSeconds":f64=300,"wait":bool=false}"#, sign_in),
        spec("publish.youtube.signOut", "Sign Out of YouTube", "{}", sign_out),
        spec(
            "publish.youtube.upload",
            "Upload to YouTube",
            r#"{"path":str,"title":str,"description":str?,"tags":[str]?,"categoryId":str?,"privacy":"private|unlisted|public"=private,"madeForKids":bool=false,"afterJob":int?,"wait":bool=false}"#,
            upload,
        ),
    ]
}

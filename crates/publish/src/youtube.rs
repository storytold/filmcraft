//! YouTube Data API v3 uploads: `videos.insert` over the resumable upload protocol
//! (<https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol>).
//!
//! [`Uploader::start`] checks the metadata against YouTube's limits and opens an upload session;
//! [`Uploader::upload`] sends the file in chunks. Before the first chunk and after every failure it
//! asks the server how many bytes it already has, so an upload cut by a dropped connection, a
//! server error or an app restart (the caller keeps the [`UploadSession`]) goes on from there.

use crate::http::{Method, Request, Response, Transport};
use crate::{Control, PublishError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::time::Duration;

/// The `videos.insert` upload endpoint.
pub const UPLOAD_URL: &str = "https://www.googleapis.com/upload/youtube/v3/videos";
/// The OAuth scope an upload needs (a sensitive scope: Google reviews apps that request it).
pub const UPLOAD_SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";

/// Every chunk but the last is a multiple of this.
pub const CHUNK_UNIT: u64 = 256 * 1024;
/// 8 MiB.
pub const DEFAULT_CHUNK_SIZE: u64 = 32 * CHUNK_UNIT;
/// 64 MiB, the largest buffer an upload holds.
pub const MAX_CHUNK_SIZE: u64 = 256 * CHUNK_UNIT;
/// Consecutive failed attempts (errors, or chunks of which the server kept nothing) before giving up.
pub const DEFAULT_MAX_FAILURES: u32 = 8;

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(64);
const RETRY_AFTER_MAX: Duration = Duration::from_secs(600);
const TITLE_MAX_CHARS: usize = 100;
const DESCRIPTION_MAX_BYTES: usize = 5000;
const TAGS_MAX_CHARS: usize = 500;
const MESSAGE_MAX_CHARS: usize = 300;

/// Who can watch the video. Uploads default to private.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    #[default]
    Private,
    Unlisted,
    Public,
}

/// What the upload form asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VideoMetadata {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    /// A YouTube category number ("22" is People & Blogs); YouTube picks one when it is unset.
    pub category_id: Option<String>,
    pub privacy: Privacy,
    /// The channel owner's declaration that the video is made for children.
    pub made_for_kids: bool,
}

impl VideoMetadata {
    /// The limits YouTube applies to `videos.insert`, checked before anything is sent: a title of
    /// 1–100 characters, a description of at most 5000 bytes, neither with `<` or `>`, and tags of
    /// at most 500 characters together (a tag with spaces counts its quotes; commas between tags
    /// count too).
    pub fn validate(&self) -> Result<(), PublishError> {
        let bad = |message: String| Err(PublishError::InvalidMetadata(message));
        let title = self.title.trim();
        if title.is_empty() {
            return bad("the title is empty".into());
        }
        if title.chars().count() > TITLE_MAX_CHARS {
            return bad(format!("the title is longer than {TITLE_MAX_CHARS} characters"));
        }
        if has_angle_bracket(title) {
            return bad("the title can't contain < or >".into());
        }
        if self.description.len() > DESCRIPTION_MAX_BYTES {
            return bad(format!("the description is longer than {DESCRIPTION_MAX_BYTES} bytes"));
        }
        if has_angle_bracket(&self.description) {
            return bad("the description can't contain < or >".into());
        }
        self.validate_tags()?;
        if let Some(category) = &self.category_id
            && (category.is_empty() || category.len() > 4 || !category.bytes().all(|b| b.is_ascii_digit()))
        {
            return bad("the category must be a YouTube category number".into());
        }
        Ok(())
    }

    fn validate_tags(&self) -> Result<(), PublishError> {
        let mut length = self.tags.len().saturating_sub(1);
        for tag in self.tags.iter().map(|t| t.trim()) {
            if tag.is_empty() {
                return Err(PublishError::InvalidMetadata("a tag is empty".into()));
            }
            if tag.contains(',') || has_angle_bracket(tag) {
                return Err(PublishError::InvalidMetadata(format!("the tag “{tag}” can't contain a comma, < or >")));
            }
            let quotes = if tag.contains(' ') { 2 } else { 0 };
            length = length.saturating_add(tag.chars().count() + quotes);
        }
        if length > TAGS_MAX_CHARS {
            return Err(PublishError::InvalidMetadata(format!("the tags are longer than {TAGS_MAX_CHARS} characters together")));
        }
        Ok(())
    }

    /// The `video` resource `videos.insert` takes, with parts `snippet` and `status`.
    pub fn resource(&self) -> Value {
        let mut snippet = Map::new();
        snippet.insert("title".into(), json!(self.title.trim()));
        snippet.insert("description".into(), json!(self.description));
        if !self.tags.is_empty() {
            snippet.insert("tags".into(), json!(self.tags.iter().map(|t| t.trim()).collect::<Vec<_>>()));
        }
        if let Some(category) = &self.category_id {
            snippet.insert("categoryId".into(), json!(category));
        }
        json!({
            "snippet": snippet,
            "status": {"privacyStatus": self.privacy, "selfDeclaredMadeForKids": self.made_for_kids},
        })
    }
}

fn has_angle_bracket(s: &str) -> bool {
    s.contains(['<', '>'])
}

/// An open upload session. Keep it (it is serde) to resume after a restart; the server keeps it
/// for about a week. Its address authorizes the upload, so `Debug` leaves it out.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSession {
    pub uri: String,
    pub total: u64,
    pub content_type: String,
}

impl std::fmt::Debug for UploadSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadSession").field("total", &self.total).field("content_type", &self.content_type).finish_non_exhaustive()
    }
}

/// The video YouTube created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedVideo {
    pub id: String,
}

impl UploadedVideo {
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
}

/// Uploads one video for the account `access_token` belongs to.
pub struct Uploader<'a> {
    transport: &'a dyn Transport,
    access_token: &'a str,
    upload_url: String,
    chunk_size: u64,
    max_failures: u32,
}

impl<'a> Uploader<'a> {
    pub fn new(transport: &'a dyn Transport, access_token: &'a str) -> Self {
        Self { transport, access_token, upload_url: UPLOAD_URL.into(), chunk_size: DEFAULT_CHUNK_SIZE, max_failures: DEFAULT_MAX_FAILURES }
    }

    /// Talk to another endpoint (a test server). Session addresses must be on the same origin.
    pub fn with_upload_url(mut self, url: &str) -> Self {
        self.upload_url = url.into();
        self
    }

    /// Bytes per request, rounded down to a multiple of 256 KiB and kept within 256 KiB–64 MiB.
    pub fn with_chunk_size(mut self, bytes: u64) -> Self {
        self.chunk_size = bytes.clamp(CHUNK_UNIT, MAX_CHUNK_SIZE) / CHUNK_UNIT * CHUNK_UNIT;
        self
    }

    pub fn with_max_failures(mut self, failures: u32) -> Self {
        self.max_failures = failures;
        self
    }

    /// Check `meta` and open a session for a file of `total` bytes. Server errors and lost
    /// connections are retried; `control` waits between attempts.
    pub fn start(&self, meta: &VideoMetadata, total: u64, content_type: &str, control: &mut dyn Control) -> Result<UploadSession, PublishError> {
        meta.validate()?;
        if total == 0 {
            return Err(PublishError::EmptyFile);
        }
        if !valid_content_type(content_type) {
            return Err(PublishError::InvalidMetadata(format!("YouTube doesn't take files of type {content_type:?}")));
        }
        let body = serde_json::to_vec(&meta.resource()).map_err(|e| PublishError::Protocol(e.to_string()))?;
        let url = format!("{}?uploadType=resumable&part=snippet,status", self.upload_url);
        let headers = vec![
            ("Authorization", self.authorization()?),
            ("Content-Type", "application/json; charset=UTF-8".into()),
            ("X-Upload-Content-Length", total.to_string()),
            ("X-Upload-Content-Type", content_type.into()),
        ];
        let request = Request { method: Method::Post, url: &url, headers, body: &body };
        let mut failures = 0;
        loop {
            let response = match self.transport.send(&request) {
                Ok(response) => response,
                Err(e) => {
                    self.retry(&mut failures, None, control, PublishError::Network(e.0))?;
                    continue;
                }
            };
            if response.status == 200 {
                let uri = response.header("location").ok_or_else(|| PublishError::Protocol("no upload session address".into()))?;
                if !same_origin(uri, &self.upload_url) {
                    return Err(PublishError::Protocol("the upload session is on another server".into()));
                }
                return Ok(UploadSession { uri: uri.into(), total, content_type: content_type.into() });
            }
            match classify(&response, total, false) {
                Reply::Retry(after, why) => self.retry(&mut failures, after, control, PublishError::Network(why))?,
                Reply::Fail(e) => return Err(e),
                Reply::Done(_) | Reply::Received(_) => return Err(PublishError::Protocol(format!("status {} opening the session", response.status))),
            }
        }
    }

    /// Send `source` (the file the session was opened for) and return the new video. Starts by
    /// asking the server how much it has, so the same call resumes an interrupted upload.
    pub fn upload<R: Read + Seek + ?Sized>(&self, session: &UploadSession, source: &mut R, control: &mut dyn Control) -> Result<UploadedVideo, PublishError> {
        let total = session.total;
        if total == 0 {
            return Err(PublishError::EmptyFile);
        }
        if !same_origin(&session.uri, &self.upload_url) {
            return Err(PublishError::Protocol("the upload session is on another server".into()));
        }
        if source.seek(SeekFrom::End(0))? != total {
            return Err(PublishError::FileChanged);
        }
        let authorization = self.authorization()?;
        let mut buf = Vec::new();
        let mut failures = 0;
        // None: ask the server how many bytes it has before sending more.
        let mut at: Option<u64> = None;
        loop {
            if let Some(from) = at
                && !control.progress(from, total)
            {
                return Err(PublishError::Cancelled);
            }
            let reply = match at {
                None => self.query(session, &authorization),
                Some(from) => self.send_chunk(session, &authorization, source, from, &mut buf)?,
            };
            match reply {
                Reply::Done(video) => {
                    control.progress(total, total);
                    return Ok(video);
                }
                // Everything arrived but the video isn't confirmed yet: ask again.
                Reply::Received(next) if next >= total => {
                    self.retry(&mut failures, None, control, PublishError::Stalled)?;
                    at = None;
                }
                Reply::Received(next) => {
                    match at {
                        Some(from) if next <= from => self.retry(&mut failures, None, control, PublishError::Stalled)?,
                        Some(_) => failures = 0,
                        None => {}
                    }
                    at = Some(next);
                }
                Reply::Retry(after, why) => {
                    self.retry(&mut failures, after, control, PublishError::Network(why))?;
                    at = None;
                }
                Reply::Fail(e) => return Err(e),
            }
        }
    }

    /// `PUT` with no body and `Content-Range: bytes */total`: how many bytes the server has.
    fn query(&self, session: &UploadSession, authorization: &str) -> Reply {
        let headers = vec![("Authorization", authorization.into()), ("Content-Range", format!("bytes */{}", session.total))];
        let request = Request { method: Method::Put, url: &session.uri, headers, body: &[] };
        match self.transport.send(&request) {
            Ok(response) => classify(&response, session.total, true),
            Err(e) => Reply::Retry(None, e.0),
        }
    }

    fn send_chunk<R: Read + Seek + ?Sized>(
        &self,
        session: &UploadSession,
        authorization: &str,
        source: &mut R,
        from: u64,
        buf: &mut Vec<u8>,
    ) -> Result<Reply, PublishError> {
        let total = session.total;
        let len = self.chunk_size.min(total.saturating_sub(from));
        let len = usize::try_from(len).map_err(|_| PublishError::Protocol("chunk too large".into()))?;
        if len == 0 {
            return Err(PublishError::Protocol("nothing left to send".into()));
        }
        buf.clear();
        buf.resize(len, 0);
        source.seek(SeekFrom::Start(from))?;
        source.read_exact(buf).map_err(|e| if e.kind() == ErrorKind::UnexpectedEof { PublishError::FileChanged } else { e.into() })?;
        let last = from.saturating_add(len as u64 - 1);
        let headers = vec![
            ("Authorization", authorization.into()),
            ("Content-Type", session.content_type.clone()),
            ("Content-Range", format!("bytes {from}-{last}/{total}")),
        ];
        let request = Request { method: Method::Put, url: &session.uri, headers, body: buf };
        Ok(match self.transport.send(&request) {
            Ok(response) => classify(&response, total, true),
            Err(e) => Reply::Retry(None, e.0),
        })
    }

    /// Count a failed attempt and wait before the next one; `give_up` once there were too many.
    fn retry(&self, failures: &mut u32, after: Option<Duration>, control: &mut dyn Control, give_up: PublishError) -> Result<(), PublishError> {
        *failures = failures.saturating_add(1);
        if *failures > self.max_failures {
            return Err(give_up);
        }
        if control.wait(after.unwrap_or_else(|| backoff(*failures))) { Ok(()) } else { Err(PublishError::Cancelled) }
    }

    fn authorization(&self) -> Result<String, PublishError> {
        let token = self.access_token;
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(PublishError::Unauthorized);
        }
        Ok(format!("Bearer {token}"))
    }
}

/// 1 s, 2 s, 4 s … up to 64 s.
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    BACKOFF_START.saturating_mul(1 << doublings).min(BACKOFF_MAX)
}

fn valid_content_type(content_type: &str) -> bool {
    (content_type.starts_with("video/") || content_type == "application/octet-stream")
        && content_type.len() <= 100
        && content_type.bytes().all(|b| b.is_ascii_graphic())
}

/// `scheme://authority` of `url`, if it is an http(s) URL.
fn origin(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if authority == 0 {
        return None;
    }
    url.get(..url.len() - rest.len() + authority)
}

fn same_origin(a: &str, b: &str) -> bool {
    matches!((origin(a), origin(b)), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
}

/// What a response means for the upload.
#[derive(Debug)]
pub(crate) enum Reply {
    Done(UploadedVideo),
    /// The server has bytes `0..n`.
    Received(u64),
    /// Try again (after the server's `Retry-After`, if it gave one).
    Retry(Option<Duration>, String),
    Fail(PublishError),
}

/// `in_session`: the response is to a request on an open session (where 404 means it expired).
pub(crate) fn classify(response: &Response, total: u64, in_session: bool) -> Reply {
    match response.status {
        200 | 201 => match uploaded_video(&response.body) {
            Ok(video) => Reply::Done(video),
            Err(e) => Reply::Fail(e),
        },
        308 => match response.header("range") {
            None => Reply::Received(0),
            Some(range) => match received_bytes(range, total) {
                Some(n) => Reply::Received(n),
                None => Reply::Fail(PublishError::Protocol(format!("Range {:?}", truncate(range, 60)))),
            },
        },
        status => api_error(status, response, in_session),
    }
}

fn api_error(status: u16, response: &Response, in_session: bool) -> Reply {
    let (reason, message) = google_error(&response.body);
    let retry_after = response.header("retry-after").and_then(retry_after);
    match (status, reason.as_deref()) {
        (_, Some("rateLimitExceeded" | "userRateLimitExceeded" | "backendError")) | (429 | 500 | 502 | 503 | 504, _) => Reply::Retry(
            retry_after,
            if message.is_empty() { format!("the server answered {status}") } else { format!("the server answered {status}: {message}") },
        ),
        (401, Some("youtubeSignupRequired")) => Reply::Fail(PublishError::NoChannel),
        (401, _) => Reply::Fail(PublishError::Unauthorized),
        (403, Some("quotaExceeded")) => Reply::Fail(PublishError::QuotaExceeded),
        (_, Some("uploadLimitExceeded")) => Reply::Fail(PublishError::UploadLimitExceeded),
        (404 | 410, _) if in_session => Reply::Fail(PublishError::SessionExpired),
        _ => Reply::Fail(PublishError::Api { status, reason, message }),
    }
}

/// `Range: bytes=0-<last>` → the count of bytes received (`last + 1`), at most `total`.
pub(crate) fn received_bytes(range: &str, total: u64) -> Option<u64> {
    let (first, last) = range.trim().strip_prefix("bytes=")?.split_once('-')?;
    if first.trim() != "0" {
        return None;
    }
    let received = last.trim().parse::<u64>().ok()?.checked_add(1)?;
    (received <= total).then_some(received)
}

/// `Retry-After` in seconds (the HTTP-date form falls back to the backoff), at most 10 minutes.
pub(crate) fn retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(|s| Duration::from_secs(s).min(RETRY_AFTER_MAX))
}

/// `(reason, message)` of a Google API error body; an empty message when it has none.
pub(crate) fn google_error(body: &[u8]) -> (Option<String>, String) {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return (None, String::new());
    };
    let error = value.get("error");
    let message = error.and_then(|e| e.get("message")).and_then(Value::as_str).map(|m| truncate(m, MESSAGE_MAX_CHARS)).unwrap_or_default();
    let reason = error
        .and_then(|e| e.get("errors"))
        .and_then(|e| e.get(0))
        .and_then(|e| e.get("reason"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty() && r.len() <= 64 && r.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(str::to_string);
    (reason, message)
}

/// The video resource YouTube returns when the upload completes.
pub(crate) fn uploaded_video(body: &[u8]) -> Result<UploadedVideo, PublishError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| PublishError::Protocol("the finished upload returned no video".into()))?;
    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(PublishError::Protocol("the finished upload returned no valid video id".into()));
    }
    Ok(UploadedVideo { id: id.into() })
}

fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((at, _)) => format!("{}…", s.get(..at).unwrap_or_default()),
        None => s.into(),
    }
}

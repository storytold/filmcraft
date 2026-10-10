//! Long exports over MCP (docs/agents.md § Long exports): `file.exportMedia` with `wait: true`
//! runs as a background engine job; the server reports its progress as MCP
//! `notifications/progress` (when the request carried a `progressToken`) and stops it on
//! `notifications/cancelled`, deleting the partial output. The session lock is only held for
//! short polls, so other requests are answered while the export runs.

use std::path::Path;
use std::time::{Duration, SystemTime};

use rmcp::RoleServer;
use rmcp::model::{CallToolResult, ContentBlock as Content, ProgressNotificationParam, ProgressToken};
use rmcp::service::{Peer, RequestContext};
use serde_json::{Value, json};

use crate::server::FilmcraftMcp;

/// Engine commands that block until a render is written when called with `wait: true`.
pub const LONG_COMMANDS: &[&str] = &["file.exportMedia"];

/// How often the job is polled (and at most how often progress is reported).
const POLL: Duration = Duration::from_millis(100);

/// Whether `command_run {id, params}` is a long call this module runs.
pub fn is_long(id: &str, params: &Value) -> bool {
    LONG_COMMANDS.contains(&id) && params.get("wait").and_then(Value::as_bool) == Some(true)
}

/// Sends `notifications/progress` for one request; progress only ever increases.
struct Reporter {
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
    last: f64,
}

impl Reporter {
    async fn report(&mut self, done: f64, total: f64, message: &str) {
        let Some(token) = self.token.clone() else { return };
        if total <= 0.0 || done <= self.last {
            return;
        }
        self.last = done;
        let mut p = ProgressNotificationParam::new(token, done).with_total(total);
        if !message.is_empty() {
            p = p.with_message(message);
        }
        let _ = self.peer.notify_progress(p).await;
    }
}

impl FilmcraftMcp {
    /// Run `id` (a [`LONG_COMMANDS`] entry, `wait: true`) as a job, reporting progress and
    /// honouring cancellation.
    pub(crate) async fn run_long(&self, id: &str, mut params: Value, context: &RequestContext<RoleServer>) -> CallToolResult {
        let started = SystemTime::now();
        if let Some(p) = params.as_object_mut() {
            p.insert("wait".into(), json!(false));
        }
        let start = match self.run(id, params).await {
            Ok(v) => v,
            Err(e) => return CallToolResult::error(vec![Content::text(e.to_string())]),
        };
        let Some(job) = start.get("job").and_then(Value::as_u64) else {
            return CallToolResult::error(vec![Content::text(format!("{id} started no job: {start}"))]);
        };
        let path = start.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut rep = Reporter { peer: context.peer.clone(), token: context.meta.get_progress_token(), last: 0.0 };
        loop {
            let state = self.job_state(job).await;
            let finished = state.as_ref().is_none_or(|j| j["finished"].as_bool() == Some(true));
            if let Some(j) = &state {
                let (done, total) = (j["done"].as_f64().unwrap_or(0.0), j["total"].as_f64().unwrap_or(0.0));
                rep.report(done, total, j["status"].as_str().unwrap_or_default()).await;
            }
            if finished {
                let result = state.map(|j| j["result"].clone()).unwrap_or(Value::Null);
                if let Some(e) = result.get("error").and_then(Value::as_str) {
                    return CallToolResult::error(vec![Content::text(format!("export failed: {e}"))]);
                }
                let mut out = start;
                if let Some(o) = out.as_object_mut() {
                    o.insert("result".into(), result);
                }
                return CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&out).unwrap_or_default())]);
            }
            tokio::select! {
                _ = context.ct.cancelled() => break,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
        // Cancelled: stop the encode at the next batch, wait for the worker, delete the partial file.
        let _ = self.run("jobs.cancel", json!({"job": job})).await;
        for _ in 0..600 {
            if self.job_state(job).await.is_none_or(|j| j["finished"].as_bool() == Some(true)) {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        remove_partial(&path, started);
        CallToolResult::error(vec![Content::text("cancelled")])
    }

    /// The job's `jobs.list` entry.
    async fn job_state(&self, job: u64) -> Option<Value> {
        let jobs = self.run("jobs.list", json!({})).await.ok()?;
        jobs.as_array()?.iter().find(|j| j["id"].as_u64() == Some(job)).cloned()
    }
}

/// Delete what an interrupted export wrote, and nothing else: the output file itself, the numbered
/// stills of an image sequence (`<stem><3+ digits>.<same extension>`, only when the output is a
/// PNG/TIFF/BMP sequence) and the caption sidecar
/// (`<stem>.srt` / `<stem>.vtt`), each only if it was written since `since`. Other files that
/// merely share the name's stem (`<stem>.aep`, `<stem>2.psd`, `<stem>1.mp4` beside a movie, …) are
/// never touched, even when they were saved during the export.
pub fn remove_partial(path: &str, since: SystemTime) {
    let p = Path::new(path);
    let (Some(dir), Some(stem), Some(file)) = (p.parent(), p.file_stem().and_then(|s| s.to_str()), p.file_name().and_then(|s| s.to_str())) else {
        return;
    };
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    // file times are coarser than the clock
    let since = since.checked_sub(Duration::from_secs(1)).unwrap_or(since);
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_export_output(name, file, stem, ext) && e.metadata().is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|t| t >= since)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Whether `name` is a file an export to `file` (`stem` + `.ext`) writes.
fn is_export_output(name: &str, file: &str, stem: &str, ext: &str) -> bool {
    if name == file {
        return true;
    }
    let Some(rest) = name.strip_prefix(stem) else { return false };
    if let Some(sidecar) = rest.strip_prefix('.') {
        return sidecar.eq_ignore_ascii_case("srt") || sidecar.eq_ignore_ascii_case("vtt");
    }
    // image sequence: the frame number (at least three digits, like `image_sequence_path`), then the
    // output's own extension; movies and audio are a single file, so numbered neighbours are not theirs
    if !is_sequence_extension(ext) {
        return false;
    }
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits >= 3 && rest.get(digits..).is_some_and(|r| r.strip_prefix('.').is_some_and(|x| x == ext))
}

/// Whether an export to a file with extension `ext` writes an image sequence (one still per frame).
fn is_sequence_extension(ext: &str) -> bool {
    use filmcraft_engine::export::Format;
    Format::from_name(ext).is_some_and(|f| matches!(f, Format::PngSequence | Format::TiffSequence | Format::BmpSequence))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exports_own_files_are_removed() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        let ours = ["trailer.png", "trailer000.png", "trailer123.png", "trailer.srt", "trailer.vtt"];
        // same stem, written during the export, but not the export's
        let theirs = ["trailer.aep", "trailer2.psd", "trailer.docx", "trailer000.jpg", "trailers.png", "trailer.png.bak", "other.png"];
        for f in ours.iter().chain(theirs.iter()) {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_partial(&dir.join("trailer.png").to_string_lossy(), started);
        for f in ours {
            assert!(!dir.join(f).exists(), "{f} should be removed");
        }
        for f in theirs {
            assert!(dir.join(f).exists(), "{f} must not be removed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn numbered_files_beside_a_movie_are_kept() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-movie-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        // a movie export writes one file: `clip1.mp4` and `clip002.mp4` are other exports
        let ours = ["clip.mp4", "clip.srt"];
        let theirs = ["clip1.mp4", "clip002.mp4", "clip2.mov"];
        for f in ours.iter().chain(theirs.iter()) {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_partial(&dir.join("clip.mp4").to_string_lossy(), started);
        for f in ours {
            assert!(!dir.join(f).exists(), "{f} should be removed");
        }
        for f in theirs {
            assert!(dir.join(f).exists(), "{f} must not be removed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sequence_frames_have_at_least_three_digits() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-digits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        // frames are numbered with at least three digits (`shot000.tiff`); `shot2.tiff` is not one
        let ours = ["shot.tiff", "shot000.tiff", "shot1234.tiff"];
        let theirs = ["shot2.tiff", "shot12.tiff"];
        for f in ours.iter().chain(theirs.iter()) {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_partial(&dir.join("shot.tiff").to_string_lossy(), started);
        for f in ours {
            assert!(!dir.join(f).exists(), "{f} should be removed");
        }
        for f in theirs {
            assert!(dir.join(f).exists(), "{f} must not be removed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_older_than_the_export_are_kept() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("clip.mp4"), b"x").unwrap();
        remove_partial(&dir.join("clip.mp4").to_string_lossy(), SystemTime::now() + Duration::from_secs(60));
        assert!(dir.join("clip.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

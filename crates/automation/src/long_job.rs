//! Long exports over MCP (docs/agents.md § Long exports): `file.exportMedia` with `wait: true`
//! runs as a background engine job; the server reports its progress as MCP
//! `notifications/progress` (when the request carried a `progressToken`) and stops it on
//! `notifications/cancelled`, deleting the partial output. The session lock is only held for
//! short polls, so other requests are answered while the export runs.

use std::ffi::OsStr;
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
        let out = partial_output(&start);
        let mut rep = Reporter { peer: context.peer.clone(), token: context.meta.get_progress_token(), last: 0.0 };
        loop {
            let state = self.job_state(job).await;
            let finished = state.as_ref().is_none_or(|j| j["finished"].as_bool() == Some(true));
            if let Some(j) = &state {
                let (done, total) = (j["done"].as_f64().unwrap_or(0.0), j["total"].as_f64().unwrap_or(0.0));
                rep.report(done, total, j["status"].as_str().unwrap_or_default()).await;
            }
            if finished {
                let offline = state.as_ref().and_then(|j| j.get("offlineMedia").cloned());
                let result = state.map(|j| j["result"].clone()).unwrap_or(Value::Null);
                if let Some(e) = result.get("error").and_then(Value::as_str) {
                    return CallToolResult::error(vec![Content::text(format!("export failed: {e}"))]);
                }
                let mut out = start;
                if let Some(o) = out.as_object_mut() {
                    o.insert("result".into(), result);
                    if let Some(off) = offline {
                        o.insert("offlineMedia".into(), off);
                    }
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
        remove_partial(&out, started);
        CallToolResult::error(vec![Content::text("cancelled")])
    }

    /// The job's `jobs.list` entry.
    async fn job_state(&self, job: u64) -> Option<Value> {
        let jobs = self.run("jobs.list", json!({})).await.ok()?;
        jobs.as_array()?.iter().find(|j| j["id"].as_u64() == Some(job)).cloned()
    }
}

/// What a started export writes, as the clean-up after a cancel finds it.
#[derive(Debug, PartialEq)]
pub struct Output {
    /// The file written, or for an image sequence the name its stills are numbered from.
    pub path: String,
    /// An image sequence's stills, `<stem><index>.<ext>`: the digits the index is padded to and the
    /// last index written (from 0). `None` for a single file, or a sequence that writes no stills.
    pub stills: Option<(usize, u64)>,
}

/// What a started `file.exportMedia` writes, from its result: for an image sequence the name in
/// `basePath`, numbered like its last still (`lastPath`), else the file in `path`.
fn partial_output(start: &Value) -> Output {
    let text = |key: &str| start.get(key).and_then(Value::as_str);
    match (text("basePath"), text("lastPath")) {
        (Some(base), Some(last)) => Output { path: base.to_string(), stills: still_numbering(base, last) },
        _ => Output { path: text("path").unwrap_or_default().to_string(), stills: None },
    }
}

/// The width and index of the last still of a sequence numbered from `base`: the digits between
/// the base name's stem and its extension.
fn still_numbering(base: &str, last: &str) -> Option<(usize, u64)> {
    let (b, l) = (Path::new(base), Path::new(last));
    if b.parent() != l.parent() || b.extension() != l.extension() {
        return None;
    }
    let digits = l.file_stem()?.to_str()?.strip_prefix(b.file_stem()?.to_str()?)?;
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((digits.len(), digits.parse().ok()?))
}

/// Delete what an interrupted export wrote, and nothing else: the output file itself (a movie or
/// an audio file; an image sequence numbers every still, so a file under its base name is not
/// its own), the stills of an image sequence (only the names it numbers them with: the same
/// digits, from 0 to the last index) and the caption sidecar (`<stem>.srt` / `<stem>.vtt`), each
/// only if it was written since `since`. Other files that merely share the name's stem
/// (`<stem>.aep`, `<stem>2.psd`, `<stem>1.mp4` beside a movie, the stills of an export to
/// `<stem>1.png`, ...) are never touched, even when they were saved during the export.
pub fn remove_partial(out: &Output, since: SystemTime) {
    let Some(dir) = Path::new(&out.path).parent() else { return };
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    // file times are coarser than the clock
    let since = since.checked_sub(Duration::from_secs(1)).unwrap_or(since);
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_export_output(name, out) && e.metadata().is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|t| t >= since)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Whether `name`, a file in the output's folder, is one the export of `out` writes.
fn is_export_output(name: &str, out: &Output) -> bool {
    let p = Path::new(&out.path);
    let (Some(file), Some(stem)) = (p.file_name().and_then(OsStr::to_str), p.file_stem().and_then(OsStr::to_str)) else {
        return false;
    };
    let ext = p.extension().and_then(OsStr::to_str).unwrap_or_default();
    if name == file {
        return out.stills.is_none() && !is_sequence_extension(ext);
    }
    let Some(rest) = name.strip_prefix(stem) else { return false };
    if let Some(sidecar) = rest.strip_prefix('.') {
        return sidecar.eq_ignore_ascii_case("srt") || sidecar.eq_ignore_ascii_case("vtt");
    }
    // a still: an index it wrote, with exactly its digits, then the output's own extension (an
    // export to `a1.jpeg` writes `a1000.jpeg`, which is not a still of a short export to `a.jpeg`)
    let Some((width, last)) = out.stills else { return false };
    let Some((digits, x)) = rest.split_once('.') else { return false };
    x == ext && digits.len() == width && digits.bytes().all(|c| c.is_ascii_digit()) && digits.parse::<u64>().is_ok_and(|i| i <= last)
}

/// Whether an export to a file with extension `ext` writes an image sequence (one still per frame).
/// `.jpeg` is one too: a JPEG sequence keeps that extension, though `jpeg` as a format name is
/// Motion-JPEG (a `.mov`).
fn is_sequence_extension(ext: &str) -> bool {
    use filmcraft_engine::export::Format;
    ext.eq_ignore_ascii_case("jpeg") || Format::from_name(ext).is_some_and(Format::is_image_sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output of an export to `name` in `dir`.
    fn output(dir: &Path, name: &str, stills: Option<(usize, u64)>) -> Output {
        Output { path: dir.join(name).to_string_lossy().to_string(), stills }
    }

    #[test]
    fn only_the_exports_own_files_are_removed() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        let ours = ["trailer000.png", "trailer123.png", "trailer.srt", "trailer.vtt"];
        // same stem, written during the export, but not the export's (a PNG sequence never writes
        // its unnumbered base name, nor a still past its last)
        let theirs =
            ["trailer.png", "trailer124.png", "trailer.aep", "trailer2.psd", "trailer.docx", "trailer000.jpg", "trailers.png", "trailer.png.bak", "other.png"];
        for f in ours.iter().chain(theirs.iter()) {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_partial(&output(&dir, "trailer.png", Some((3, 123))), started);
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
        remove_partial(&output(&dir, "clip.mp4", None), started);
        for f in ours {
            assert!(!dir.join(f).exists(), "{f} should be removed");
        }
        for f in theirs {
            assert!(dir.join(f).exists(), "{f} must not be removed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stills_have_the_digits_and_the_range_the_export_wrote() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-digits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        // stills numbered with four digits up to 1234: three or five digits, or 1235, are another export's
        let ours = ["shot0000.tiff", "shot1234.tiff"];
        let theirs = ["shot2.tiff", "shot12.tiff", "shot000.tiff", "shot01234.tiff", "shot1235.tiff"];
        for f in ours.iter().chain(theirs.iter()) {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_partial(&output(&dir, "shot.tiff", Some((4, 1234))), started);
        for f in ours {
            assert!(!dir.join(f).exists(), "{f} should be removed");
        }
        for f in theirs {
            assert!(dir.join(f).exists(), "{f} must not be removed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Which files an export writes: a movie its own name, an image sequence only the stills it
    /// numbered (never its unnumbered base name, nor a still of another export in the same folder),
    /// whatever the still extension. The output comes from the export's result, as on a cancel.
    #[test]
    fn export_outputs_by_name() {
        // (file in the folder, the export's name, its last still, written by that export, why)
        let rows = [
            ("a.png", "a.png", Some("a120.png"), false, "a PNG sequence's base name"),
            ("a.png", "a.png", None, false, "the base name of a PNG sequence that wrote no stills"),
            ("a.jpeg", "a.jpeg", Some("a120.jpeg"), false, "a JPEG sequence's base name"),
            ("a000.png", "a.png", Some("a120.png"), true, "a PNG still"),
            ("a000.jpg", "a.jpg", Some("a120.jpg"), true, "a JPEG still (.jpg)"),
            ("a000.jpeg", "a.jpeg", Some("a120.jpeg"), true, "a JPEG still (.jpeg)"),
            ("a120.tif", "a.tif", Some("a120.tif"), true, "the last TIFF still"),
            ("a0001.exr", "a.exr", Some("a1500.exr"), true, "an EXR still of a longer sequence"),
            ("a121.png", "a.png", Some("a120.png"), false, "a still past the last"),
            ("a0001.png", "a.png", Some("a120.png"), false, "more digits than the sequence's"),
            ("a1000.jpeg", "a.jpeg", Some("a120.jpeg"), false, "the first still of an export to a1.jpeg"),
            ("a.srt", "a.png", Some("a120.png"), true, "a sequence's caption sidecar"),
            ("a.mp4", "a.mp4", None, true, "a movie"),
            ("a001.mp4", "a.mp4", None, false, "a numbered file beside a movie"),
            ("a000.jpg", "a.png", Some("a120.png"), false, "a still of another format"),
            ("notes.txt", "a.png", Some("a120.png"), false, "an unrelated file"),
        ];
        for (name, base, last, written, why) in rows {
            let start = match last {
                Some(last) => json!({"path": "a000", "lastPath": last, "basePath": base}),
                None => json!({"path": base}),
            };
            assert_eq!(is_export_output(name, &partial_output(&start)), written, "{why}: {name} for an export to {base}");
        }
    }

    /// A cancelled image sequence: the stills and the caption sidecar go by the name the stills are
    /// numbered from, not by the first still `file.exportMedia` reports as `path` (#4009).
    #[test]
    fn a_cancelled_image_sequence_removes_its_stills_and_sidecar() {
        let dir = std::env::temp_dir().join(format!("fc-remove-partial-sequence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let path = dir.join("a.png").to_string_lossy().to_string();
        let p = json!({"path": path, "format": "png", "range": "custom", "startSeconds": 0, "endSeconds": 0.1, "scale": 0.125, "wait": true});
        let start = s.execute("file.exportMedia", p).unwrap();
        assert!(start["path"].as_str().is_some_and(|f| f.ends_with("a000.png")), "{start}");
        let mut ours: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(ours.len() >= 3, "{ours:?}");
        ours.push("a.srt".into());
        // a still past the last one, and the first of an export to `a1.png`
        let next = format!("a{:03}.png", ours.len() - 1);
        let theirs = ["b000.png", "a2.png", "a.mp4", "a.png.bak", "a1000.png", next.as_str()];
        for f in ours.iter().map(String::as_str).filter(|f| f.ends_with(".srt")).chain(theirs) {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_partial(&partial_output(&start), started);
        for f in &ours {
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
        remove_partial(&output(&dir, "clip.mp4", None), SystemTime::now() + Duration::from_secs(60));
        assert!(dir.join("clip.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

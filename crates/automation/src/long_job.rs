//! Long exports over MCP (docs/agents.md § Long exports): `file.exportMedia` with `wait: true`
//! runs as a background engine job; the server reports its progress as MCP
//! `notifications/progress` (when the request carried a `progressToken`) and stops it on
//! `notifications/cancelled`. Output is retained when file ownership is unconfirmed. The session lock is only held for
//! short polls, so other requests are answered while the export runs.

use std::path::Path;
use std::time::{Duration, SystemTime};

use rmcp::RoleServer;
use rmcp::model::{CallToolResult, ContentBlock as Content, ProgressNotificationParam, ProgressToken};
use rmcp::service::{Peer, RequestContext};
use serde_json::{Value, json};

use crate::AutomationError;
use crate::server::FilmcraftMcp;

/// Engine commands that block until a render is written when called with `wait: true`.
pub const LONG_COMMANDS: &[&str] = &["file.exportMedia"];

/// How often the job is polled (and at most how often progress is reported).
const POLL: Duration = Duration::from_millis(100);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(60);

enum TerminalResult {
    Success,
    Failure,
    Unknown,
}

fn terminal_result(state: &Value) -> TerminalResult {
    let Some(result) = state.get("result").and_then(Value::as_object) else { return TerminalResult::Unknown };
    match result.get("error") {
        Some(Value::String(error)) if !error.trim().is_empty() => TerminalResult::Failure,
        Some(_) => TerminalResult::Unknown,
        None if result.get("frames").and_then(Value::as_u64).is_some() => TerminalResult::Success,
        None => TerminalResult::Unknown,
    }
}

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
        if let Some(p) = params.as_object_mut() {
            p.insert("wait".into(), json!(false));
        }
        // Retain startup until its job ticket arrives. Abandoning this command
        // could start an export without leaving us an ID to cancel or observe.
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
            let queried = tokio::select! {
                biased;
                _ = context.ct.cancelled() => break,
                result = self.job_state(job) => result,
            };
            let state = match queried {
                Ok(state) => state,
                Err(error) => {
                    // A failed observation cannot establish completion. Ask for
                    // cancellation, retaining the original query diagnostic.
                    let cleanup = self.cancel_long_job(job, &path, CLEANUP_TIMEOUT).await;
                    let message = match cleanup {
                        Ok(()) => format!("export state query failed: {error}; export completed successfully, output retained"),
                        Err(cleanup) => format!("export state query failed: {error}; cleanup failed: {cleanup}"),
                    };
                    return CallToolResult::error(vec![Content::text(message)]);
                }
            };
            let finished = state.get("finished").and_then(Value::as_bool) == Some(true);
            let (done, total) = (state.get("done").and_then(Value::as_f64).unwrap_or(0.0), state.get("total").and_then(Value::as_f64).unwrap_or(0.0));
            tokio::select! {
                biased;
                _ = context.ct.cancelled() => break,
                _ = rep.report(done, total, state.get("status").and_then(Value::as_str).unwrap_or_default()) => {}
            }
            if finished {
                let offline = state.get("offlineMedia").cloned();
                let result = state.get("result").cloned().unwrap_or(Value::Null);
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
        // A cancelled request owns this cleanup until a terminal state is
        // confirmed or an explicit unconfirmed-cleanup error is returned.
        match self.cancel_long_job(job, &path, CLEANUP_TIMEOUT).await {
            Ok(()) => CallToolResult::error(vec![Content::text("request cancelled after export completed; successful output retained")]),
            Err(error) => CallToolResult::error(vec![Content::text(error.to_string())]),
        }
    }

    /// Missing, malformed and failed observations are not terminal job states.
    async fn job_state(&self, job: u64) -> Result<Value, AutomationError> {
        let jobs = self.run("jobs.list", json!({})).await?;
        let jobs = jobs.as_array().ok_or_else(|| AutomationError::Other("export job query returned a non-array response".into()))?;
        let state = jobs
            .iter()
            .find(|state| state.get("id").and_then(Value::as_u64) == Some(job))
            .ok_or_else(|| AutomationError::Other(format!("export job {job} is no longer observable; completion unconfirmed")))?;
        if state.get("finished").and_then(Value::as_bool).is_none() {
            return Err(AutomationError::Other(format!("export job {job} has no valid completion state")));
        }
        if state.get("finished").and_then(Value::as_bool) == Some(true) && matches!(terminal_result(state), TerminalResult::Unknown) {
            return Err(AutomationError::Other(format!("export job {job} has no valid terminal result; completion outcome unconfirmed")));
        }
        Ok(state.clone())
    }

    /// The same bounded deadline covers cancellation and every completion query.
    /// Successful output is preserved. A terminal failure confirms that the job
    /// stopped, but cannot prove ownership of files at its output path.
    async fn cancel_long_job(&self, job: u64, path: &str, budget: Duration) -> Result<(), AutomationError> {
        let unconfirmed = |reason: String| AutomationError::Other(format!("export job {job} cleanup unconfirmed; partial output retained: {reason}"));
        let deadline = tokio::time::Instant::now().checked_add(budget).ok_or_else(|| unconfirmed("cleanup deadline exceeds the clock range".into()))?;
        tokio::time::timeout_at(deadline, self.run("jobs.cancel", json!({"job": job})))
            .await
            .map_err(|_| unconfirmed("cancellation deadline expired".into()))?
            .map_err(|error| unconfirmed(error.to_string()))?;
        loop {
            let state = tokio::time::timeout_at(deadline, self.job_state(job))
                .await
                .map_err(|_| unconfirmed("worker completion deadline expired".into()))?
                .map_err(|error| unconfirmed(error.to_string()))?;
            if state.get("finished").and_then(Value::as_bool) == Some(true) {
                match terminal_result(&state) {
                    TerminalResult::Success => return Ok(()),
                    TerminalResult::Failure => {
                        return Err(unconfirmed(format!(
                            "export stopped, but file ownership is unconfirmed for {path:?}; filenames and timestamps do not establish ownership"
                        )));
                    }
                    TerminalResult::Unknown => return Err(unconfirmed("terminal result is missing or malformed".into())),
                }
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(unconfirmed("worker completion deadline expired".into()));
            }
            tokio::time::sleep(POLL.min(deadline.saturating_duration_since(now))).await;
        }
    }
}

/// Explicit cleanup for files the caller has established it owns exclusively: the output file itself, the numbered
/// stills of an image sequence (`<stem><3+ digits>.<same extension>`, only when the output is a
/// PNG/TIFF/BMP/JPEG/Targa/DPX sequence) and the caption sidecar
/// (`<stem>.srt` / `<stem>.vtt`), each only if it was written since `since`. Other files that
/// merely share the name's stem (`<stem>.aep`, `<stem>2.psd`, `<stem>1.mp4` beside a movie, …) are
/// never touched, even when they were saved during the export.
///
/// Filenames and modification times only narrow the candidate set; they do not prove ownership.
/// Callers must establish exclusive ownership before invoking this helper. MCP does not invoke it
/// because engine job observations do not provide per-job file ownership evidence.
pub fn remove_partial(path: &str, since: SystemTime) {
    if let Err(error) = remove_partial_checked(path, since) {
        log::warn!("partial export cleanup: {error}");
    }
}

fn remove_partial_checked(path: &str, since: SystemTime) -> Result<(), AutomationError> {
    let p = Path::new(path);
    let (Some(dir), Some(stem), Some(file)) = (p.parent(), p.file_stem().and_then(|s| s.to_str()), p.file_name().and_then(|s| s.to_str())) else {
        return Ok(());
    };
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AutomationError::Other(format!("read export output directory: {error}"))),
    };
    // file times are coarser than the clock
    let since = since.checked_sub(Duration::from_secs(1)).unwrap_or(since);
    for entry in entries {
        let e = entry.map_err(|error| AutomationError::Other(format!("read export output entry: {error}")))?;
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_export_output(name, file, stem, ext) {
            continue;
        }
        let metadata = match e.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(AutomationError::Other(format!("read partial export metadata: {error}"))),
        };
        if metadata.is_file()
            && metadata.modified().map_err(|error| AutomationError::Other(format!("read partial export modification time: {error}")))? >= since
        {
            match std::fs::remove_file(e.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(AutomationError::Other(format!("remove partial export: {error}"))),
            }
        }
    }
    Ok(())
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
    Format::from_name(ext).is_some_and(Format::is_image_sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_cleanup_reports_an_unreadable_output_directory() {
        let path = std::env::temp_dir().join(format!("fc-cleanup-not-directory-{}", std::process::id()));
        std::fs::write(&path, b"a file cannot be an output directory").unwrap();
        let error =
            remove_partial_checked(&path.join("partial.mov").to_string_lossy(), SystemTime::now()).expect_err("invalid output directory must be actionable");
        let _ = std::fs::remove_file(path);
        assert!(error.to_string().contains("read export output directory"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn partial_cleanup_reports_export_locked_against_deletion() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = std::env::temp_dir().join(format!("fc-cleanup-delete-locked-{}.mov", std::process::id()));
        let started = SystemTime::now();
        std::fs::write(&path, b"synthetic partial export with deletion sharing denied").unwrap();
        // Permit read/write sharing, but omit FILE_SHARE_DELETE. This owned
        // handle makes deletion fail independently of read-only-bit behavior.
        let file = std::fs::OpenOptions::new().read(true).write(true).share_mode(0x3).open(&path).unwrap();
        let result = remove_partial_checked(&path.to_string_lossy(), started);
        let retained = path.exists();
        // Release the fixture's handle before cleanup or an assertion unwinds.
        drop(file);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
        let error = result.expect_err("export locked against deletion must report failed removal");
        assert!(retained, "failed deletion must retain the locked export");
        assert!(error.to_string().contains("remove partial export"), "{error}");
    }

    async fn cleanup_helper_fixture(state: Value, cancel_error: Option<&str>, delay: Duration, budget: Duration) -> (Result<(), AutomationError>, bool) {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fc-long-job-helper-{}-{}.mov", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&path, b"partial synthetic export").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let cancel_error = cancel_error.map(str::to_owned);
        let app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (input, mut output) = stream.into_split();
            let mut input = BufReader::new(input).lines();
            while let Ok(Some(line)) = input.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let reply = match request["params"]["command"].as_str() {
                    Some("jobs.cancel") => {
                        tokio::time::sleep(delay).await;
                        match &cancel_error {
                            Some(error) => json!({"ok": false, "error": error}),
                            None => json!({"ok": true, "result": null}),
                        }
                    }
                    Some("jobs.list") => json!({"ok": true, "result": state}),
                    _ => panic!("unexpected cleanup helper command: {request}"),
                };
                // A bounded cancellation may intentionally abandon this connection.
                if output.write_all(format!("{reply}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let server = FilmcraftMcp::bridge(&address).unwrap();
        let result = server.cancel_long_job(7, &path.to_string_lossy(), budget).await;
        drop(server);
        tokio::time::timeout(Duration::from_secs(1), app).await.unwrap().unwrap();
        let retained = path.exists();
        let _ = std::fs::remove_file(path);
        (result, retained)
    }

    #[tokio::test]
    async fn long_export_cleanup_timeout_retains_unconfirmed_output() {
        let (result, retained) = cleanup_helper_fixture(json!([{"id": 7, "finished": false}]), None, Duration::ZERO, Duration::from_millis(20)).await;
        let error = result.expect_err("running worker must reach the bounded deadline");
        assert!(error.to_string().contains("cleanup unconfirmed"), "{error}");
        assert!(error.to_string().contains("deadline expired"), "{error}");
        assert!(retained, "deadline must not authorize deleting an active worker's output");
    }

    #[tokio::test]
    async fn long_export_cleanup_budget_includes_cancellation() {
        let (result, retained) =
            cleanup_helper_fixture(json!([{"id": 7, "finished": true}]), None, Duration::from_millis(100), Duration::from_millis(20)).await;
        let error = result.expect_err("cancellation must use the same deadline");
        assert!(error.to_string().contains("cancellation deadline expired"), "{error}");
        assert!(retained);
    }

    #[tokio::test]
    async fn long_export_cleanup_cancel_error_is_actionable_and_retains_output() {
        let (result, retained) =
            cleanup_helper_fixture(json!([{"id": 7, "finished": true}]), Some("synthetic cancellation denied"), Duration::ZERO, Duration::from_secs(1)).await;
        let error = result.expect_err("cancellation error must propagate");
        assert!(error.to_string().contains("synthetic cancellation denied"), "{error}");
        assert!(retained);
    }

    #[tokio::test]
    async fn long_export_cleanup_missing_or_malformed_job_retains_output() {
        for state in [json!({}), json!([]), json!([{"id": 7}]), json!([{"id": 7, "finished": "unknown"}])] {
            let (result, retained) = cleanup_helper_fixture(state, None, Duration::ZERO, Duration::from_secs(1)).await;
            let error = result.expect_err("unknown state must not become terminal");
            assert!(error.to_string().contains("cleanup unconfirmed"), "{error}");
            assert!(retained);
        }
    }

    #[tokio::test]
    async fn long_export_failed_job_cannot_delete_successful_job_output_at_same_path() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        struct OwnedOutput(std::path::PathBuf);
        impl Drop for OwnedOutput {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let file =
            OwnedOutput(std::env::temp_dir().join(format!("fc-shared-export-owner-{}-{}.mov", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))));
        let completed: &[u8] = b"successful job 8 owns these completed export bytes";
        // The successful file is deliberately recent enough to match the old
        // cleanup heuristic. Its path and mtime cannot establish job 7 ownership.
        std::fs::write(&file.0, completed).unwrap();
        let target = file.0.to_string_lossy().into_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let mut app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (input, mut output) = stream.into_split();
            let mut input = BufReader::new(input).lines();
            while let Ok(Some(line)) = input.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let result = match request["params"]["command"].as_str() {
                    Some("jobs.cancel") => Value::Null,
                    Some("jobs.list") => json!([
                        {"id": 7, "path": target, "finished": true, "result": {"error": "cancelled"}},
                        {"id": 8, "path": target, "finished": true, "result": {"path": target, "frames": 8}}
                    ]),
                    _ => panic!("unexpected shared-output fixture command: {request}"),
                };
                if output.write_all(format!("{}\n", json!({"ok": true, "result": result})).as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let server = FilmcraftMcp::bridge(&address).unwrap();
        let result = server.cancel_long_job(7, &file.0.to_string_lossy(), Duration::from_secs(1)).await;
        drop(server);
        let stopped = tokio::time::timeout(Duration::from_secs(1), &mut app).await;
        if stopped.is_err() {
            app.abort();
            let _ = app.await;
        }
        let retained = std::fs::read(&file.0).is_ok_and(|bytes| bytes == completed);
        drop(file); // Remove only the fixture's file before any behavior assertion unwinds.
        assert!(matches!(stopped, Ok(Ok(()))), "synthetic bridge did not stop successfully after its client closed");
        assert!(retained, "failed job 7 deleted or altered successful job 8 output at the same path");
        let error = result.expect_err("job state and recent path do not prove output ownership");
        assert!(error.to_string().contains("ownership") && error.to_string().contains("unconfirmed"), "{error}");
    }

    #[tokio::test]
    async fn long_export_terminal_failure_without_file_ownership_retains_output() {
        let (result, retained) =
            cleanup_helper_fixture(json!([{"id": 7, "finished": true, "result": {"error": "cancelled"}}]), None, Duration::ZERO, Duration::from_secs(1)).await;
        let error = result.expect_err("terminal failure does not prove file ownership");
        assert!(error.to_string().contains("ownership") && error.to_string().contains("unconfirmed"), "{error}");
        assert!(retained, "terminal worker state alone must not authorize deleting output");
    }

    #[tokio::test]
    async fn long_export_cleanup_unknown_terminal_result_preserves_output() {
        for result in [None, Some(Value::Null), Some(json!({})), Some(json!({"error": false})), Some(json!({"error": ""}))] {
            let mut state = json!({"id": 7, "finished": true});
            if let Some(result) = result {
                state["result"] = result;
            }
            let (result, retained) = cleanup_helper_fixture(json!([state]), None, Duration::ZERO, Duration::from_secs(1)).await;
            let error = result.expect_err("unknown terminal result must retain output");
            assert!(error.to_string().contains("cleanup unconfirmed"), "{error}");
            assert!(retained);
        }
    }

    #[tokio::test]
    async fn long_export_cleanup_success_reports_preservation_outcome() {
        let (result, retained) =
            cleanup_helper_fixture(json!([{"id": 7, "finished": true, "result": {"frames": 8}}]), None, Duration::ZERO, Duration::from_secs(1)).await;
        result.expect("successful terminal result");
        assert!(retained);
    }

    async fn unsafe_cleanup_fixture(cancel_request: bool, successful_terminal: bool) -> (Option<Value>, bool) {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fc-long-job-retain-{}-{}.mov", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let export_path = path.to_string_lossy().into_owned();
        let export_file = path.clone();
        let (polled, first_poll) = tokio::sync::oneshot::channel();
        let app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (input, mut output) = stream.into_split();
            let mut input = BufReader::new(input).lines();
            let mut polled = Some(polled);
            let mut cancelled = false;
            while let Some(line) = input.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let reply = match request["params"]["command"].as_str() {
                    Some("file.exportMedia") => {
                        // This write occurs after run_long records its start time;
                        // slow MCP initialization cannot defeat the mtime regression.
                        std::fs::write(&export_file, b"unfinished export must survive unconfirmed cleanup").unwrap();
                        json!({"ok": true, "result": {"job": 7, "path": export_path}})
                    }
                    Some("jobs.list") if cancelled && successful_terminal => {
                        std::fs::write(&export_file, b"completed synthetic export").unwrap();
                        json!({"ok": true, "result": [{"id": 7, "finished": true, "result": {"frames": 8}}]})
                    }
                    Some("jobs.list") if cancel_request && !cancelled => {
                        if let Some(polled) = polled.take() {
                            let _ = polled.send(());
                        }
                        json!({"ok": true, "result": [{"id": 7, "finished": false, "result": null}]})
                    }
                    Some("jobs.cancel") => {
                        cancelled = true;
                        if successful_terminal { json!({"ok": true, "result": null}) } else { json!({"ok": false, "error": "synthetic jobs.cancel failure"}) }
                    }
                    Some("jobs.list") => json!({"ok": false, "error": "synthetic jobs.list failure"}),
                    _ => json!({"ok": false, "error": "unexpected fixture command"}),
                };
                output.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
            }
        });
        let (mut requests, input) = tokio::io::duplex(4096);
        let (output, replies) = tokio::io::duplex(4096);
        let service = tokio::spawn(FilmcraftMcp::bridge(&address).unwrap().serve_io(input, output));
        let mut replies = BufReader::new(replies).lines();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"cleanup-regression\",\"version\":\"1\"}}}\n").await.unwrap();
        replies.next_line().await.unwrap().unwrap();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"command_run\",\"arguments\":{\"id\":\"file.exportMedia\",\"params\":{\"wait\":true}}}}\n").await.unwrap();
        if cancel_request {
            first_poll.await.unwrap();
            requests.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":2}}\n").await.unwrap();
        }
        // rmcp intentionally suppresses the response for a cancelled request.
        // EOF drains the completed handler; its confirmed return is the file check boundary.
        let reply = if cancel_request {
            None
        } else {
            Some(serde_json::from_str(&tokio::time::timeout(Duration::from_secs(3), replies.next_line()).await.unwrap().unwrap().unwrap()).unwrap())
        };
        drop(requests);
        tokio::time::timeout(Duration::from_secs(3), service).await.unwrap().unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), app).await.unwrap().unwrap();
        let expected: &[u8] = if successful_terminal { b"completed synthetic export" } else { b"unfinished export must survive unconfirmed cleanup" };
        let retained = std::fs::read(&path).is_ok_and(|bytes| bytes == expected);
        let _ = std::fs::remove_file(path);
        (reply, retained)
    }

    #[tokio::test]
    async fn long_export_query_failure_is_not_successful_completion() {
        let (reply, retained) = unsafe_cleanup_fixture(false, false).await;
        let reply = reply.expect("uncancelled request has a response");
        assert_eq!(reply["result"]["isError"], true, "query failure became success: {reply}");
        assert!(reply.to_string().contains("synthetic jobs.list failure"), "original query error was lost: {reply}");
        assert!(retained, "unconfirmed export output was deleted");
    }

    #[tokio::test]
    async fn long_export_cancel_failure_preserves_unconfirmed_output() {
        let (_, retained) = unsafe_cleanup_fixture(true, false).await;
        assert!(retained, "failed cancellation deleted output without worker completion");
    }

    #[tokio::test]
    async fn long_export_query_error_preserves_successfully_completed_output() {
        let (reply, retained) = unsafe_cleanup_fixture(false, true).await;
        let reply = reply.expect("uncancelled query failure has a response");
        assert_eq!(reply["result"]["isError"], true, "original query failure must remain visible: {reply}");
        assert!(retained, "query-error cleanup altered or deleted a successfully completed export");
    }

    #[tokio::test]
    async fn long_export_cancel_race_preserves_successfully_completed_output() {
        let (_, retained) = unsafe_cleanup_fixture(true, true).await;
        assert!(retained, "successful export completion raced cancellation and its output was altered or deleted");
    }

    #[tokio::test]
    async fn long_export_pending_query_is_interrupted_to_issue_cancellation() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (polled, first_poll) = tokio::sync::oneshot::channel();
        let (cancelled, cancellation_seen) = tokio::sync::oneshot::channel();
        let polled = Arc::new(Mutex::new(Some(polled)));
        let cancelled = Arc::new(Mutex::new(Some(cancelled)));
        let terminal = Arc::new(AtomicBool::new(false));
        let (release_query, release) = tokio::sync::watch::channel(false);
        let (stop_app, mut stopping) = tokio::sync::watch::channel(false);
        let app = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let stream = tokio::select! {
                    _ = stopping.changed() => break,
                    stream = listener.accept() => stream.unwrap().0,
                };
                let (polled, cancelled, terminal, mut release, mut stopping) =
                    (polled.clone(), cancelled.clone(), terminal.clone(), release.clone(), stopping.clone());
                connections.spawn(async move {
                    let (input, mut output) = stream.into_split();
                    let mut input = BufReader::new(input).lines();
                    loop {
                        let line = tokio::select! {
                            _ = stopping.changed() => break,
                            line = input.next_line() => match line { Ok(Some(line)) => line, _ => break },
                        };
                        let request: Value = serde_json::from_str(&line).unwrap();
                        let result = match request["params"]["command"].as_str() {
                            Some("file.exportMedia") => json!({"job": 7, "path": ""}),
                            Some("jobs.cancel") => {
                                terminal.store(true, Ordering::SeqCst);
                                if let Some(cancelled) = cancelled.lock().unwrap().take() {
                                    let _ = cancelled.send(());
                                }
                                Value::Null
                            }
                            Some("jobs.list") => {
                                if !terminal.load(Ordering::SeqCst) {
                                    if let Some(polled) = polled.lock().unwrap().take() {
                                        let _ = polled.send(());
                                    }
                                    // The original connection intentionally withholds its reply.
                                    // Cancellation must abandon it and use another connection.
                                    tokio::select! {
                                        _ = stopping.changed() => break,
                                        _ = release.changed() => {}
                                    }
                                }
                                json!([{"id": 7, "finished": terminal.load(Ordering::SeqCst), "result": {"error": "cancelled"}}])
                            }
                            _ => panic!("unexpected pending-query fixture command: {request}"),
                        };
                        if output.write_all(format!("{}\n", json!({"ok": true, "result": result})).as_bytes()).await.is_err() {
                            break;
                        }
                    }
                });
            }
            connections.shutdown().await;
        });
        let (mut requests, input) = tokio::io::duplex(4096);
        let (output, replies) = tokio::io::duplex(4096);
        let mut service = tokio::spawn(FilmcraftMcp::bridge(&address).unwrap().serve_io(input, output));
        let mut replies = BufReader::new(replies).lines();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"pending-query-regression\",\"version\":\"1\"}}}\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), replies.next_line()).await.unwrap().unwrap().unwrap();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"command_run\",\"arguments\":{\"id\":\"file.exportMedia\",\"params\":{\"wait\":true}}}}\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), first_poll).await.unwrap().unwrap();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":2}}\n").await.unwrap();
        let prompt_cancellation = tokio::time::timeout(Duration::from_millis(500), cancellation_seen).await.is_ok_and(|seen| seen.is_ok());
        // Release the intentionally pending reply even on RED before making an
        // assertion. The fixture never leaves its service or socket tasks alive.
        release_query.send_replace(true);
        drop(requests);
        let stopped = tokio::time::timeout(Duration::from_secs(8), &mut service).await;
        if stopped.is_err() {
            service.abort();
            let _ = service.await;
        }
        stop_app.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), app).await.unwrap().unwrap();
        assert!(stopped.is_ok(), "fixture service did not stop after its pending query was released");
        assert!(prompt_cancellation, "jobs.cancel waited for the pending jobs.list reply instead of interrupting it");
    }

    #[tokio::test]
    async fn long_export_service_waits_beyond_rmcp_drain_for_confirmed_cleanup() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (polled, first_poll) = tokio::sync::oneshot::channel();
        let (cancelled, cancellation_started) = tokio::sync::oneshot::channel();
        let (release, completion) = tokio::sync::oneshot::channel();
        let app = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (input, mut output) = stream.into_split();
            let mut input = BufReader::new(input).lines();
            let mut polled = Some(polled);
            let mut cancelled = Some(cancelled);
            let mut completion = Some(completion);
            let mut finished = false;
            while let Some(line) = input.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let result = match request["params"]["command"].as_str() {
                    Some("file.exportMedia") => json!({"job": 7, "path": ""}),
                    Some("jobs.list") => {
                        if let Some(polled) = polled.take() {
                            let _ = polled.send(());
                        }
                        json!([{"id": 7, "finished": finished, "result": {"error": "cancelled"}}])
                    }
                    Some("jobs.cancel") => {
                        if let Some(cancelled) = cancelled.take() {
                            let _ = cancelled.send(());
                        }
                        if let Some(completion) = completion.take() {
                            completion.await.unwrap();
                        }
                        finished = true;
                        Value::Null
                    }
                    _ => panic!("unexpected synthetic bridge command: {request}"),
                };
                output.write_all(format!("{}\n", json!({"ok": true, "result": result})).as_bytes()).await.unwrap();
            }
        });
        let (mut requests, input) = tokio::io::duplex(4096);
        let (output, replies) = tokio::io::duplex(4096);
        let mut service = tokio::spawn(FilmcraftMcp::bridge(&address).unwrap().serve_io(input, output));
        let mut replies = BufReader::new(replies).lines();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"cleanup-ownership\",\"version\":\"1\"}}}\n").await.unwrap();
        replies.next_line().await.unwrap().unwrap();
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"command_run\",\"arguments\":{\"id\":\"file.exportMedia\",\"params\":{\"wait\":true}}}}\n").await.unwrap();
        first_poll.await.unwrap();
        drop(replies);
        requests.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), cancellation_started).await.unwrap().unwrap();
        // Aborting the input reader can make rmcp observe EOF before cancellation:
        // its Closed drain is five seconds, versus two for Cancelled. Keep the
        // admitted cleanup gated beyond both drains and the two-second writer budget.
        let premature = tokio::time::timeout(Duration::from_secs(7), &mut service).await;
        release.send(()).unwrap();
        let returned_early = premature.is_ok();
        let result = match premature {
            Ok(result) => result,
            Err(_) => tokio::time::timeout(Duration::from_secs(3), service).await.unwrap(),
        };
        assert!(result.unwrap().is_err(), "output failure must remain an error");
        drop(requests);
        tokio::time::timeout(Duration::from_secs(3), app).await.unwrap().unwrap();
        assert!(!returned_early, "serve_io returned before admitted long-export cleanup completed");
    }

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

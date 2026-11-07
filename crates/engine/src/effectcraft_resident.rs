//! Private stdio MCP session: keep the authorized renderer and its project loaded.
use std::io::Write;
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use filmcraft_export::Progress;
use serde_json::{Value, json};
use web_time::Instant;

use crate::effectcraft_process::{Chunk, MAX_LINE, MAX_OUTPUT, reader};

pub(super) struct Resident {
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicBool>,
    input: Option<SyncSender<Vec<u8>>>,
    output: Option<Receiver<Chunk>>,
    threads: Vec<JoinHandle<()>>,
    pending: Vec<u8>,
    serial: u64,
    previous: bool,
}

impl Resident {
    pub(super) fn alive(&self) -> bool {
        self.child.lock().unwrap_or_else(PoisonError::into_inner).try_wait().is_ok_and(|status| status.is_none())
    }

    fn spawn(cli: &str, args: &[String], progress: Arc<Progress>) -> Result<Self, String> {
        if cancelled(&progress) {
            return Err("EffectCraft session cancelled".into());
        }
        let mut command = crate::effectcraft_link::cli_command(cli, args);
        command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|e| format!("start EffectCraft session: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, output) = mpsc::sync_channel(16);
        let (input, inbox) = mpsc::sync_channel::<Vec<u8>>(1);
        let mut session = Self {
            child: Arc::new(Mutex::new(child)),
            stop: Arc::default(),
            input: Some(input),
            output: Some(output),
            threads: Vec::new(),
            pending: Vec::new(),
            serial: 0,
            previous: false,
        };
        session.threads.push(reader(stdout, true, sender.clone()).map_err(|e| e.to_string())?);
        session.threads.push(reader(stderr, false, sender.clone()).map_err(|e| e.to_string())?);
        session.threads.push(
            std::thread::Builder::new()
                .name("effectcraft-input".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut stdin = stdin.ok_or("missing EffectCraft input pipe")?;
                        while let Ok(bytes) = inbox.recv() {
                            stdin.write_all(&bytes).and_then(|()| stdin.flush()).map_err(|e| e.to_string())?;
                        }
                        Ok::<_, String>(())
                    }))
                    .unwrap_or_else(|_| Err("EffectCraft input writer failed".into()));
                    if let Err(error) = result {
                        let _ = sender.send(Err(error));
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        let child = session.child.clone();
        let stop = session.stop.clone();
        session.threads.push(
            std::thread::Builder::new()
                .name("effectcraft-lifetime".into())
                .spawn(move || {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        loop {
                            if stop.load(Ordering::Relaxed) || progress.cancel.load(Ordering::Relaxed) {
                                let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
                                let _ = child.kill();
                                let _ = child.wait();
                                break;
                            }
                            if child.lock().unwrap_or_else(PoisonError::into_inner).try_wait().ok().flatten().is_some() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    }));
                })
                .map_err(|e| e.to_string())?,
        );
        Ok(session)
    }

    pub(super) fn open(cli: &str, project: &str, progress: Arc<Progress>) -> Result<Self, String> {
        let mut session = Self::spawn(cli, &["mcp".into(), "--empty".into()], progress.clone())?;
        session.call(
            "initialize",
            json!({"protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"FilmCraft", "version":"1"}}),
            &progress,
            None,
        )?;
        // Queue edits belong only to this private session, never an auto-save of the source.
        session.command("prefs.set", json!({"key":"autoSave.enabled", "value":false}), &progress, None)?;
        session.command("file.open", json!({"path":project}), &progress, None)?;
        let queue = session.command("renderQueue.list", json!({}), &progress, None)?;
        let items = queue["items"].as_array().ok_or("invalid EffectCraft queue reply")?;
        if items.len() > 256 {
            return Err("EffectCraft source queue exceeds 256 items".into());
        }
        for _ in items {
            session.command("renderQueue.remove", json!({"index":1}), &progress, None)?;
        }
        session.command("edit.purgeUndo", json!({}), &progress, None)?;
        Ok(session)
    }

    pub(super) fn render(&mut self, params: Value, output: &Path, max_bytes: u64, progress: &Progress) -> Result<(), String> {
        if self.previous {
            self.command("renderQueue.remove", json!({"index":1}), progress, None)?;
        }
        self.command("renderQueue.add", params, progress, None)?;
        self.previous = true;
        let started = Instant::now();
        let mut result = self.command("renderQueue.render", json!({"wait":false}), progress, Some((output, max_bytes)))?;
        // The blocking MCP render polls every 100 ms, longer than a playback frame interval.
        while result["rendering"].as_bool() == Some(true) {
            if started.elapsed() > Duration::from_secs(600) {
                return Err("EffectCraft render exceeded ten minutes".into());
            }
            std::thread::sleep(Duration::from_millis(2));
            result = self.command("renderQueue.list", json!({}), progress, Some((output, max_bytes)))?;
        }
        let items = result["items"].as_array().ok_or("invalid EffectCraft render reply")?;
        if items.len() != 1 || items.first().and_then(|i| i["statusLabel"].as_str()) != Some("Done") {
            return Err(format!("EffectCraft render failed: {result}"));
        }
        // A long playback must not accumulate project snapshots in the renderer's Undo stack.
        self.command("edit.purgeUndo", json!({}), progress, None)?;
        Ok(())
    }

    fn command(&mut self, id: &str, params: Value, progress: &Progress, output: Option<(&Path, u64)>) -> Result<Value, String> {
        let result = self.call("tools/call", json!({"name":"command_run", "arguments":{"id":id,"params":params}}), progress, output)?;
        let text =
            result["content"].as_array().and_then(|blocks| blocks.iter().find_map(|block| block["text"].as_str())).ok_or("missing EffectCraft tool result")?;
        if result["isError"].as_bool() != Some(false) {
            return Err(format!("EffectCraft {id}: {text}"));
        }
        serde_json::from_str(text).map_err(|e| format!("invalid EffectCraft tool result: {e}"))
    }

    fn call(&mut self, method: &str, params: Value, progress: &Progress, output: Option<(&Path, u64)>) -> Result<Value, String> {
        self.serial = self.serial.checked_add(1).ok_or("EffectCraft request counter exhausted")?;
        let id = self.serial;
        let mut bytes = serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_LINE {
            return Err("EffectCraft request exceeds 64 KiB".into());
        }
        bytes.push(b'\n');
        self.input.as_ref().ok_or("EffectCraft session closed")?.try_send(bytes).map_err(|e| e.to_string())?;
        let started = Instant::now();
        let mut diagnostics = 0_usize;
        loop {
            if cancelled(progress) {
                return Err("EffectCraft frame request cancelled".into());
            }
            if started.elapsed() > Duration::from_secs(600) {
                return Err("EffectCraft request exceeded ten minutes".into());
            }
            if output.is_some_and(|(path, limit)| std::fs::metadata(path).is_ok_and(|m| m.len() > limit)) {
                return Err("EffectCraft output exceeds the available cache quota or per-render limit".into());
            }
            while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                if end > MAX_LINE {
                    return Err("EffectCraft reply exceeds 64 KiB".into());
                }
                let line: Vec<_> = self.pending.drain(..=end).collect();
                if let Some(result) = reply(&line, id)? {
                    return Ok(result);
                }
            }
            if self.pending.len() > MAX_LINE {
                return Err("EffectCraft reply exceeds 64 KiB".into());
            }
            match self.output.as_ref().ok_or("EffectCraft session closed")?.recv_timeout(Duration::from_millis(10)) {
                Ok(chunk) => {
                    let (stdout, bytes) = chunk?;
                    diagnostics = diagnostics.saturating_add(bytes.len());
                    if diagnostics > MAX_OUTPUT {
                        return Err("EffectCraft request output exceeds 16 MiB".into());
                    }
                    if stdout {
                        self.pending.extend(bytes);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("EffectCraft session exited; retry the frame request".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.lock().unwrap_or_else(PoisonError::into_inner).try_wait().map_err(|e| e.to_string())? {
                        return Err(format!("EffectCraft session exited ({status}); retry the frame request"));
                    }
                }
            }
        }
    }
}

fn cancelled(progress: &Progress) -> bool {
    filmcraft_media::cancel::cancelled() || progress.cancel.load(Ordering::Relaxed)
}

fn reply(line: &[u8], id: u64) -> Result<Option<Value>, String> {
    let value: Value = serde_json::from_slice(line).map_err(|e| format!("invalid EffectCraft reply: {e}"))?;
    if value["jsonrpc"] != "2.0" {
        return Err("invalid EffectCraft protocol version".into());
    }
    if value.get("id").is_none() && value["method"].as_str().is_some() {
        return Ok(None);
    }
    if value["id"].as_u64() != Some(id) {
        return Err("EffectCraft reply has an unexpected request ID".into());
    }
    if let Some(error) = value.get("error") {
        return Err(format!("EffectCraft protocol error: {error}"));
    }
    value.get("result").cloned().map(Some).ok_or_else(|| "EffectCraft reply has no result".into())
}

impl Drop for Resident {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        {
            let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = child.kill();
            let _ = child.wait();
        }
        self.input.take();
        self.output.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_correlated_and_malformed_or_failed_replies_are_rejected() {
        assert_eq!(reply(br#"{"jsonrpc":"2.0","id":2,"result":{"ok":true}}"#, 2).unwrap(), Some(json!({"ok":true})));
        assert_eq!(reply(br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#, 2).unwrap(), None);
        for value in ["{}", "not json", r#"{"jsonrpc":"2.0","id":3,"result":{}}"#, r#"{"jsonrpc":"2.0","id":2,"error":{"message":"failed"}}"#] {
            assert!(reply(value.as_bytes(), 2).is_err());
        }
    }

    #[test]
    fn cancellation_reaps_even_an_idle_resident_child() {
        let progress = Arc::new(Progress::default());
        let args = ["--exact", "effectcraft_process::tests::waiting_child_fixture", "--ignored", "--nocapture"].map(String::from);
        let session = Resident::spawn(&std::env::current_exe().unwrap().to_string_lossy(), &args, progress.clone()).unwrap();
        progress.cancel.store(true, Ordering::Relaxed);
        let started = Instant::now();
        loop {
            if session.child.lock().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn native_session_reuses_process_and_preserves_source() {
        let Ok(cli) = std::env::var("EFFECTCRAFT_CLI") else { return };
        let root = crate::media_test_util::tmp_dir("resident-effectcraft");
        let script = root.join("source.jsx");
        let project = root.join("source.ecproj");
        std::fs::write(&script, "var c=app.project.items.addComp('Resident',64,36,1,1,24); c.layers.addSolid([1,0,0],'Red',32,36,1,1);").unwrap();
        crate::effectcraft_process::output(
            &cli,
            &["script".into(), script.to_string_lossy().into_owned(), "--empty".into(), "--save-as".into(), project.to_string_lossy().into_owned()],
            None,
            u64::MAX,
        )
        .unwrap();
        let original = std::fs::read(&project).unwrap();
        let progress = Arc::new(Progress::default());
        let started = Instant::now();
        let mut session = Resident::open(&cli, &project.to_string_lossy(), progress.clone()).unwrap();
        let startup = started.elapsed();
        let pid = session.child.lock().unwrap().id();
        let started = Instant::now();
        for chunk in 0..6 {
            let output = root.join(format!("resident-{chunk}.mov"));
            let params = json!({"comp":1,"output":output,"start":f64::from(chunk * 4)/24.0,"end":f64::from((chunk + 1)*4)/24.0,"format":"prores","proresProfile":"4444","channels":"rgba","audio":"off"});
            session.render(params, &output, crate::linked_cache::MAX_RENDER_BYTES, &progress).unwrap();
            assert!(std::fs::metadata(output).unwrap().len() > 0);
            assert_eq!(session.child.lock().unwrap().id(), pid);
            assert!(session.child.lock().unwrap().try_wait().unwrap().is_none());
        }
        let resident_time = started.elapsed();
        let started = Instant::now();
        for frame in 0..24 {
            let output = root.join(format!("cold-{frame}.mov"));
            let args = [
                "render".into(),
                "--project".into(),
                project.to_string_lossy().into_owned(),
                "--comp".into(),
                "1".into(),
                "--out".into(),
                output.to_string_lossy().into_owned(),
                "--start".into(),
                format!("{:.12}", f64::from(frame) / 24.0),
                "--end".into(),
                format!("{:.12}", f64::from(frame + 1) / 24.0),
                "--format".into(),
                "prores".into(),
                "--prores".into(),
                "4444".into(),
                "--channels".into(),
                "rgba".into(),
                "--audio".into(),
                "off".into(),
            ];
            crate::effectcraft_process::output(&cli, &args, None, crate::linked_cache::MAX_RENDER_BYTES).unwrap();
        }
        let cold_time = started.elapsed();
        println!(
            "resident benchmark: startup={:.3}s; 24 warm frames={:.3}s ({:.1}fps); 24 one-shot frames={:.3}s; speedup={:.2}x",
            startup.as_secs_f64(),
            resident_time.as_secs_f64(),
            24.0 / resident_time.as_secs_f64(),
            cold_time.as_secs_f64(),
            cold_time.as_secs_f64() / resident_time.as_secs_f64()
        );
        assert_eq!(std::fs::read(&project).unwrap(), original, "the renderer must never save its queue edits into the source");
        let child = session.child.clone();
        drop(session);
        assert!(child.lock().unwrap().try_wait().unwrap().is_some(), "dropping the source reaps its renderer");
    }
}

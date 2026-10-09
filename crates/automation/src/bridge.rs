//! Client for the desktop app's JSON-lines control protocol. One connection, reconnect on failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::AutomationError;

/// How often a blocking export's app job is polled.
const JOB_POLL: Duration = Duration::from_millis(250);

type Conn = (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf);

pub struct BridgeClient {
    addr: String,
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
    /// The app to start (with `--control <port>`) when nothing answers on the port; tried once.
    launcher: Option<std::path::PathBuf>,
    launched: std::sync::atomic::AtomicBool,
}

/// How long a freshly launched app has to open its control port.
const LAUNCH_WAIT: Duration = Duration::from_secs(45);

/// The FilmCraft app next to this program: `$FILMCRAFT_APP`, else the `.app` bundle this binary is
/// in (the MCP binary ships inside FilmCraft.app), else a `filmcraft` binary beside it (a build
/// tree's `target/release`).
pub fn default_app() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("FILMCRAFT_APP").filter(|p| !p.is_empty()) {
        return Some(p.into());
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    if dir.ends_with("Contents/MacOS")
        && let Some(app) = dir.parent().and_then(|c| c.parent()).filter(|a| a.extension().is_some_and(|e| e == "app"))
    {
        return Some(app.to_path_buf());
    }
    let sibling = dir.join(if cfg!(windows) { "filmcraft.exe" } else { "filmcraft" });
    sibling.is_file().then_some(sibling)
}

impl BridgeClient {
    /// `addr` such as `127.0.0.1:9876` (loopback only).
    pub fn new(addr: impl Into<String>) -> Result<Self, AutomationError> {
        let addr = addr.into();
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(AutomationError::BadRequest(format!("bridge address must be loopback, got `{addr}`")));
        }
        Ok(Self { addr, conn: Mutex::new(None), next_id: AtomicU64::new(1), launcher: None, launched: Default::default() })
    }

    /// Start `app` (an `.app` bundle or the `filmcraft` binary) with `--control <port>` when the
    /// first connection finds nothing listening, so an agent never has to ask for the app to be
    /// opened. An app that is already running without its control port can't be reached this way:
    /// the error then says to turn on Settings ▸ Agents.
    pub fn with_launcher(mut self, app: Option<std::path::PathBuf>) -> Self {
        self.launcher = app;
        self
    }

    async fn connect(&self) -> Result<TcpStream, AutomationError> {
        let once = || async {
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
                .await
                .map_err(|_| AutomationError::Bridge(format!("timed out connecting to {}", self.addr)))?
                .map_err(|e| e.to_string())
                .map_err(AutomationError::Bridge)
        };
        let first = match once().await {
            Ok(s) => return Ok(s),
            Err(e) => e,
        };
        let hint = "start it with `filmcraft --control <port>`, or turn on Settings ▸ Agents ▸ Let AI agents control FilmCraft and restart it";
        let Some(app) = self.launcher.clone().filter(|_| !self.launched.swap(true, Ordering::Relaxed)) else {
            return Err(AutomationError::Bridge(format!("cannot connect to {} ({first}); {hint}", self.addr)));
        };
        let port = self.addr.rsplit_once(':').map(|(_, p)| p.to_string()).unwrap_or_default();
        let spawned = if app.extension().is_some_and(|e| e == "app") {
            std::process::Command::new("open").arg("-a").arg(&app).args(["--args", "--control", &port]).spawn()
        } else {
            std::process::Command::new(&app)
                .args(["--control", &port])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        };
        if let Err(e) = spawned {
            return Err(AutomationError::Bridge(format!("cannot connect to {} and could not start {}: {e}", self.addr, app.display())));
        }
        let deadline = tokio::time::Instant::now() + LAUNCH_WAIT;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Ok(s) = once().await {
                return Ok(s);
            }
        }
        Err(AutomationError::Bridge(format!(
            "started {} but nothing answered on {} within {}s (if FilmCraft was already open without agent control: {hint})",
            app.display(),
            self.addr,
            LAUNCH_WAIT.as_secs()
        )))
    }

    /// Run engine command `id` in the app. A blocking export (`file.exportMedia` with
    /// `wait: true`, see [`crate::long_job::LONG_COMMANDS`]) is started as an app job and polled
    /// here until it finishes, so the app keeps repainting (its status bar shows the progress) and
    /// answering other requests, and no request waits on the control server's 60 s reply limit
    /// (#91, #92). The reply is the same as a blocking export's: `{job, path, result}`, or the
    /// export's error.
    pub async fn execute(&self, id: &str, mut params: Value) -> Result<Value, AutomationError> {
        if !crate::long_job::is_long(id, &params) {
            return self.call("engine.execute", json!({"command": id, "params": params})).await;
        }
        if let Some(p) = params.as_object_mut() {
            p.insert("wait".into(), json!(false));
        }
        let mut start = self.call("engine.execute", json!({"command": id, "params": params})).await?;
        let Some(job) = start.get("job").and_then(Value::as_u64) else { return Ok(start) };
        let result = loop {
            tokio::time::sleep(JOB_POLL).await;
            let jobs = self.call("engine.execute", json!({"command": "jobs.list", "params": {}})).await?;
            let Some(j) = jobs.as_array().and_then(|a| a.iter().find(|j| j["id"].as_u64() == Some(job))).cloned() else {
                break Value::Null;
            };
            if j["finished"].as_bool() == Some(true) {
                break j["result"].clone();
            }
        };
        if let Some(e) = result.get("error").and_then(Value::as_str) {
            let what = if id == "file.exportMedia" { "export" } else { id };
            return Err(AutomationError::App(format!("{what} failed: {e}")));
        }
        if let Some(o) = start.as_object_mut() {
            o.insert("result".into(), result);
        }
        Ok(start)
    }

    /// Call a control method; returns `result` or the app's error.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, AutomationError> {
        let mut guard = self.conn.lock().await;
        for attempt in 0..2 {
            if guard.is_none() {
                let s = self.connect().await?;
                let (r, w) = s.into_split();
                *guard = Some((BufReader::new(r), w));
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let Some(conn) = guard.as_mut() else {
                return Err(AutomationError::Bridge(format!("not connected to {}", self.addr)));
            };
            let line = format!("{}\n", json!({"id": id, "method": method, "params": params}));
            let res: Result<Value, AutomationError> = async {
                conn.1.write_all(line.as_bytes()).await.map_err(|e| AutomationError::Bridge(e.to_string()))?;
                let mut buf = String::new();
                tokio::time::timeout(Duration::from_secs(90), conn.0.read_line(&mut buf))
                    .await
                    .map_err(|_| AutomationError::Bridge("timeout".into()))?
                    .map_err(|e| AutomationError::Bridge(e.to_string()))?;
                serde_json::from_str(&buf).map_err(|e| AutomationError::Bridge(format!("bad reply: {e}")))
            }
            .await;
            match res {
                Ok(v) => {
                    return if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(AutomationError::App(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string()))
                    };
                }
                Err(e) if attempt == 0 => {
                    *guard = None;
                    let _ = e;
                }
                Err(e) => return Err(e),
            }
        }
        Err(AutomationError::Bridge("unreachable".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// A stand-in for the app's control server: the export starts job 7, which `jobs.list` reports
    /// running twice and then finished with `result`. Every request is recorded.
    async fn fake_app(result: Value) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (r, mut w) = s.into_split();
            let mut lines = BufReader::new(r).lines();
            let mut polls = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                log.lock().await.push(req.clone());
                let reply = match req["params"]["command"].as_str() {
                    Some("file.exportMedia") => json!({"job": 7, "path": "/tmp/out.mov"}),
                    Some("jobs.list") => {
                        polls += 1;
                        let finished = polls >= 3;
                        json!([{"id": 7, "finished": finished, "result": if finished { result.clone() } else { Value::Null }}])
                    }
                    _ => json!(null),
                };
                let line = format!("{}\n", json!({"id": req["id"], "ok": true, "result": reply}));
                w.write_all(line.as_bytes()).await.unwrap();
            }
        });
        (addr, seen)
    }

    /// A blocking bridge export runs as an app job (#91, #92): the app never sees `wait: true` (it
    /// would encode on its UI thread and the reply would time out after 60 s), and the caller still
    /// gets the finished result.
    #[tokio::test]
    async fn a_blocking_export_is_polled_as_a_job() {
        let (addr, seen) = fake_app(json!({"frames": 642})).await;
        let b = BridgeClient::new(addr).unwrap();
        let r = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap();
        assert_eq!(r, json!({"job": 7, "path": "/tmp/out.mov", "result": {"frames": 642}}));
        let seen = seen.lock().await;
        assert_eq!(seen[0]["params"]["params"]["wait"], json!(false));
        assert!(seen.iter().all(|q| q["params"]["params"]["wait"] != json!(true)));
        assert_eq!(seen.iter().filter(|q| q["params"]["command"] == "file.exportMedia").count(), 1, "started once");
        assert_eq!(seen.iter().filter(|q| q["params"]["command"] == "jobs.list").count(), 3);
    }

    #[tokio::test]
    async fn a_failed_blocking_export_is_an_error() {
        let (addr, _) = fake_app(json!({"error": "encode: disk full"})).await;
        let b = BridgeClient::new(addr).unwrap();
        let e = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap_err();
        assert!(e.to_string().contains("export failed: encode: disk full"), "{e}");
    }

    /// Transcription and pause analysis run as polled jobs too (unless told `wait: false`):
    /// transcribing an hour of footage takes longer than the control server's reply limit.
    #[test]
    fn transcription_is_a_long_job_unless_told_not_to_wait() {
        use crate::long_job::is_long;
        assert!(is_long("transcript.generate", &json!({})));
        assert!(is_long("transcript.generate", &json!({"wait": true})));
        assert!(!is_long("transcript.generate", &json!({"wait": false})));
        assert!(is_long("transcript.findPauses", &json!({})));
        assert!(!is_long("file.exportMedia", &json!({})), "an export still waits only when asked");
        assert!(is_long("file.exportMedia", &json!({"wait": true})));
        assert!(!is_long("sequence.inspect", &json!({"wait": true})));
    }

    /// Nothing on the port and nothing to launch: the error says how to let an agent in.
    #[tokio::test]
    async fn an_unreachable_app_says_how_to_turn_agent_control_on() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let e = BridgeClient::new(addr.clone()).unwrap().call("ui.inspect", json!({})).await.unwrap_err().to_string();
        assert!(e.contains("Settings ▸ Agents"), "{e}");
        // a launcher that can't start fails cleanly (once), it doesn't hang
        let b = BridgeClient::new(addr).unwrap().with_launcher(Some("/nonexistent/FilmCraft-test-binary".into()));
        let e = b.call("ui.inspect", json!({})).await.unwrap_err().to_string();
        assert!(e.contains("could not start"), "{e}");
        let e2 = b.call("ui.inspect", json!({})).await.unwrap_err().to_string();
        assert!(e2.contains("Settings ▸ Agents"), "the launch is tried once: {e2}");
    }

    /// Everything else is forwarded unchanged.
    #[tokio::test]
    async fn other_commands_are_forwarded() {
        let (addr, seen) = fake_app(Value::Null).await;
        let b = BridgeClient::new(addr).unwrap();
        b.execute("file.exportMedia", json!({"path": "/tmp/out.mov"})).await.unwrap();
        b.execute("sequence.inspect", json!({})).await.unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1]["params"]["command"], "sequence.inspect");
    }
}

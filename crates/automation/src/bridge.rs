//! Client for the desktop app's JSON-lines control protocol. A connection per request in flight,
//! reused once its reply is in; reconnect on failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::AutomationError;

/// How often a blocking export's app job is polled.
const JOB_POLL: Duration = Duration::from_millis(250);
/// Idle connections kept for reuse; the app serves at most 16 at once.
const MAX_IDLE: usize = 4;
/// How long an app started by [`BridgeClient::launching`] has to open its control server (a cold
/// start loads fonts, the last project and its media).
const LAUNCH_WAIT: Duration = Duration::from_secs(90);

type Conn = (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf);

pub struct BridgeClient {
    addr: String,
    /// Inherited from the same owner-controlled environment as the desktop app.
    token: Option<String>,
    /// Connections with no request in flight. A request takes one (or opens one) for its whole
    /// round trip and puts it back only once the reply is read, so a slow or abandoned request
    /// never holds up the others or leaves its late reply for the next one to read (#1538).
    idle: Mutex<Vec<Conn>>,
    next_id: AtomicU64,
    /// The app to start when nothing listens at `addr` (`mcp --bridge … --launch-app`).
    launch: Option<std::path::PathBuf>,
    /// The app this client started last, so a slow start is waited for rather than started twice.
    started: std::sync::Mutex<Option<std::process::Child>>,
}

impl BridgeClient {
    /// `addr` such as `127.0.0.1:9876` (loopback only). Reads `FILMCRAFT_CONTROL_TOKEN`
    /// from the environment; the desktop app requires this credential on every request.
    pub fn new(addr: impl Into<String>) -> Result<Self, AutomationError> {
        let addr = addr.into();
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(AutomationError::BadRequest(format!("bridge address must be loopback, got `{addr}`")));
        }
        Ok(Self {
            addr,
            token: std::env::var("FILMCRAFT_CONTROL_TOKEN").ok(),
            idle: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            launch: None,
            started: std::sync::Mutex::new(None),
        })
    }

    /// Start `app --control <port>` whenever nothing listens at the bridge address (the app is not
    /// running, or was closed), then wait for its control server, so an agent never needs someone
    /// to open the app first. The app keeps running after the client is gone.
    pub fn launching(mut self, app: impl Into<std::path::PathBuf>) -> Self {
        self.launch = Some(app.into());
        self
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
        let (result, offline) = loop {
            tokio::time::sleep(JOB_POLL).await;
            let jobs = self.call("engine.execute", json!({"command": "jobs.list", "params": {}})).await?;
            let Some(j) = jobs.as_array().and_then(|a| a.iter().find(|j| j["id"].as_u64() == Some(job))).cloned() else {
                break (Value::Null, None);
            };
            if j["finished"].as_bool() == Some(true) {
                break (j["result"].clone(), j.get("offlineMedia").cloned());
            }
        };
        if let Some(e) = result.get("error").and_then(Value::as_str) {
            return Err(AutomationError::App(format!("export failed: {e}")));
        }
        if let Some(o) = start.as_object_mut() {
            o.insert("result".into(), result);
            if let Some(off) = offline {
                o.insert("offlineMedia".into(), off);
            }
        }
        Ok(start)
    }

    /// Call a control method; returns `result` or the app's error.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, AutomationError> {
        for attempt in 0..2 {
            // A retry gets a fresh connection: the failed one may have been closed by an app restart.
            let idle = if attempt == 0 { self.idle.lock().await.pop() } else { None };
            let mut conn = match idle {
                Some(c) => c,
                None => self.connect().await?,
            };
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let line = format!("{}\n", json!({"id": id, "token": self.token, "method": method, "params": params}));
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
                    let mut idle = self.idle.lock().await;
                    if idle.len() < MAX_IDLE {
                        idle.push(conn);
                    }
                    drop(idle);
                    return if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(AutomationError::App(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string()))
                    };
                }
                Err(_) if attempt == 0 => {}
                Err(e) => return Err(e),
            }
        }
        Err(AutomationError::Bridge("unreachable".into()))
    }

    async fn connect(&self) -> Result<Conn, AutomationError> {
        let refused = match self.try_connect().await {
            Ok(c) => return Ok(c),
            Err(e) => e,
        };
        let Some(app) = self.launch.as_deref() else { return Err(refused) };
        self.start_app(app)?;
        let deadline = tokio::time::Instant::now() + LAUNCH_WAIT;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let e = match self.try_connect().await {
                Ok(c) => return Ok(c),
                Err(e) => e,
            };
            // the app quit before opening its control server: say so now, not after the wait
            if let Some(status) = self.started_exit() {
                return Err(AutomationError::Bridge(format!("started {} but it exited ({status}) before its control server answered", app.display())));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AutomationError::Bridge(format!(
                    "started {} but its control server did not answer within {}s: {e}",
                    app.display(),
                    LAUNCH_WAIT.as_secs()
                )));
            }
        }
    }

    async fn try_connect(&self) -> Result<Conn, AutomationError> {
        let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
            .await
            .map_err(|_| AutomationError::Bridge(format!("timed out connecting to {}", self.addr)))?
            .map_err(|e| AutomationError::Bridge(format!("cannot connect to {} ({e}); start the app with `filmcraft --control <port>`", self.addr)))?;
        let (r, w) = s.into_split();
        Ok((BufReader::new(r), w))
    }

    /// How the app this client started ended, once it has.
    fn started_exit(&self) -> Option<std::process::ExitStatus> {
        let mut started = self.started.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        started.as_mut().and_then(|child| child.try_wait().ok().flatten())
    }

    /// Start `app --control <port>`, unless the one started before is still starting.
    fn start_app(&self, app: &std::path::Path) -> Result<(), AutomationError> {
        // the app's control server listens on 127.0.0.1 only: an IPv6 address could never reach the
        // app started here, nor could `localhost` where it resolves to ::1 alone
        let host = self.addr.rsplit_once(':').map_or("", |(h, _)| h);
        if host != "127.0.0.1" {
            return Err(AutomationError::BadRequest(format!(
                "--launch-app starts the app on 127.0.0.1, which `{}` does not reach: use --bridge 127.0.0.1:PORT",
                self.addr
            )));
        }
        // without the session token the app keeps its control server closed, and this would wait
        // out the whole launch timeout for a port that never opens
        // (the same format the app's listener requires: 64 hex characters)
        let Some(token) = self.token.as_deref().filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())) else {
            return Err(AutomationError::BadRequest(
                "--launch-app needs FILMCRAFT_CONTROL_TOKEN set to 64 hex characters in this environment: the app opens its control server only with such a token (see docs/control-protocol.md)"
                    .into(),
            ));
        };
        let mut started = self.started.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = started.as_mut()
            && matches!(child.try_wait(), Ok(None))
        {
            return Ok(());
        }
        let port = self
            .addr
            .rsplit_once(':')
            .map(|(_, p)| p)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| AutomationError::BadRequest(format!("no port in `{}`", self.addr)))?;
        let spawn = |detach_from_job: bool| {
            let mut cmd = std::process::Command::new(app);
            // the same session token this client sends, so the app accepts its requests
            cmd.args(["--control", port])
                .env("FILMCRAFT_CONTROL_TOKEN", token)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                // no console of its own, and out of the agent host's job object when it allows
                // that, so closing the agent session does not close the editor
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
                cmd.creation_flags(DETACHED_PROCESS | if detach_from_job { CREATE_BREAKAWAY_FROM_JOB } else { 0 });
            }
            #[cfg(not(windows))]
            let _ = detach_from_job;
            cmd.spawn()
        };
        let child = spawn(true).or_else(|_| spawn(false)).map_err(|e| AutomationError::Bridge(format!("cannot start {}: {e}", app.display())))?;
        *started = Some(child);
        Ok(())
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
        fake_app_with(result, None).await
    }

    /// [`fake_app`] whose finished job also lists `offline` as its `offlineMedia`.
    async fn fake_app_with(result: Value, offline: Option<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
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
                        let mut j = json!({"id": 7, "finished": finished, "result": if finished { result.clone() } else { Value::Null }});
                        if let Some(o) = offline.clone().filter(|_| finished) {
                            j["offlineMedia"] = o;
                        }
                        json!([j])
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

    /// The offline media a polled export drew comes back as in a blocking export's reply (#1054).
    #[tokio::test]
    async fn a_polled_export_reports_its_offline_media() {
        let off = json!([{"item": 8, "status": "missing"}]);
        let (addr, _) = fake_app_with(json!({"frames": 3}), Some(off.clone())).await;
        let b = BridgeClient::new(addr).unwrap();
        let r = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap();
        assert_eq!(r["offlineMedia"], off, "{r}");
    }

    #[tokio::test]
    async fn a_failed_blocking_export_is_an_error() {
        let (addr, _) = fake_app(json!({"error": "encode: disk full"})).await;
        let b = BridgeClient::new(addr).unwrap();
        let e = b.execute("file.exportMedia", json!({"path": "/tmp/out.mov", "wait": true})).await.unwrap_err();
        assert!(e.to_string().contains("export failed: encode: disk full"), "{e}");
    }

    /// A stand-in for the app that serves any number of connections and answers each request on
    /// its own, as the app does: `slow` after `slow_reply` (never when `None`), anything else at
    /// once with its command id. The ids of the requests it receives are sent to the returned channel.
    async fn slow_app(slow_reply: Option<Duration>) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (seen, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let seen = seen.clone();
                tokio::spawn(async move {
                    let (r, w) = s.into_split();
                    let w = Arc::new(Mutex::new(w));
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let cmd = req["params"]["command"].as_str().unwrap_or_default().to_string();
                        let _ = seen.send(cmd.clone());
                        let delay = if cmd == "slow" { slow_reply } else { Some(Duration::ZERO) };
                        let w = w.clone();
                        tokio::spawn(async move {
                            let Some(d) = delay else { return };
                            tokio::time::sleep(d).await;
                            let line = format!("{}\n", json!({"id": req["id"], "ok": true, "result": cmd}));
                            let _ = w.lock().await.write_all(line.as_bytes()).await;
                        });
                    }
                });
            }
        });
        (addr, rx)
    }

    /// A command the app is slow to answer holds up only itself: a later call is answered at once
    /// instead of queueing behind it, and behind every call queued after it (#1538).
    #[tokio::test]
    async fn a_slow_command_does_not_block_later_calls() {
        let (addr, mut seen) = slow_app(None).await;
        let b = Arc::new(BridgeClient::new(addr).unwrap());
        let slow = tokio::spawn({
            let b = b.clone();
            async move { b.execute("slow", json!({})).await }
        });
        assert_eq!(seen.recv().await.as_deref(), Some("slow"));
        let fast = tokio::time::timeout(Duration::from_secs(5), b.execute("fast", json!({}))).await;
        assert_eq!(fast.expect("the later call waited on the slow one").unwrap(), json!("fast"));
        slow.abort();
    }

    /// A call given up while it waits leaves no late reply on a connection for the next call to read
    /// as its own (#1538).
    #[tokio::test]
    async fn an_abandoned_call_leaves_no_late_reply_behind() {
        let (addr, mut seen) = slow_app(Some(Duration::from_millis(100))).await;
        let b = Arc::new(BridgeClient::new(addr).unwrap());
        let slow = tokio::spawn({
            let b = b.clone();
            async move { b.execute("slow", json!({})).await }
        });
        assert_eq!(seen.recv().await.as_deref(), Some("slow"));
        slow.abort();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(b.execute("fast", json!({})).await.unwrap(), json!("fast"));
    }

    /// With an app to launch, a running app is used as it is: nothing is started.
    #[tokio::test]
    async fn a_running_app_is_not_launched_again() {
        let (addr, _) = fake_app(Value::Null).await;
        let b = BridgeClient::new(addr).unwrap().launching("/no/such/filmcraft");
        assert_eq!(b.execute("sequence.inspect", json!({})).await.unwrap(), Value::Null);
        assert!(b.started.lock().unwrap().is_none());
    }

    /// A client with the session token the launched app needs.
    fn with_token(mut b: BridgeClient) -> BridgeClient {
        b.token = Some("0123456789abcdef".repeat(4));
        b
    }

    /// When nothing listens and the app cannot be started, the error says which app and why.
    #[tokio::test]
    async fn an_app_that_cannot_start_is_a_clear_error() {
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
        let b = with_token(BridgeClient::new(free).unwrap().launching("/no/such/filmcraft"));
        let e = b.execute("sequence.inspect", json!({})).await.unwrap_err().to_string();
        assert!(e.contains("cannot start") && e.contains("filmcraft"), "{e}");
    }

    /// Without the session token the app would keep its control server closed: launching is
    /// refused at once, with what to set, instead of waiting out the launch timeout.
    #[tokio::test]
    async fn launching_needs_the_session_token() {
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
        // missing, or not in the 64-hex-character form the app's listener accepts
        for token in [None, Some(String::new()), Some("a".repeat(63)), Some("z".repeat(64))] {
            let mut b = BridgeClient::new(free.clone()).unwrap().launching(std::env::current_exe().unwrap());
            b.token = token.clone();
            let e = b.execute("sequence.inspect", json!({})).await.unwrap_err().to_string();
            assert!(e.contains("FILMCRAFT_CONTROL_TOKEN"), "{token:?}: {e}");
            assert!(b.started.lock().unwrap().is_none(), "{token:?}: nothing was started");
        }
    }

    /// An app that starts but quits before opening its control server is reported at once, with
    /// how it ended, instead of after the whole launch wait. (This test binary stands in for it:
    /// it rejects `--control` and exits.)
    #[tokio::test]
    async fn an_app_that_exits_while_starting_is_reported_at_once() {
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
        let b = with_token(BridgeClient::new(free).unwrap().launching(std::env::current_exe().unwrap()));
        let t = std::time::Instant::now();
        let e = b.execute("sequence.inspect", json!({})).await.unwrap_err().to_string();
        assert!(e.contains("exited"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(30), "reported after {:?}", t.elapsed());
    }

    /// The launched app listens on 127.0.0.1 only, so an IPv6 bridge address is refused before
    /// anything is started.
    #[tokio::test]
    async fn launching_needs_an_ipv4_loopback_address() {
        // a free port, so `localhost` is not answered by something else before the check
        let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        for addr in ["[::1]:9".to_string(), format!("localhost:{port}")] {
            let b = BridgeClient::new(addr.clone()).unwrap().launching(std::env::current_exe().unwrap());
            let e = b.execute("sequence.inspect", json!({})).await.unwrap_err().to_string();
            assert!(e.contains("127.0.0.1"), "{addr}: {e}");
            assert!(b.started.lock().unwrap().is_none(), "{addr}: nothing was started");
        }
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

    #[tokio::test]
    async fn credential_is_sent_on_every_request() {
        let (addr, seen) = fake_app(Value::Null).await;
        let mut b = BridgeClient::new(addr).unwrap();
        // Inject directly to avoid mutating the process environment in parallel tests.
        let token = "0123456789abcdef".repeat(4);
        b.token = Some(token.clone());
        b.call("ui.inspect", json!({})).await.unwrap();
        b.execute("file.saveAs", json!({"path": "/owner/project.fcproj"})).await.unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|request| request["token"] == token));
        assert_eq!(seen[1]["params"]["params"]["path"], "/owner/project.fcproj");
    }
}

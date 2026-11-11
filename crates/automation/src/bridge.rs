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

type Conn = (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf);

/// The app's reply to a request without a valid credential (`control_server.rs`).
const AUTH_FAILED: &str = "control authentication failed";

pub struct BridgeClient {
    addr: String,
    /// Inherited from the same owner-controlled environment as the desktop app, or read from its
    /// `control-token` file.
    token: std::sync::RwLock<Option<String>>,
    /// The data folder whose `control-token` file the credential came from (no
    /// `FILMCRAFT_CONTROL_TOKEN`): read again when the app refuses the credential, because every
    /// Settings start of the app writes a new one.
    token_dir: Option<std::path::PathBuf>,
    /// Connections with no request in flight. A request takes one (or opens one) for its whole
    /// round trip and puts it back only once the reply is read, so a slow or abandoned request
    /// never holds up the others or leaves its late reply for the next one to read (#1538).
    idle: Mutex<Vec<Conn>>,
    next_id: AtomicU64,
}

impl BridgeClient {
    /// `addr` such as `127.0.0.1:9876` (loopback only). Reads `FILMCRAFT_CONTROL_TOKEN`
    /// from the environment, else the `control-token` file in the default data folder (written by
    /// the app when Settings ▸ Control Server starts the server); the app requires it on every
    /// request. See [`Self::with_data_dir`] for an app started with `--data-dir`.
    pub fn new(addr: impl Into<String>) -> Result<Self, AutomationError> {
        Self::with_data_dir(addr, None)
    }

    /// [`Self::new`], looking for the `control-token` file in `data_dir` (the app's `--data-dir`)
    /// instead of the default data folder when it is given. `FILMCRAFT_CONTROL_TOKEN` still wins;
    /// a value of it that is not 64 hex characters is an error here, before any connection.
    pub fn with_data_dir(addr: impl Into<String>, data_dir: Option<&std::path::Path>) -> Result<Self, AutomationError> {
        let data_dir = data_dir.map(std::path::Path::to_path_buf).or_else(filmcraft_engine::autosave::default_data_dir);
        Self::with_credential(addr, std::env::var("FILMCRAFT_CONTROL_TOKEN").ok(), data_dir)
    }

    /// [`Self::with_data_dir`] with the value of `FILMCRAFT_CONTROL_TOKEN` passed in (tests don't
    /// touch the process environment) and the data folder already resolved.
    fn with_credential(addr: impl Into<String>, env_token: Option<String>, data_dir: Option<std::path::PathBuf>) -> Result<Self, AutomationError> {
        let addr = addr.into();
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(AutomationError::BadRequest(format!("bridge address must be loopback, got `{addr}`")));
        }
        let env_token = env_token.filter(|t| !t.is_empty());
        if let Some(t) = &env_token
            && !filmcraft_engine::autosave::is_control_token(t)
        {
            return Err(AutomationError::BadRequest("FILMCRAFT_CONTROL_TOKEN must contain 64 hex characters (see docs/control-protocol.md)".into()));
        }
        let token_dir = if env_token.is_some() { None } else { data_dir };
        let token = filmcraft_engine::autosave::control_token_from(env_token, token_dir.as_deref());
        Ok(Self { addr, token: std::sync::RwLock::new(token), token_dir, idle: Mutex::new(Vec::new()), next_id: AtomicU64::new(1) })
    }

    /// The credential the next request sends.
    fn token(&self) -> Option<String> {
        self.token.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Read the token file again after the app refused `sent`, the credential the refused request
    /// carried. True when the file now holds a usable token other than `sent` (the app was
    /// restarted from Settings and wrote a new one, or a concurrent request already picked it up),
    /// which the next request then sends.
    fn refresh_token(&self, sent: Option<&str>) -> bool {
        let Some(dir) = &self.token_dir else { return false };
        let Some(fresh) = filmcraft_engine::autosave::read_control_token(dir) else { return false };
        if Some(fresh.as_str()) == sent {
            return false;
        }
        *self.token.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fresh);
        true
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
    ///
    /// Retries are bounded: one on a fresh connection after a transport failure (the reused one may
    /// have been closed by an app restart), and one after reading the token file again when the
    /// app refused a credential that came from it (a restart from Settings writes a new token).
    /// Both can happen for the same call: a restarted app first fails the stale connection, then
    /// refuses the old token.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, AutomationError> {
        let (mut reconnected, mut refreshed) = (false, false);
        loop {
            let idle = if reconnected || refreshed { None } else { self.idle.lock().await.pop() };
            let mut conn = match idle {
                Some(c) => c,
                None => self.connect().await?,
            };
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let sent = self.token();
            let line = format!("{}\n", json!({"id": id, "token": sent, "method": method, "params": params}));
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
                // The app closes the connection after refusing a credential: retry on a new one.
                Ok(v) if auth_failed(&v) && !refreshed && self.refresh_token(sent.as_deref()) => refreshed = true,
                Ok(v) => {
                    if !auth_failed(&v) {
                        let mut idle = self.idle.lock().await;
                        if idle.len() < MAX_IDLE {
                            idle.push(conn);
                        }
                    }
                    return if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(AutomationError::App(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string()))
                    };
                }
                Err(_) if !reconnected => reconnected = true,
                Err(e) => return Err(e),
            }
        }
    }

    async fn connect(&self) -> Result<Conn, AutomationError> {
        let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
            .await
            .map_err(|_| AutomationError::Bridge(format!("timed out connecting to {}", self.addr)))?
            .map_err(|e| AutomationError::Bridge(format!(
                "cannot connect to {} ({e}); start the app with `filmcraft --control <port>` (or turn on Settings ▸ Control Server) and set FILMCRAFT_CONTROL_TOKEN to the same 64-hex-character value on both sides, or leave it unset on both to use the control-token file in the data folder (see docs/control-protocol.md)",
                self.addr
            )))?;
        let (r, w) = s.into_split();
        Ok((BufReader::new(r), w))
    }
}

/// Whether `reply` is the app refusing the request's credential.
fn auth_failed(reply: &Value) -> bool {
    reply.get("ok").and_then(Value::as_bool) != Some(true) && reply.get("error").and_then(Value::as_str).is_some_and(|e| e.starts_with(AUTH_FAILED))
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

    /// A refused connection names the launch command and the token requirement (#3436): the
    /// reporter spent two ports and a debug-level console capture on a bare "connection refused".
    #[tokio::test]
    async fn refused_connection_error_names_the_token_requirement() {
        // Bind and drop: connecting to the freed address is deterministically refused.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let b = BridgeClient::new(addr.as_str()).unwrap();
        let err = b.call("ui.inspect", json!({})).await.unwrap_err().to_string();
        assert!(err.contains("--control"), "{err}");
        assert!(err.contains("FILMCRAFT_CONTROL_TOKEN"), "{err}");
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

    const ENV_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Write `token` as an app's Settings start does: owner-only.
    fn write_token(dir: &std::path::Path, token: &str) {
        let path = dir.join(filmcraft_engine::autosave::CONTROL_TOKEN_FILE);
        let tmp = dir.join("token.tmp");
        std::fs::write(&tmp, format!("{token}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::rename(tmp, path).unwrap();
    }

    fn client(env: Option<&str>, dir: &std::path::Path) -> Result<BridgeClient, AutomationError> {
        BridgeClient::with_credential("127.0.0.1:9876", env.map(str::to_string), Some(dir.to_path_buf()))
    }

    /// Where the credential comes from: the environment variable wins over the file; an empty or
    /// unset variable falls back to the file in the given data folder (the app's `--data-dir`);
    /// no usable file gives no credential; a malformed variable is an error before connecting.
    #[test]
    fn the_credential_comes_from_the_environment_else_the_token_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_token = "fedcba9876543210".repeat(4);
        write_token(dir.path(), &file_token);
        assert_eq!(client(Some(ENV_TOKEN), dir.path()).unwrap().token().as_deref(), Some(ENV_TOKEN));
        assert_eq!(client(None, dir.path()).unwrap().token().as_deref(), Some(file_token.as_str()));
        assert_eq!(client(Some(""), dir.path()).unwrap().token().as_deref(), Some(file_token.as_str()));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(client(None, empty.path()).unwrap().token(), None);
        for bad in ["abc", &"g".repeat(64), &"a".repeat(65)] {
            let Err(e) = client(Some(bad), dir.path()) else { panic!("`{bad}` was accepted") };
            assert!(e.to_string().contains("FILMCRAFT_CONTROL_TOKEN"), "{e}");
        }
    }

    /// A stand-in for the app's control server that accepts only the token `accept()` returns at
    /// the time of each request and, like the app, closes the connection after a refusal.
    async fn strict_app(accept: Arc<std::sync::Mutex<String>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let accept = accept.clone();
                tokio::spawn(async move {
                    let (r, mut w) = s.into_split();
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let want = accept.lock().unwrap().clone();
                        if req["token"] != json!(want) {
                            let reply = json!({"ok": false, "error": "control authentication failed; closing the connection"});
                            let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                            return;
                        }
                        let _ = w.write_all(format!("{}\n", json!({"id": req["id"], "ok": true, "result": "done"})).as_bytes()).await;
                    }
                });
            }
        });
        addr
    }

    /// The app was started again from Settings and wrote a new token: a running bridge reads the
    /// file again and goes on, instead of failing until it is restarted.
    #[tokio::test]
    async fn a_rotated_token_file_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let old = "1111111111111111".repeat(4);
        let new = "2222222222222222".repeat(4);
        write_token(dir.path(), &old);
        let accept = Arc::new(std::sync::Mutex::new(old.clone()));
        let addr = strict_app(accept.clone()).await;
        let b = BridgeClient::with_credential(addr, None, Some(dir.path().to_path_buf())).unwrap();
        assert_eq!(b.execute("a", json!({})).await.unwrap(), json!("done"));
        write_token(dir.path(), &new);
        *accept.lock().unwrap() = new.clone();
        assert_eq!(b.execute("b", json!({})).await.unwrap(), json!("done"));
        assert_eq!(b.token().as_deref(), Some(new.as_str()));
        // A credential the app still refuses is reported, not retried forever.
        *accept.lock().unwrap() = "3333333333333333".repeat(4);
        let e = b.execute("c", json!({})).await.unwrap_err();
        assert!(e.to_string().contains("control authentication failed"), "{e}");
        // An environment credential is never replaced by the file.
        write_token(dir.path(), &"3333333333333333".repeat(4));
        let from_env = BridgeClient::with_credential(b.addr.clone(), Some(old.clone()), Some(dir.path().to_path_buf())).unwrap();
        assert!(from_env.execute("d", json!({})).await.is_err());
        assert_eq!(from_env.token().as_deref(), Some(old.as_str()));
    }

    /// FilmCraft restarted from Settings: the reused connection is dead and the token was rotated.
    /// The call reconnects and then reads the new token, instead of failing on the old one.
    #[tokio::test]
    async fn a_restart_with_a_new_token_is_survived_after_a_dead_connection() {
        let dir = tempfile::tempdir().unwrap();
        let old = "4444444444444444".repeat(4);
        let new = "5555555555555555".repeat(4);
        write_token(dir.path(), &old);
        let accept = Arc::new(std::sync::Mutex::new(old.clone()));
        let addr = strict_app(accept.clone()).await;
        let b = BridgeClient::with_credential(addr, None, Some(dir.path().to_path_buf())).unwrap();
        assert_eq!(b.execute("a", json!({})).await.unwrap(), json!("done"));
        // Replace the pooled connection with one whose peer (the "old app") has closed it.
        let gone = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stale = TcpStream::connect(gone.local_addr().unwrap()).await.unwrap();
        drop(gone.accept().await.unwrap());
        let (r, w) = stale.into_split();
        *b.idle.lock().await = vec![(BufReader::new(r), w)];
        write_token(dir.path(), &new);
        *accept.lock().unwrap() = new.clone();
        assert_eq!(b.execute("b", json!({})).await.unwrap(), json!("done"));
        assert_eq!(b.token().as_deref(), Some(new.as_str()));
    }

    /// Another request already picked up the new token: a request refused with the old one still
    /// retries, because the file's token differs from the one it sent.
    #[test]
    fn a_refresh_compares_with_the_token_the_request_sent() {
        let dir = tempfile::tempdir().unwrap();
        let old = "6666666666666666".repeat(4);
        let new = "7777777777777777".repeat(4);
        write_token(dir.path(), &new);
        let b = BridgeClient::with_credential("127.0.0.1:9876", None, Some(dir.path().to_path_buf())).unwrap();
        assert_eq!(b.token().as_deref(), Some(new.as_str()), "the shared token is already the new one");
        assert!(b.refresh_token(Some(&old)), "the request sent the old token: retry");
        assert!(!b.refresh_token(Some(&new)), "the request sent the file's token: nothing to retry with");
        let empty = tempfile::tempdir().unwrap();
        let c = BridgeClient::with_credential("127.0.0.1:9876", None, Some(empty.path().to_path_buf())).unwrap();
        assert!(!c.refresh_token(Some(&old)), "no usable file token: no retry");
    }

    #[tokio::test]
    async fn credential_is_sent_on_every_request() {
        let (addr, seen) = fake_app(Value::Null).await;
        let mut b = BridgeClient::new(addr).unwrap();
        // Inject directly to avoid mutating the process environment in parallel tests.
        let token = "0123456789abcdef".repeat(4);
        b.token = std::sync::RwLock::new(Some(token.clone()));
        b.call("ui.inspect", json!({})).await.unwrap();
        b.execute("file.saveAs", json!({"path": "/owner/project.fcproj"})).await.unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|request| request["token"] == token));
        assert_eq!(seen[1]["params"]["params"]["path"], "/owner/project.fcproj");
    }
}

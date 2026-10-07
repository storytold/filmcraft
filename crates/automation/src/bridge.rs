//! Client for the desktop app's JSON-lines control protocol. One connection, reconnect on failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::AutomationError;

type Conn = (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf);

pub struct BridgeClient {
    addr: String,
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
}

impl BridgeClient {
    /// `addr` such as `127.0.0.1:9876` (loopback only).
    pub fn new(addr: impl Into<String>) -> Result<Self, AutomationError> {
        let addr = addr.into();
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(AutomationError::BadRequest(format!("bridge address must be loopback, got `{addr}`")));
        }
        Ok(Self { addr, conn: Mutex::new(None), next_id: AtomicU64::new(1) })
    }

    /// Call a control method; returns `result` or the app's error.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, AutomationError> {
        let mut guard = self.conn.lock().await;
        for attempt in 0..2 {
            if guard.is_none() {
                let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
                    .await
                    .map_err(|_| AutomationError::Bridge(format!("timed out connecting to {}", self.addr)))?
                    .map_err(|e| AutomationError::Bridge(format!("cannot connect to {} ({e}); start the app with `filmcraft --control <port>`", self.addr)))?;
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

    /// Run engine command `id` in the app (`engine.execute`). A long command with `wait: true`
    /// ([`crate::long_job::LONG_COMMANDS`]) is started as a job and `jobs.list` polled until it
    /// finishes, then returned as headless mode returns it (`{job, path, result}`). Sent with `wait`,
    /// the app would encode on its UI thread, with no progress and no other replies, and answer
    /// `timeout` after 60 s while the export carries on.
    pub async fn execute(&self, id: &str, mut params: Value) -> Result<Value, AutomationError> {
        if !crate::long_job::is_long(id, &params) {
            return self.call("engine.execute", json!({"command": id, "params": params})).await;
        }
        if let Some(p) = params.as_object_mut() {
            p.insert("wait".into(), json!(false));
        }
        let mut out = self.call("engine.execute", json!({"command": id, "params": params})).await?;
        let job = out.get("job").and_then(Value::as_u64).ok_or_else(|| AutomationError::App(format!("{id} started no job: {out}")))?;
        let result = loop {
            let jobs = self.call("engine.execute", json!({"command": "jobs.list", "params": {}})).await?;
            match jobs.as_array().and_then(|a| a.iter().find(|j| j["id"].as_u64() == Some(job))) {
                None => break Value::Null,
                Some(j) if j["finished"].as_bool() == Some(true) => break j["result"].clone(),
                Some(_) => tokio::time::sleep(crate::long_job::POLL).await,
            }
        };
        if let Some(e) = result.get("error").and_then(Value::as_str) {
            return Err(AutomationError::App(format!("export failed: {e}")));
        }
        if let Some(o) = out.as_object_mut() {
            o.insert("result".into(), result);
        }
        Ok(out)
    }
}

/// A stand-in for the desktop app's control channel, answering `engine.execute` the way the app does
/// for an export that takes longer than its 60 s reply limit.
#[cfg(test)]
pub(crate) mod fake_app {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    /// Commands received, in order (`file.exportMedia` with its `wait` flag).
    pub type Log = Arc<Mutex<Vec<String>>>;

    /// Listen on a free loopback port; returns its address and the command log. `file.exportMedia`
    /// with `wait: true` gets the app's `timeout` reply; without it, job 7 starts and `jobs.list`
    /// reports it finished on the third poll.
    pub fn start() -> (String, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let log: Log = Arc::default();
        let l = Arc::clone(&log);
        std::thread::spawn(move || {
            let mut polls = 0;
            for stream in listener.incoming().flatten() {
                let mut out = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let (cmd, p) = (req["params"]["command"].as_str().unwrap_or_default().to_string(), &req["params"]["params"]);
                    let reply = match cmd.as_str() {
                        "file.exportMedia" => {
                            let wait = p["wait"].as_bool() == Some(true);
                            l.lock().unwrap().push(format!("{cmd} wait={wait}"));
                            if wait { json!({"ok": false, "error": "timeout"}) } else { json!({"ok": true, "result": {"job": 7, "path": p["path"]}}) }
                        }
                        "jobs.list" => {
                            polls += 1;
                            let finished = polls >= 3;
                            let result = if finished { json!({"frames": 642, "bytes": 1000}) } else { Value::Null };
                            json!({"ok": true, "result": [{"id": 7, "done": polls * 200, "total": 642, "status": "Exporting", "finished": finished, "result": result}]})
                        }
                        _ => json!({"ok": true, "result": null}),
                    };
                    let mut reply = reply;
                    reply["id"] = req["id"].clone();
                    if writeln!(out, "{reply}").is_err() {
                        break;
                    }
                }
            }
        });
        (addr, log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blocking export over the bridge returns the finished job instead of the app's 60 s `timeout`
    /// (#91), and starts exactly one export.
    #[tokio::test]
    async fn blocking_export_is_polled_as_a_job() {
        let (addr, log) = fake_app::start();
        let b = BridgeClient::new(addr).unwrap();
        let v = b.execute("file.exportMedia", json!({"path": "/tmp/long.mov", "format": "prores", "wait": true})).await.unwrap();
        assert_eq!((v["job"].as_u64(), v["path"].as_str(), v["result"]["frames"].as_u64()), (Some(7), Some("/tmp/long.mov"), Some(642)), "{v}");
        assert_eq!(*log.lock().unwrap(), ["file.exportMedia wait=false"]);
        // without `wait` the call is forwarded as it is
        let v = b.execute("file.exportMedia", json!({"path": "/tmp/b.mov"})).await.unwrap();
        assert_eq!((v["job"].as_u64(), v.get("result")), (Some(7), None), "{v}");
    }
}

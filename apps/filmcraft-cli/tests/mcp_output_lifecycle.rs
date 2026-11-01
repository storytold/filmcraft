//! Real CLI response-pipe failure regressions. The request pipe remains open until parent exit.
#![cfg(windows)]

use serde_json::Value;
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct ErrorLog(std::path::PathBuf);

impl ErrorLog {
    fn new() -> Self {
        let name = format!(
            "filmcraft-mcp-errors-{}-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        std::fs::OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
        Self(path)
    }
}

impl Drop for ErrorLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
/// Hold the request pipe open through a failed response. EOF must not rescue a stuck service.
async fn mcp_with_failed_output(during_initialization: bool) -> std::io::Result<Output> {
    use std::io;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let stderr = ErrorLog::new();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"));
    command.args(["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(std::fs::File::create(&stderr.0)?).kill_on_drop(true).creation_flags(0x0800_0000);
    let mut child = command.spawn()?;
    drop(command); // Release the parent's stderr handle before the fixture file is removed.
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut input = child.stdin.take().ok_or_else(|| io::Error::other("MCP request pipe missing"))?;
        let mut output = BufReader::new(child.stdout.take().ok_or_else(|| io::Error::other("MCP response pipe missing"))?);
        if during_initialization {
            // Close before initialize: even the first response must stop the service and owner.
            drop(output);
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"output-failure-acceptance\",\"version\":\"1\"}}}\n").await?;
        } else {
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"output-failure-acceptance\",\"version\":\"1\"}}}\n").await?;
            let mut response = Vec::new();
            let length = tokio::time::timeout(Duration::from_secs(5), (&mut output).take(64 * 1024 + 1).read_until(b'\n', &mut response)).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "MCP initialization response deadline expired"))??;
            if length == 0 || length > 64 * 1024 || response.last() != Some(&b'\n') {
                return Err(io::Error::other("MCP initialization response missing or exceeds its limit"));
            }
            let response: Value = serde_json::from_slice(&response).map_err(io::Error::other)?;
                if response["jsonrpc"] != "2.0" || response["id"] != 1 || !response["result"].is_object() || response.get("error").is_some() {
                return Err(io::Error::other(format!("MCP initialization failed: {response}")));
            }
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await?;
            drop(output);
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n").await?;
        }
        input.flush().await?;
        let started = tokio::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if std::fs::metadata(&stderr.0)?.len() > 64 * 1024 {
                return Err(io::Error::other("MCP error output exceeds its storage limit"));
            }
            if started.elapsed() >= Duration::from_secs(10) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "MCP output failure did not stop the parent while input remained open"));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        // Keeping input owned until here is part of the regression contract.
        drop(input);
        use std::io::Read;
        let mut errors = Vec::new();
        std::fs::File::open(&stderr.0)?.take(64 * 1024 + 1).read_to_end(&mut errors)?;
        if errors.len() > 64 * 1024 {
            return Err(io::Error::other("MCP error output exceeds its storage limit"));
        }
        Ok(Output { status, stdout: Vec::new(), stderr: errors })
    }).await.unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "MCP output-failure fixture exceeded its total deadline")));
    if outcome.is_err() && child.try_wait()?.is_none() {
        child.start_kill()?;
        tokio::time::timeout(Duration::from_millis(500), child.wait())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "MCP failure fixture could not confirm owned parent cleanup"))??;
    }
    outcome
}

fn assert_failed_mcp_output(during_initialization: bool) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let output = runtime.block_on(mcp_with_failed_output(during_initialization)).unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("MCP output write") || error.contains("MCP output flush"), "transport error was lost: {error}");
}

#[test]
fn mcp_failed_output_with_open_input_exits_as_failure() {
    assert_failed_mcp_output(false);
}

#[test]
fn mcp_failed_initial_output_with_open_input_exits_as_failure() {
    assert_failed_mcp_output(true);
}

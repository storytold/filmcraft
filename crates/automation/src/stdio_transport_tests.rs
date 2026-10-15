//! Response transport failures must stop service without waiting for client EOF.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWrite;
#[derive(Clone, Copy)]
enum OutputFault {
    Write,
    Flush,
}

struct FailingOutput {
    stream: tokio::io::DuplexStream,
    armed: Arc<AtomicBool>,
    fault: OutputFault,
    failed: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FailingOutput {
    fn failure(&mut self) -> std::io::Error {
        if let Some(failed) = self.failed.take() {
            let _ = failed.send(());
        }
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "synthetic output failure")
    }
}

impl AsyncWrite for FailingOutput {
    fn poll_write(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, bytes: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.armed.load(Ordering::SeqCst) && matches!(this.fault, OutputFault::Write) {
            return std::task::Poll::Ready(Err(this.failure()));
        }
        std::pin::Pin::new(&mut this.stream).poll_write(cx, bytes)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.armed.load(Ordering::SeqCst) && matches!(this.fault, OutputFault::Flush) {
            return std::task::Poll::Ready(Err(this.failure()));
        }
        std::pin::Pin::new(&mut this.stream).poll_flush(cx)
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

async fn failed_output(fault: OutputFault, during_init: bool, input_eof: bool) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let server = FilmcraftMcp::headless(Session::default());
    let (mut client_input, input) = tokio::io::duplex(4096);
    let (stream, client_output) = tokio::io::duplex(4096);
    let armed = Arc::new(AtomicBool::new(during_init));
    let (failed, failure_observed) = tokio::sync::oneshot::channel();
    let output = FailingOutput { stream, armed: armed.clone(), fault, failed: Some(failed) };
    let mut replies = tokio::io::BufReader::new(client_output).lines();
    let mut serving = tokio::spawn(server.serve_io(input, output));
    client_input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n").await.unwrap();
    if !during_init {
        let reply = tokio::time::timeout(std::time::Duration::from_secs(1), replies.next_line()).await.unwrap().unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["id"], 0);
        client_input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
        armed.store(true, Ordering::SeqCst);
        client_input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n").await.unwrap();
    }
    // Observe the actual write/flush failure before allowing EOF to race service shutdown.
    tokio::time::timeout(std::time::Duration::from_secs(1), failure_observed).await.unwrap().unwrap();
    if input_eof {
        client_input.shutdown().await.unwrap();
    }
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(3), &mut serving).await;
    if stopped.is_err() {
        serving.abort();
    }
    let error = stopped.expect("failed output must stop MCP without waiting for input EOF").unwrap().expect_err("output failure must remain an error");
    let stage = match fault {
        OutputFault::Write => "MCP output write",
        OutputFault::Flush => "MCP output flush",
    };
    assert!(error.to_string().contains(stage), "{error}");
    assert!(error.to_string().contains("synthetic output failure"), "{error}");
    drop(client_input);
    drop(replies);
}

#[tokio::test]
async fn failed_write_with_open_input_stops_service() {
    failed_output(OutputFault::Write, false, false).await;
}

#[tokio::test]
async fn failed_flush_with_open_input_stops_service() {
    failed_output(OutputFault::Flush, false, false).await;
}

#[tokio::test]
async fn failed_write_then_eof_preserves_transport_error() {
    failed_output(OutputFault::Write, false, true).await;
}

#[tokio::test]
async fn failed_flush_then_eof_preserves_transport_error() {
    failed_output(OutputFault::Flush, false, true).await;
}

#[tokio::test]
async fn failed_initial_response_stops_service_with_open_input() {
    failed_output(OutputFault::Write, true, false).await;
}

#[tokio::test]
async fn eof_before_initialize_keeps_the_initialization_error() {
    use tokio::io::AsyncWriteExt;
    let (mut requests, input) = tokio::io::duplex(4096);
    let (output, _replies) = tokio::io::duplex(4096);
    requests.shutdown().await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), FilmcraftMcp::headless(Session::default()).serve_io(input, output))
        .await
        .expect("bare pre-initialize EOF must settle")
        .expect_err("EOF without initialization must remain an initialization failure");
    assert!(error.to_string().contains("MCP init:"), "{error}");
    assert!(!error.to_string().contains("MCP output write") && !error.to_string().contains("MCP output flush"), "{error}");
}

struct ObservedInput {
    stream: tokio::io::DuplexStream,
    bytes: Option<tokio::sync::oneshot::Sender<()>>,
    eof: Option<tokio::sync::oneshot::Sender<()>>,
}

impl tokio::io::AsyncRead for ObservedInput {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let polled = std::pin::Pin::new(&mut self.stream).poll_read(cx, buffer);
        if let std::task::Poll::Ready(Ok(())) = &polled {
            if buffer.filled().len() > before {
                if let Some(bytes) = self.bytes.take() {
                    let _ = bytes.send(());
                }
            } else if let Some(eof) = self.eof.take() {
                let _ = eof.send(());
            }
        }
        polled
    }
}

async fn poll_service_once<F: std::future::Future>(service: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    let mut service = service;
    std::future::poll_fn(|cx| std::task::Poll::Ready(service.as_mut().poll(cx))).await
}

async fn output_failure_and_preinitialize_eof(fault: OutputFault) {
    use tokio::io::AsyncWriteExt;
    let (mut requests, input) = tokio::io::duplex(4096);
    let (read, read_observed) = tokio::sync::oneshot::channel();
    let (eof, eof_observed) = tokio::sync::oneshot::channel();
    let input = ObservedInput { stream: input, bytes: Some(read), eof: Some(eof) };
    let (stream, _replies) = tokio::io::duplex(4096);
    let (failed, failure_observed) = tokio::sync::oneshot::channel();
    let output = FailingOutput { stream, armed: Arc::new(AtomicBool::new(true)), fault, failed: Some(failed) };
    let service = FilmcraftMcp::headless(Session::default()).serve_io(input, output);
    tokio::pin!(service);
    requests.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n").await.unwrap();
    assert!(poll_service_once(service.as_mut()).await.is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(1), read_observed).await.unwrap().unwrap();
    // Pre-initialize ping is permitted. Poll just far enough to queue its response,
    // then park this future while the transport tasks observe failure and EOF.
    assert!(poll_service_once(service.as_mut()).await.is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(1), failure_observed).await.unwrap().unwrap();
    requests.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), eof_observed).await.unwrap().unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), service)
        .await
        .expect("competing initialization failure must settle")
        .expect_err("actual output failure must remain an error");
    let stage = match fault {
        OutputFault::Write => "MCP output write",
        OutputFault::Flush => "MCP output flush",
    };
    assert!(error.to_string().contains(stage), "initialization masked the actual transport failure: {error}");
    assert!(error.to_string().contains("synthetic output failure"), "{error}");
}

#[tokio::test]
async fn failed_preinitialize_ping_response_preserves_write_error_when_eof_competes() {
    // Both wakeups are observed before polling the service again. Exercise the
    // randomized select order repeatedly; either winner must preserve the cause.
    for _ in 0..16 {
        output_failure_and_preinitialize_eof(OutputFault::Write).await;
    }
}

#[tokio::test]
async fn failed_preinitialize_ping_response_preserves_flush_error_when_eof_competes() {
    for _ in 0..16 {
        output_failure_and_preinitialize_eof(OutputFault::Flush).await;
    }
}

mod reusable_service_tests {
    use crate::{AutomationError, FilmcraftMcp};
    use filmcraft_engine::Session;
    use serde_json::{Value, json};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};

    const REPLY_BUDGET: Duration = Duration::from_secs(3);
    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

    struct Client {
        input: Option<DuplexStream>,
        output: BufReader<DuplexStream>,
        service: Option<tokio::task::JoinHandle<Result<(), AutomationError>>>,
    }

    impl Client {
        fn start(server: FilmcraftMcp) -> Self {
            let (requests, input) = tokio::io::duplex(64 * 1024);
            let (output, replies) = tokio::io::duplex(64 * 1024);
            let service = tokio::spawn(server.serve_io(input, output));
            Self { input: Some(requests), output: BufReader::new(replies), service: Some(service) }
        }

        async fn send(&mut self, value: Value) {
            let line = format!("{value}\n");
            tokio::time::timeout(REPLY_BUDGET, self.input.as_mut().unwrap().write_all(line.as_bytes())).await.unwrap().unwrap();
        }

        async fn ask(&mut self, request: Value) -> Value {
            let id = request["id"].clone();
            self.send(request).await;
            let mut bytes = Vec::new();
            let length = tokio::time::timeout(REPLY_BUDGET, (&mut self.output).take(64 * 1024 + 1).read_until(b'\n', &mut bytes)).await.unwrap().unwrap();
            assert!(length > 0 && length <= 64 * 1024 && bytes.last() == Some(&b'\n'), "missing or unbounded MCP response");
            let reply: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(reply["id"], id, "{reply}");
            reply
        }

        async fn initialize(&mut self) {
            let reply = self.ask(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"reusable-transport","version":"1"}}})).await;
            assert!(reply["result"].is_object() && reply.get("error").is_none(), "{reply}");
            self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
        }

        async fn command_list(&mut self, id: u64) {
            let reply =
                self.ask(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"command_list","arguments":{"filter":"file.save"}}})).await;
            assert_eq!(reply["result"]["isError"], false, "another transport closed this client's admission: {reply}");
            let text = reply["result"]["content"][0]["text"].as_str().unwrap();
            let commands: Value = serde_json::from_str(text).unwrap();
            assert!(commands.as_array().is_some_and(|commands| !commands.is_empty()), "{commands}");
        }

        async fn eof_and_join(&mut self) {
            drop(self.input.take());
            let service = self.service.as_mut().unwrap();
            tokio::time::timeout(SHUTDOWN_BUDGET, service).await.expect("EOF must finish its own service").unwrap().unwrap();
            self.service = None;
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            // Last resort on a failed fixture. These read-only requests launch no
            // exports or native workers; a dropped fixture is not a cleanup pass.
            drop(self.input.take());
            if let Some(service) = &self.service {
                service.abort();
            }
        }
    }

    #[tokio::test]
    async fn generic_service_clones_remain_reusable_after_another_client_eof() {
        let base = FilmcraftMcp::headless(Session::default());
        let mut first = Client::start(base.clone());
        first.initialize().await;
        first.command_list(2).await;
        first.eof_and_join().await;
        let mut second = Client::start(base.clone());
        second.initialize().await;
        second.command_list(2).await;
        second.eof_and_join().await;
    }

    #[tokio::test]
    async fn concurrent_generic_service_client_survives_another_client_eof() {
        let base = FilmcraftMcp::headless(Session::default());
        let mut first = Client::start(base.clone());
        let mut second = Client::start(base.clone());
        first.initialize().await;
        second.initialize().await;
        first.command_list(2).await;
        second.command_list(2).await;
        first.eof_and_join().await;
        second.command_list(3).await;
        second.eof_and_join().await;
    }
}

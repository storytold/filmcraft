//! Bounded fixtures for the isolated stdin owner, without reading the process stdin.

use super::{BRIDGE_BYTES, StdioInput};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::sync::oneshot;

const TEST_BUDGET: Duration = Duration::from_secs(5);

async fn finish(input: &mut StdioInput) -> Result<(), crate::AutomationError> {
    tokio::time::timeout(TEST_BUDGET, input.finish()).await.expect("stdin owner finish exceeded the fixture deadline")
}

async fn eof(stream: &mut tokio::io::DuplexStream) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    tokio::time::timeout(TEST_BUDGET, stream.take((BRIDGE_BYTES * 4) as u64).read_to_end(&mut bytes))
        .await
        .expect("stdin bridge did not close within the fixture deadline")?;
    Ok(bytes)
}

struct IdleRead {
    polled: Option<oneshot::Sender<()>>,
    dropped: Option<oneshot::Sender<()>>,
}

impl AsyncRead for IdleRead {
    fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if let Some(polled) = self.polled.take() {
            let _ = polled.send(());
        }
        Poll::Pending
    }
}

impl Drop for IdleRead {
    fn drop(&mut self) {
        if let Some(dropped) = self.dropped.take() {
            let _ = dropped.send(());
        }
    }
}

fn idle() -> (IdleRead, oneshot::Receiver<()>, oneshot::Receiver<()>) {
    let (polled, polling) = oneshot::channel();
    let (dropped, dropping) = oneshot::channel();
    (IdleRead { polled: Some(polled), dropped: Some(dropped) }, polling, dropping)
}

#[tokio::test]
async fn normal_eof_preserves_bytes_and_finish_is_repeatable() {
    // Dropping copied bytes or closing the bridge before EOF must fail this comparison.
    let expected: Vec<u8> = (0..(BRIDGE_BYTES * 3 + 17)).map(|offset| (offset % 251) as u8).collect();
    let source = expected.clone();
    let (mut input, mut stream) = StdioInput::start_with(move || io::Cursor::new(source)).unwrap();
    assert_eq!(eof(&mut stream).await.unwrap(), expected);
    finish(&mut input).await.unwrap();
    finish(&mut input).await.unwrap();
}

#[tokio::test]
async fn finish_cancels_an_idle_reader_without_waiting_for_eof() {
    // Waiting on the source read instead of selecting stop must exceed TEST_BUDGET.
    let (reader, polling, dropping) = idle();
    let (mut input, mut stream) = StdioInput::start_with(move || reader).unwrap();
    tokio::time::timeout(TEST_BUDGET, polling).await.unwrap().unwrap();
    finish(&mut input).await.unwrap();
    tokio::time::timeout(TEST_BUDGET, dropping).await.unwrap().unwrap();
    assert!(eof(&mut stream).await.unwrap().is_empty());
}

struct FullRead {
    copied: Arc<AtomicUsize>,
    saturated: Option<oneshot::Sender<()>>,
}

impl AsyncRead for FullRead {
    fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let amount = buffer.remaining().min(4096);
        if amount == 0 {
            return Poll::Ready(Ok(()));
        }
        let fill = [42; 4096];
        buffer.put_slice(&fill[..amount]);
        let total = self.copied.fetch_add(amount, Ordering::SeqCst) + amount;
        if total > BRIDGE_BYTES
            && let Some(saturated) = self.saturated.take()
        {
            let _ = saturated.send(());
        }
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn finish_cancels_a_copy_blocked_by_the_full_bridge() {
    // Stop must cancel both a pending source read and a backpressured destination write.
    let copied = Arc::new(AtomicUsize::new(0));
    let (saturated, saturation) = oneshot::channel();
    let reader = FullRead { copied: Arc::clone(&copied), saturated: Some(saturated) };
    let (mut input, mut stream) = StdioInput::start_with(move || reader).unwrap();
    tokio::time::timeout(TEST_BUDGET, saturation).await.unwrap().unwrap();
    assert!(copied.load(Ordering::SeqCst) > BRIDGE_BYTES);
    finish(&mut input).await.unwrap();
    let retained = eof(&mut stream).await.unwrap();
    assert_eq!(retained.len(), BRIDGE_BYTES);
    assert!(retained.iter().all(|byte| *byte == 42));
}

struct FailedRead;

impl AsyncRead for FailedRead {
    fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("synthetic stdin read failure")))
    }
}

#[tokio::test]
async fn read_failure_closes_bridge_and_is_reported_by_finish() {
    // Discarding the driver's copy Result must fail the error assertion.
    let (mut input, mut stream) = StdioInput::start_with(|| FailedRead).unwrap();
    assert!(eof(&mut stream).await.unwrap().is_empty());
    let error = finish(&mut input).await.unwrap_err().to_string();
    assert!(error.contains("synthetic stdin read failure"), "{error}");
    finish(&mut input).await.unwrap();
}

#[tokio::test]
async fn factory_panic_closes_bridge_and_returns_an_actionable_error() {
    // A panic escaping the dedicated driver must become a joined error, never a hang.
    let (mut input, mut stream) = StdioInput::start_with(|| -> tokio::io::Empty { panic!("synthetic stdin factory panic") }).unwrap();
    assert!(eof(&mut stream).await.unwrap().is_empty());
    let error = finish(&mut input).await.unwrap_err().to_string();
    assert!(error.contains("stdin") && error.contains("panic"), "{error}");
}

struct PanickingRead;

impl AsyncRead for PanickingRead {
    fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        panic!("synthetic stdin poll panic")
    }
}

#[tokio::test]
async fn read_poll_panic_closes_bridge_and_returns_an_actionable_error() {
    // The panic guard must cover polling the source, as well as building it.
    let (mut input, mut stream) = StdioInput::start_with(|| PanickingRead).unwrap();
    assert!(eof(&mut stream).await.unwrap().is_empty());
    let error = finish(&mut input).await.unwrap_err().to_string();
    assert!(error.contains("stdin") && error.contains("panic"), "{error}");
}

#[tokio::test]
async fn dropping_the_owner_signals_stop_and_closes_the_bridge() {
    // Cancellation of the owning future must still notify the driver through Drop.
    let (reader, polling, dropping) = idle();
    let (input, mut stream) = StdioInput::start_with(move || reader).unwrap();
    tokio::time::timeout(TEST_BUDGET, polling).await.unwrap().unwrap();
    drop(input);
    tokio::time::timeout(TEST_BUDGET, dropping).await.unwrap().unwrap();
    assert!(eof(&mut stream).await.unwrap().is_empty());
}

#[test]
fn stopping_stdin_keeps_main_runtime_blocking_writes_owned_until_completion() {
    // Only the auxiliary stdin runtime gets a shutdown timeout. A blocking write on
    // the main runtime must still hold its runtime owner until the write completes.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("filmcraft-stdin-main-write-{}-{unique}.txt", std::process::id()));
    let write_path = path.clone();
    let (started, starting) = std::sync::mpsc::channel();
    let (release, releasing) = std::sync::mpsc::channel();
    let write = runtime.spawn_blocking(move || -> io::Result<()> {
        started.send(()).map_err(io::Error::other)?;
        releasing.recv_timeout(TEST_BUDGET).map_err(io::Error::other)?;
        std::fs::write(write_path, b"pending project save finished")
    });
    starting.recv_timeout(TEST_BUDGET).unwrap();
    runtime.block_on(async {
        let (reader, polling, dropping) = idle();
        let (mut input, mut stream) = StdioInput::start_with(move || reader).unwrap();
        tokio::time::timeout(TEST_BUDGET, polling).await.unwrap().unwrap();
        finish(&mut input).await.unwrap();
        tokio::time::timeout(TEST_BUDGET, dropping).await.unwrap().unwrap();
        assert!(eof(&mut stream).await.unwrap().is_empty());
    });
    let (returned, returning) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        drop(runtime);
        let _ = returned.send(());
    });
    let returned_before_write = returning.recv_timeout(Duration::from_millis(100));
    // Always release the owned writer before assertions or joining its runtime.
    let released = release.send(());
    let joined = returning.recv_timeout(TEST_BUDGET);
    owner.join().unwrap();
    let bytes = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    assert!(matches!(returned_before_write, Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
    released.unwrap();
    joined.unwrap();
    assert!(write.is_finished());
    assert_eq!(bytes.unwrap(), b"pending project save finished");
}

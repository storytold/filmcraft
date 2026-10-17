//! Keep the uncancellable OS stdin read out of the runtime that drains engine work.
//!
//! Only the input runtime may time out during shutdown. Its residual blocking stdin
//! read lives until EOF or process exit; explicit finish joins the outer driver.
use crate::AutomationError;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, DuplexStream};
use tokio::sync::watch;

const BRIDGE_BYTES: usize = 64 * 1024;
const INPUT_RUNTIME_SHUTDOWN: Duration = Duration::from_millis(100);
const DRIVER_FINISH: Duration = Duration::from_secs(1);

pub(super) struct StdioInput {
    stop: watch::Sender<bool>,
    driver: Option<JoinHandle<Result<(), AutomationError>>>,
}

impl StdioInput {
    pub(super) fn start() -> Result<(Self, DuplexStream), AutomationError> {
        Self::start_with(tokio::io::stdin)
    }

    fn start_with<F, R>(factory: F) -> Result<(Self, DuplexStream), AutomationError>
    where
        F: FnOnce() -> R + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (input, writer) = tokio::io::duplex(BRIDGE_BYTES);
        let (stop, stopping) = watch::channel(false);
        let driver = std::thread::Builder::new()
            .name("filmcraft-mcp-stdin".into())
            .spawn(move || {
                // The runtime is retained outside the read's panic guard: unwinding
                // through Runtime::drop would wait on the uncancellable stdin read.
                let runtime = catch_unwind(AssertUnwindSafe(|| tokio::runtime::Builder::new_current_thread().enable_all().build()))
                    .map_err(|_| AutomationError::Other("MCP input runtime construction panicked".into()))?
                    .map_err(|error| AutomationError::Other(format!("MCP input runtime: {error}")))?;
                let result = catch_unwind(AssertUnwindSafe(|| {
                    runtime.block_on(async move {
                        let mut reader = factory();
                        let mut writer = writer;
                        let mut stopping = stopping;
                        tokio::select! {
                            biased;
                            _ = stopping.changed() => Ok(()),
                            result = tokio::io::copy(&mut reader, &mut writer) => {
                                result.map(|_| ()).map_err(|error| AutomationError::Other(format!("MCP input read/copy: {error}")))
                            }
                        }
                    })
                }))
                .unwrap_or_else(|_| Err(AutomationError::Other("MCP stdin input driver panicked".into())));
                // This runtime owns input only. Never time out the main runtime:
                // it still owns pending project writes and export cleanup.
                runtime.shutdown_timeout(INPUT_RUNTIME_SHUTDOWN);
                result
            })
            .map_err(|error| AutomationError::Other(format!("MCP input driver start: {error}")))?;
        Ok((Self { stop, driver: Some(driver) }, input))
    }

    pub(super) async fn finish(&mut self) -> Result<(), AutomationError> {
        use futures_util::FutureExt;
        AssertUnwindSafe(self.finish_inner())
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(AutomationError::Other("MCP input shutdown panicked; driver retained".into())))
    }

    async fn finish_inner(&mut self) -> Result<(), AutomationError> {
        self.stop.send_replace(true);
        let Some(driver) = self.driver.as_ref() else { return Ok(()) };
        let started = Instant::now();
        while !driver.is_finished() {
            if started.elapsed() >= DRIVER_FINISH {
                return Err(AutomationError::Other("MCP input shutdown timed out; driver retained".into()));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let Some(driver) = self.driver.take() else { return Ok(()) };
        driver.join().map_err(|_| AutomationError::Other("MCP input driver panicked during shutdown".into()))?
    }
}

impl Drop for StdioInput {
    fn drop(&mut self) {
        // Last resort for a cancelled caller; only finish confirms driver completion.
        self.stop.send_replace(true);
    }
}

#[cfg(test)]
#[path = "stdio_input_tests.rs"]
mod tests;

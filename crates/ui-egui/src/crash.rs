//! Panic reporting: a panic anywhere (UI pass, frame workers, background jobs) is written to a
//! crash log with its thread, location and backtrace, and the last one is kept for the in-app
//! error window. Without this a panic on the UI thread closes the app silently and one in a
//! worker leaves a monitor blank with no trace.

use std::sync::Mutex;

/// The most recent panic, as shown to the user: message, `file:line`, thread.
static LAST: Mutex<Option<String>> = Mutex::new(None);
/// Where crash logs go (desktop); `None` = stderr only (web, tests).
static LOG_DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Intentional-panic tests share the process-wide hook and last-panic slot.
#[cfg(test)]
pub(crate) static HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Install the panic hook (idempotent). Panics are also passed to the previous hook, so they
/// still reach stderr / the browser console.
pub fn install(log_dir: Option<std::path::PathBuf>) {
    *LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()) = log_dir;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            let summary = format!("{msg} (at {at}, thread {thread})");
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(summary.clone());
            write_log(&summary);
            prev(info);
        }));
    });
}

/// Deliberately panic: the fault-injection hooks (`ui.injectPanic` and the frame-job equivalent)
/// use this to prove the guard above keeps the session alive.
#[allow(clippy::panic)]
pub fn injected_fault(what: &str) -> ! {
    panic!("{what}")
}

/// Write a non-panic fatal error (e.g. the window could not be created) to the crash log.
pub fn record(msg: &str) {
    write_log(msg);
}

/// Take the last panic summary (the error window shows it once).
pub fn take_last() -> Option<String> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The crash log file for today, if logging to disk.
pub fn log_path() -> Option<std::path::PathBuf> {
    let dir = LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Some(dir.join(format!("crash-{}.log", secs / 86_400)))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_log(summary: &str) {
    use std::io::Write;
    let Some(path) = log_path() else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let bt = std::backtrace::Backtrace::force_capture();
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "=== {} FilmCraft {} (unix {secs})\n{summary}\n{bt}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    }
}

#[cfg(target_arch = "wasm32")]
fn write_log(_summary: &str) {}

#[cfg(test)]
mod tests {
    #[test]
    fn hook_records_and_logs_worker_panics() {
        let _guard = super::HOOK_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fc-crash-{}", std::process::id()));
        super::install(Some(dir.clone()));
        let _ = super::take_last();
        let r = std::thread::Builder::new().name("filmcraft-test-worker".into()).spawn(|| panic!("boom in a worker")).unwrap().join();
        assert!(r.is_err());
        let last = super::take_last().expect("recorded");
        assert!(last.contains("boom in a worker") && last.contains("filmcraft-test-worker"), "{last}");
        let log = std::fs::read_to_string(super::log_path().unwrap()).unwrap();
        assert!(log.contains("boom in a worker"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! Native export supervision lives outside Rayon and never joins a possibly wedged worker.
//! A timeout cannot revoke an OS write already in flight: keep the worker's output lease until
//! it actually returns, report the file as incomplete, and never accept its late result.
//! Default inactivity allowance: 900 seconds, twice that for setup. Configure
//! FILMCRAFT_EXPORT_STALL_TIMEOUT_SECS from 30 to 86400; invalid values reject the job before
//! output setup. The lease covers the primary filename, not every derived MXF/sequence file.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;
use web_time::Instant;

use filmcraft_export::{Progress, Report, Stage};

type Outcome = Result<Report, String>;
type SharedResult = Arc<Mutex<Option<Outcome>>>;

const TIMEOUT_ENV: &str = "FILMCRAFT_EXPORT_STALL_TIMEOUT_SECS";

#[derive(Clone, Copy)]
pub(super) struct Policy {
    idle: Duration,
    poll: Duration,
    cancel_grace: Duration,
}

impl Policy {
    pub(super) fn configured() -> Result<Self, String> {
        #[cfg(test)]
        if let Some(policy) = TEST_POLICY.with(|p| p.take()) {
            return Ok(policy);
        }
        let seconds = match std::env::var(TIMEOUT_ENV) {
            Ok(value) => Self::seconds(&value)?,
            Err(std::env::VarError::NotPresent) => 900,
            Err(_) => return Err(format!("{TIMEOUT_ENV} must be valid Unicode; the export was not started")),
        };
        Ok(Self { idle: Duration::from_secs(seconds), poll: Duration::from_secs(1), cancel_grace: Duration::from_secs(5) })
    }

    fn seconds(value: &str) -> Result<u64, String> {
        value
            .parse::<u64>()
            .ok()
            .filter(|v| (30..=86400).contains(v))
            .ok_or_else(|| format!("{TIMEOUT_ENV} must be an integer from 30 to 86400 seconds (default: 900); the export was not started"))
    }
}

fn paths() -> &'static Mutex<HashSet<PathBuf>> {
    static PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    PATHS.get_or_init(Mutex::default)
}

/// Held by the worker, not its monitor. A timed-out worker may still have a file descriptor.
pub(super) struct OutputLease(Option<PathBuf>);

impl OutputLease {
    fn virtual_output() -> Self {
        Self(None)
    }

    pub(super) fn acquire(path: &str) -> Result<Self, String> {
        let path = Path::new(path);
        let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().map_err(|e| e.to_string())?.join(path) };
        // Resolve symlinks BEFORE collapsing '..': symlink/.. follows the physical parent.
        let mut ancestor = absolute.as_path();
        let mut missing = Vec::new();
        let mut key = loop {
            if let Ok(resolved) = ancestor.canonicalize() {
                break resolved;
            }
            if let Some(component) = ancestor.components().next_back() {
                missing.push(component.as_os_str().to_owned());
            }
            let Some(parent) = ancestor.parent() else {
                return Err(format!("cannot resolve output directory: {}", absolute.display()));
            };
            ancestor = parent;
        };
        for name in missing.into_iter().rev() {
            match Path::new(&name).components().next() {
                Some(Component::ParentDir) => {
                    key.pop();
                }
                Some(Component::CurDir) => {}
                _ => key.push(name),
            }
        }
        let key = comparison_key(key);
        let mut active = paths().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !active.insert(key.clone()) {
            return Err(format!("output is still owned by an export worker: {}; wait for that worker to exit before retrying this path", key.display()));
        }
        Ok(Self(Some(key)))
    }
}

impl Drop for OutputLease {
    fn drop(&mut self) {
        if let Some(key) = &self.0 {
            paths().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(key);
        }
    }
}

fn comparison_key(path: PathBuf) -> PathBuf {
    // Conservative ASCII case folding where case-insensitive volumes are common. This may
    // reject distinct case-sensitive names; it changes no output path or Windows drive/UNC
    // prefix. Non-ASCII filesystem aliases remain OS-dependent.
    if cfg!(any(target_os = "macos", windows)) { path.as_os_str().to_ascii_lowercase().into() } else { path }
}

struct Observer {
    activity: (u64, u64),
    advanced: Instant,
    checked: Instant,
    cancelling: Option<Instant>,
}

impl Observer {
    fn new(progress: &Progress, now: Instant) -> Self {
        Self { activity: (progress.activity_count(), progress.done.load(Ordering::Relaxed)), advanced: now, checked: now, cancelling: None }
    }

    fn check(&mut self, progress: &Progress, now: Instant, policy: Policy) -> Option<String> {
        // A suspended machine/process suspends its monitor too. Do not charge sleep time to a
        // running stage. A normally scheduled monitor still detects a worker-only deadlock.
        if now.duration_since(self.checked) > policy.poll.saturating_mul(10).max(Duration::from_secs(30)) {
            self.advanced = now;
            self.cancelling = None;
        }
        self.checked = now;
        let activity = (progress.activity_count(), progress.done.load(Ordering::Relaxed));
        if activity != self.activity {
            self.activity = activity;
            self.advanced = now;
        }
        if progress.cancel.load(Ordering::Relaxed) {
            let since = *self.cancelling.get_or_insert(now);
            if now.duration_since(since) >= policy.cancel_grace {
                return Some("cancelled; the worker has not exited, so its output is incomplete and remains reserved until it exits".into());
            }
            return None;
        }
        let stage = progress.active_stage();
        let allowance = if stage == Stage::Setup { policy.idle.saturating_mul(2) } else { policy.idle };
        if now.duration_since(self.advanced) >= allowance {
            return Some(format!(
                "export stalled in {}: no completed work for {} seconds ({} of {} frames). The output is incomplete; the worker may still own it. Do not use this partial file or retry the same path until the worker exits. See {TIMEOUT_ENV} for slow workloads",
                stage.name(),
                allowance.as_secs(),
                progress.done.load(Ordering::Relaxed),
                progress.total.load(Ordering::Relaxed)
            ));
        }
        None
    }
}

fn publish(progress: &Progress, result: &SharedResult, outcome: Outcome) {
    // Only the monitor writes the result, never the worker. The lock is never held across any
    // render/encode/I/O/user callback, so a stalled worker cannot prevent this publication.
    let mut slot = result.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_some() {
        return;
    }
    if let Err(message) = &outcome {
        progress.abort(message);
    }
    *slot = Some(outcome);
    progress.finished.store(true, Ordering::Release);
}

fn supervise(progress: &Progress, result: &SharedResult, receiver: mpsc::Receiver<Outcome>, policy: Policy) {
    let mut observer = Observer::new(progress, Instant::now());
    loop {
        let outcome = match receiver.recv_timeout(policy.poll) {
            Ok(result) => Some(result),
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err("export worker exited without reporting a result; output is incomplete".into())),
            Err(mpsc::RecvTimeoutError::Timeout) => observer.check(progress, Instant::now(), policy).map(Err),
        };
        if let Some(outcome) = outcome {
            publish(progress, result, outcome);
            return;
        }
    }
}

pub(super) fn start(
    progress: Arc<Progress>,
    result: SharedResult,
    wait: bool,
    run: impl FnOnce() -> Outcome + Send + 'static,
    policy: Policy,
) -> Result<(), String> {
    #[cfg(test)]
    let run: Box<dyn FnOnce() -> Outcome + Send> = match TEST_WORKER.with(|r| r.borrow_mut().take()) {
        Some(replacement) => {
            let p = progress.clone();
            Box::new(move || replacement(p))
        }
        None => Box::new(run),
    };
    start_with_policy(progress, result, OutputLease::virtual_output(), wait, run, policy)
}

#[cfg(test)]
type TestWorker = Box<dyn FnOnce(Arc<Progress>) -> Outcome + Send>;
#[cfg(test)]
thread_local! {
    static TEST_POLICY: std::cell::Cell<Option<Policy>> = const { std::cell::Cell::new(None) };
    static TEST_WORKER: std::cell::RefCell<Option<TestWorker>> = const { std::cell::RefCell::new(None) };
}

fn start_with_policy(
    progress: Arc<Progress>,
    result: SharedResult,
    lease: OutputLease,
    wait: bool,
    run: impl FnOnce() -> Outcome + Send + 'static,
    policy: Policy,
) -> Result<(), String> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("filmcraft-export".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .unwrap_or_else(|_| Err("internal error in export worker (see the crash log); output is incomplete".into()));
            drop(lease);
            let _ = send.send(outcome);
        })
        .map_err(|e| e.to_string())?;
    let (monitor_progress, monitor_result) = (progress.clone(), result.clone());
    let monitor = move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| supervise(&monitor_progress, &monitor_result, receive, policy))).is_err() {
            publish(&monitor_progress, &monitor_result, Err("internal error in export monitor (see the crash log); output is incomplete".into()));
        }
    };
    if wait {
        monitor();
    } else if let Err(error) = std::thread::Builder::new().name("filmcraft-export-monitor".into()).spawn(monitor) {
        let message = format!("could not start export monitor: {error}; output is incomplete");
        publish(&progress, &result, Err(message.clone()));
        return Err(message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn policy() -> Policy {
        Policy { idle: Duration::from_millis(60), poll: Duration::from_millis(5), cancel_grace: Duration::from_millis(20) }
    }

    struct Override;
    impl Override {
        fn new(policy: Policy, worker: Option<TestWorker>) -> Self {
            TEST_POLICY.with(|p| p.set(Some(policy)));
            TEST_WORKER.with(|r| *r.borrow_mut() = worker);
            Self
        }
    }
    impl Drop for Override {
        fn drop(&mut self) {
            TEST_POLICY.with(|p| p.set(None));
            TEST_WORKER.with(|r| *r.borrow_mut() = None);
        }
    }

    #[test]
    fn command_wait_and_job_queue_snapshots_do_not_reenter_stalled_status_lock() {
        for wait in [true, false] {
            let mut session = crate::Session::default();
            session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
            let (release, blocked) = mpsc::channel();
            let (ready, running) = mpsc::channel();
            let _override = Override::new(
                policy(),
                Some(Box::new(move |progress| {
                    progress.enter_stage(Stage::Render);
                    let _status = progress.status.lock().unwrap();
                    ready.send(()).unwrap();
                    // Bound the negative control too: old Job::to_json waits here until the
                    // synthetic worker self-releases, then the elapsed-time assertion fails.
                    let _ = blocked.recv_timeout(Duration::from_secs(3));
                    Ok(report())
                })),
            );
            let started = Instant::now();
            let command = session.execute("file.exportMedia", serde_json::json!({"path":path(), "format":"wav", "wait":wait}));
            running.recv_timeout(Duration::from_secs(1)).unwrap();
            if wait {
                assert!(command.unwrap_err().to_string().contains("stalled"));
            } else {
                assert!(command.is_ok());
            }
            let job = session.jobs.last().unwrap().clone();
            let snapshot = session.execute("jobs.list", serde_json::json!({})).unwrap();
            assert!(!snapshot.as_array().unwrap().is_empty());
            session.execute("export.queue.add", serde_json::json!({"path":path(), "format":"wav"})).unwrap();
            let queued = session.export_queue.items.last_mut().unwrap();
            queued.status = crate::export_tools::QueueStatus::Encoding;
            queued.job = Some(job.id);
            assert!(session.execute("export.queue.list", serde_json::json!({})).is_ok());
            assert!(terminal(&job.result).unwrap_err().contains("stalled"));
            assert!(started.elapsed() < Duration::from_secs(2), "host serialization waited for the worker's display lock");
            release.send(()).unwrap();
        }
    }

    #[derive(Default)]
    struct MemoryFiles {
        written: Mutex<Vec<String>>,
    }
    impl crate::Services for MemoryFiles {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn write_file(&self, path: &str, _: &[u8]) -> std::io::Result<()> {
            self.written.lock().unwrap().push(path.to_owned());
            Ok(())
        }
        fn export_in_memory(&self) -> bool {
            true
        }
    }

    #[test]
    fn independent_virtual_storage_is_not_blocked_by_a_physical_output_lease() {
        let output = PathBuf::from(path()).with_extension("wav").to_string_lossy().into_owned();
        let _physical = OutputLease::acquire(&output).unwrap();
        for _ in 0..2 {
            let files = Arc::new(MemoryFiles::default());
            let mut session = crate::Session::new(files.clone());
            session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
            let _override = Override::new(Policy { idle: Duration::from_secs(5), ..policy() }, None);
            session
                .execute(
                    "file.exportMedia",
                    serde_json::json!({
                        "path":output, "format":"wav", "wait":true,
                        "range":"custom", "startSeconds":0, "endSeconds":0.1
                    }),
                )
                .unwrap();
            assert!(files.written.lock().unwrap().contains(&output));
        }
    }

    #[test]
    fn sidecar_setup_is_supervised_and_cancelled_worker_cannot_create_media_after_resuming() {
        struct BlockedSidecar(Mutex<mpsc::Receiver<()>>);
        impl crate::Services for BlockedSidecar {
            fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
                Err(std::io::ErrorKind::NotFound.into())
            }
            fn write_file(&self, path: &str, _: &[u8]) -> std::io::Result<()> {
                assert!(path.ends_with(".srt"));
                let _ = self.0.lock().unwrap().recv_timeout(Duration::from_secs(3));
                Ok(())
            }
        }
        let (release, blocked) = mpsc::channel();
        let mut session = crate::Session::new(Arc::new(BlockedSidecar(Mutex::new(blocked))));
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.execute("captions.add", serde_json::json!({"seconds":0, "durationSeconds":1, "text":"synthetic setup"})).unwrap();
        let output = PathBuf::from(path()).with_extension("wav");
        let params = serde_json::json!({"path":output, "format":"wav", "captionSidecar":"srt", "wait":true});
        let _override = Override::new(policy(), None);
        let started = Instant::now();
        let error = session.execute("file.exportMedia", params.clone()).unwrap_err().to_string();
        assert!(error.contains("stalled in setup"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!output.exists());
        let failed_job = session.jobs.last().unwrap().clone();
        let _override = Override::new(policy(), None);
        let retry = session.execute("file.exportMedia", params).unwrap_err().to_string();
        assert!(retry.contains("still owned"), "{retry}");
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(lease) = OutputLease::acquire(output.to_str().unwrap()) {
                drop(lease);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!output.exists(), "a cancelled setup must not proceed to create media");
        assert!(terminal(&failed_job.result).unwrap_err().contains("stalled in setup"));
    }

    #[test]
    fn cancellation_grace_keeps_queue_cancelled_with_diagnostic() {
        let mut session = crate::Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let (release, blocked) = mpsc::channel();
        let _override = Override::new(
            policy(),
            Some(Box::new(move |progress| {
                progress.cancel.store(true, Ordering::Release);
                let _ = blocked.recv_timeout(Duration::from_secs(3));
                Ok(report())
            })),
        );
        session.execute("export.queue.add", serde_json::json!({"path":path(), "format":"wav"})).unwrap();
        session.execute("export.queue.start", serde_json::json!({"wait":true})).unwrap();
        let item = session.export_queue.items.last().unwrap();
        assert_eq!(item.status, crate::export_tools::QueueStatus::Cancelled);
        assert!(item.error.as_deref().unwrap().contains("incomplete"));
        release.send(()).unwrap();
    }

    #[test]
    fn timeout_configuration_is_finite_bounded_and_strict() {
        for value in ["", "0", "29", "86401", "-1", "NaN", "1.5", "disabled"] {
            assert!(Policy::seconds(value).is_err());
        }
        for value in ["30", "180", "900", "86400"] {
            assert_eq!(Policy::seconds(value).unwrap().to_string(), value);
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn case_insensitive_lease_comparison_preserves_absolute_prefix() {
        let absolute = std::env::temp_dir().join("FilmCraft-Output.MP4");
        let folded = comparison_key(absolute.clone());
        assert!(folded.is_absolute());
        assert_eq!(folded, comparison_key(absolute.with_file_name("filmcraft-output.mp4")));
        #[cfg(windows)]
        assert!(matches!(folded.components().next(), Some(Component::Prefix(_))));
    }

    fn path() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!("fc-watchdog-{}-{}.mov", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).to_string_lossy().into_owned()
    }

    fn report() -> Report {
        Report { path: "synthetic".into(), frames: 1, seconds: 0.1, bytes: 1, render_fps: 10.0, extra_files: vec![] }
    }

    fn terminal(result: &SharedResult) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(outcome) = result.lock().unwrap().clone() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "monitor did not publish a terminal result");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn both_wait_modes_return_on_stall_without_joining_and_reject_late_success() {
        for wait in [true, false] {
            let progress = Arc::new(Progress::default());
            progress.enter_stage(Stage::Render);
            let result: SharedResult = Arc::default();
            let output = path();
            let lease = OutputLease::acquire(&output).unwrap();
            let (release, blocked) = mpsc::channel();
            let started = Instant::now();
            start_with_policy(
                progress.clone(),
                result.clone(),
                lease,
                wait,
                move || {
                    blocked.recv().unwrap();
                    Ok(report())
                },
                policy(),
            )
            .unwrap();
            let failure = terminal(&result).unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(failure.contains("stalled in render"));
            assert!(failure.contains("incomplete"));
            assert!(progress.cancel.load(Ordering::Acquire));
            assert!(OutputLease::acquire(&output).is_err(), "a detached worker still owns its output");
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(lease) = OutputLease::acquire(&output) {
                    drop(lease);
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(terminal(&result).unwrap_err(), failure, "late success must not replace the failure");
        }
    }

    #[test]
    fn diagnostics_do_not_wait_for_a_workers_status_or_error_lock() {
        let progress = Arc::new(Progress::default());
        progress.enter_stage(Stage::Encode);
        let result: SharedResult = Arc::default();
        let status_lock = progress.status.lock().unwrap();
        let error_lock = progress.error.lock().unwrap();
        let (release, blocked) = mpsc::channel();
        start_with_policy(
            progress.clone(),
            result.clone(),
            OutputLease::acquire(&path()).unwrap(),
            true,
            move || {
                blocked.recv().unwrap();
                Ok(report())
            },
            policy(),
        )
        .unwrap();
        assert!(terminal(&result).unwrap_err().contains("stalled in encode"));
        release.send(()).unwrap();
        drop((status_lock, error_lock));
    }

    #[test]
    fn panicking_or_disconnected_worker_is_a_terminal_error() {
        let progress = Arc::new(Progress::default());
        let result: SharedResult = Arc::default();
        start_with_policy(progress.clone(), result.clone(), OutputLease::acquire(&path()).unwrap(), true, || panic!("synthetic worker failure"), policy())
            .unwrap();
        assert!(terminal(&result).unwrap_err().contains("internal error in export worker"));
        let result: SharedResult = Arc::default();
        let (send, receiver) = mpsc::channel();
        drop(send);
        supervise(&Progress::default(), &result, receiver, policy());
        assert!(terminal(&result).unwrap_err().contains("without reporting a result"));
    }

    #[test]
    fn watchdog_observes_completed_work_not_polling_and_allows_suspend_and_setup() {
        let progress = Progress::default();
        let now = Instant::now();
        let policy = Policy { idle: Duration::from_secs(10), poll: Duration::from_secs(1), cancel_grace: Duration::from_secs(2) };
        let mut observer = Observer::new(&progress, now);
        assert!(observer.check(&progress, now + Duration::from_secs(11), policy).is_none(), "setup gets twice the allowance");
        progress.enter_stage(Stage::Loudness);
        for second in [12, 19, 26, 33] {
            progress.made_progress(Stage::Loudness);
            assert!(observer.check(&progress, now + Duration::from_secs(second), policy).is_none(), "audio chunks count before any video frame");
        }
        progress.enter_stage(Stage::Render);
        assert!(observer.check(&progress, now + Duration::from_secs(42), policy).is_none());
        progress.enter_stage(Stage::Wait);
        assert!(observer.check(&progress, now + Duration::from_secs(44), policy).is_some(), "changing stage alone is not progress");
        assert!(observer.check(&progress, now + Duration::from_secs(200), policy).is_none(), "monitor was suspended too");
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(observer.check(&progress, now + Duration::from_secs(201), policy).is_none());
        assert!(observer.check(&progress, now + Duration::from_secs(203), policy).unwrap().starts_with("cancelled"));
    }

    #[test]
    fn successful_worker_releases_output_and_preserves_report() {
        let result: SharedResult = Arc::default();
        let output = path();
        start_with_policy(Arc::default(), result.clone(), OutputLease::acquire(&output).unwrap(), true, || Ok(report()), policy()).unwrap();
        assert_eq!(terminal(&result).unwrap().frames, 1);
        assert!(OutputLease::acquire(&output).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn output_lease_resolves_symlink_before_parent_component() {
        let dir = PathBuf::from(path());
        std::fs::create_dir_all(dir.join("physical/child")).unwrap();
        std::os::unix::fs::symlink(dir.join("physical/child"), dir.join("alias")).unwrap();
        let via_alias = dir.join("alias/../movie.mp4");
        let direct = dir.join("physical/movie.mp4");
        let lease = OutputLease::acquire(via_alias.to_str().unwrap()).unwrap();
        let duplicate = OutputLease::acquire(direct.to_str().unwrap());
        // Clean up our synthetic fixture before asserting the old implementation's failure.
        drop(lease);
        let collided = duplicate.is_err();
        drop(duplicate);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(collided, "symlink/../output and its physical path must share a lease");
    }
}

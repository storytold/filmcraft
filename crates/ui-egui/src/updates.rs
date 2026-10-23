//! Native self-update state owned by the frontend.
//!
//! Downloading and extracting run on a named worker.  The UI only observes messages from that
//! worker, so a slow network or a large release never stalls editing.

use std::path::PathBuf;

/// What the update affordance should say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    Staging,
    Ready {
        version: String,
        staged: PathBuf,
    },
    UpToDate,
    Throttled,
    Error(String),
}

/// State for Help ▸ Check for Updates.  It deliberately contains no project state.
pub struct UpdateState {
    pub phase: Phase,
    #[cfg(not(target_arch = "wasm32"))]
    updater: Option<filmcraft_updater::Updater>,
    #[cfg(not(target_arch = "wasm32"))]
    check_state: Option<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    receiver: Option<std::sync::mpsc::Receiver<Event>>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            #[cfg(not(target_arch = "wasm32"))]
            updater: None,
            #[cfg(not(target_arch = "wasm32"))]
            check_state: None,
            #[cfg(not(target_arch = "wasm32"))]
            receiver: None,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum Event {
    Staging,
    Ready { version: String, staged: PathBuf },
    UpToDate,
    Throttled,
    Error(String),
}

impl UpdateState {
    /// Give the native frontend its already-validated executable and its app-data locations.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn configure(&mut self, updater: filmcraft_updater::Updater, check_state: PathBuf) {
        self.updater = Some(updater);
        self.check_state = Some(check_state);
    }

    /// Start a check and, when a newer release is found, stage it.  A staged release is never
    /// installed by this worker.
    pub fn start_check(&mut self) -> Result<(), String> {
        if matches!(self.phase, Phase::Checking | Phase::Staging) {
            return Err("an update check is already running".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let updater = self.updater.clone().ok_or("automatic updates are not configured for this application")?;
            let check_state = self.check_state.clone().ok_or("the update check location is unavailable")?;
            let (tx, rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new().name("self-update".into()).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    // This is an explicit request from Help ▸ Check for Updates.  It must always
                    // make a fresh request; the daily cadence applies only to a future automatic
                    // background check.  Keep recording the result so that background check can
                    // still use the same state file.
                    match updater.check(Some((&check_state, now, std::time::Duration::ZERO))) {
                        Ok(filmcraft_updater::Availability::Available(release)) => {
                            let _ = tx.send(Event::Staging);
                            updater
                                .stage(&release, &mut |_, _| true)
                                .map(|staged| Event::Ready { version: release.version.to_string(), staged })
                                .map_err(|e| e.to_string())
                        }
                        Ok(filmcraft_updater::Availability::UpToDate) => Ok(Event::UpToDate),
                        Ok(filmcraft_updater::Availability::Throttled) => Ok(Event::Throttled),
                        Err(e) => Err(e.to_string()),
                    }
                }));
                match result {
                    Ok(Ok(event)) => {
                        let _ = tx.send(event);
                    }
                    Ok(Err(error)) => {
                        let _ = tx.send(Event::Error(error));
                    }
                    Err(_) => {
                        let _ = tx.send(Event::Error("the update worker stopped unexpectedly; no update was installed".into()));
                    }
                }
            });
            if let Err(e) = spawned {
                return Err(format!("could not start the update worker: {e}"));
            }
            self.receiver = Some(rx);
            self.phase = Phase::Checking;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        Err("automatic updates are only available in the desktop app".into())
    }

    /// Consume completed worker messages. Returns true when the caller should repaint.
    pub fn poll(&mut self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut changed = false;
            if let Some(receiver) = self.receiver.take() {
                let mut keep = true;
                loop {
                    match receiver.try_recv() {
                        Ok(event) => {
                            changed = true;
                            self.phase = match event {
                                Event::Staging => Phase::Staging,
                                Event::Ready { version, staged } => {
                                    // `Ready` is terminal.  Without clearing `keep`, a worker that
                                    // finishes staging between UI frames closes the channel here
                                    // and incorrectly replaces this result with a worker-stopped
                                    // error below.
                                    keep = false;
                                    Phase::Ready { version, staged }
                                }
                                Event::UpToDate => {
                                    keep = false;
                                    Phase::UpToDate
                                }
                                Event::Throttled => {
                                    keep = false;
                                    Phase::Throttled
                                }
                                Event::Error(error) => {
                                    keep = false;
                                    Phase::Error(error)
                                }
                            };
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            if keep {
                                changed = true;
                                keep = false;
                                self.phase = Phase::Error("the update worker stopped before reporting a result".into());
                            }
                            break;
                        }
                    }
                }
                if keep {
                    self.receiver = Some(receiver);
                }
            }
            changed
        }
        #[cfg(target_arch = "wasm32")]
        false
    }

    pub fn working(&self) -> bool {
        matches!(self.phase, Phase::Checking | Phase::Staging)
    }

    pub fn ready_version(&self) -> Option<&str> {
        match &self.phase {
            Phase::Ready { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Spawn this executable in its small apply-helper mode. Call only after flushing the journal.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn spawn_apply_helper(&self) -> Result<(), String> {
        let Phase::Ready { staged, .. } = &self.phase else {
            return Err("there is no staged update to apply".into());
        };
        let updater = self.updater.as_ref().ok_or("automatic updates are not configured for this application")?;
        // The restart must reopen the editor as it was started (`--data-dir`, `--empty`, project and
        // media paths), so the helper gets this process's own command line to hand back.
        let launch: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        std::process::Command::new(&updater.exe)
            .args(helper_args(std::process::id(), staged, &updater.exe, &launch))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start the update helper: {e}"))
    }
}

/// The apply helper's command line: the updater flags, then, after `--`, the arguments the editor
/// itself was started with. The helper passes that tail to the restarted build untouched (the
/// parser is `args::parse_update_apply` in `apps/filmcraft`; its test builds a line with this
/// function so the two cannot drift apart). With no launch arguments there is no `--`.
#[cfg(not(target_arch = "wasm32"))]
pub fn helper_args(pid: u32, staged: &std::path::Path, target: &std::path::Path, launch: &[std::ffi::OsString]) -> Vec<std::ffi::OsString> {
    let mut line: Vec<std::ffi::OsString> =
        vec!["--update-apply".into(), "--pid".into(), pid.to_string().into(), "--staged".into(), staged.into(), "--target".into(), target.into()];
    if !launch.is_empty() {
        line.push("--".into());
        line.extend(launch.iter().cloned());
    }
    line
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<std::ffi::OsString> {
        args.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn helper_line_carries_the_launch_arguments_after_a_separator() {
        let launch = os(&["--data-dir", "/custom/path", "--empty", "--", "--odd.mov"]);
        let line = helper_args(42, std::path::Path::new("/s"), std::path::Path::new("/opt/filmcraft/bin/filmcraft"), &launch);
        let flags = os(&["--update-apply", "--pid", "42", "--staged", "/s", "--target", "/opt/filmcraft/bin/filmcraft", "--"]);
        assert_eq!(line[..flags.len()], flags[..]);
        assert_eq!(line[flags.len()..], launch[..], "the launch arguments follow unchanged, even a later `--`");
    }

    #[test]
    fn helper_line_without_launch_arguments_has_no_separator() {
        let line = helper_args(7, std::path::Path::new("/s"), std::path::Path::new("/t"), &[]);
        assert_eq!(line, os(&["--update-apply", "--pid", "7", "--staged", "/s", "--target", "/t"]));
    }

    #[test]
    fn ready_result_survives_worker_disconnect() {
        let (tx, rx) = std::sync::mpsc::channel();
        let staged = PathBuf::from("/tmp/filmcraft-update-test");
        tx.send(Event::Ready { version: "1.2.3".into(), staged: staged.clone() }).unwrap();
        drop(tx);

        let mut updates = UpdateState { phase: Phase::Checking, updater: None, check_state: None, receiver: Some(rx) };
        assert!(updates.poll());
        assert_eq!(updates.phase, Phase::Ready { version: "1.2.3".into(), staged });
        assert!(updates.receiver.is_none());
    }
}

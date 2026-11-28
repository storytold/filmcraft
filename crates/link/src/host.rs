//! What the running app gives Craft Link: window calls for focus handoff, a way to start
//! EffectCraft, and where discovery records live.
//!
//! The desktop app installs [`Host::window`]. The CLI and tests use [`Host::headless`] or build
//! their own. [`host`] hands the installed one to the commands, which have no other way to reach
//! app state: a command is `fn(&mut Session, &Value)`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use craft_ipc::activate::{self, ActivationBackend, FocusResult};
use craft_ipc::server::{Activate, Activation};
use craft_ipc::{InstanceRecord, LinkError, RuntimeDir};
use serde_json::Value;

use crate::prompt::EFFECTCRAFT_APP;
use crate::spawn::{LAUNCH_CEILING, Launch, Step, wait_for};

/// Window calls the commands and the `app.activate` handler share. The real one wraps an egui
/// context, which is `Send + Sync`, so the connection thread can answer `app.activate`.
pub trait WindowBackend: ActivationBackend + Send + Sync {}

impl<T: ActivationBackend + Send + Sync> WindowBackend for T {}

/// What a call to [`Launcher::launch`] or [`Launcher::poll`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Launched {
    /// The app answers. This is its record.
    Ready(Box<InstanceRecord>),
    /// The app was started and has not published yet. Look again later.
    Starting,
}

/// How far a start has come, for the progress dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchProgress {
    pub elapsed: Duration,
    /// When the app is stopped if it still has not published.
    pub ceiling: Duration,
}

/// Starts EffectCraft when no instance runs.
///
/// A start takes from 0.3 s to over 15 s, and a window that waits on it cannot draw. So a launcher
/// can answer [`Launched::Starting`], keep the child, and be asked again with [`Launcher::poll`].
pub trait Launcher: Send + Sync {
    /// Start the app, or continue the start that is running, and report.
    ///
    /// `wait` blocks until the app is ready or has failed, and never answers `Starting`. A
    /// launcher with no window to draw progress in blocks whatever `wait` says.
    /// [`LinkError::NotInstalled`] means there is no binary to start.
    fn launch(&self, runtime: &RuntimeDir, wait: bool) -> Result<Launched, LinkError>;

    /// Look at the start that is running, without starting one. Errors when none runs.
    fn poll(&self, runtime: &RuntimeDir) -> Result<Launched, LinkError>;

    /// Progress of the running start. `None` when none runs.
    fn progress(&self) -> Option<LaunchProgress>;

    /// Stop the running start. The app that was started is killed. Does nothing when none runs.
    fn cancel(&self);
}

/// Finds the EffectCraft binary the way every Craft app does ([`craft_ipc::launch::resolve`], so
/// `CRAFT_EFFECTCRAFT_BIN` still wins) and starts it as a [`Launch`]. The child is stopped when it
/// exits, when the user cancels, and at [`LAUNCH_CEILING`], and never before.
pub struct SystemLauncher {
    interactive: bool,
    bin: Option<PathBuf>,
    args: Vec<OsString>,
    ceiling: Duration,
    running: Mutex<Option<Launch>>,
}

impl SystemLauncher {
    /// For a session with a window: `launch` returns `Starting` and the UI shows progress.
    pub fn interactive() -> Self {
        Self { interactive: true, bin: None, args: Vec::new(), ceiling: LAUNCH_CEILING, running: Mutex::new(None) }
    }

    /// For a session with no window: `launch` blocks.
    pub fn blocking() -> Self {
        Self { interactive: false, ..Self::interactive() }
    }

    /// Start this binary and not the one `resolve` finds. For tests.
    pub fn with_bin(mut self, bin: PathBuf) -> Self {
        self.bin = Some(bin);
        self
    }

    /// Start the binary with these arguments. For tests, which run a script through `sh`.
    pub fn with_args(mut self, args: Vec<OsString>) -> Self {
        self.args = args;
        self
    }

    /// Stop a child that has not published after this long. For tests.
    pub fn with_ceiling(mut self, ceiling: Duration) -> Self {
        self.ceiling = ceiling;
        self
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Launch>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(&self) -> Result<Launch, LinkError> {
        let bin = match &self.bin {
            Some(bin) => bin.clone(),
            None => craft_ipc::launch::resolve(EFFECTCRAFT_APP)?,
        };
        Launch::start(EFFECTCRAFT_APP, &bin, &self.args, self.ceiling)
    }
}

impl Launcher for SystemLauncher {
    fn launch(&self, runtime: &RuntimeDir, wait: bool) -> Result<Launched, LinkError> {
        let current = self.slot().take();
        let launch = match current {
            Some(launch) => launch,
            None => self.begin()?,
        };
        if wait || !self.interactive {
            return wait_for(launch, runtime).map(|record| Launched::Ready(Box::new(record)));
        }
        self.step(launch, runtime)
    }

    fn poll(&self, runtime: &RuntimeDir) -> Result<Launched, LinkError> {
        let Some(launch) = self.slot().take() else {
            return Err(LinkError::NotReady("EffectCraft is not being started".to_string()));
        };
        self.step(launch, runtime)
    }

    fn progress(&self) -> Option<LaunchProgress> {
        self.slot().as_ref().map(|launch| LaunchProgress { elapsed: launch.elapsed(), ceiling: launch.ceiling() })
    }

    fn cancel(&self) {
        if let Some(launch) = self.slot().take() {
            launch.cancel();
        }
    }
}

impl SystemLauncher {
    /// One look. A child still starting goes back in the slot.
    fn step(&self, launch: Launch, runtime: &RuntimeDir) -> Result<Launched, LinkError> {
        match launch.poll(runtime)? {
            Step::Ready(record) => Ok(Launched::Ready(Box::new(record))),
            Step::Waiting(launch) => {
                *self.slot() = Some(launch);
                Ok(Launched::Starting)
            }
        }
    }
}

/// Window calls for a session that has no window.
///
/// Every answer says so: no token, focus refused. Nothing here claims a window was raised.
pub struct Headless;

impl ActivationBackend for Headless {
    fn mint_token(&self) -> Option<String> {
        None
    }
    fn set_pending_token(&self, _token: &str) {}
    fn focus(&self) -> FocusResult {
        FocusResult::Refused
    }
    fn request_user_attention(&self) {}
    fn yield_activation_to(&self, _bundle_id: &str) -> bool {
        false
    }
    fn activate_application(&self) -> bool {
        false
    }
}

/// Window calls through an egui context: [`egui::ViewportCommand::Focus`] and
/// [`egui::ViewportCommand::RequestUserAttention`].
///
/// Known gaps, each answered honestly and none faked:
/// - `can_apply_token` is `false`. Stock winit 0.30 has an empty Wayland `focus_window()` and no call
///   that applies an activation token to a window that already exists (rust-windowing/winit#3633).
///   A token from the peer is ignored, the focus request is not counted as a request, and the
///   receiver asks for user attention and answers `false`.
/// - `mint_token` returns `None`. eframe can mint a token (`Frame::request_activation_token`, then
///   `Frame::take_activation_token` on a later frame), but only the UI update has a `Frame`, and the
///   token has no user until winit can apply one.
/// - `yield_activation_to` and `activate_application` return `false` (macOS `objc2-app-kit` calls
///   are not written), so on macOS `app.activate` answers `false` and asks for attention.
pub struct EguiWindow {
    ctx: egui::Context,
}

impl EguiWindow {
    pub fn new(ctx: egui::Context) -> Self {
        Self { ctx }
    }
}

impl ActivationBackend for EguiWindow {
    fn mint_token(&self) -> Option<String> {
        None
    }

    fn can_apply_token(&self) -> bool {
        false
    }

    fn set_pending_token(&self, _token: &str) {}

    fn focus(&self) -> FocusResult {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.ctx.request_repaint();
        // The window system decides later. The command was queued, nothing more is known.
        FocusResult::Pending
    }

    fn request_user_attention(&self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        self.ctx.request_repaint();
    }

    fn yield_activation_to(&self, _bundle_id: &str) -> bool {
        false
    }

    fn activate_application(&self) -> bool {
        false
    }
}

/// Answers `app.activate` on the connection thread.
pub struct WindowActivate {
    backend: Arc<dyn WindowBackend>,
}

impl Activate for WindowActivate {
    fn activate(&self, token: Option<String>, _from_pid: Option<u32>, _open: Option<Value>) -> Activation {
        activate::apply_activation(&*self.backend, token.as_deref())
    }
}

/// Everything the commands need from the app.
pub struct Host {
    pub backend: Arc<dyn WindowBackend>,
    pub launcher: Arc<dyn Launcher>,
    /// `None` uses [`RuntimeDir::resolve`]. Tests give a private directory.
    pub runtime: Option<RuntimeDir>,
}

impl Host {
    /// A session with a window.
    pub fn window(ctx: egui::Context) -> Self {
        Self { backend: Arc::new(EguiWindow::new(ctx)), launcher: Arc::new(SystemLauncher::interactive()), runtime: None }
    }

    /// A session without a window (the CLI).
    pub fn headless() -> Self {
        Self { backend: Arc::new(Headless), launcher: Arc::new(SystemLauncher::blocking()), runtime: None }
    }

    /// The directory discovery records live in.
    pub fn runtime_dir(&self) -> Result<RuntimeDir, LinkError> {
        match &self.runtime {
            Some(dir) => RuntimeDir::at(dir.path()),
            None => RuntimeDir::resolve(),
        }
    }

    /// The `app.activate` handler for this host's window.
    pub fn activate_handler(&self) -> Arc<dyn Activate> {
        Arc::new(WindowActivate { backend: Arc::clone(&self.backend) })
    }
}

static HOST: Mutex<Option<Arc<Host>>> = Mutex::new(None);

/// Make `host` the one the commands use.
pub fn install(host: Host) -> Arc<Host> {
    let host = Arc::new(host);
    *HOST.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&host));
    host
}

/// The installed host, or a headless one when nothing was installed.
pub fn host() -> Arc<Host> {
    let mut slot = HOST.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(slot.get_or_insert_with(|| Arc::new(Host::headless())))
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;
    use std::time::Instant;

    use craft_ipc::Discovery;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fc-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A launcher that runs `body` as a shell script.
    fn running(dir: &Path, body: &str, interactive: bool) -> SystemLauncher {
        let script = dir.join("effectcraft");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        let base = if interactive { SystemLauncher::interactive() } else { SystemLauncher::blocking() };
        base.with_bin(PathBuf::from("/bin/sh")).with_args(vec![script.into_os_string()])
    }

    #[test]
    fn a_token_the_window_cannot_apply_is_a_refusal_with_attention() {
        use craft_ipc::activate::{Platform, apply_activation_on};
        let ctx = egui::Context::default();
        let window = EguiWindow::new(ctx.clone());
        assert!(!window.can_apply_token());
        let mut got = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
            got = Some(apply_activation_on(Platform::Linux, &window, Some("tok")));
        });
        output.textures_delta.clear();
        assert_eq!(got, Some(craft_ipc::server::Activation::No), "Linux must not claim a request it cannot make");
        let commands = &output.viewport_output.get(&egui::ViewportId::ROOT).expect("root viewport").commands;
        assert!(commands.iter().any(|c| matches!(c, egui::ViewportCommand::RequestUserAttention(_))), "{commands:?}");
    }

    fn pid_of(file: &Path) -> u32 {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = std::fs::read_to_string(file).ok().and_then(|text| text.trim().parse().ok()) {
                return pid;
            }
            assert!(Instant::now() < until, "the script never wrote its pid");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z"))
    }

    fn publish(runtime: &RuntimeDir, pid: u32) -> Discovery {
        let endpoint = craft_ipc::Endpoint::Unix { path: format!("{}/effectcraft-{pid}.sock", runtime.path().display()) };
        Discovery::publish(runtime, InstanceRecord::new(EFFECTCRAFT_APP, "0.8.3", pid, endpoint).unwrap()).unwrap()
    }

    #[test]
    fn a_window_session_gets_starting_with_progress_and_cancel_kills_the_child() {
        let dir = scratch("cancel");
        let pid_file = dir.join("pid");
        let launcher = running(&dir, &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()), true);
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        assert_eq!(launcher.progress(), None, "nothing runs yet");
        assert!(launcher.poll(&runtime).is_err(), "polling must not start anything");
        assert_eq!(launcher.launch(&runtime, false).unwrap(), Launched::Starting);
        let pid = pid_of(&pid_file);
        let progress = launcher.progress().expect("a start is running");
        assert_eq!(progress.ceiling, LAUNCH_CEILING);
        // A second call joins the start that runs.
        assert_eq!(launcher.launch(&runtime, false).unwrap(), Launched::Starting);
        assert_eq!(pid_of(&pid_file), pid, "a second child was started");
        launcher.cancel();
        assert_eq!(launcher.progress(), None);
        assert!(!alive(pid), "cancel left the child {pid} running");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_that_exits_ends_the_start_with_its_status() {
        let dir = scratch("exit");
        let launcher = running(&dir, "exit 3", true);
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let err = loop {
            match launcher.launch(&runtime, false) {
                Ok(Launched::Starting) => {}
                Ok(Launched::Ready(record)) => panic!("{record:?}"),
                Err(err) => break err,
            }
            assert!(Instant::now() < until, "an exited child was still waited for after 5 s");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(err.to_string().contains("stopped at start-up"), "{err}");
        assert_eq!(launcher.progress(), None, "the failed start must clear its state");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_blocks_until_the_child_publishes() {
        let dir = scratch("wait");
        let pid_file = dir.join("pid");
        let launcher = running(&dir, &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()), true);
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let publisher = {
            let (pid_file, path) = (pid_file.clone(), runtime.path().to_path_buf());
            std::thread::spawn(move || {
                let pid = pid_of(&pid_file);
                std::thread::sleep(Duration::from_millis(300));
                (pid, publish(&RuntimeDir::at(&path).unwrap(), pid))
            })
        };
        let Launched::Ready(record) = launcher.launch(&runtime, true).unwrap() else { panic!("wait must not answer Starting") };
        let (pid, published) = publisher.join().unwrap();
        assert_eq!(record.pid, pid);
        assert_eq!(launcher.progress(), None);
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        drop(published);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launcher_without_a_window_blocks_whatever_wait_says() {
        let dir = scratch("blocking");
        let pid_file = dir.join("pid");
        let launcher = running(&dir, &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()), false);
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let publisher = {
            let (pid_file, path) = (pid_file.clone(), runtime.path().to_path_buf());
            std::thread::spawn(move || {
                let pid = pid_of(&pid_file);
                std::thread::sleep(Duration::from_millis(300));
                (pid, publish(&RuntimeDir::at(&path).unwrap(), pid))
            })
        };
        assert!(matches!(launcher.launch(&runtime, false).unwrap(), Launched::Ready(_)));
        let (pid, published) = publisher.join().unwrap();
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        drop(published);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

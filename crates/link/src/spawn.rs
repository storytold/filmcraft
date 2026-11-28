//! Start EffectCraft and wait for it to publish its discovery record.
//!
//! `craft_ipc::launch::launch_at` waits a fixed 15 s for a record, whatever the child does. This
//! module watches the child instead. A [`Launch`] is a started child plus the time it started.
//! [`Launch::poll`] looks at the child once and answers at once, so the UI can keep drawing a
//! progress dialog between looks.
//!
//! A start ends in one of four ways:
//! - the child publishes its record (ready),
//! - the child exits (a build without Craft Link support, a bad binary): the start fails at once,
//! - the user cancels: the child is killed,
//! - the ceiling passes ([`LAUNCH_CEILING`]): the child is killed and the error says so.
//!
//! A healthy but slow child is never killed before the ceiling. A first start into an empty shader
//! cache took 7 to 14 s in the second live run, so the old fixed 8 s limit killed healthy apps.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use craft_ipc::{Discovery, InstanceRecord, LinkError, RuntimeDir};

/// The longest a started app has to publish its record. Past it the app is stopped. A healthy cold
/// start takes up to about 15 s on a loaded machine, so this leaves four times that.
pub const LAUNCH_CEILING: Duration = Duration::from_secs(60);
/// How often the blocking wait looks.
const POLL_EVERY: Duration = Duration::from_millis(50);

/// A started app that has not published its record yet.
#[derive(Debug)]
pub struct Launch {
    app: String,
    bin: PathBuf,
    child: Child,
    started: Instant,
    ceiling: Duration,
}

/// What one look at a [`Launch`] found.
#[derive(Debug)]
pub enum Step {
    /// The app published its record. It keeps running and is not ours to stop.
    Ready(InstanceRecord),
    /// The app runs and has not published yet.
    Waiting(Launch),
}

impl Launch {
    /// Start `bin` in its own process group, so closing FilmCraft's terminal does not close it.
    ///
    /// [`LinkError::Io`] when the process cannot be started at all.
    pub fn start(app: &str, bin: &Path, args: &[OsString], ceiling: Duration) -> Result<Self, LinkError> {
        let mut command = Command::new(bin);
        command.args(args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().map_err(|err| LinkError::Io(format!("could not start {}: {err}", bin.display())))?;
        Ok(Self { app: app.to_string(), bin: bin.to_path_buf(), child, started: Instant::now(), ceiling })
    }

    /// Time since the start.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The time the app has to publish before it is stopped.
    pub fn ceiling(&self) -> Duration {
        self.ceiling
    }

    /// The binary that was started.
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// Look once. Never waits.
    ///
    /// An error means the app is stopped and reaped: it exited ([`LinkError::NotReady`] with its
    /// status), the ceiling passed, or the discovery directory could not be read.
    pub fn poll(mut self, runtime: &RuntimeDir) -> Result<Step, LinkError> {
        let pid = self.child.id();
        let found = match Discovery::scan(runtime) {
            Ok(records) => records.into_iter().find(|record| record.app == self.app && record.pid == pid),
            Err(err) => {
                self.stop();
                return Err(err);
            }
        };
        if let Some(record) = found {
            reap_later(self.child)?;
            return Ok(Step::Ready(record));
        }
        // A record may be written just before the child exits, so look at the child second.
        match self.child.try_wait() {
            Ok(Some(status)) => return Err(exited(&self.bin, status)),
            Ok(None) => {}
            Err(err) => {
                self.stop();
                return Err(LinkError::io(err));
            }
        }
        if self.started.elapsed() >= self.ceiling {
            self.stop();
            return Err(LinkError::NotReady(format!(
                "{} did not become ready within {} s, so FilmCraft stopped it. It may be an old build without Craft Link support.",
                self.bin.display(),
                self.ceiling.as_secs()
            )));
        }
        Ok(Step::Waiting(self))
    }

    /// Kill the app and collect its exit status, so it leaves no zombie.
    pub fn cancel(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // `kill` fails when the child has exited already. `wait` then returns that status at once.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn exited(bin: &Path, status: ExitStatus) -> LinkError {
    LinkError::NotReady(format!("{} stopped at start-up ({status}) and never became ready. It may be an old build without Craft Link support.", bin.display()))
}

/// Start `bin` and block until its record appears. `wait` is the ceiling.
///
/// For callers that have no window to draw progress in (the CLI, `wait: true` automation). Errors
/// are [`LinkError::NotReady`] with a sentence for the user, or [`LinkError::Io`] when the process
/// cannot be started at all. A child that fails is not left running.
pub fn spawn_and_wait(app: &str, bin: &Path, args: &[OsString], runtime: &RuntimeDir, wait: Duration) -> Result<InstanceRecord, LinkError> {
    wait_for(Launch::start(app, bin, args, wait)?, runtime)
}

/// Block until `launch` is ready or has failed.
pub fn wait_for(mut launch: Launch, runtime: &RuntimeDir) -> Result<InstanceRecord, LinkError> {
    loop {
        match launch.poll(runtime)? {
            Step::Ready(record) => return Ok(record),
            Step::Waiting(again) => launch = again,
        }
        std::thread::sleep(POLL_EVERY);
    }
}

/// A running app is not ours to stop. A thread waits for it, so its exit leaves no zombie.
fn reap_later(mut child: Child) -> Result<(), LinkError> {
    std::thread::Builder::new()
        .name("craft-ipc-wait".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map(|_| ())
        .map_err(LinkError::io)
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    const APP: &str = "effectcraft";

    /// Write a shell script. Tests run it as `sh <script>`: executing a file that another test thread
    /// still holds open for writing fails with ETXTBSY, and `sh` only reads it.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        path
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fc-launch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sh(script: &Path) -> Vec<OsString> {
        vec![script.as_os_str().to_owned()]
    }

    /// The pid of a script that wrote `$$` to `pid`, once it has.
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
        // A zombie keeps its /proc entry until it is reaped. `stop` and the reaper thread reap.
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z"))
    }

    /// Play the app: publish a record under the script's pid after `delay`, once the script says who it is.
    fn publish_after(runtime: &RuntimeDir, pid_file: &Path, delay: Duration) -> std::thread::JoinHandle<Discovery> {
        let (runtime_path, pid_file) = (runtime.path().to_path_buf(), pid_file.to_path_buf());
        std::thread::spawn(move || {
            let pid = pid_of(&pid_file);
            std::thread::sleep(delay);
            let runtime = RuntimeDir::at(&runtime_path).unwrap();
            let endpoint = craft_ipc::Endpoint::Unix { path: format!("{}/effectcraft-{pid}.sock", runtime_path.display()) };
            let record = InstanceRecord::new(APP, "0.8.3", pid, endpoint).unwrap();
            Discovery::publish(&runtime, record).unwrap()
        })
    }

    #[test]
    fn a_binary_that_exits_at_once_fails_at_once() {
        let dir = scratch("exit");
        let bin = script(&dir, "effectcraft", "echo old build >&2\nexit 3");
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let began = Instant::now();
        let err = spawn_and_wait(APP, Path::new("/bin/sh"), &sh(&bin), &runtime, LAUNCH_CEILING).expect_err("no record can appear");
        assert!(began.elapsed() < Duration::from_secs(5), "waited {:?} for a process that had already exited: {err}", began.elapsed());
        let LinkError::NotReady(text) = err else { panic!("expected NotReady, got {err:?}") };
        assert!(text.contains("stopped at start-up") && text.contains("exit status: 3"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_silent_binary_is_stopped_at_the_ceiling_and_not_left_running() {
        let dir = scratch("silent");
        let bin = script(&dir, "effectcraft", "exec sleep 60");
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let began = Instant::now();
        // The pid comes from the child handle, not from a file the script writes: on a loaded machine
        // the ceiling can pass before `sh` has run its first line, and the test must still know whom to look for.
        let launch = Launch::start(APP, Path::new("/bin/sh"), &sh(&bin), Duration::from_millis(400)).unwrap();
        let pid = launch.child.id();
        let err = wait_for(launch, &runtime).expect_err("it never publishes");
        let waited = began.elapsed();
        assert!(waited >= Duration::from_millis(400) && waited < Duration::from_secs(5), "{waited:?}");
        let LinkError::NotReady(text) = err else { panic!("expected NotReady, got {err:?}") };
        assert!(text.contains("did not become ready within 0 s") || text.contains("did not become ready"), "{text}");
        assert!(!alive(pid), "the orphan {pid} is still running");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live run saw healthy first starts take 7 to 14 s. The old limit was 8 s.
    #[test]
    fn a_slow_but_healthy_binary_is_waited_for_past_the_old_8_second_limit() {
        let dir = scratch("slow");
        let pid_file = dir.join("pid");
        let bin = script(&dir, "effectcraft", &format!("echo $$ > '{}'\nexec sleep 40", pid_file.display()));
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let publisher = publish_after(&runtime, &pid_file, Duration::from_secs(12));
        let mut launch = Launch::start(APP, Path::new("/bin/sh"), &sh(&bin), LAUNCH_CEILING).unwrap();
        let mut waiting_at_9s = false;
        let record = loop {
            match launch.poll(&runtime).expect("a healthy slow start must not fail") {
                Step::Ready(record) => break record,
                Step::Waiting(again) => {
                    if again.elapsed() > Duration::from_secs(9) {
                        waiting_at_9s = true;
                        assert!(alive(again.child.id()), "the child was killed while it was still starting");
                    }
                    launch = again;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(waiting_at_9s, "the launch was never seen waiting past 9 s");
        assert_eq!(record.pid, pid_of(&pid_file));
        assert!(alive(record.pid), "a healthy app must keep running");
        let _ = std::process::Command::new("kill").arg(record.pid.to_string()).status();
        drop(publisher.join().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_answers_at_once_while_the_child_runs() {
        let dir = scratch("poll");
        let pid_file = dir.join("pid");
        let bin = script(&dir, "effectcraft", &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()));
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let launch = Launch::start(APP, Path::new("/bin/sh"), &sh(&bin), LAUNCH_CEILING).unwrap();
        let began = Instant::now();
        let Step::Waiting(launch) = launch.poll(&runtime).unwrap() else { panic!("nothing published yet") };
        assert!(began.elapsed() < Duration::from_secs(1), "poll blocked for {:?}", began.elapsed());
        let pid = pid_of(&pid_file);
        launch.cancel();
        assert!(!alive(pid), "cancel left the child {pid} running");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_that_exits_fails_the_next_poll_not_the_ceiling() {
        let dir = scratch("poll-exit");
        let bin = script(&dir, "effectcraft", "exit 4");
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let mut launch = Launch::start(APP, Path::new("/bin/sh"), &sh(&bin), LAUNCH_CEILING).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let err = loop {
            match launch.poll(&runtime) {
                Err(err) => break err,
                Ok(Step::Waiting(again)) => launch = again,
                Ok(Step::Ready(record)) => panic!("nothing can publish: {record:?}"),
            }
            assert!(Instant::now() < until, "an exited child was still waited for after 5 s");
            std::thread::sleep(Duration::from_millis(20));
        };
        let LinkError::NotReady(text) = err else { panic!("expected NotReady, got {err:?}") };
        assert!(text.contains("stopped at start-up") && text.contains("exit status: 4"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binary_that_publishes_is_returned_and_left_running() {
        let dir = scratch("ok");
        let pid_file = dir.join("pid");
        let bin = script(&dir, "effectcraft", &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()));
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let publisher = publish_after(&runtime, &pid_file, Duration::ZERO);
        let record = spawn_and_wait(APP, Path::new("/bin/sh"), &sh(&bin), &runtime, Duration::from_secs(10)).expect("the record appears");
        let published = publisher.join().unwrap();
        assert_eq!(record.pid, pid_of(&pid_file));
        assert!(alive(record.pid), "a healthy app must keep running");
        // Stop the script: it is not an app.
        let _ = std::process::Command::new("kill").arg(record.pid.to_string()).status();
        drop(published);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binary_that_cannot_start_is_an_io_error() {
        let dir = scratch("nobin");
        std::fs::create_dir_all(&dir).unwrap();
        let runtime = RuntimeDir::at(dir.join("rt")).unwrap();
        let err = spawn_and_wait(APP, &dir.join("missing"), &[], &runtime, Duration::from_secs(1)).expect_err("no such file");
        assert!(matches!(err, LinkError::Io(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

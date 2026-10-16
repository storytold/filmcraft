//! Graphics start-up: the wgpu backends and adapter the window is created with, and the fallback
//! when a start fails (docs/gaps.md G4, #820).
//!
//! A graphics driver can crash inside wgpu instance or device creation (Intel's Vulkan driver
//! `igvk64.dll`, #512 #413; AMD's OpenGL driver `atio6axx.dll`, #477), where no Rust error
//! handling runs, so every launch died the same way until the user set `WGPU_BACKEND` by hand.
//!
//! # The adapter
//! In the default mode on Windows the adapter is wgpu's own choice, except for a GPU that Vulkan
//! lists through two installed versions of one driver: there the newer one draws, because a
//! window drawn through the stale one dies inside the driver (`amdvlk64.dll`; see
//! `filmcraft_gpu::adapter`). That case is avoided up front instead of left to the fallback
//! below, which would end in DirectX 12 only.
//!
//! # When a start counts as failed
//! The data folder keeps the mode to start with and the failed starts in a row in it
//! ([`STATE_FILE`]). Each start also leaves its own attempt file (`graphics-start/<pid>-<n>.json`,
//! so concurrent launches never write the same file) that is removed once the window has painted
//! two frames, when the process ends normally, or [`DEATH_WINDOW`] after launch, whatever happened
//! by then. An attempt file found by a later launch therefore means a process that died within
//! [`DEATH_WINDOW`] of its start without painting: a driver crash, typically. A start that is
//! force-quit or crashes later is not counted; neither is another instance that is still starting
//! (its attempt is younger than [`DEATH_WINDOW`] and is left alone). After [`FAILURES_TO_MOVE_ON`]
//! such failures in a row in the same mode, the launch moves on to the next [`Mode`] of the
//! platform's [`LADDER`] and keeps it; a start that paints resets the count. A start-up error that
//! wgpu does report (no adapter, device creation failed) moves on at once, within the same process.
//!
//! # A start that hangs
//! A driver can also hang inside instance, adapter or device creation instead of crashing (#1061):
//! the process lives on without a window, so no later launch ever finds a dead attempt. A watchdog
//! ([`GraphicsStart::watch_for_hang`]) gives the graphics device [`HANG_TIMEOUT`] from launch; if
//! it still isn't there, the next mode is saved at once, like a reported error, so the next launch
//! opens in it. The stuck process can't recover or even end itself: on a GTX 1050 laptop neither
//! starting a new process nor `abort` got past the hang, most likely because a driver stuck while
//! being loaded holds the Windows loader lock. Another process can end it, so the watchdog leaves
//! a marker ([`HUNG_DIR`]) and the next launch ends the stuck start before it loads the driver
//! itself ([`GraphicsStart::end_stuck_starts`]).
//! A mode picked by hand (`--graphics`, `WGPU_BACKEND`) and the last mode of the ladder are left
//! as they are: the hang is only logged.
//!
//! # Getting back
//! A fallback mode shows a notice with a reset button in the window (`graphics.notice.reset`,
//! [`GraphicsStart::reset_to_default`]); `--graphics default` does the same from the command
//! line, and `--graphics dx12|software` picks a mode by hand. An explicit `WGPU_BACKEND` leaves
//! the saved state alone: the user is in charge.
//!
//! All files are written beside and renamed into place, so a reader never sees half a file; two
//! instances updating [`STATE_FILE`] at the same moment can lose one update (a failure counted
//! once less, a confirmation repeated by the next start), never corrupt it.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::{
    NativeOptions,
    egui_wgpu::WgpuSetup,
    wgpu::{self, Backends},
};

/// The saved graphics state in the data folder: `{"mode": "<mode>", "failures": <n>}`.
pub const STATE_FILE: &str = "graphics-start.json";
/// The folder in the data folder with one attempt file per start that has not painted yet.
pub const ATTEMPTS_DIR: &str = "graphics-start";
/// The folder in the data folder with one marker per start the watchdog found stuck:
/// `<pid>.json` with its launch time, `{"started": <Unix seconds>}`.
pub const HUNG_DIR: &str = "graphics-hung";
/// How long after launch a start that has not painted may die and still count as failed. A
/// driver crash in instance or device creation happens within a second or two.
pub const DEATH_WINDOW: Duration = Duration::from_secs(15);
/// Failed starts in a row, in the same mode, that move on to the next mode.
pub const FAILURES_TO_MOVE_ON: u32 = 2;
/// How long after launch the graphics device may take before the start counts as hung. Creating
/// it takes a second or two; a cold start from a slow disk well under this.
pub const HANG_TIMEOUT: Duration = Duration::from_secs(30);
/// Attempt files looked at per launch; anything beyond is left for a later launch.
const MAX_ATTEMPTS: usize = 64;
/// The state and attempt files are a few bytes; anything longer is not ours.
const MAX_FILE: u64 = 4096;

/// How the window's graphics device is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The platform default: Vulkan or DirectX 12 on Windows (no OpenGL, see [`apply`]), Metal on
    /// macOS, Vulkan or OpenGL elsewhere.
    Default,
    /// DirectX 12 only (Windows), for Vulkan drivers that crash at start-up.
    Dx12,
    /// A software adapter (WARP on Windows, lavapipe / llvmpipe elsewhere); the preview then
    /// composites on the CPU (`FilmcraftApp::set_wgpu`).
    Software,
}

impl Mode {
    /// The name in [`STATE_FILE`] and on the command line (`--graphics <name>`).
    pub fn name(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::Dx12 => "dx12",
            Mode::Software => "software",
        }
    }

    /// The English name shown in the interface (translated there).
    pub fn label(self) -> &'static str {
        match self {
            Mode::Default => "default graphics",
            Mode::Dx12 => "DirectX 12 only",
            Mode::Software => "software rendering",
        }
    }

    pub fn parse(name: &str) -> Option<Mode> {
        [Mode::Default, Mode::Dx12, Mode::Software].into_iter().find(|m| m.name() == name)
    }
}

/// The modes tried in order on this platform. macOS has only Metal, and no software adapter.
pub const LADDER: &[Mode] = if cfg!(target_os = "windows") {
    &[Mode::Default, Mode::Dx12, Mode::Software]
} else if cfg!(target_os = "macos") {
    &[Mode::Default]
} else {
    &[Mode::Default, Mode::Software]
};

/// The mode after `mode` in `ladder`, if any.
fn next_in(ladder: &[Mode], mode: Mode) -> Option<Mode> {
    let i = ladder.iter().position(|m| *m == mode)?;
    ladder.get(i.checked_add(1)?).copied()
}

/// [`STATE_FILE`]: the mode to start with and the failed starts in a row in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Saved {
    mode: Mode,
    failures: u32,
}

/// An attempt file left by a start: its process id (from the file name), mode and launch time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attempt {
    pid: u32,
    mode: Mode,
    /// Seconds since the Unix epoch.
    started: u64,
}

/// What a launch makes of the saved state and the attempt files it found.
#[derive(Debug, PartialEq, Eq)]
struct Decision {
    /// The state to start with and save.
    saved: Saved,
    /// Moved on to the next mode because the last starts failed.
    moved_on: bool,
    /// Per attempt: done with (counted, or not ours to count), delete it.
    delete: Vec<bool>,
}

/// This launch's mode from the saved state and the attempts other starts left. `now` is in Unix
/// seconds, `pid` is this process (an attempt with our process id is a dead process whose id was
/// reused), `window` is [`DEATH_WINDOW`] in seconds.
fn decide(saved: Option<Saved>, attempts: &[Attempt], now: u64, pid: u32, window: u64, ladder: &[Mode]) -> Decision {
    let first = ladder.first().copied().unwrap_or(Mode::Default);
    // Nothing saved, or a mode this platform does not use (a data folder copied from another OS).
    let saved = saved.filter(|s| ladder.contains(&s.mode)).unwrap_or(Saved { mode: first, failures: 0 });
    let mut failures = saved.failures;
    let mut delete = Vec::with_capacity(attempts.len());
    for a in attempts {
        if a.started > now.saturating_add(window) {
            // From the future (a clock that jumped back): unknown, and not worth keeping.
            delete.push(true);
            continue;
        }
        let dead = a.pid == pid || now.saturating_sub(a.started) >= window;
        // A younger attempt is another instance starting right now: leave it to that instance.
        delete.push(dead);
        // Only failures of the mode we would start in count; older ones are history.
        if dead && a.mode == saved.mode {
            failures = failures.saturating_add(1);
        }
    }
    if failures >= FAILURES_TO_MOVE_ON {
        return match next_in(ladder, saved.mode) {
            Some(next) => Decision { saved: Saved { mode: next, failures: 0 }, moved_on: true, delete },
            // Nothing left to try: the last mode is still the best chance.
            None => Decision { saved: Saved { mode: saved.mode, failures: 0 }, moved_on: false, delete },
        };
    }
    Decision { saved: Saved { mode: saved.mode, failures }, moved_on: false, delete }
}

fn read_small(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path).ok()?.take(MAX_FILE).read_to_string(&mut text).ok()?;
    Some(text)
}

/// [`Saved`] from its text; `None` for anything else. A missing failure count is none.
fn parse_state(text: &str) -> Option<Saved> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let mode = Mode::parse(v.get("mode")?.as_str()?)?;
    let failures = match v.get("failures") {
        None => 0,
        Some(n) => u32::try_from(n.as_u64()?).ok()?,
    };
    Some(Saved { mode, failures })
}

fn state_json(s: Saved) -> String {
    serde_json::json!({ "mode": s.mode.name(), "failures": s.failures }).to_string()
}

/// An attempt from its file name (`<pid>-<n>.json`) and text; `None` for anything else.
fn parse_attempt(file_name: &str, text: &str) -> Option<Attempt> {
    let pid = file_name.strip_suffix(".json")?.split_once('-')?.0.parse().ok()?;
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(Attempt { pid, mode: Mode::parse(v.get("mode")?.as_str()?)?, started: v.get("started")?.as_u64()? })
}

fn hung_json(started: u64) -> String {
    serde_json::json!({ "started": started }).to_string()
}

/// A [`HUNG_DIR`] marker from its file name (`<pid>.json`) and text: process id and launch time.
fn parse_hung(file_name: &str, text: &str) -> Option<(u32, u64)> {
    let pid = file_name.strip_suffix(".json")?.parse().ok()?;
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some((pid, v.get("started")?.as_u64()?))
}

fn attempt_json(mode: Mode, started: u64) -> String {
    serde_json::json!({ "mode": mode.name(), "started": started }).to_string()
}

/// Write `text` beside `path` (a name only this process uses) and rename it into place, so a
/// start that dies mid-write, or another instance reading, never sees half a file.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn remove_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => log::warn!("can't remove {}: {e}", path.display()),
        _ => {}
    }
}

/// The attempt files in `dir` (at most [`MAX_ATTEMPTS`]): path and contents, `None` when unreadable.
fn list_attempts(dir: &Path) -> Vec<(PathBuf, Option<Attempt>)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.ends_with(".json").then(|| {
                let path = e.path();
                let attempt = read_small(&path).and_then(|text| parse_attempt(&name, &text));
                (path, attempt)
            })
        })
        .take(MAX_ATTEMPTS)
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Attempt numbers within this process, so each attempt file has its own name.
static NEXT_ATTEMPT: AtomicU32 = AtomicU32::new(0);

/// This launch's graphics mode and its files in the data folder.
#[derive(Clone, Debug)]
pub struct GraphicsStart {
    /// [`STATE_FILE`]; `None` without a data folder or with `WGPU_BACKEND` set.
    state: Option<PathBuf>,
    /// This start's attempt file (with `state`).
    attempt: Option<PathBuf>,
    mode: Mode,
    /// The mode was picked with `--graphics`: a hang does not move on from it.
    forced: bool,
    /// [`Self::reset_to_default`] was used: a later [`Self::confirm`] must not save this mode over it.
    reset: Arc<AtomicBool>,
    /// The graphics device exists, or this start is over: the hang watchdog stands down.
    ready: Arc<AtomicBool>,
    /// The mode this process is starting in now, shared with the watchdog's clone (which a
    /// [`Self::step_down`] after it was made would otherwise not see).
    running: Arc<Mutex<Mode>>,
    /// This process, and its launch in Unix seconds: what a [`HUNG_DIR`] marker records.
    pid: u32,
    launched: u64,
}

impl GraphicsStart {
    /// Pick this launch's mode from the data folder and record the attempt. `forced`
    /// (`--graphics`) starts in that mode and saves it. `explicit_backend` (`WGPU_BACKEND` set)
    /// leaves the data folder alone and starts in `forced`, else [`Mode::Default`].
    pub fn begin(data_dir: Option<&Path>, explicit_backend: bool, forced: Option<Mode>) -> Self {
        let start = Self::begin_at(data_dir, explicit_backend, forced, unix_now(), std::process::id());
        if let Some(path) = start.attempt.clone() {
            expire_later(path);
        }
        start
    }

    /// [`Self::begin`] at `now` (Unix seconds) as process `pid`, without the expiry timer.
    fn begin_at(data_dir: Option<&Path>, explicit_backend: bool, forced: Option<Mode>, now: u64, pid: u32) -> Self {
        let first = LADDER.first().copied().unwrap_or(Mode::Default);
        let forced = forced.map(|m| if LADDER.contains(&m) { m } else { first });
        let reset = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let Some(dir) = data_dir.filter(|_| !explicit_backend) else {
            let mode = forced.unwrap_or(Mode::Default);
            let running = Arc::new(Mutex::new(mode));
            return Self { state: None, attempt: None, mode, forced: forced.is_some(), reset, ready, running, pid, launched: now };
        };
        let state = dir.join(STATE_FILE);
        let attempts_dir = dir.join(ATTEMPTS_DIR);
        let saved = read_small(&state).and_then(|t| parse_state(&t));
        let found = list_attempts(&attempts_dir);
        let known: Vec<Attempt> = found.iter().filter_map(|(_, a)| *a).collect();
        let decision = decide(saved, &known, now, pid, DEATH_WINDOW.as_secs(), LADDER);
        let mut to_delete = decision.delete.iter();
        for (path, attempt) in &found {
            // unreadable files in our folder are debris (a different version, a damaged disk)
            if attempt.is_none() || to_delete.next().copied().unwrap_or(false) {
                remove_quietly(path);
            }
        }
        let file = state.display();
        let next = match forced {
            Some(mode) => {
                log::info!("graphics mode {} chosen with --graphics", mode.name());
                Saved { mode, failures: 0 }
            }
            None => {
                if decision.moved_on {
                    let failed = saved.map_or(first, |s| s.mode).name();
                    log::warn!(
                        "the last {FAILURES_TO_MOVE_ON} starts with graphics mode {failed} died before their window painted; starting with {} (`--graphics default` or the notice in the window goes back)",
                        decision.saved.mode.name()
                    );
                } else if decision.saved.failures > 0 {
                    log::warn!("the last start with graphics mode {} died before its window painted", decision.saved.mode.name());
                } else if decision.saved.mode != Mode::Default {
                    log::info!("graphics mode {} after earlier failed starts ({file}; `--graphics default` goes back)", decision.saved.mode.name());
                }
                decision.saved
            }
        };
        if saved != Some(next)
            && let Err(e) = write_atomic(&state, &state_json(next))
        {
            log::warn!("can't write {file}: {e}");
        }
        let running = Arc::new(Mutex::new(next.mode));
        let start = Self { state: Some(state), attempt: None, mode: next.mode, forced: forced.is_some(), reset, ready, running, pid, launched: now };
        start.with_new_attempt(&attempts_dir, now, pid)
    }

    /// Record a new attempt file for the current mode.
    fn with_new_attempt(mut self, dir: &Path, now: u64, pid: u32) -> Self {
        let n = NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{pid}-{n}.json"));
        match write_atomic(&path, &attempt_json(self.mode, now)) {
            Ok(()) => self.attempt = Some(path),
            Err(e) => log::warn!("can't write {}: {e}", path.display()),
        }
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Running in a fallback mode, i.e. not the platform default.
    pub fn degraded(&self) -> bool {
        self.mode != Mode::Default
    }

    /// Move on to the next mode after a start-up error wgpu reported; `false` when none is left.
    /// The error is certain, so the next mode is saved at once.
    pub fn step_down(&mut self) -> bool {
        let Some(next) = next_in(LADDER, self.mode) else {
            return false;
        };
        log::warn!("graphics mode {} failed to start; trying {}", self.mode.name(), next.name());
        self.mode = next;
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) = next;
        if let Some(state) = self.state.clone() {
            self.save(&state, Saved { mode: next, failures: 0 });
            if let Some(old) = self.attempt.take() {
                remove_quietly(&old);
                if let Some(dir) = old.parent() {
                    let path = self.clone().with_new_attempt(dir, unix_now(), std::process::id()).attempt;
                    if let Some(path) = &path {
                        expire_later(path.clone());
                    }
                    self.attempt = path;
                }
            }
        }
        true
    }

    /// The window is up: keep this mode, and the failed starts in a row are over.
    pub fn confirm(&self) {
        self.finish();
        let Some(state) = &self.state else { return };
        if self.reset.load(Ordering::Relaxed) {
            return;
        }
        let ok = Saved { mode: self.mode, failures: 0 };
        if read_small(state).and_then(|t| parse_state(&t)) != Some(ok) {
            self.save(state, ok);
        }
    }

    /// This start is over, whether it painted or not (the window closed, or every mode failed
    /// with an error that was reported): its attempt no longer means a crash.
    pub fn finish(&self) {
        self.device_ready();
        if let Some(path) = &self.attempt {
            remove_quietly(path);
        }
    }

    /// The graphics device exists (eframe is creating the app): this start did not hang, however
    /// long the app itself then takes to open a project and paint.
    pub fn device_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }

    /// After [`HANG_TIMEOUT`], save the next mode for the next launch if the graphics device still
    /// isn't there (see the module docs), and say so in the crash log.
    pub fn watch_for_hang(&self) {
        let start = self.clone();
        let spawned = std::thread::Builder::new().name("graphics-watchdog".into()).spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                std::thread::sleep(HANG_TIMEOUT);
                if let Some(next) = start.hung() {
                    filmcraft_ui_egui::crash::record(&format!("the graphics start hung; the next start uses graphics mode {}", next.name()));
                }
            }));
        });
        if let Err(e) = spawned {
            log::warn!("can't start the graphics watchdog: {e}");
        }
    }

    /// The start has not created its graphics device in [`HANG_TIMEOUT`]: save the next mode and
    /// return it, or `None` when the device is there after all, or the mode is left alone (picked
    /// by hand, the last one, or no data folder to save it in).
    fn hung(&self) -> Option<Mode> {
        if self.ready.load(Ordering::Relaxed) {
            return None;
        }
        // stuck whatever the mode: the next launch ends this process
        if let Some(dir) = self.state.as_deref().and_then(Path::parent) {
            let marker = dir.join(HUNG_DIR).join(format!("{}.json", self.pid));
            if let Err(e) = write_atomic(&marker, &hung_json(self.launched)) {
                log::warn!("can't write {}: {e}", marker.display());
            }
        }
        let secs = HANG_TIMEOUT.as_secs();
        let mode = *self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let now = mode.name();
        let next = next_in(LADDER, mode).filter(|_| !self.forced && !self.reset.load(Ordering::Relaxed));
        let (Some(next), Some(state)) = (next, &self.state) else {
            log::error!("no window after {secs} s with graphics mode {now}; the graphics driver may be stuck (`--graphics <mode>` picks another)");
            return None;
        };
        log::error!(
            "no window after {secs} s with graphics mode {now}; the graphics driver seems stuck: the next start uses {} and ends this one",
            next.name()
        );
        self.save(state, Saved { mode: next, failures: 0 });
        Some(next)
    }

    /// End the starts an earlier watchdog found stuck in the graphics driver ([`HUNG_DIR`]) with
    /// `end(pid, launched)`, before this start loads the driver. `end` must only end a process
    /// that is still that start (`filmcraft_platform::stuck_process::end_stuck_process`). Each
    /// marker is used once, whatever `end` says.
    pub fn end_stuck_starts(&self, end: impl Fn(u32, u64) -> Result<bool, String>) {
        let Some(dir) = self.state.as_deref().and_then(Path::parent) else { return };
        let Ok(entries) = std::fs::read_dir(dir.join(HUNG_DIR)) else { return };
        for entry in entries.filter_map(Result::ok).take(MAX_ATTEMPTS) {
            let Ok(name) = entry.file_name().into_string() else { continue };
            if !name.ends_with(".json") {
                continue;
            }
            let path = entry.path();
            // our own id on a marker is a dead process whose id was reused
            if let Some((pid, launched)) = read_small(&path).and_then(|t| parse_hung(&name, &t)).filter(|(pid, _)| *pid != self.pid) {
                match end(pid, launched) {
                    Ok(true) => log::info!("ended FilmCraft process {pid}, stuck in the graphics driver since it started"),
                    Ok(false) => {}
                    Err(e) => log::warn!("can't end FilmCraft process {pid}, stuck in the graphics driver: {e}"),
                }
            }
            remove_quietly(&path);
        }
    }

    /// Start with the default mode next time (the notice's reset button). This process keeps
    /// its mode: the graphics device can't be swapped under a running window.
    pub fn reset_to_default(&self) -> Result<(), String> {
        self.reset.store(true, Ordering::Relaxed);
        let Some(state) = &self.state else { return Ok(()) };
        let first = LADDER.first().copied().unwrap_or(Mode::Default);
        write_atomic(state, &state_json(Saved { mode: first, failures: 0 })).map_err(|e| format!("can't write {}: {e}", state.display()))?;
        log::info!("graphics mode reset to {} for the next start", first.name());
        Ok(())
    }

    fn save(&self, path: &Path, s: Saved) {
        if let Err(e) = write_atomic(path, &state_json(s)) {
            log::warn!("can't write {}: {e}", path.display());
        }
    }
}

/// Remove the attempt file at `path` [`DEATH_WINDOW`] from now: a start still alive then is not
/// counted as a failed one, painted or not.
fn expire_later(path: PathBuf) {
    let spawned = std::thread::Builder::new().name("graphics-start".into()).spawn(move || {
        let _ = std::panic::catch_unwind(move || {
            std::thread::sleep(DEATH_WINDOW);
            remove_quietly(&path);
        });
    });
    if let Err(e) = spawned {
        log::warn!("can't start the graphics start timer: {e}");
    }
}

/// Configure `options` for `mode`, before eframe creates the wgpu instance. `backend_override`
/// is normally `Backends::from_env()`, i.e. `WGPU_BACKEND`.
pub fn apply(options: &mut NativeOptions, mode: Mode, backend_override: Option<Backends>) {
    apply_for(options, mode, backend_override, cfg!(target_os = "windows"));
}

fn apply_for(options: &mut NativeOptions, mode: Mode, backend_override: Option<Backends>, windows: bool) {
    let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup else {
        return;
    };
    // Nothing draws or dispatches indirectly, so wgpu's check of indirect arguments only costs a
    // compute shader when the device is created, one that Metal can't compile on an Intel Iris
    // Pro: the device is lost and the window never opens. `WGPU_VALIDATION_INDIRECT_CALL=1` turns
    // the check back on.
    create.instance_descriptor.flags = create.instance_descriptor.flags.difference(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL).with_env();
    match mode {
        // eframe's default (`WGPU_BACKEND`, else PRIMARY | GL), except on Windows: there it
        // includes GL. Creating that backend can crash inside AMD's atio6axx.dll before adapter
        // selection or any Rust error handling runs, so the window never appears. Choosing an
        // adapter afterwards is too late: only leaving GL out of the instance avoids it. Keep
        // Vulkan and DX12 both: forcing DX12 alone sends shaders through FXC (which rejects
        // fx.wgsl, silently falling back to the CPU) and leaves machines without DX12 with no
        // backend at all.
        Mode::Default if windows => {
            create.instance_descriptor.backends = backend_override.unwrap_or(Backends::PRIMARY);
            // wgpu's own choice of adapter, except for a GPU that Windows lists through two
            // versions of one driver: the first entry can be a stale one that crashes inside the
            // driver (`filmcraft_gpu::adapter`). A selector replaces `power_preference`
            // (`WGPU_POWER_PREF`), so it takes that along.
            let power = create.power_preference;
            create.native_adapter_selector =
                Some(Arc::new(move |adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| filmcraft_gpu::adapter::pick(adapters, surface, power)));
        }
        Mode::Default => {}
        // Without Vulkan, whose driver is what crashes; GPU effects may fall back to the CPU.
        Mode::Dx12 => create.instance_descriptor.backends = Backends::DX12,
        Mode::Software => {
            create.instance_descriptor.backends = software_backends(windows);
            create.native_adapter_selector = Some(Arc::new(pick_software));
        }
    }
}

/// The backends a software adapter is looked for in: WARP is a DX12 adapter, and GL stays out on
/// Windows for the reason in [`apply_for`].
fn software_backends(windows: bool) -> Backends {
    if windows { Backends::DX12 } else { Backends::VULKAN | Backends::GL }
}

/// The first software adapter that can draw to the window's surface.
fn pick_software(adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>) -> Result<wgpu::Adapter, String> {
    adapters
        .iter()
        .find(|a| filmcraft_ui_egui::software_rasterizer(&a.get_info()).is_some() && surface.is_none_or(|s| a.is_surface_supported(s)))
        .cloned()
        .ok_or_else(|| format!("no software graphics adapter (WARP, lavapipe, llvmpipe) among {} adapters", adapters.len()))
}

/// Whether `err` from `eframe::run_native` is a graphics start-up failure that another mode may
/// avoid (as opposed to a display, event-loop or app error).
pub fn is_graphics_failure(err: &eframe::Error) -> bool {
    matches!(err, eframe::Error::Wgpu(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: &[Mode] = &[Mode::Default, Mode::Dx12, Mode::Software];
    const LINUX: &[Mode] = &[Mode::Default, Mode::Software];
    const MACOS: &[Mode] = &[Mode::Default];

    fn configured(mode: Mode, backend_override: Option<Backends>, windows: bool) -> (Backends, bool) {
        let mut options = NativeOptions::default();
        apply_for(&mut options, mode, backend_override, windows);
        let WgpuSetup::CreateNew(create) = options.wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        (create.instance_descriptor.backends, create.native_adapter_selector.is_some())
    }

    #[test]
    fn no_mode_validates_indirect_calls() {
        if std::env::var_os("WGPU_VALIDATION_INDIRECT_CALL").is_some() {
            return;
        }
        for mode in [Mode::Default, Mode::Dx12, Mode::Software] {
            let mut options = NativeOptions::default();
            apply_for(&mut options, mode, None, false);
            let WgpuSetup::CreateNew(create) = options.wgpu_options.wgpu_setup else {
                panic!("default native options must create a wgpu instance");
            };
            // Intel Metal can't compile the validation's compute shader, and the device is lost.
            assert!(!create.instance_descriptor.flags.contains(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL), "{mode:?}");
        }
    }

    #[test]
    fn windows_default_leaves_out_gl() {
        // eframe's own default (PRIMARY | GL) would initialize OpenGL as well.
        let WgpuSetup::CreateNew(eframe_default) = NativeOptions::default().wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        assert!(eframe_default.instance_descriptor.backends.contains(Backends::GL));
        let (backends, selector) = configured(Mode::Default, None, true);
        assert_eq!(backends, Backends::PRIMARY);
        assert!(!backends.contains(Backends::GL));
        assert!(backends.contains(Backends::DX12) && backends.contains(Backends::VULKAN));
        // and chooses the adapter itself: a GPU listed through two versions of one driver
        assert!(selector);
    }

    #[test]
    fn only_the_windows_default_and_software_modes_choose_the_adapter() {
        for backend_override in [None, Some(Backends::VULKAN), Some(Backends::DX12)] {
            assert!(configured(Mode::Default, backend_override, true).1);
            // elsewhere, and in DirectX 12 only mode, the adapter is wgpu's own choice
            assert!(!configured(Mode::Default, backend_override, false).1);
            assert!(!configured(Mode::Dx12, backend_override, true).1);
        }
        assert!(configured(Mode::Software, None, true).1 && configured(Mode::Software, None, false).1);
    }

    #[test]
    fn explicit_backend_override_is_preserved() {
        for backends in [Backends::VULKAN, Backends::GL, Backends::DX12, Backends::VULKAN | Backends::DX12, Backends::empty()] {
            assert_eq!(configured(Mode::Default, Some(backends), true).0, backends);
        }
    }

    #[test]
    fn fallback_modes_leave_out_the_crashing_backends() {
        // DX12 only: Intel's and AMD's Vulkan drivers are what crashed (#512, #413).
        assert_eq!(configured(Mode::Dx12, None, true), (Backends::DX12, false));
        // Software on Windows is WARP (DX12), never GL (atio6axx.dll, #477).
        assert_eq!(configured(Mode::Software, None, true), (Backends::DX12, true));
        assert_eq!(configured(Mode::Software, None, false), (Backends::VULKAN | Backends::GL, true));
        // Off Windows the default is eframe's own.
        let WgpuSetup::CreateNew(eframe_default) = NativeOptions::default().wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        assert_eq!(configured(Mode::Default, None, false), (eframe_default.instance_descriptor.backends, false));
    }

    const W: u64 = 15;
    const NOW: u64 = 1_000_000;
    const ME: u32 = 7;

    fn saved(mode: Mode, failures: u32) -> Option<Saved> {
        Some(Saved { mode, failures })
    }

    fn dead(mode: Mode) -> Attempt {
        Attempt { pid: 1, mode, started: NOW - 100 }
    }

    fn mode_after(s: Option<Saved>, attempts: &[Attempt], ladder: &[Mode]) -> (Mode, u32, bool) {
        let d = decide(s, attempts, NOW, ME, W, ladder);
        (d.saved.mode, d.saved.failures, d.moved_on)
    }

    #[test]
    fn two_failed_starts_in_a_row_move_on_to_the_next_mode_and_keep_it() {
        // first launch
        assert_eq!(mode_after(None, &[], WINDOWS), (Mode::Default, 0, false));
        // one start died: counted, but the same mode is tried again
        assert_eq!(mode_after(None, &[dead(Mode::Default)], WINDOWS), (Mode::Default, 1, false));
        // the second in a row moves on: DX12, then software
        assert_eq!(mode_after(saved(Mode::Default, 1), &[dead(Mode::Default)], WINDOWS), (Mode::Dx12, 0, true));
        assert_eq!(mode_after(saved(Mode::Dx12, 1), &[dead(Mode::Dx12)], WINDOWS), (Mode::Software, 0, true));
        assert_eq!(mode_after(saved(Mode::Default, 1), &[dead(Mode::Default)], LINUX), (Mode::Software, 0, true));
        // two dead instances found at once count as two
        assert_eq!(mode_after(None, &[dead(Mode::Default), dead(Mode::Default)], WINDOWS), (Mode::Dx12, 0, true));
        // a saved mode is kept
        assert_eq!(mode_after(saved(Mode::Dx12, 0), &[], WINDOWS), (Mode::Dx12, 0, false));
        assert_eq!(mode_after(saved(Mode::Default, 1), &[], WINDOWS), (Mode::Default, 1, false));
        // nothing left to try: stay
        assert_eq!(mode_after(saved(Mode::Software, 1), &[dead(Mode::Software)], WINDOWS), (Mode::Software, 0, false));
        assert_eq!(mode_after(saved(Mode::Default, 5), &[dead(Mode::Default)], MACOS), (Mode::Default, 0, false));
        // a mode this platform has not: start over
        assert_eq!(mode_after(saved(Mode::Dx12, 1), &[dead(Mode::Dx12)], LINUX), (Mode::Default, 0, false));
        assert_eq!(mode_after(saved(Mode::Software, 0), &[], MACOS), (Mode::Default, 0, false));
        assert_eq!(mode_after(None, &[], &[]), (Mode::Default, 0, false));
        // a hostile count doesn't overflow
        assert_eq!(mode_after(saved(Mode::Default, u32::MAX), &[dead(Mode::Default)], WINDOWS), (Mode::Dx12, 0, true));
    }

    #[test]
    fn only_starts_that_died_soon_after_launch_in_the_current_mode_count() {
        let starting = Attempt { pid: 2, mode: Mode::Default, started: NOW - 3 };
        let just_now = Attempt { pid: 3, mode: Mode::Default, started: NOW };
        let future = Attempt { pid: 4, mode: Mode::Default, started: NOW + 3600 };
        let older_mode = dead(Mode::Dx12);
        let mine = Attempt { pid: ME, mode: Mode::Default, started: NOW - 1 };
        let d = decide(saved(Mode::Default, 0), &[starting, just_now, future, older_mode, mine], NOW, ME, W, WINDOWS);
        // another instance still starting is left alone; one from the future is dropped; a
        // failure in another mode is history; our own process id can only be a dead process
        assert_eq!(d.delete, [false, false, true, true, true]);
        assert_eq!((d.saved, d.moved_on), (Saved { mode: Mode::Default, failures: 1 }, false));
        // the clock a little behind another instance's: still starting
        let ahead = Attempt { pid: 5, mode: Mode::Default, started: NOW + 2 };
        assert_eq!(decide(None, &[ahead], NOW, ME, W, WINDOWS).delete, [false]);
    }

    #[test]
    fn the_files_round_trip_and_reject_anything_else() {
        for mode in WINDOWS {
            for failures in [0, 1, 2, u32::MAX] {
                assert_eq!(parse_state(&state_json(Saved { mode: *mode, failures })), saved(*mode, failures));
            }
            assert_eq!(parse_attempt("12-0.json", &attempt_json(*mode, NOW)), Some(Attempt { pid: 12, mode: *mode, started: NOW }));
        }
        // without a count (an earlier build of this change wrote `pending`): none
        assert_eq!(parse_state(r#"{"mode":"dx12","pending":true}"#), saved(Mode::Dx12, 0));
        for bad in [
            "",
            "{",
            "null",
            "[]",
            r#"{"mode":"vulkan","failures":1}"#,
            r#"{"failures":1}"#,
            r#"{"mode":1,"failures":1}"#,
            r#"{"mode":"dx12","failures":"one"}"#,
            r#"{"mode":"dx12","failures":-1}"#,
            r#"{"mode":"dx12","failures":4294967296}"#,
        ] {
            assert_eq!(parse_state(bad), None, "{bad}");
        }
        let good = attempt_json(Mode::Default, NOW);
        for (name, text) in [
            ("12-0.json", "{"),
            ("12-0.json", r#"{"mode":"default"}"#),
            ("12-0.json", r#"{"mode":"default","started":-5}"#),
            ("x-0.json", good.as_str()),
            ("12.json", good.as_str()),
            ("-0.json", good.as_str()),
            ("99999999999-0.json", good.as_str()),
            ("12-0.txt", good.as_str()),
        ] {
            assert_eq!(parse_attempt(name, text), None, "{name} {text}");
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("filmcraft-graphics-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn state_of(dir: &Path) -> Option<Saved> {
        read_small(&dir.join(STATE_FILE)).and_then(|t| parse_state(&t))
    }

    fn attempt_files(dir: &Path) -> usize {
        std::fs::read_dir(dir.join(ATTEMPTS_DIR)).map_or(0, |d| d.filter_map(Result::ok).count())
    }

    /// A launch as another process (`pid`) at `now`.
    fn launch(dir: &Path, pid: u32, now: u64) -> GraphicsStart {
        GraphicsStart::begin_at(Some(dir), false, None, now, pid)
    }

    #[test]
    fn a_start_that_paints_keeps_its_mode_and_clears_its_attempt() {
        let dir = temp_dir("paint");
        let start = launch(&dir, 10, NOW);
        assert_eq!(start.mode(), Mode::Default);
        assert!(!start.degraded());
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        assert_eq!(attempt_files(&dir), 1);
        start.confirm();
        assert_eq!(attempt_files(&dir), 0);
        // a window closed before it painted is no failure either
        let start = launch(&dir, 11, NOW + 60);
        start.finish();
        assert_eq!(attempt_files(&dir), 0);
        assert_eq!(launch(&dir, 12, NOW + 120).mode(), Mode::Default);
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_dead_starts_move_on_and_a_painted_one_resets_the_count() {
        let dir = temp_dir("dead");
        let second = LADDER.get(1).copied().unwrap_or(Mode::Default);
        // dies without painting
        let _ = launch(&dir, 20, NOW);
        // the next launch counts it, tries the same mode again and dies too
        let again = launch(&dir, 21, NOW + 60);
        assert_eq!(again.mode(), Mode::Default);
        assert_eq!(state_of(&dir), saved(Mode::Default, 1));
        // two in a row: the next mode, kept
        let moved = launch(&dir, 22, NOW + 120);
        assert_eq!(moved.mode(), second);
        assert_eq!(moved.degraded(), second != Mode::Default);
        moved.confirm();
        assert_eq!(launch(&dir, 23, NOW + 180).mode(), second);
        // one failure, then a start that paints: the count starts over
        let _ = std::fs::remove_dir_all(&dir);
        let _ = launch(&dir, 30, NOW);
        let painted = launch(&dir, 31, NOW + 60);
        painted.confirm();
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        let _ = launch(&dir, 32, NOW + 120);
        assert_eq!(launch(&dir, 33, NOW + 180).mode(), Mode::Default);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_launches_neither_count_nor_disturb_each_other() {
        let dir = temp_dir("race");
        // two instances start within the same seconds; neither has painted yet
        let a = launch(&dir, 40, NOW);
        let b = launch(&dir, 41, NOW + 1);
        let c = launch(&dir, 42, NOW + 2);
        assert_eq!((a.mode(), b.mode(), c.mode()), (Mode::Default, Mode::Default, Mode::Default));
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        assert_eq!(attempt_files(&dir), 3, "each instance has its own attempt file");
        b.confirm();
        a.confirm();
        c.confirm();
        assert_eq!(attempt_files(&dir), 0);
        assert_eq!(launch(&dir, 43, NOW + 60).mode(), Mode::Default);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_huge_or_foreign_files_start_with_the_default() {
        let dir = temp_dir("corrupt");
        let _ = std::fs::create_dir_all(dir.join(ATTEMPTS_DIR));
        let huge = format!(r#"{{"mode":"software","failures":1,"pad":"{}"}}"#, "x".repeat(10_000));
        for text in ["garbage", huge.as_str()] {
            let _ = std::fs::write(dir.join(STATE_FILE), text);
            let _ = std::fs::write(dir.join(ATTEMPTS_DIR).join("50-0.json"), text);
            let _ = std::fs::write(dir.join(ATTEMPTS_DIR).join("notes.txt"), text);
            let start = launch(&dir, 51, NOW);
            assert_eq!(start.mode(), Mode::Default);
            // replaced with a well-formed state; the unreadable attempt is gone, other files stay
            assert_eq!(state_of(&dir), saved(Mode::Default, 0));
            assert!(!dir.join(ATTEMPTS_DIR).join("50-0.json").exists());
            assert!(dir.join(ATTEMPTS_DIR).join("notes.txt").exists());
            start.finish();
        }
        // a data folder that is a file: nothing can be written, the default still starts
        let file = dir.join("not-a-folder");
        let _ = std::fs::write(&file, "x");
        let mut start = GraphicsStart::begin_at(Some(&file), false, None, NOW, 52);
        assert_eq!(start.mode(), Mode::Default);
        start.confirm();
        let _ = start.step_down();
        assert!(start.reset_to_default().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_attempt_files_are_bounded_per_launch() {
        let dir = temp_dir("many");
        let attempts = dir.join(ATTEMPTS_DIR);
        let _ = std::fs::create_dir_all(&attempts);
        let n = MAX_ATTEMPTS + 10;
        for pid in 0..n {
            let _ = std::fs::write(attempts.join(format!("{}-0.json", pid + 1000)), attempt_json(Mode::Software, NOW + 1));
        }
        // all of them belong to instances still starting: none is counted or removed
        let start = launch(&dir, 60, NOW);
        assert_eq!(start.mode(), Mode::Default);
        assert_eq!(attempt_files(&dir), n + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn graphics_flag_picks_the_mode_and_default_undoes_a_fallback() {
        let dir = temp_dir("forced");
        let last = LADDER.last().copied().unwrap_or(Mode::Default);
        let _ = write_atomic(&dir.join(STATE_FILE), &state_json(Saved { mode: last, failures: 1 }));
        launch(&dir, 70, NOW).confirm();
        // `--graphics default`
        let start = GraphicsStart::begin_at(Some(&dir), false, Some(Mode::Default), NOW + 60, 71);
        assert_eq!(start.mode(), Mode::Default);
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        start.confirm();
        assert_eq!(launch(&dir, 72, NOW + 120).mode(), Mode::Default);
        // `--graphics <the last mode>` is kept for the next launches
        GraphicsStart::begin_at(Some(&dir), false, Some(last), NOW + 180, 73).confirm();
        assert_eq!(launch(&dir, 74, NOW + 240).mode(), last);
        // a mode this platform has not starts with the first one
        if !LADDER.contains(&Mode::Dx12) {
            assert_eq!(GraphicsStart::begin_at(Some(&dir), false, Some(Mode::Dx12), NOW + 300, 75).mode(), Mode::Default);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_to_default_wins_over_a_later_confirm() {
        let dir = temp_dir("reset");
        let last = LADDER.last().copied().unwrap_or(Mode::Default);
        let _ = write_atomic(&dir.join(STATE_FILE), &state_json(Saved { mode: last, failures: 0 }));
        let start = launch(&dir, 80, NOW);
        assert_eq!(start.mode(), last);
        // the notice's button, from a clone of the start (the app holds one)
        assert_eq!(start.clone().reset_to_default(), Ok(()));
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        start.confirm();
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        assert_eq!(launch(&dir, 81, NOW + 60).mode(), Mode::Default);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_backend_leaves_the_data_folder_alone() {
        let dir = temp_dir("explicit");
        let _ = write_atomic(&dir.join(STATE_FILE), &state_json(Saved { mode: Mode::Software, failures: 1 }));
        let mut start = GraphicsStart::begin_at(Some(&dir), true, None, NOW, 90);
        assert_eq!(start.mode(), Mode::Default);
        start.confirm();
        let _ = start.step_down();
        assert_eq!(start.reset_to_default(), Ok(()));
        assert_eq!(state_of(&dir), saved(Mode::Software, 1));
        assert_eq!(attempt_files(&dir), 0);
        // with --graphics too, that mode starts, still without touching the folder
        let last = LADDER.last().copied().unwrap_or(Mode::Default);
        assert_eq!(GraphicsStart::begin_at(Some(&dir), true, Some(last), NOW, 91).mode(), last);
        assert_eq!(state_of(&dir), saved(Mode::Software, 1));
        // no data folder: nothing is written, the default still starts
        assert_eq!(GraphicsStart::begin_at(None, false, None, NOW, 92).mode(), Mode::Default);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn step_down_walks_the_ladder_and_saves_each_mode_at_once() {
        let dir = temp_dir("step");
        let mut start = launch(&dir, ME, NOW);
        for expected in LADDER.iter().skip(1) {
            assert!(start.step_down());
            assert_eq!(start.mode(), *expected);
            assert_eq!(state_of(&dir), saved(*expected, 0));
            assert_eq!(attempt_files(&dir), 1, "one attempt file per process");
        }
        assert!(!start.step_down(), "nothing after the last mode");
        start.finish();
        assert_eq!(attempt_files(&dir), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_start_that_hangs_moves_on_unless_the_mode_was_picked_by_hand() {
        let dir = temp_dir("hang");
        let second = LADDER.get(1).copied();
        // the device came up: no hang, however long the app then takes to paint
        let start = launch(&dir, 100, NOW);
        start.device_ready();
        assert_eq!(start.hung(), None);
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        start.finish();
        // stuck in the driver (#1061): the watchdog's clone saves the next mode at once, and the
        // start after it opens in that mode
        let stuck = launch(&dir, 101, NOW + 60);
        assert_eq!(stuck.clone().hung(), second);
        if let Some(next) = second {
            assert_eq!(state_of(&dir), saved(next, 0));
            assert_eq!(launch(&dir, 102, NOW + 120).mode(), next);
        }
        // a clone made before a reported error stepped down sees the mode the process is in now
        let _ = std::fs::remove_dir_all(&dir);
        let mut stepped = launch(&dir, 103, NOW);
        let watchdog = stepped.clone();
        if stepped.step_down() {
            assert_eq!(watchdog.hung(), next_in(LADDER, stepped.mode()));
        }
        // a mode picked by hand stays, and so does a start that has finished or was reset
        let _ = std::fs::remove_dir_all(&dir);
        let forced = GraphicsStart::begin_at(Some(&dir), false, Some(Mode::Default), NOW, 104);
        assert_eq!(forced.hung(), None);
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        let finished = launch(&dir, 105, NOW + 60);
        finished.finish();
        assert_eq!(finished.hung(), None);
        let reset = launch(&dir, 106, NOW + 120);
        assert_eq!(reset.reset_to_default(), Ok(()));
        assert_eq!(reset.hung(), None);
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        // `WGPU_BACKEND` set: the data folder is left alone
        let explicit = GraphicsStart::begin_at(Some(&dir), true, None, NOW + 180, 107);
        assert_eq!(explicit.hung(), None);
        assert_eq!(state_of(&dir), saved(Mode::Default, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stuck_start_is_marked_and_the_next_launch_ends_it_once() {
        let dir = temp_dir("stuck");
        let hung = |pid: u32| dir.join(HUNG_DIR).join(format!("{pid}.json")).exists();
        // stuck: marked with its launch time, whatever its mode
        let _ = launch(&dir, 200, NOW).hung();
        let _ = GraphicsStart::begin_at(Some(&dir), false, Some(Mode::Default), NOW + 1, 201).hung();
        assert!(hung(200) && hung(201));
        // a start that came up, or one with `WGPU_BACKEND` (data folder left alone): no marker
        let fine = launch(&dir, 202, NOW + 2);
        fine.device_ready();
        assert_eq!(fine.hung(), None);
        let _ = GraphicsStart::begin_at(Some(&dir), true, None, NOW + 3, 203).hung();
        assert!(!hung(202) && !hung(203));
        // debris, and a marker with the next launch's own id (a dead process whose id was reused)
        let _ = std::fs::write(dir.join(HUNG_DIR).join("junk.json"), "{");
        let _ = std::fs::write(dir.join(HUNG_DIR).join("204.json"), hung_json(NOW));
        let _ = std::fs::write(dir.join(HUNG_DIR).join("notes.txt"), "x");
        // the next launch ends the stuck ones it is given, with their launch times
        let next = launch(&dir, 204, NOW + 60);
        let asked = std::cell::RefCell::new(Vec::new());
        next.end_stuck_starts(|pid, launched| {
            asked.borrow_mut().push((pid, launched));
            if pid == 200 { Ok(true) } else { Err("access denied".into()) }
        });
        let mut asked = asked.into_inner();
        asked.sort_unstable();
        assert_eq!(asked, [(200, NOW), (201, NOW + 1)]);
        // every marker is used once, ended or not; other files stay
        assert!(!hung(200) && !hung(201) && !hung(204) && !dir.join(HUNG_DIR).join("junk.json").exists());
        assert!(dir.join(HUNG_DIR).join("notes.txt").exists());
        next.end_stuck_starts(|_, _| panic!("no markers left"));
        // without a data folder there is nothing to read
        GraphicsStart::begin_at(None, false, None, NOW, 205).end_stuck_starts(|_, _| panic!("no data folder"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markers_round_trip_and_reject_anything_else() {
        assert_eq!(parse_hung("12.json", &hung_json(NOW)), Some((12, NOW)));
        for (name, text) in [
            ("12.json", "{"),
            ("12.json", r#"{"started":-1}"#),
            ("x.json", r#"{"started":1}"#),
            ("12-0.json", r#"{"started":1}"#),
            ("12.txt", r#"{"started":1}"#),
        ] {
            assert_eq!(parse_hung(name, text), None, "{name} {text}");
        }
    }

    #[test]
    fn the_software_selector_only_picks_a_software_adapter() {
        if wgpu::Instance::enabled_backend_features().is_empty() {
            return;
        }
        // only the backends software mode uses: no driver this test loads is one the app avoids
        let backends = software_backends(cfg!(target_os = "windows"));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let adapters = pollster::block_on(instance.enumerate_adapters(backends));
        match pick_software(&adapters, None) {
            Ok(adapter) => assert!(filmcraft_ui_egui::software_rasterizer(&adapter.get_info()).is_some()),
            Err(why) => {
                assert!(adapters.iter().all(|a| filmcraft_ui_egui::software_rasterizer(&a.get_info()).is_none()), "{why}");
            }
        }
        assert!(pick_software(&[], None).is_err());
    }
}

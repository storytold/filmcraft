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
//! [`DEATH_WINDOW`] of its start without painting: a driver crash, typically. A start that hangs,
//! or is force-quit or crashes later, is not counted; neither is another instance that is still
//! starting (its attempt is younger than [`DEATH_WINDOW`] and is left alone). After
//! [`FAILURES_TO_MOVE_ON`] such failures in a row in the same mode, the launch moves on to the next
//! [`Mode`] of the platform's [`LADDER`] and keeps it; a start that paints resets the count. A
//! start-up error that wgpu does report (no adapter, device creation failed) moves on at once,
//! within the same process.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
/// How long after launch a start that has not painted may die and still count as failed. A
/// driver crash in instance or device creation happens within a second or two.
pub const DEATH_WINDOW: Duration = Duration::from_secs(15);
/// Failed starts in a row, in the same mode, that move on to the next mode.
pub const FAILURES_TO_MOVE_ON: u32 = 2;
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
    /// [`Self::reset_to_default`] was used: a later [`Self::confirm`] must not save this mode over it.
    reset: Arc<AtomicBool>,
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
        let Some(dir) = data_dir.filter(|_| !explicit_backend) else {
            return Self { state: None, attempt: None, mode: forced.unwrap_or(Mode::Default), reset };
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
        let start = Self { state: Some(state), attempt: None, mode: next.mode, reset };
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
        if let Some(path) = &self.attempt {
            remove_quietly(path);
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
mod tests;

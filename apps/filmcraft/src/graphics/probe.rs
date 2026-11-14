//! A test start of the graphics device in a child process, before this process loads any
//! graphics driver (Windows; #1061, #2634, #1038).
//!
//! A driver can hang inside wgpu's instance, adapter or device creation instead of failing. The
//! process then lives on with no window and nothing reported, and the start-up ladder in
//! `graphics.rs`, which counts starts that died, never moves on: every launch hangs the same way.
//! No thread of a process stuck like that can reliably report it or end it, since the driver
//! holds the Windows loader lock. So the test runs in a child process (this program started with
//! [`CHILD_ARG`]). The child does what eframe will do with the mode's settings: create the
//! instance, list the adapters, choose one and create the device, and it writes a line on its
//! standard output as each step begins. A child that has not finished within [`TIMEOUT`] is
//! ended by this process, which started it and holds its handle, and the next mode is tried. The
//! window then starts in the first mode that passed, with the fallback notice. When none passes,
//! the user gets a message naming each mode, where it stopped and on which adapter, instead of a
//! FilmCraft that never opens.
//!
//! The test runs only when something changed since the last start ([`TEST_FILE`]): another
//! FilmCraft version, another mode, other display drivers, or a last start that never got its
//! window and never came back. Other launches start directly. A start that passed its test, or
//! skipped it, but neither painted nor returned within [`HUNG_AFTER`] is taken as hung in the
//! window itself, and the next launch moves on from its mode.

use std::ffi::OsString;
use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::{egui_wgpu::WgpuSetup, wgpu};

use super::{GraphicsStart, LADDER, Mode, next_untried, read_small, unix_now, write_atomic};

/// The first argument of the test-start child: `filmcraft --test-graphics-start <mode>`.
pub const CHILD_ARG: &str = "--test-graphics-start";
/// How long a test start may take. The slowest working start seen took about 9 s from launch to
/// an adapter on a cold start (#1808); a hung one never gets there.
pub const TIMEOUT: Duration = Duration::from_secs(25);
/// The record of the last start, in the data folder.
pub const TEST_FILE: &str = "graphics-test.json";
/// A start still without its window after this long is taken as hung, not slow.
pub const HUNG_AFTER: Duration = Duration::from_secs(60);
/// How long an ended child gets to go before it is left behind.
const END_GRACE: Duration = Duration::from_secs(3);
/// How often a running child is looked at.
const POLL: Duration = Duration::from_millis(50);
/// Lines read from a child; it writes five.
const MAX_LINES: usize = 32;
/// Adapters named in a message.
const MAX_ADAPTERS: usize = 8;

/// The step a test start was at: what each line from the child says it begins.
fn stage_text(stage: &str) -> &'static str {
    match stage {
        "instance" => "loading the graphics driver",
        "adapters" => "listing the graphics adapters",
        "adapter" => "choosing a graphics adapter",
        "device" => "creating the graphics device",
        _ => "starting",
    }
}

/// A short description of an adapter: name, backend and driver.
fn describe(info: &wgpu::AdapterInfo) -> String {
    let driver = [info.driver.trim(), info.driver_info.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    if driver.is_empty() { format!("{} ({:?})", info.name, info.backend) } else { format!("{} ({:?}, {driver})", info.name, info.backend) }
}

// ----------------------------------------------------------------------------------- the child

/// In the test-start child (`args` without the program name begin with [`CHILD_ARG`]): run the
/// test with the wgpu setup `setup` makes for the mode, and return the exit code. `None` in any
/// other launch.
pub fn child_main(args: &[OsString], setup: impl Fn(Mode) -> WgpuSetup) -> Option<i32> {
    let mut args = args.iter();
    if args.next()?.to_str()? != CHILD_ARG {
        return None;
    }
    let Some(mode) = args.next().and_then(|m| m.to_str()).and_then(Mode::parse) else {
        say("error", serde_json::json!({ "error": "no graphics mode given" }));
        return Some(2);
    };
    Some(run_child(setup(mode)))
}

/// Write one line for the parent and flush it, so it arrives even if the next step hangs.
fn say(stage: &str, extra: serde_json::Value) {
    let mut line = serde_json::json!({ "stage": stage });
    if let (Some(line), serde_json::Value::Object(extra)) = (line.as_object_mut(), extra) {
        line.extend(extra);
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn run_child(setup: WgpuSetup) -> i32 {
    let WgpuSetup::CreateNew(create) = &setup else {
        say("error", serde_json::json!({ "error": "the graphics setup does not create a device" }));
        return 1;
    };
    say("instance", serde_json::json!({}));
    let instance = pollster::block_on(setup.new_instance());
    say("adapters", serde_json::json!({}));
    let adapters = pollster::block_on(instance.enumerate_adapters(create.instance_descriptor.backends));
    let names: Vec<String> = adapters.iter().map(|a| describe(&a.get_info())).collect();
    say("adapter", serde_json::json!({ "adapters": names }));
    let adapter = match &create.native_adapter_selector {
        Some(select) => select(&adapters, None),
        None => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: create.power_preference,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| e.to_string()),
    };
    let adapter = match adapter {
        Ok(a) => a,
        Err(e) => {
            say("error", serde_json::json!({ "error": e }));
            return 1;
        }
    };
    say("device", serde_json::json!({ "chosen": describe(&adapter.get_info()) }));
    match pollster::block_on(adapter.request_device(&(create.device_descriptor)(&adapter))) {
        Ok(_) => {
            say("done", serde_json::json!({}));
            0
        }
        Err(e) => {
            say("error", serde_json::json!({ "error": e.to_string() }));
            1
        }
    }
}

// ---------------------------------------------------------------------------------- the parent

/// What a child reported, line by line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Report {
    /// The last step it began.
    stage: Option<String>,
    /// The adapters it listed.
    adapters: Vec<String>,
    /// The adapter it chose.
    chosen: Option<String>,
    /// The error it reported.
    error: Option<String>,
    /// It created the device.
    done: bool,
}

impl Report {
    fn take(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let text = |key: &str| v.get(key).and_then(|s| s.as_str()).map(str::to_string);
        if let Some(adapters) = v.get("adapters").and_then(|a| a.as_array()) {
            self.adapters = adapters.iter().filter_map(|a| a.as_str().map(str::to_string)).take(MAX_ADAPTERS).collect();
        }
        if let Some(chosen) = text("chosen") {
            self.chosen = Some(chosen);
        }
        match text("stage").as_deref() {
            Some("done") => self.done = true,
            Some("error") => self.error = text("error").or_else(|| Some("unknown error".to_string())),
            Some(stage) => self.stage = Some(stage.to_string()),
            None => {}
        }
    }
}

/// How a test start ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Passed(Report),
    /// It reported an error.
    Failed(Report),
    /// It was still running after the timeout (seconds) and was ended.
    Hung(Report, u64),
    /// It ended without a result: a crash inside the driver, typically.
    Died(Report, Option<i32>),
    /// The child could not be started; nothing is known about the mode.
    NotRun(String),
}

/// Run `cmd` as a test start, reading its lines, and end it after `timeout`.
fn run_test(mut cmd: Command, timeout: Duration) -> Outcome {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: no console window flashes up for the child.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::NotRun(e.to_string()),
    };
    let report = Arc::new(Mutex::new(Report::default()));
    if let Some(out) = child.stdout.take() {
        let report = Arc::clone(&report);
        let read = move || {
            for line in std::io::BufReader::new(out).lines().take(MAX_LINES) {
                let Ok(line) = line else { break };
                if let Ok(mut r) = report.lock() {
                    r.take(&line);
                }
            }
        };
        if let Err(e) = std::thread::Builder::new().name("graphics-test".into()).spawn(read) {
            log::warn!("can't read the graphics test start: {e}");
        }
    }
    let snapshot = |report: &Arc<Mutex<Report>>| report.lock().map(|r| r.clone()).unwrap_or_default();
    let began = Instant::now();
    loop {
        let r = snapshot(&report);
        if r.done {
            // The device exists: the mode works, however long the child takes to go.
            end(&mut child);
            return Outcome::Passed(r);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Let the reader take the last lines.
                std::thread::sleep(POLL);
                let r = snapshot(&report);
                return if r.done {
                    Outcome::Passed(r)
                } else if r.error.is_some() {
                    Outcome::Failed(r)
                } else {
                    Outcome::Died(r, status.code())
                };
            }
            Ok(None) => {}
            Err(e) => {
                end(&mut child);
                return Outcome::NotRun(e.to_string());
            }
        }
        if began.elapsed() >= timeout {
            end(&mut child);
            return Outcome::Hung(snapshot(&report), timeout.as_secs());
        }
        std::thread::sleep(POLL);
    }
}

/// End `child` and give it [`END_GRACE`] to go; one stuck in the kernel is left behind.
fn end(child: &mut Child) {
    let _ = child.kill();
    let began = Instant::now();
    while began.elapsed() < END_GRACE {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(POLL);
    }
    log::warn!("the graphics test start (process {}) did not end", child.id());
}

/// The text for a mode that did not pass: what happened, at which step, on which adapter.
fn outcome_text(outcome: &Outcome) -> String {
    let at = |r: &Report| {
        let step = stage_text(r.stage.as_deref().unwrap_or(""));
        match &r.chosen {
            Some(adapter) => format!("{step} on {adapter}"),
            None if !r.adapters.is_empty() => format!("{step} (adapters: {})", r.adapters.join("; ")),
            None => step.to_string(),
        }
    };
    match outcome {
        Outcome::Passed(_) => "passed".to_string(),
        Outcome::Failed(r) => {
            let error = r.error.as_deref().unwrap_or("unknown error");
            if r.adapters.is_empty() { error.to_string() } else { format!("{error} (adapters: {})", r.adapters.join("; ")) }
        }
        Outcome::Hung(r, secs) => format!("stopped responding while {}, and was ended after {secs} s", at(r)),
        Outcome::Died(r, code) => {
            let code = code.map_or_else(|| "no exit code".to_string(), |c| format!("exit code {c}"));
            format!("ended unexpectedly while {} ({code})", at(r))
        }
        Outcome::NotRun(e) => format!("the test start could not run: {e}"),
    }
}

// ---------------------------------------------------------------------------- when to test

/// [`TEST_FILE`]: the last start's version, mode and display drivers, when it began, and whether
/// it is still open (began without painting or returning yet).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    version: String,
    mode: Mode,
    drivers: String,
    started: u64,
    open: bool,
}

fn parse_record(text: &str) -> Option<Record> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(Record {
        version: v.get("version")?.as_str()?.to_string(),
        mode: Mode::parse(v.get("mode")?.as_str()?)?,
        drivers: v.get("drivers")?.as_str()?.to_string(),
        started: v.get("started")?.as_u64()?,
        open: v.get("open")?.as_bool()?,
    })
}

fn record_json(r: &Record) -> String {
    serde_json::json!({ "version": r.version, "mode": r.mode.name(), "drivers": r.drivers, "started": r.started, "open": r.open }).to_string()
}

/// Whether this launch tests its mode first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    /// Nothing changed since a start that worked: start directly.
    No,
    /// Something changed, or nothing is known: test.
    Test,
    /// The last start in this mode never got its window and never came back: move on from it.
    LastHung,
}

fn need(record: Option<&Record>, mode: Mode, version: &str, drivers: &str, now: u64) -> Need {
    let Some(r) = record else { return Need::Test };
    if r.version != version || r.mode != mode || r.drivers != drivers {
        return Need::Test;
    }
    if r.open {
        // Another FilmCraft may still be starting: only a start this old counts as hung.
        return if now.saturating_sub(r.started) >= HUNG_AFTER.as_secs() { Need::LastHung } else { Need::Test };
    }
    Need::No
}

/// The modes to test, in order: `current` first (unless `skip_current`), then the others as the
/// ladder's fallback goes ([`next_untried`]). `forced` (`--graphics`) tests that mode only.
fn candidates(ladder: &[Mode], current: Mode, skip_current: bool, forced: bool) -> Vec<Mode> {
    if forced {
        return vec![current];
    }
    // `current` counts as tried either way, so a skipped one doesn't come back at the end.
    let mut tried = vec![current];
    let mut order = if skip_current { Vec::new() } else { vec![current] };
    let mut at = current;
    while let Some(next) = next_untried(ladder, at, &tried) {
        tried.push(next);
        order.push(next);
        at = next;
    }
    order
}

/// The installed display drivers, as one text that changes when any of them does: names and
/// versions from the registry's display adapter class. Empty where unknown.
#[cfg(windows)]
fn display_drivers() -> String {
    use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
    const CLASS: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let Ok(class) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(CLASS) else { return String::new() };
    let mut drivers: Vec<String> = class
        .enum_keys()
        .take(64)
        .filter_map(Result::ok)
        .filter(|k| k.len() == 4 && k.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|k| {
            let key = class.open_subkey(&k).ok()?;
            let desc: String = key.get_value("DriverDesc").ok()?;
            let version: String = key.get_value("DriverVersion").unwrap_or_default();
            Some(format!("{desc} {version}"))
        })
        .collect();
    drivers.sort();
    drivers.join("; ")
}

#[cfg(not(windows))]
fn display_drivers() -> String {
    String::new()
}

/// Write [`TEST_FILE`] for a start in `mode` that begins now and is open until it paints or ends.
fn open_record(dir: &Path, mode: Mode, drivers: &str) {
    let r = Record { version: env!("CARGO_PKG_VERSION").to_string(), mode, drivers: drivers.to_string(), started: unix_now(), open: true };
    let path = dir.join(TEST_FILE);
    if let Err(e) = write_atomic(&path, &record_json(&r)) {
        log::warn!("can't write {}: {e}", path.display());
    }
}

/// This start painted its window or ended: it was not a hang ([`GraphicsStart::finish`]).
pub(super) fn close_record(dir: &Path, mode: Mode) {
    let path = dir.join(TEST_FILE);
    let Some(mut r) = read_small(&path).and_then(|t| parse_record(&t)) else { return };
    if !r.open || r.mode != mode {
        return;
    }
    r.open = false;
    if let Err(e) = write_atomic(&path, &record_json(&r)) {
        log::warn!("can't write {}: {e}", path.display());
    }
}

// ------------------------------------------------------------------------------- the choice

/// Before this process loads a graphics driver (Windows), test `start`'s mode in a child process
/// when [`need`] says so, and move `start` to the first mode that passes. `forced`
/// (`--graphics`) tests only that mode; `explicit_backend` (`WGPU_BACKEND`) skips the test, as the
/// user is in charge. `Err` is the message for the user when no mode passed; each failure is also
/// in `start`'s failures.
pub fn choose(start: &mut GraphicsStart, forced: bool, explicit_backend: bool, log_file: Option<&Path>) -> Result<(), String> {
    if !cfg!(windows) || explicit_backend {
        return Ok(());
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log::warn!("no graphics test start: {e}");
            return Ok(());
        }
    };
    choose_with(start, forced, log_file, &exe, &display_drivers(), |exe, mode| {
        let mut cmd = Command::new(exe);
        cmd.arg(CHILD_ARG).arg(mode.name());
        run_test(cmd, TIMEOUT)
    })
}

/// [`choose`] for the program `exe` with the display drivers `drivers`, testing a mode with
/// `test`, on any platform.
fn choose_with(
    start: &mut GraphicsStart,
    forced: bool,
    log_file: Option<&Path>,
    exe: &Path,
    drivers: &str,
    test: impl Fn(&Path, Mode) -> Outcome,
) -> Result<(), String> {
    let Some(dir) = start.folder().map(Path::to_path_buf) else { return Ok(()) };
    let drivers = drivers.to_string();
    let record = read_small(&dir.join(TEST_FILE)).and_then(|t| parse_record(&t));
    let needed = need(record.as_ref(), start.mode(), env!("CARGO_PKG_VERSION"), &drivers, unix_now());
    if needed == Need::No {
        open_record(&dir, start.mode(), &drivers);
        return Ok(());
    }
    let skip_current = needed == Need::LastHung && !forced;
    if skip_current {
        let hung = "the last start passed its graphics test but never showed its window";
        log::warn!("graphics mode {}: {hung}", start.mode().name());
        start.add_failure(start.mode(), hung);
    }
    let modes = candidates(LADDER, start.mode(), skip_current, forced);
    let result = test_modes(exe, start, &modes, &dir, &drivers, test);
    result.map_err(|()| {
        let what = if forced { "the chosen graphics mode did not start in a test" } else { "no graphics mode started in a test" };
        super::failure_message(what, start.failed(), log_file)
    })
}

/// Test `modes` in order with `test` and start in the first that passes; `Err` when none did.
fn test_modes(exe: &Path, start: &mut GraphicsStart, modes: &[Mode], dir: &Path, drivers: &str, test: impl Fn(&Path, Mode) -> Outcome) -> Result<(), ()> {
    for &mode in modes {
        log::info!("graphics test start: {}", mode.name());
        let outcome = test(exe, mode);
        match &outcome {
            Outcome::Passed(r) => {
                log::info!("graphics mode {} passed its test start{}", mode.name(), r.chosen.as_deref().map(|a| format!(" on {a}")).unwrap_or_default());
            }
            // The test itself could not run: start as before rather than not at all.
            Outcome::NotRun(e) => log::warn!("graphics test start for {} could not run: {e}; starting without it", mode.name()),
            other => {
                let text = outcome_text(other);
                log::warn!("graphics mode {}: {text}", mode.name());
                start.add_failure(mode, &text);
                continue;
            }
        }
        if mode != start.mode() {
            log::warn!("starting with graphics mode {} after its test start (`--graphics default` or the notice in the window goes back)", mode.name());
            start.switch_to(mode);
        }
        open_record(dir, mode, drivers);
        return Ok(());
    }
    Err(())
}

#[cfg(test)]
mod tests;

use super::*;
use std::path::PathBuf;

const WINDOWS: &[Mode] = &[Mode::Default, Mode::Dx12, Mode::Software];
const LINUX: &[Mode] = &[Mode::Default, Mode::Software];
const V: &str = "1.2.3";
const NOW: u64 = 1_000_000;

fn record(mode: Mode, open: bool, started: u64) -> Record {
    Record { version: V.to_string(), mode, drivers: "GPU 1.0".to_string(), started, open }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filmcraft-graphics-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[test]
fn a_start_tests_its_mode_when_anything_changed_or_the_last_start_hung() {
    let ok = record(Mode::Default, false, NOW - 3600);
    assert_eq!(need(None, Mode::Default, V, "GPU 1.0", NOW), Need::Test, "nothing known yet");
    assert_eq!(need(Some(&ok), Mode::Default, V, "GPU 1.0", NOW), Need::No, "nothing changed");
    assert_eq!(need(Some(&ok), Mode::Default, "1.2.4", "GPU 1.0", NOW), Need::Test, "another version");
    assert_eq!(need(Some(&ok), Mode::Dx12, V, "GPU 1.0", NOW), Need::Test, "another mode");
    assert_eq!(need(Some(&ok), Mode::Default, V, "GPU 1.1", NOW), Need::Test, "a driver update");
    // A start that never painted nor came back: hung once it is old enough, else maybe still starting.
    let open = record(Mode::Default, true, NOW - HUNG_AFTER.as_secs());
    assert_eq!(need(Some(&open), Mode::Default, V, "GPU 1.0", NOW), Need::LastHung);
    let young = record(Mode::Default, true, NOW - 5);
    assert_eq!(need(Some(&young), Mode::Default, V, "GPU 1.0", NOW), Need::Test);
}

#[test]
fn the_modes_tested_follow_the_fallback_and_skip_one_that_hung() {
    assert_eq!(candidates(WINDOWS, Mode::Default, false, false), vec![Mode::Default, Mode::Dx12, Mode::Software]);
    assert_eq!(candidates(WINDOWS, Mode::Default, true, false), vec![Mode::Dx12, Mode::Software], "the hung mode does not come back");
    assert_eq!(candidates(WINDOWS, Mode::Software, false, false), vec![Mode::Software, Mode::Default, Mode::Dx12]);
    assert_eq!(candidates(WINDOWS, Mode::Dx12, true, false), vec![Mode::Software, Mode::Default]);
    assert_eq!(candidates(LINUX, Mode::Default, false, false), vec![Mode::Default, Mode::Software]);
    // `--graphics`: only that mode
    assert_eq!(candidates(WINDOWS, Mode::Dx12, false, true), vec![Mode::Dx12]);
}

#[test]
fn the_report_follows_the_childs_lines() {
    let mut r = Report::default();
    for line in [
        r#"{"stage":"instance"}"#,
        "not json",
        r#"{"stage":"adapters"}"#,
        r#"{"stage":"adapter","adapters":["NVIDIA GeForce RTX 3060 (Vulkan, NVIDIA 617.14)","Microsoft Basic Render Driver (Dx12)"]}"#,
        r#"{"stage":"device","chosen":"NVIDIA GeForce RTX 3060 (Vulkan, NVIDIA 617.14)"}"#,
    ] {
        r.take(line);
    }
    assert_eq!(r.stage.as_deref(), Some("device"));
    assert_eq!(r.adapters.len(), 2);
    assert_eq!(r.chosen.as_deref(), Some("NVIDIA GeForce RTX 3060 (Vulkan, NVIDIA 617.14)"));
    assert!(!r.done && r.error.is_none());
    let text = outcome_text(&Outcome::Hung(r.clone(), 25));
    assert_eq!(text, "stopped responding while creating the graphics device on NVIDIA GeForce RTX 3060 (Vulkan, NVIDIA 617.14), and was ended after 25 s");
    r.take(r#"{"stage":"done"}"#);
    assert!(r.done);
    let mut failed = Report::default();
    failed.take(r#"{"stage":"adapter","adapters":["llvmpipe (Vulkan)"]}"#);
    failed.take(r#"{"stage":"error","error":"no software graphics adapter"}"#);
    assert_eq!(outcome_text(&Outcome::Failed(failed)), "no software graphics adapter (adapters: llvmpipe (Vulkan))");
    let died = Report { stage: Some("instance".to_string()), ..Report::default() };
    assert_eq!(outcome_text(&Outcome::Died(died, Some(-1073741819))), "ended unexpectedly while loading the graphics driver (exit code -1073741819)");
}

#[test]
fn only_the_childs_own_argument_runs_a_test_start() {
    let none: Vec<OsString> = Vec::new();
    let setup = |_| WgpuSetup::without_display_handle();
    assert_eq!(child_main(&none, setup), None);
    assert_eq!(child_main(&[OsString::from("--demo")], setup), None);
    assert_eq!(child_main(&[OsString::from(CHILD_ARG), OsString::from("vulkan")], setup), Some(2), "an unknown mode");
}

/// A command that writes `lines` and exits with `code`.
#[cfg(windows)]
fn lines_then_exit(lines: &[&str], code: i32) -> Command {
    use std::os::windows::process::CommandExt as _;
    // cmd's `echo` prints the rest of its command verbatim, quotes included.
    let script: String = lines.iter().map(|l| format!("echo {l}&")).collect();
    let mut cmd = Command::new("cmd");
    cmd.raw_arg(format!("/C {script}exit {code}"));
    cmd
}

/// A command that writes `lines` and exits with `code`.
#[cfg(not(windows))]
fn lines_then_exit(lines: &[&str], code: i32) -> Command {
    let script: String = lines.iter().map(|l| format!("printf '%s\\n' '{l}'; ")).collect();
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!("{script}exit {code}"));
    cmd
}

/// A command that runs for `secs` seconds without writing anything.
fn hangs_for(secs: u32) -> Command {
    let mut cmd = if cfg!(windows) { Command::new("ping") } else { Command::new("sleep") };
    if cfg!(windows) {
        cmd.args(["-n", &(secs + 1).to_string(), "127.0.0.1"]);
    } else {
        cmd.arg(secs.to_string());
    }
    cmd
}

#[test]
fn a_test_start_that_hangs_is_ended_after_the_timeout() {
    let began = Instant::now();
    let outcome = run_test(hangs_for(30), Duration::from_secs(1));
    assert!(matches!(outcome, Outcome::Hung(_, 1)), "{outcome:?}");
    assert!(began.elapsed() < Duration::from_secs(15), "ended, not waited for: {:?}", began.elapsed());
}

#[test]
fn a_test_start_passes_fails_or_dies_as_its_lines_and_exit_say() {
    let passed = run_test(lines_then_exit(&[r#"{"stage":"instance"}"#, r#"{"stage":"done"}"#], 0), Duration::from_secs(20));
    assert!(matches!(passed, Outcome::Passed(_)), "{passed:?}");
    let failed = run_test(lines_then_exit(&[r#"{"stage":"error","error":"no adapter"}"#], 1), Duration::from_secs(20));
    assert!(matches!(&failed, Outcome::Failed(r) if r.error.as_deref() == Some("no adapter")), "{failed:?}");
    let died = run_test(lines_then_exit(&[r#"{"stage":"device"}"#], 3), Duration::from_secs(20));
    assert!(matches!(&died, Outcome::Died(r, Some(3)) if r.stage.as_deref() == Some("device")), "{died:?}");
    let missing = run_test(Command::new("filmcraft-no-such-program-for-the-test"), Duration::from_secs(20));
    assert!(matches!(missing, Outcome::NotRun(_)), "{missing:?}");
}

#[test]
fn a_hung_mode_moves_on_to_one_that_passes_and_the_record_closes_when_the_start_ends() {
    let dir = temp_dir("move-on");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 7);
    let first = start.mode();
    let modes = candidates(LADDER, first, false, false);
    let passed = test_modes(Path::new("filmcraft"), &mut start, &modes, &dir, "GPU 1.0", |_, mode| {
        if mode == first { Outcome::Hung(Report { stage: Some("device".to_string()), ..Report::default() }, 25) } else { Outcome::Passed(Report::default()) }
    });
    if LADDER.len() > 1 {
        assert_eq!(passed, Ok(()));
        assert_ne!(start.mode(), first, "moved on from the mode that hung");
        assert_eq!(start.failed().len(), 1);
        assert!(start.failed()[0].1.starts_with("stopped responding while creating the graphics device"), "{:?}", start.failed());
        let saved = read_small(&dir.join(TEST_FILE)).and_then(|t| parse_record(&t));
        assert_eq!(saved.as_ref().map(|r| (r.mode, r.open)), Some((start.mode(), true)), "open until the start paints or ends");
        start.finish();
        let closed = read_small(&dir.join(TEST_FILE)).and_then(|t| parse_record(&t));
        assert_eq!(closed.map(|r| r.open), Some(false));
    } else {
        assert_eq!(passed, Err(()), "nothing to move on to");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_mode_passing_ends_in_a_message_naming_each_mode() {
    let dir = temp_dir("none");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 8);
    let modes = candidates(LADDER, start.mode(), false, false);
    let result = test_modes(Path::new("filmcraft"), &mut start, &modes, &dir, "", |_, _| {
        Outcome::Hung(Report { stage: Some("instance".to_string()), ..Report::default() }, 25)
    });
    assert_eq!(result, Err(()));
    assert_eq!(start.failed().len(), modes.len());
    let msg = crate::graphics::failure_message("no graphics mode started in a test", start.failed(), None);
    for mode in &modes {
        assert!(msg.contains(mode.label()), "{msg}");
    }
    assert!(msg.contains("stopped responding while loading the graphics driver"), "{msg}");
    start.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_test_start_that_cannot_run_starts_as_before() {
    let dir = temp_dir("not-run");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 9);
    let first = start.mode();
    let result = test_modes(Path::new("filmcraft"), &mut start, &[first], &dir, "", |_, _| Outcome::NotRun("access denied".to_string()));
    assert_eq!(result, Ok(()));
    assert_eq!(start.mode(), first);
    assert!(start.failed().is_empty());
    start.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_record_reads_back_and_rejects_anything_else() {
    let r = record(Mode::Dx12, true, NOW);
    assert_eq!(parse_record(&record_json(&r)), Some(r));
    assert_eq!(parse_record("{}"), None);
    assert_eq!(parse_record(r#"{"version":"1","mode":"vulkan","drivers":"","started":1,"open":true}"#), None);
    assert_eq!(parse_record("not json"), None);
}

/// The display drivers are read without failing; their text is whatever the machine has.
#[test]
fn the_display_drivers_read_without_failing() {
    let drivers = display_drivers();
    if !cfg!(windows) {
        assert!(drivers.is_empty());
    }
}

/// The wgpu setup the window would use for `mode` on this platform.
fn setup_for(mode: Mode) -> WgpuSetup {
    let mut options = eframe::NativeOptions::default();
    super::super::apply(&mut options, mode, None);
    options.wgpu_options.wgpu_setup
}

/// The child's own steps, run in this process: software rendering (lavapipe or llvmpipe on
/// Linux, WARP on Windows) creates a device or reports why not; it never hangs or panics.
#[test]
fn the_test_start_runs_its_steps_in_software_rendering() {
    if cfg!(target_os = "macos") {
        return;
    }
    let code = run_child(setup_for(Mode::Software));
    assert!(code == 0 || code == 1, "exit code {code}");
}

/// A mode whose backend this platform lacks reports an error instead of a device.
#[test]
fn the_test_start_reports_a_missing_backend_as_an_error() {
    if cfg!(windows) || cfg!(target_os = "macos") {
        return;
    }
    // DirectX 12 does not exist off Windows: no adapter, an error line and exit code 1.
    assert_eq!(run_child(setup_for(Mode::Dx12)), 1);
}

/// A counter of the modes a fake test start was asked to test.
fn counting(outcome: impl Fn(Mode) -> Outcome) -> (std::rc::Rc<std::cell::RefCell<Vec<Mode>>>, impl Fn(&Path, Mode) -> Outcome) {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = std::rc::Rc::clone(&seen);
    (seen, move |_: &Path, mode: Mode| {
        log.borrow_mut().push(mode);
        outcome(mode)
    })
}

#[test]
fn a_start_tests_once_then_starts_directly_until_something_changes() {
    let dir = temp_dir("choose-once");
    let exe = Path::new("filmcraft");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 11);
    let (seen, test) = counting(|_| Outcome::Passed(Report::default()));
    assert_eq!(choose_with(&mut start, false, None, exe, "GPU 1.0", test), Ok(()));
    assert_eq!(seen.borrow().len(), 1, "nothing known yet: one test");
    start.finish();
    // The same version, mode and drivers, and that start ended: no test this time.
    let mut again = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 12);
    let (seen, test) = counting(|_| Outcome::Passed(Report::default()));
    assert_eq!(choose_with(&mut again, false, None, exe, "GPU 1.0", test), Ok(()));
    assert!(seen.borrow().is_empty(), "started directly");
    again.finish();
    // A driver update tests again.
    let mut updated = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 13);
    let (seen, test) = counting(|_| Outcome::Passed(Report::default()));
    assert_eq!(choose_with(&mut updated, false, None, exe, "GPU 2.0", test), Ok(()));
    assert_eq!(seen.borrow().len(), 1);
    updated.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_start_that_hung_in_its_window_moves_on_from_its_mode() {
    let dir = temp_dir("choose-hung");
    let exe = Path::new("filmcraft");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 14);
    let first = start.mode();
    // The last start in this mode began long ago and is still open: it never got its window.
    let stuck = Record {
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: first,
        drivers: "GPU 1.0".to_string(),
        started: unix_now().saturating_sub(HUNG_AFTER.as_secs() * 2),
        open: true,
    };
    let _ = write_atomic(&dir.join(TEST_FILE), &record_json(&stuck));
    let (seen, test) = counting(|_| Outcome::Passed(Report::default()));
    let result = choose_with(&mut start, false, None, exe, "GPU 1.0", test);
    if LADDER.len() > 1 {
        assert_eq!(result, Ok(()));
        assert!(!seen.borrow().contains(&first), "the hung mode is not tested again: {:?}", seen.borrow());
        assert_ne!(start.mode(), first);
        assert!(start.failed().iter().any(|(m, e)| *m == first && e.contains("never showed its window")), "{:?}", start.failed());
    } else {
        assert!(result.is_err(), "nothing to move on to");
    }
    start.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_mode_passing_or_a_chosen_mode_failing_returns_the_message() {
    let dir = temp_dir("choose-none");
    let exe = Path::new("filmcraft");
    let log = PathBuf::from("/data/Logs/filmcraft.log");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 15);
    let (seen, test) = counting(|_| Outcome::Hung(Report { stage: Some("device".to_string()), ..Report::default() }, 25));
    let msg = choose_with(&mut start, false, Some(&log), exe, "", test).err().unwrap_or_default();
    assert_eq!(seen.borrow().len(), LADDER.len(), "every mode tested");
    assert!(msg.contains("no graphics mode started in a test"), "{msg}");
    assert!(msg.contains("stopped responding while creating the graphics device"), "{msg}");
    assert!(msg.contains(&log.display().to_string()), "{msg}");
    start.finish();
    // `--graphics`: only that mode, and its failure is the message.
    let mut forced = GraphicsStart::begin_at(Some(&dir), false, Some(Mode::Default), NOW, 16);
    let (seen, test) = counting(|_| Outcome::Failed(Report { error: Some("no adapter".to_string()), ..Report::default() }));
    let msg = choose_with(&mut forced, true, None, exe, "", test).err().unwrap_or_default();
    assert_eq!(*seen.borrow(), vec![Mode::Default]);
    assert!(msg.contains("the chosen graphics mode did not start in a test") && msg.contains("no adapter"), "{msg}");
    forced.finish();
    // Without a data folder there is nothing to compare with: start as before.
    let mut bare = GraphicsStart::begin_at(None, false, None, NOW, 17);
    let (seen, test) = counting(|_| Outcome::Passed(Report::default()));
    assert_eq!(choose_with(&mut bare, false, None, exe, "", test), Ok(()));
    assert!(seen.borrow().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Off Windows, and with `WGPU_BACKEND`, the choice is left alone.
#[test]
fn the_choice_runs_only_on_windows_without_an_explicit_backend() {
    let dir = temp_dir("choose-gate");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 18);
    assert_eq!(choose(&mut start, false, true, None), Ok(()), "WGPU_BACKEND: the user is in charge");
    if !cfg!(windows) {
        assert_eq!(choose(&mut start, false, false, None), Ok(()));
        assert!(!dir.join(TEST_FILE).exists(), "no record off Windows");
    }
    start.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A start that ends in the message, because no mode passed, keeps an earlier hang on record:
/// closing it would let the next launch start the hung mode untested.
#[test]
fn a_start_that_ends_in_the_message_keeps_an_earlier_hang_on_record() {
    let dir = temp_dir("choose-abandon");
    let exe = Path::new("filmcraft");
    let mut start = GraphicsStart::begin_at(Some(&dir), false, None, NOW, 19);
    let first = start.mode();
    let stuck = Record {
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: first,
        drivers: "GPU 1.0".to_string(),
        started: unix_now().saturating_sub(HUNG_AFTER.as_secs() * 2),
        open: true,
    };
    let _ = write_atomic(&dir.join(TEST_FILE), &record_json(&stuck));
    let (_, test) = counting(|_| Outcome::Hung(Report::default(), 25));
    assert!(choose_with(&mut start, false, None, exe, "GPU 1.0", test).is_err());
    start.abandon();
    let kept = read_small(&dir.join(TEST_FILE)).and_then(|t| parse_record(&t));
    assert_eq!(kept.as_ref().map(|r| (r.mode, r.open)), Some((first, true)), "still open");
    assert_eq!(need(kept.as_ref(), first, env!("CARGO_PKG_VERSION"), "GPU 1.0", unix_now()), Need::LastHung, "the next launch moves on again");
    // `finish` would have closed it, which is why the message path doesn't use it.
    start.finish();
    let closed = read_small(&dir.join(TEST_FILE)).and_then(|t| parse_record(&t));
    assert_eq!(closed.map(|r| r.open), Some(false));
    let _ = std::fs::remove_dir_all(&dir);
}

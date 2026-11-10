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

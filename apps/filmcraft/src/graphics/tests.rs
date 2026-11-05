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

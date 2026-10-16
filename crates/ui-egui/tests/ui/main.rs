//! ui-egui's UI tests, as one test program. Every test program links the whole app, and in release
//! with LTO, so one program per file meant one link and one LTO pass per file. Each file here is a
//! module of this program instead. A new file needs its `mod` line below, which
//! `every_ui_test_file_is_a_module` checks. One file's tests:
//! `cargo test -p filmcraft-ui-egui --test ui color_ui::`. A file whose tests need a process of their
//! own stays in `tests/` and is named in `OWN_PROGRAM` below.

mod audio51_ui;
mod audio_fx_editor_ui;
mod captions_font_ui;
mod chrome_ui;
mod clip_audio_ui;
mod clip_dialogs_ui;
mod clipboard_paste_ui;
mod color_ui;
mod delete_caption_tracks_ui;
mod deselect_ui;
mod edit_point_menu_ui;
mod escape_drag_ui;
mod essential_sound_ui;
mod export_menu_ui;
mod export_ui;
mod frame_export_ui;
mod graphics_handles_ui;
mod graphics_notice_ui;
mod graphics_templates_ui;
mod graphics_ui;
mod interchange_export_ui;
mod keyboard_ui;
mod keyframes_ui;
mod links_ui;
mod marked_playback_position;
mod mask_roundness_ui;
mod masks_ui;
mod media_ui;
mod menus_ui;
mod meters_ui;
mod mixer_ui;
mod monitor_geometry;
mod multicam_ui;
mod native_dialogs_ui;
mod nest_editing_ui;
mod nest_rendering_ui;
mod nested_ui;
mod panels_ui;
mod par_ui;
mod perf_ui;
mod play_ahead;
mod playback_clock_ui;
mod project_panel_ui;
mod properties_scroll_ui;
mod remix_ui;
mod scripted;
mod sequence_tabs_ui;
mod sequence_ui;
mod settings_ui;
mod shared_docking_ui;
mod source_drag_ui;
mod source_playback_ui;
mod timecode_ui;
mod timeline_drop_ui;
mod timeline_escape_ui;
mod timeline_gap_ui;
mod timeline_marquee_ui;
mod timeline_middle_pan_ui;
mod timeline_option_drag_ui;
mod timeline_scroll_ui;
mod timeline_tabs_ui;
mod timeline_track_height_ui;
mod timeline_transition_move_ui;
mod timeline_trim_ui;
mod track_select_drag_ui;
mod transcript_ui;
mod transform_handles_ui;
mod transition_drop_ui;
mod transitions_ui;
mod tts_ui;
mod ui_control_input;
mod ui_dock_resize;
mod ui_keyboard_focus;
mod unlink_ui;
mod view_ui;
mod voiceover_ui;
mod workspaces_ui;

/// Files left in `tests/` that need a process of their own, each its own test program.
/// `robustness_ui` injects frame-job panics through a process-wide counter that every frame worker
/// in the process takes from, and installs the crash hook that records the last panic.
const OWN_PROGRAM: &[&str] = &["robustness_ui"];

/// A file in `tests/ui/` without its `mod` line would never build or run, and a new file in `tests/`
/// would be a test program of its own again.
#[test]
fn every_ui_test_file_is_a_module() {
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let names = |dir: std::path::PathBuf| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok()?.strip_suffix(".rs").map(str::to_owned))
            .collect();
        names.sort();
        names
    };
    let declared: Vec<&str> = include_str!("main.rs").lines().filter_map(|line| line.strip_prefix("mod ")?.strip_suffix(';')).collect();
    let missing: Vec<String> = names(tests.join("ui")).into_iter().filter(|name| name != "main" && !declared.contains(&name.as_str())).collect();
    assert!(missing.is_empty(), "tests/ui/main.rs needs a `mod` line for {missing:?}");
    let stray: Vec<String> = names(tests).into_iter().filter(|name| !OWN_PROGRAM.contains(&name.as_str())).collect();
    assert!(stray.is_empty(), "move {stray:?} into tests/ui/ with a `mod` line in tests/ui/main.rs, or list it in OWN_PROGRAM");
}

//! Architectural guard: newly added application dialogs must adopt shared chrome.
#[test]
fn application_dialogs_use_the_shared_window_style() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src.join("panels")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap();
        if matches!(name, "project.rs" | "frame_export.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("egui::Window::new("), "{} bypasses shared dialog chrome", path.display());
    }
    let custom = std::fs::read_to_string(src.join("panels/frame_export.rs")).unwrap();
    assert!(custom.contains("dialog_style::header") && custom.contains("dialog_style::frame") && custom.contains("dialog_style::actions"));
}

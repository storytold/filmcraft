use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::json;

#[test]
fn premiere_project_picker_imports_and_shows_the_fidelity_report() {
    let path = std::env::temp_dir().join(format!("filmcraft-native-ui-{}.prproj", std::process::id()));
    std::fs::write(&path,r#"<PremiereData Version="3"><Project ObjectID="1"><RootProjectItem ObjectURef="root"/></Project><RootProjectItem ObjectUID="root"><ProjectItemContainer><Items/></ProjectItemContainer></RootProjectItem></PremiereData>"#).unwrap();
    let selected = path.to_string_lossy().into_owned();
    let mut app = FilmcraftApp::new(Session::default());
    app.hooks.pick_files = Some(Box::new(move |extensions| {
        assert!(extensions.contains(&"prproj"));
        vec![selected.clone()]
    }));
    let result = app.file_dialog("file.import", &json!({})).unwrap();
    assert_eq!(result["documents"][0]["format"], "Premiere Pro project");
    assert!(app.ui.status.contains("no sequences"), "{}", app.ui.status);
    if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SHOTS") {
        let (tx, rx) = std::sync::mpsc::channel();
        let app = app.with_control(rx);
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 980.0)).wgpu().with_pixels_per_point(1.0).build_eframe(move |_cc| app);
        let (request, reply) = ControlRequest::new("ui.panel.show", json!({"panel":"Effects"}));
        tx.send(request).unwrap();
        for _ in 0..8 {
            harness.step();
        }
        assert_eq!(reply.try_recv().unwrap()["ok"], true);
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        harness.render().unwrap().save(dir.join("premiere-import-report.png")).unwrap();
    }
    let _ = std::fs::remove_file(path);
}

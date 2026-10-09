//! Home / Import mode: media sources and the ten most recently opened or saved projects.
//! Recent rows use `import.recent.<n>` automation ids, newest first.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, t.radius, t.panel_bg);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("import-home"));
    child.set_clip_rect(rect);
    egui::ScrollArea::vertical().id_salt("import-home-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        let width = (ui.available_width() - 48.0).clamp(1.0, 1088.0);
        let columns = if width >= 1088.0 {
            4
        } else if width >= 536.0 {
            2
        } else {
            1
        };
        let recent: Vec<String> = app.session.prefs.general.recent_projects.iter().take(10).cloned().collect();
        let recent_y = 90.0 + (4 / columns) as f32 * 166.0 + 20.0;
        let recent_h = if recent.is_empty() { 76.0 } else { recent.len() as f32 * 68.0 };
        let community_y = recent_y + 54.0 + recent_h + 36.0;
        let (content, _) = ui.allocate_exact_size(vec2(ui.available_width(), community_y + 76.0), Sense::hover());
        sources(app, ui, content, width, columns);
        recent_projects(app, ui, content.min + vec2(24.0, recent_y), width, &recent);
        community(app, ui, content.min + vec2(24.0, community_y));
    });
}

fn sources(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, width: f32, columns: usize) {
    let t = app.tokens;
    ui.painter().text(rect.min + vec2(24.0, 30.0), Align2::LEFT_CENTER, "Import", Tokens::semibold(22.0), t.text);
    ui.painter().text(
        rect.min + vec2(24.0, 58.0),
        Align2::LEFT_CENTER,
        "Add media to your project. Drop files anywhere in the window, or choose a source below.",
        Tokens::ui(13.0),
        t.text_dim,
    );
    let ctx = ui.ctx().clone();
    let cards = [
        ("Browse files…", "file.import", "Movies, audio and stills from disk"),
        ("Demo footage", "file.importDemoFootage", "Six procedural 1080p clips with sound"),
        ("Demo project", "file.openDemoProject", "A cut sequence with transitions, effects and music"),
        ("Bars and Tone", "file.newBarsAndTone", "SMPTE HD bars with 1 kHz reference tone"),
    ];
    let cw = ((width - (columns - 1) as f32 * 16.0) / columns as f32).min(260.0);
    for (i, (title, cmd, sub)) in cards.iter().enumerate() {
        let r = Rect::from_min_size(rect.min + vec2(24.0 + (i % columns) as f32 * (cw + 16.0), 90.0 + (i / columns) as f32 * 166.0), vec2(cw, 150.0));
        let resp = ui.interact(r, egui::Id::new(("imp", *cmd)), Sense::click());
        app.auto.add(&format!("import.{cmd}"), r, title);
        ui.painter().rect_filled(r, 8.0, if resp.hovered() { t.hover } else { t.tl_header_bg });
        ui.painter().text(r.min + vec2(16.0, 110.0), Align2::LEFT_CENTER, *title, Tokens::semibold(14.0), t.text);
        ui.painter().text(r.min + vec2(16.0, 132.0), Align2::LEFT_CENTER, *sub, Tokens::ui(11.5), t.text_dim);
        crate::icons::paint(
            ui.painter(),
            Rect::from_min_size(r.min + vec2(16.0, 18.0), vec2(56.0, 56.0)),
            [crate::icons::Icon::Folder, crate::icons::Icon::Film, crate::icons::Icon::Sequence, crate::icons::Icon::Grid][i],
            Color32::from_rgb(140, 150, 255),
        );
        if resp.clicked() {
            let _ = crate::menus::invoke(app, &ctx, cmd, json!({}));
            app.ui.mode = crate::state::Mode::Edit;
        }
    }
}

fn recent_projects(app: &mut FilmcraftApp, ui: &mut egui::Ui, origin: egui::Pos2, width: f32, paths: &[String]) {
    let t = app.tokens;
    ui.painter().text(origin, Align2::LEFT_CENTER, "Recent Projects", Tokens::semibold(18.0), t.text);
    ui.painter().text(origin + vec2(0.0, 26.0), Align2::LEFT_CENTER, "Pick up where you left off.", Tokens::ui(13.0), t.text_dim);
    if paths.is_empty() {
        ui.painter().text(
            origin + vec2(0.0, 68.0),
            Align2::LEFT_CENTER,
            "No recent projects yet. Open or save a project to see it here.",
            Tokens::ui(13.0),
            t.text_dim,
        );
    }
    for (i, path) in paths.iter().enumerate() {
        let name = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or(path);
        let r = Rect::from_min_size(origin + vec2(0.0, 54.0 + i as f32 * 68.0), vec2(width, 60.0));
        let response = ui.interact(r, egui::Id::new(("recent-project", path)), Sense::click()).on_hover_text(path);
        if ui.is_rect_visible(r) {
            app.auto.add(&format!("import.recent.{i}"), r.intersect(ui.clip_rect()), name);
        }
        ui.painter().rect_filled(r, 8.0, if response.hovered() { t.hover } else { t.field_bg });
        crate::icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 26.0, r.center().y), vec2(22.0, 22.0)), crate::icons::Icon::Sequence, t.accent);
        for (text, y, font, color) in [(name, 12.0, Tokens::semibold(14.0), t.text), (path.as_str(), 35.0, Tokens::ui(11.5), t.text_dim)] {
            let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, (width - 76.0).max(1.0));
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(r.min + vec2(52.0, y), galley, color);
        }
        if response.clicked() {
            open_recent(app, ui.ctx(), path);
        }
    }
}

/// The UI asks before replacing an edited project; opening a missing/corrupt file keeps it intact.
fn open_recent(app: &mut FilmcraftApp, ctx: &egui::Context, path: &str) {
    if app.session.is_dirty() {
        app.ui.clip_dialog =
            Some(crate::state::ClipDialogDraft { command: "file.open".into(), params: json!({"path": path}), info: json!({}), error: String::new() });
    } else if crate::menus::invoke(app, ctx, "file.open", json!({"path": path})).is_ok() {
        app.stop();
        app.ui.mode = crate::state::Mode::Edit;
    }
}

fn community(app: &mut FilmcraftApp, ui: &mut egui::Ui, origin: egui::Pos2) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    // ArtCraft wordmark (first-party trademark, docs/brand/), then the section title.
    let logo = crate::brand::paint_wordmark(ui, origin, 15.0, app.ui.dark);
    let tx = logo.map_or(origin.x, |r| r.max.x + 12.0);
    ui.painter().text(pos2(tx, origin.y), Align2::LEFT_CENTER, "Community", Tokens::semibold(15.0), t.text);
    let mut x = origin.x;
    for (i, (id, label, url)) in crate::links::ALL.iter().take(4).enumerate() {
        let primary = i == 0;
        let label = if primary { "Join us on Discord" } else { *label };
        let g = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(13.0), t.text);
        let r = Rect::from_min_size(pos2(x, origin.y + 18.0), vec2(g.size().x + 44.0, 34.0));
        let resp = ui.interact(r, egui::Id::new(("imp-link", *id)), Sense::click()).on_hover_text(*url);
        app.auto.add(&format!("import.link.{}", id.trim_start_matches("help.")), r, label);
        let (bg, fg) = if primary {
            (if resp.hovered() { t.accent_hover } else { t.accent }, Color32::WHITE)
        } else {
            (if resp.hovered() { t.hover } else { t.tl_header_bg }, t.text)
        };
        ui.painter().rect_filled(r, 8.0, bg);
        let icon = [crate::icons::Icon::Chat, crate::icons::Icon::Globe, crate::icons::Icon::Globe, crate::icons::Icon::Code][i];
        crate::icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 20.0, r.center().y), vec2(16.0, 16.0)), icon, fg);
        ui.painter().galley(pos2(r.min.x + 34.0, r.center().y - g.size().y / 2.0), g, fg);
        if resp.clicked() {
            crate::links::open(&ctx, url);
        }
        x = r.max.x + 12.0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use egui_kittest::Harness;
    use filmcraft_engine::Session;

    use super::*;
    use crate::state::Mode;

    struct ProjectFile(String);

    impl ProjectFile {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path =
                filmcraft_engine::temp_dir().join(format!("filmcraft-recent-{}-{}-中文工程.fcproj", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let file = Self(path.to_string_lossy().into_owned());
            let mut session = Session::default();
            session.execute("file.newProject", json!({"name": "中文工程"})).unwrap();
            session.execute("file.saveAs", json!({"path": file.0})).unwrap();
            file
        }
    }

    impl Drop for ProjectFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn harness(session: Session) -> Harness<'static, FilmcraftApp> {
        let mut app = FilmcraftApp::new(session);
        app.ui.mode = Mode::Import;
        let mut h = Harness::builder().with_size(vec2(1200.0, 850.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        for _ in 0..4 {
            h.step();
        }
        h
    }

    fn click(h: &mut Harness<'static, FilmcraftApp>, id: &str) {
        let r = h.state().auto.find(id).unwrap_or_else(|| panic!("missing element {id}"));
        let p = pos2(r.rect[0] + r.rect[2] * 0.5, r.rect[1] + r.rect[3] * 0.5);
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerMoved(p));
            h.input_mut().events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
        // Modal geometry settles after its first layout; use the presented button positions.
        for _ in 0..4 {
            h.step();
        }
    }

    #[test]
    fn recent_project_click_opens_chinese_path_and_updates_recency() {
        let file = ProjectFile::new();
        let mut session = Session::default();
        session.prefs.note_recent(&file.0);
        session.prefs.note_recent("missing.fcproj");
        let mut h = harness(session);
        assert!(h.state().auto.find("import.recent.1").unwrap().label.ends_with("中文工程"));
        click(&mut h, "import.recent.1");
        assert_eq!(h.state().session.path.as_deref(), Some(file.0.as_str()));
        assert_eq!(h.state().session.project.name, std::path::Path::new(&file.0).file_stem().unwrap().to_string_lossy());
        assert_eq!(h.state().ui.mode, Mode::Edit);
        assert_eq!(h.state().session.prefs.general.recent_projects.first(), Some(&file.0));
    }

    #[test]
    fn missing_or_corrupt_recent_project_keeps_current_project() {
        let file = ProjectFile::new();
        for corrupt in [false, true] {
            if corrupt {
                std::fs::write(&file.0, b"broken project").unwrap();
            } else {
                std::fs::remove_file(&file.0).unwrap();
            }
            let mut session = Session::default();
            session.prefs.note_recent(&file.0);
            let before = session.project.clone();
            let mut h = harness(session);
            click(&mut h, "import.recent.0");
            assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &before));
            assert_eq!(h.state().ui.mode, Mode::Import);
            assert!(h.state().ui.status.contains(&file.0));
            h.state_mut().session.execute("file.newSequence", json!({"name": "不要丢失这段剪辑"})).unwrap();
            let edited = h.state().session.project.clone();
            click(&mut h, "import.recent.0");
            click(&mut h, "openProject.dontSave");
            assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &edited));
            assert!(h.state().session.is_dirty());
            assert!(!h.state().ui.clip_dialog.as_ref().unwrap().error.is_empty(), "{}", json!(h.state().ui.clip_dialog));
        }
    }

    #[test]
    fn unsaved_project_cancel_and_cancelled_save_as_preserve_edits() {
        let file = ProjectFile::new();
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"name": "未保存的剪辑"})).unwrap();
        assert!(session.is_dirty());
        session.prefs.note_recent(&file.0);
        let before = session.project.clone();
        let mut h = harness(session);
        h.state_mut().hooks.pick_save = Some(Box::new(|_| None));
        click(&mut h, "import.recent.0");
        click(&mut h, "openProject.save");
        assert!(h.state().ui.clip_dialog.is_some());
        assert!(h.state().session.is_dirty());
        assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &before));
        click(&mut h, "openProject.cancel");
        assert!(h.state().ui.clip_dialog.is_none());
        assert_eq!(h.state().ui.mode, Mode::Import);
        assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &before));
        click(&mut h, "import.recent.0");
        click(&mut h, "openProject.dontSave");
        assert_eq!(h.state().session.path.as_deref(), Some(file.0.as_str()));
        assert_eq!(h.state().ui.mode, Mode::Edit);
    }

    #[test]
    fn save_before_opening_persists_current_edits() {
        let original = ProjectFile::new();
        let next = ProjectFile::new();
        let mut session = Session::default();
        session.execute("file.open", json!({"path": original.0})).unwrap();
        session.execute("file.newSequence", json!({"name": "保留这段剪辑"})).unwrap();
        session.prefs.note_recent(&next.0);
        let mut h = harness(session);
        click(&mut h, "import.recent.0");
        click(&mut h, "openProject.save");
        assert_eq!(h.state().session.path.as_deref(), Some(next.0.as_str()), "{}", json!(h.state().ui.clip_dialog));
        assert_eq!(h.state().ui.mode, Mode::Edit);
        let mut reopened = Session::default();
        reopened.execute("file.open", json!({"path": original.0})).unwrap();
        let sequence = reopened.state.active_sequence.unwrap();
        assert_eq!(reopened.project.item(sequence).unwrap().name, "保留这段剪辑");
    }
}

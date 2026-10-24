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

/// Rendering two independent dialogs must not restyle the application or each other's controls.
#[test]
fn dialog_chrome_tracks_theme_and_custom_accent_without_global_style_changes() {
    use filmcraft_ui_egui::{
        dialog_style,
        theme::{self, ThemeKind, Tokens},
    };
    for kind in [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Light] {
        let ctx = egui::Context::default();
        let tokens = Tokens::for_kind(kind).with_appearance(Some([240, 210, 30]), false);
        theme::install(&ctx, &tokens);
        let original = ctx.global_style();
        let mut out = egui::FullOutput::default();
        for pass in 0..4 {
            out = ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(pass)),
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
                    ..Default::default()
                },
                |root| {
                    let ctx = root.ctx();
                    dialog_style::Window::new(&tokens, "Themed dialog").id(egui::Id::new("theme-dialog")).collapsible(false).show(ctx, |ui| {
                        assert_eq!(ui.visuals().override_text_color, original.visuals.override_text_color);
                        dialog_style::header(ui, &tokens, "Custom header", 360.0);
                        dialog_style::actions(ui, |ui| {
                            ui.add(dialog_style::secondary(&tokens, "Cancel"));
                            ui.add(dialog_style::primary(&tokens, "Apply"));
                        });
                    });
                    egui::Window::new("Independent window").default_pos(egui::pos2(500.0, 300.0)).show(ctx, |ui| {
                        assert_eq!(ui.visuals().widgets.inactive.bg_fill, original.visuals.widgets.inactive.bg_fill);
                        ui.button("Independent action").on_hover_text("Unchanged popup style");
                    });
                },
            );
            out.textures_delta.clear();
            assert!(std::sync::Arc::ptr_eq(&original, &ctx.global_style()), "dialog replaced global style in {kind:?}");
        }
        fn fills(shape: &egui::Shape, color: egui::Color32) -> bool {
            match shape {
                egui::Shape::Rect(r) => r.fill == color,
                egui::Shape::Vec(v) => v.iter().any(|s| fills(s, color)),
                _ => false,
            }
        }
        assert!(out.shapes.iter().any(|s| fills(&s.shape, tokens.header_bg)), "missing theme header in {kind:?}");
        assert!(out.shapes.iter().any(|s| fills(&s.shape, tokens.accent)), "primary action ignored custom accent in {kind:?}");
    }
}

#[test]
fn primary_foreground_stays_legible_for_dark_and_bright_custom_accents() {
    use filmcraft_ui_egui::theme::{ThemeKind, Tokens};
    let luma = |color: egui::Color32| {
        let c = egui::Rgba::from(color);
        0.2126 * c.r() + 0.7152 * c.g() + 0.0722 * c.b()
    };
    for accent in [[20, 20, 20], [255, 255, 0], [47, 107, 223], [210, 210, 210]] {
        let tokens = Tokens::for_kind(ThemeKind::Light).with_appearance(Some(accent), false);
        let (a, b) = (luma(tokens.accent), luma(tokens.accent_text()));
        let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
        assert!(ratio >= 4.5, "insufficient contrast {ratio} for {accent:?}");
    }
}

#[test]
fn dialog_body_unwind_does_not_change_context_style() {
    use filmcraft_ui_egui::{
        dialog_style,
        theme::{self, ThemeKind, Tokens},
    };
    let ctx = egui::Context::default();
    let t = Tokens::for_kind(ThemeKind::Light);
    theme::install(&ctx, &t);
    let original = ctx.global_style();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = ctx.run_ui(egui::RawInput::default(), |root| {
            let ctx = root.ctx();
            dialog_style::Window::new(&t, "Unwinding dialog").show(ctx, |_| panic!("synthetic dialog failure"));
        });
    }));
    let failure = result.expect_err("dialog body must be exercised");
    assert_eq!(failure.downcast_ref::<&str>(), Some(&"synthetic dialog failure"));
    assert!(std::sync::Arc::ptr_eq(&original, &ctx.global_style()));
}

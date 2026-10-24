//! Create Captions from Transcript (Premiere's Text panel ▸ Create captions dialog).
//!
//! `transcript.createCaptions` run without params from the UI (the Transcript toolbar, the
//! Captions tab, Sequence ▸ Transcript) opens this dialog instead; with params the engine command
//! runs directly. The dialog opens with the settings used last time (`Preferences::captions`),
//! where Premiere goes back to its default preset on every use.
//!
//! The open dialog lives in `UiState::caption_dialog` (serde) so agents can set it with `ui.set`.
//! Automation ids: `captions.preset`, `captions.presetMenu`, `captions.savePreset`,
//! `captions.deletePreset`, `captions.presetName`, `captions.presetSave`, `captions.presetCancel`,
//! `captions.prefs` (the section toggle), `captions.format`, `captions.style`,
//! `captions.layout.single`, `captions.layout.double`, `captions.maxChars`, `captions.minSeconds`,
//! `captions.gapFrames`, `captions.wordsPerCaption`, `captions.create`, `captions.cancel`.

use egui::{Align2, Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, Ui, vec2};
use filmcraft_engine::transcript::{CaptionPreset, CaptionSettings, builtin_caption_presets};
use filmcraft_project::{CaptionFormat, CaptionStyle};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::CaptionDialogDraft;
use crate::theme::Tokens;

const COMMAND: &str = "transcript.createCaptions";
const W: f32 = 440.0;
const PAD: f32 = 20.0;

/// `transcript.createCaptions` without params opens the dialog. Returns None for other ids.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != COMMAND || !params.as_object().is_none_or(|m| m.is_empty()) {
        return None;
    }
    if let Err(e) = filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session)) {
        app.ui.status = e.clone();
        return Some(Err(e));
    }
    app.ui.caption_dialog = Some(CaptionDialogDraft { settings: app.session.prefs.captions.last.clone(), ..Default::default() });
    Some(Ok(json!({"dialog": "captions"})))
}

/// The preset the settings match: a saved preset exactly, a built-in one whatever the style
/// (built-in presets leave the style alone). None = Custom.
fn matching_preset(s: &CaptionSettings, user: &[CaptionPreset]) -> Option<(String, bool)> {
    if let Some(p) = user.iter().find(|p| p.settings == *s) {
        return Some((p.name.clone(), true));
    }
    builtin_caption_presets()
        .into_iter()
        .find(|p| CaptionSettings { style: s.style.clone(), style_name: s.style_name.clone(), ..p.settings.clone() } == *s)
        .map(|p| (p.name, false))
}

/// Apply a preset: built-in presets keep the chosen style.
fn apply_preset(s: &mut CaptionSettings, p: &CaptionPreset, builtin: bool) {
    let (style, name) = (s.style.clone(), s.style_name.clone());
    *s = p.settings.clone();
    if builtin {
        s.style = style;
        s.style_name = name;
    }
}

fn label_with_help(app: &mut FilmcraftApp, ui: &mut Ui, label: &str, help: &str) {
    let t = app.tokens;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(t.text));
        if !help.is_empty() {
            let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 7.5, t.text_dim);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, "?", Tokens::semibold(11.0), t.panel_bg);
            resp.on_hover_text(help);
        }
    });
}

/// A slider with a number field and unit, like Premiere's rows. Returns whether it changed.
#[allow(clippy::too_many_arguments)]
fn slider_row(
    app: &mut FilmcraftApp,
    ui: &mut Ui,
    id: &str,
    label: &str,
    help: &str,
    v: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    decimals: usize,
    unit: &str,
) -> bool {
    label_with_help(app, ui, label, help);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = W - 2.0 * PAD - 150.0;
        let step = if decimals == 0 { 1.0 } else { 0.1 };
        let r = ui.add(egui::Slider::new(v, range.clone()).show_value(false).step_by(step));
        changed |= r.changed();
        let r2 = ui.add(egui::DragValue::new(v).range(range).fixed_decimals(decimals).speed(step).min_decimals(decimals));
        changed |= r2.changed();
        ui.label(RichText::new(unit).size(13.0).color(app.tokens.text));
        app.auto.add(id, r2.rect, label);
        app.auto.add(&format!("{id}.slider"), r.rect, label);
    });
    ui.add_space(6.0);
    changed
}

fn combo(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, shown: &str, width: f32, add: impl FnOnce(&mut Ui)) {
    let r = egui::ComboBox::from_id_salt(("captions", id)).selected_text(RichText::new(shown).size(13.0)).width(width).height(320.0).show_ui(ui, add);
    app.auto.add(id, r.response.rect, label);
}

fn pill(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, primary: bool) -> bool {
    let t = app.tokens;
    let text = RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text });
    let b = egui::Button::new(text)
        .min_size(vec2(96.0, 30.0))
        .corner_radius(CornerRadius::same(15))
        .fill(if primary { t.accent } else { Color32::TRANSPARENT })
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.5, t.text_faint) });
    let r = ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand);
    app.auto.add(id, r.rect, label);
    r.clicked()
}

fn save_prefs(app: &mut FilmcraftApp, f: impl FnOnce(&mut filmcraft_engine::transcript::CaptionPrefs)) {
    let mut p = app.session.prefs.clone();
    f(&mut p.captions);
    if p != app.session.prefs
        && let Err(e) = app.session.set_prefs(p)
    {
        app.ui.status = tlf!("Couldn't save caption settings: {e}", e = e.to_string());
    }
}

/// Draw the open dialog, if any.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.caption_dialog.clone() else { return };
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::new("caption-dialog-before-drag"), &mut d);
    let t = app.tokens;
    // Enter creates the captions, unless it is committing a number field or the preset name
    let field_focused = ctx.memory(|m| m.focused().is_some());
    let user = app.session.prefs.captions.user_presets.clone();
    // the Style menu: the default style, then each caption track's style in this sequence
    let tracks: Vec<(String, CaptionStyle)> = app
        .session
        .active_sequence()
        .map(|q| q.caption_tracks.iter().enumerate().map(|(i, tr)| (format!("C{} · {}", i + 1, tr.name), tr.style.clone())).collect())
        .unwrap_or_default();
    let mut action: Option<&str> = None;
    let mut save_as: Option<String> = None;
    let mut delete: Option<String> = None;
    let frame = Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0));
    egui::Modal::new(egui::Id::new("caption-dialog")).frame(frame).show(ctx, |ui| {
        ui.set_width(W);
        let (bar, _) = ui.allocate_exact_size(vec2(W, 30.0), Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, t.header_bg);
        ui.painter().text(bar.left_center() + vec2(PAD, 0.0), Align2::LEFT_CENTER, tl!("Create captions"), Tokens::semibold(13.0), t.text_dim);
        app.auto.add("captions.window", bar, "Create captions");
        Frame::new().inner_margin(Margin { left: PAD as i8, right: PAD as i8, top: 12, bottom: 16 }).show(ui, |ui| {
            ui.set_width(W - 2.0 * PAD);
            ui.spacing_mut().item_spacing.y = 6.0;
            let s = &mut d.settings;

            // ---- Caption preset
            label_with_help(app, ui, tl!("Caption preset"), "");
            let matched = matching_preset(s, &user);
            ui.horizontal(|ui| {
                let shown = matched.as_ref().map_or_else(|| tl!("Custom").to_string(), |(n, _)| crate::i18n::t(n).to_string());
                combo(app, ui, "captions.preset", "Caption preset", &shown, W - 2.0 * PAD - 44.0, |ui| {
                    for p in builtin_caption_presets() {
                        let on = matched.as_ref().is_some_and(|(n, u)| !u && *n == p.name);
                        if ui.selectable_label(on, crate::i18n::t(&p.name)).clicked() {
                            apply_preset(s, &p, true);
                        }
                    }
                    if !user.is_empty() {
                        ui.separator();
                    }
                    for p in &user {
                        let on = matched.as_ref().is_some_and(|(n, u)| *u && *n == p.name);
                        if ui.selectable_label(on, &p.name).clicked() {
                            apply_preset(s, p, false);
                        }
                    }
                });
                let r = ui.add(egui::Button::new(RichText::new("•••").size(13.0).color(t.text_dim)).frame(false).min_size(vec2(32.0, 24.0)));
                app.auto.add("captions.presetMenu", r.rect, "Preset options");
                egui::Popup::menu(&r).show(|ui| {
                    let b = ui.button(tl!("Save as preset…"));
                    app.auto.add("captions.savePreset", b.rect, "Save as preset…");
                    if b.clicked() {
                        d.saving = Some(matched.as_ref().filter(|(_, u)| *u).map(|(n, _)| n.clone()).unwrap_or_default());
                    }
                    let saved = matched.as_ref().filter(|(_, u)| *u).map(|(n, _)| n.clone());
                    let b = ui.add_enabled(saved.is_some(), egui::Button::new(tl!("Delete preset")));
                    app.auto.add("captions.deletePreset", b.rect, "Delete preset");
                    if b.clicked() {
                        delete = saved;
                    }
                });
            });
            if let Some(name) = d.saving.as_mut() {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text(tl!("Preset name")).desired_width(W - 2.0 * PAD - 160.0));
                    app.auto.add("captions.presetName", r.rect, "Preset name");
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let b = ui.add_enabled(!name.trim().is_empty(), egui::Button::new(tl!("Save")));
                    app.auto.add("captions.presetSave", b.rect, "Save");
                    if (b.clicked() || enter) && !name.trim().is_empty() {
                        save_as = Some(name.trim().to_string());
                    }
                    let b = ui.button(tl!("Cancel"));
                    app.auto.add("captions.presetCancel", b.rect, "Cancel");
                    if b.clicked() {
                        action = Some("cancelSave");
                    }
                });
            }
            ui.add_space(8.0);

            // ---- Captioning Preferences
            let (resp, open) = crate::widgets::section_header(
                ui,
                egui::Id::new("captions-prefs"),
                tl!("Captioning Preferences (format, max/min length, lines)"),
                !d.collapsed,
                &t,
                true,
            );
            app.auto.add("captions.prefs", resp.rect, "Captioning Preferences");
            d.collapsed = !open;
            if open {
                ui.add_space(4.0);
                label_with_help(app, ui, tl!("Format"), "");
                let fmt = CaptionFormat::from_name(&s.format).unwrap_or_default();
                combo(app, ui, "captions.format", "Format", crate::i18n::t(fmt.label()), W - 2.0 * PAD - 8.0, |ui| {
                    for f in CaptionFormat::ALL {
                        if ui.selectable_label(f == fmt, crate::i18n::t(f.label())).clicked() {
                            s.format = f.label().into();
                            if let Some(cols) = f.max_columns() {
                                s.max_chars = s.max_chars.min(cols as u32);
                            }
                        }
                    }
                });
                label_with_help(
                    app,
                    ui,
                    tl!("Style"),
                    tl!("The caption track style of the new track: the default style, or the style of a caption track in this sequence."),
                );
                let style_shown = match &s.style {
                    None => tl!("Default").to_string(),
                    Some(_) if s.style_name.is_empty() => tl!("Custom").to_string(),
                    Some(_) => s.style_name.clone(),
                };
                combo(app, ui, "captions.style", "Style", &style_shown, W - 2.0 * PAD - 8.0, |ui| {
                    if ui.selectable_label(s.style.is_none(), tl!("Default")).clicked() {
                        s.style = None;
                        s.style_name.clear();
                    }
                    for (name, style) in &tracks {
                        let on = s.style.as_ref() == Some(style);
                        if ui.selectable_label(on, name).clicked() {
                            s.style = Some(style.clone());
                            // "C2 · TEXS.ORG" → "TEXS.ORG": track numbers change between sequences
                            s.style_name = name.split_once(" · ").map_or(name.as_str(), |(_, n)| n).to_string();
                        }
                    }
                });
                ui.add_space(8.0);
                label_with_help(app, ui, tl!("Layout"), "");
                ui.horizontal(|ui| {
                    for (lines, id, label) in [(1, "captions.layout.single", tl!("Single Line")), (2, "captions.layout.double", tl!("Double Line"))] {
                        let r = ui.radio(s.lines == lines, RichText::new(label).size(13.0));
                        app.auto.add(id, r.rect, label);
                        if r.clicked() {
                            s.lines = lines;
                        }
                        ui.add_space(16.0);
                    }
                });
                ui.add_space(6.0);
                let max_cols = CaptionFormat::from_name(&s.format).unwrap_or_default().max_columns().unwrap_or(80) as f64;
                let mut v = f64::from(s.max_chars);
                if slider_row(
                    app,
                    ui,
                    "captions.maxChars",
                    tl!("Maximum length in characters"),
                    tl!("Words wrap to a new line, or a new caption, at this many characters."),
                    &mut v,
                    1.0..=max_cols,
                    0,
                    tl!("characters"),
                ) {
                    s.max_chars = v.round().clamp(1.0, max_cols) as u32;
                }
                let mut v = s.min_seconds;
                if slider_row(
                    app,
                    ui,
                    "captions.minSeconds",
                    tl!("Minimum duration in seconds"),
                    tl!("A shorter caption is held into the silence after it, never over the next caption."),
                    &mut v,
                    0.0..=10.0,
                    1,
                    tl!("seconds"),
                ) {
                    s.min_seconds = (v * 10.0).round().clamp(0.0, 100.0) / 10.0;
                }
                let mut v = f64::from(s.gap_frames);
                if slider_row(
                    app,
                    ui,
                    "captions.gapFrames",
                    tl!("Gap between captions (frames)"),
                    tl!("Frames left empty between one caption and the next."),
                    &mut v,
                    0.0..=30.0,
                    0,
                    tl!("frames"),
                ) {
                    s.gap_frames = v.round().clamp(0.0, 1000.0) as u32;
                }
                let mut v = f64::from(s.words_per_caption);
                let unit = if s.words_per_caption == 0 { tl!("words (no limit)") } else { tl!("words") };
                if slider_row(
                    app,
                    ui,
                    "captions.wordsPerCaption",
                    tl!("Maximum words per caption"),
                    tl!("0 = no limit. 1 makes word-by-word captions; with a minimum duration of 0, each word keeps its own timing."),
                    &mut v,
                    0.0..=20.0,
                    0,
                    unit,
                ) {
                    s.words_per_caption = v.round().clamp(0.0, 1000.0) as u32;
                }
            }
            if !d.error.is_empty() {
                ui.colored_label(Color32::from_rgb(0xe0, 0x60, 0x60), &d.error);
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if pill(app, ui, "captions.create", tl!("Create captions"), true) {
                    action = Some("create");
                }
                ui.add_space(6.0);
                if pill(app, ui, "captions.cancel", tl!("Cancel"), false) {
                    action = Some("cancel");
                }
            });
        });
    });
    let typing = d.saving.is_some();
    if crate::widgets::escape_closes(ctx) {
        action = Some(if typing { "cancelSave" } else { "cancel" });
    }
    if !typing && action.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !field_focused {
        action = Some("create");
    }
    if let Some(name) = save_as {
        let settings = d.settings.clone();
        save_prefs(app, |c| match c.user_presets.iter_mut().find(|p| p.name == name) {
            Some(p) => p.settings = settings,
            None => c.user_presets.push(CaptionPreset { name, settings }),
        });
        d.saving = None;
    }
    if let Some(name) = delete {
        save_prefs(app, |c| c.user_presets.retain(|p| p.name != name));
    }
    match action {
        Some("cancel") => {
            app.ui.caption_dialog = None;
            return;
        }
        Some("cancelSave") => d.saving = None,
        Some("create") => match app.session.execute(COMMAND, d.settings.params()) {
            Ok(r) => {
                let settings = d.settings.clone();
                save_prefs(app, |c| c.last = settings);
                let n = r["captions"].as_u64().unwrap_or(0);
                app.ui.status = tlf!("Created {n} captions", n = n);
                app.ui.text_tab = "Captions".into();
                app.ui.caption_dialog = None;
                return;
            }
            Err(e) => d.error = e.to_string(),
        },
        _ => {}
    }
    app.ui.caption_dialog = Some(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_match_whatever_the_style() {
        let mut s = builtin_caption_presets()[2].settings.clone();
        s.style = Some(CaptionStyle::default());
        s.style_name = "TEXS.ORG".into();
        assert_eq!(matching_preset(&s, &[]), Some(("Word by Word".into(), false)));
        s.max_chars = 7;
        assert_eq!(matching_preset(&s, &[]), None);
        let user = vec![CaptionPreset { name: "Mine".into(), settings: s.clone() }];
        assert_eq!(matching_preset(&s, &user), Some(("Mine".into(), true)));
    }

    #[test]
    fn applying_a_builtin_preset_keeps_the_style() {
        let mut s = CaptionSettings { style: Some(CaptionStyle::default()), style_name: "TEXS.ORG".into(), max_chars: 7, ..Default::default() };
        let presets = builtin_caption_presets();
        apply_preset(&mut s, &presets[0], true);
        assert_eq!(s.max_chars, presets[0].settings.max_chars);
        assert_eq!(s.style_name, "TEXS.ORG");
        assert!(s.style.is_some());
    }
}

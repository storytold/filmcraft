//! The "Lettering" window: lettering animated by the animation app (EffectCraft), made for a span of
//! the speech and not for the whole video.
//!
//! Opened by right-clicking a caption (or Graphics and Titles ▸ Lettering with Animation Templates…)
//! with captions selected. It shows the span (start, duration, frame size, frame rate), the words as
//! buttons (a click marks the word as the highlight, `hero`), and the menu of animation templates
//! read from the template bank (`lettering.fx.templates`). Send runs `lettering.fx.send`, which
//! writes the order and starts the animation app in the background; the window then shows the
//! order's progress and can cancel it. Putting the rendered `.mov` back on the timeline is not part
//! of this window.
//!
//! The words per line (1 to 12) go to `lettering.fx.send` as `palavrasPorLinha`: they set how
//! `words.json` is cut into lines.
//! The fonts (text, highlight words) are chosen from the installed families and go as `fontes`.
//! The colours (text, outline, spoken word, highlight words) start as the style's own; a changed one
//! goes to `lettering.fx.send` as `cores`.
//!
//! Automation ids: `letteringFx.word.<index>`, `letteringFx.template.<id>`, `letteringFx.send`,
//! `letteringFx.perLine`, `letteringFx.solo`, `letteringFx.font.<key>` (+ `.style`, `.reset`), `letteringFx.color.<key>` (+ `.reset`),
//! `letteringFx.cancel` (closes, or cancels the running order), `letteringFx.close`.

use crate::widgets::CenteredDialog as _;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::FilmcraftApp;

#[derive(Clone, Debug)]
struct Template {
    id: String,
    name: String,
    description: String,
}

/// Words per line when the user does not change it (the engine's default).
const DEFAULT_PER_LINE: u8 = 4;

/// The colour options: the key sent in `cores` and the colour offered when it is changed.
const COLORS: [(&str, [u8; 3]); 4] = [("texto", [255, 255, 255]), ("contorno", [0, 0, 0]), ("falada", [255, 214, 10]), ("destaque", [255, 255, 255])];

/// The font options: the key sent in `fontes`.
const FONT_KEYS: [&str; 2] = ["texto", "destaque"];

/// The translated label of a font option.
fn font_label(i: usize) -> String {
    match i {
        0 => tl!("Text font"),
        _ => tl!("Highlight font"),
    }
    .to_string()
}

/// The font offered when one is changed: Inter when there is one (the app's own UI font), else the
/// first family, with its regular style when it has one.
fn default_font(fams: &[(String, Vec<String>)]) -> Option<(String, String)> {
    let (name, styles) = fams.iter().find(|(n, _)| n == "Inter").or_else(|| fams.first())?;
    let style = styles.iter().find(|s| s.as_str() == "Regular").or_else(|| styles.first()).cloned().unwrap_or_default();
    Some((name.clone(), style))
}

/// The `fontes` of a send: family and style for each font the user changed, nothing for the others.
fn fontes_json(picks: &[Option<(String, String)>; 2]) -> serde_json::Map<String, Value> {
    FONT_KEYS
        .iter()
        .zip(picks.iter())
        .filter_map(|(key, p)| {
            let (family, style) = p.as_ref()?;
            let mut one = serde_json::Map::new();
            one.insert("familia".into(), json!(family));
            if !style.is_empty() {
                one.insert("estilo".into(), json!(style));
            }
            Some(((*key).to_string(), Value::Object(one)))
        })
        .collect()
}

/// The translated label of a colour option.
fn color_label(i: usize) -> String {
    match i {
        0 => tl!("Text color"),
        1 => tl!("Outline color"),
        2 => tl!("Spoken word color"),
        _ => tl!("Highlight color"),
    }
    .to_string()
}

#[derive(Clone, Debug)]
enum Phase {
    Editing,
    /// The order was sent: the job's id.
    Sent(u64),
}

#[derive(Clone, Debug)]
struct Draft {
    summary: String,
    /// The captions the words came from: sent back so the order matches what the window shows.
    captions: Vec<u64>,
    words: Vec<String>,
    heroes: Vec<bool>,
    /// Words per line (1 to 12; the default is 4).
    per_line: u8,
    /// Each highlight word alone on the screen while it is spoken.
    solo: bool,
    /// The colours the user changed, in [`COLORS`] order (`None`: the style's own).
    colors: [Option<[u8; 3]>; 4],
    /// The font families (with their styles), read the first time a font is changed: reading them
    /// scans the system fonts.
    fonts: Option<Arc<Vec<(String, Vec<String>)>>>,
    /// The fonts the user changed, in [`FONT_KEYS`] order: family and style (`None`: the style's own).
    font_picks: [Option<(String, String)>; 2],
    templates: Vec<Template>,
    template: Option<String>,
    /// Why the menu is empty, or why the last send failed.
    problem: Option<String>,
    phase: Phase,
}

/// The `cores` of a send: `#rrggbb` for each colour the user changed, nothing for the others.
fn cores_json(colors: &[Option<[u8; 3]>; 4]) -> serde_json::Map<String, Value> {
    COLORS.iter().zip(colors.iter()).filter_map(|((key, _), c)| c.map(|[r, g, b]| ((*key).to_string(), json!(format!("#{r:02x}{g:02x}{b:02x}"))))).collect()
}

/// Where the open window keeps its draft.
fn draft_id() -> egui::Id {
    egui::Id::unique("lettering-fx-draft")
}

/// Read the span and the template menu, and open the window.
pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let span = app.session.execute("lettering.fx.prepare", json!({})).map_err(|e| e.to_string())?;
    let words: Vec<String> = span["words"].as_array().map(|a| a.iter().filter_map(|w| w["text"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    let (templates, problem) = match app.session.execute("lettering.fx.templates", json!({})) {
        Ok(v) => (
            v["templates"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| {
                            Some(Template {
                                id: t["id"].as_str()?.into(),
                                name: t["nome"].as_str().unwrap_or_default().into(),
                                description: t["descricao"].as_str().unwrap_or_default().into(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let secs = |k: &str| span[k].as_f64().unwrap_or(0.0);
    let summary = format!(
        "{:.2} s · {:.2} s · {}×{} · {} fps",
        secs("startSeconds"),
        secs("duration"),
        span["width"].as_u64().unwrap_or(0),
        span["height"].as_u64().unwrap_or(0),
        span["fps"].as_f64().unwrap_or(0.0)
    );
    let captions: Vec<u64> = span["captions"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let template = templates.first().map(|t| t.id.clone());
    let n = words.len();
    ctx.data_mut(|m| {
        m.insert_temp(
            draft_id(),
            Some(Draft {
                summary,
                captions,
                words,
                heroes: vec![false; n],
                per_line: DEFAULT_PER_LINE,
                solo: false,
                colors: [None; 4],
                fonts: None,
                font_picks: [None, None],
                templates,
                template,
                problem,
                phase: Phase::Editing,
            }),
        )
    });
    Ok(json!({"dialog": "letteringFx"}))
}

/// Draw the Lettering window while it is open.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(Some(mut d)) = ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())) else { return };
    let mut close = false;
    let mut send = false;
    let mut cancel_job: Option<u64> = None;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let status = match d.phase {
        Phase::Sent(job) => app.session.execute("lettering.fx.status", json!({"job": job})).ok().and_then(|v| v["jobs"].get(0).cloned()),
        Phase::Editing => None,
    };
    let state = status.as_ref().and_then(|s| s["status"].as_str()).unwrap_or("").to_string();
    if state == "running" {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    egui::Window::new(tl!("Lettering")).id(egui::Id::unique("Lettering FX")).collapsible(false).resizable(false).centered(ctx).show(ctx, |ui| {
        ui.set_min_width(460.0);
        ui.label(egui::RichText::new(&d.summary).weak());
        ui.add_space(6.0);
        if let Phase::Sent(_) = d.phase {
            match state.as_str() {
                "running" => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tl!("Rendering the lettering in the animation app…"));
                    });
                }
                "done" => {
                    let out = status.as_ref().and_then(|s| s["out"].as_str()).unwrap_or_default();
                    ui.label(tlf!("Done: {out}", out));
                }
                "cancelled" => {
                    ui.label(tl!("Cancelled."));
                }
                _ => {
                    let e = status.as_ref().and_then(|s| s["error"].as_str()).unwrap_or("unknown error");
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if state == "running" { tl!("Cancel order") } else { tl!("Close") };
                let b = ui.button(label);
                elems.push(("letteringFx.cancel".into(), b.rect, label.into()));
                if b.clicked() {
                    match (&d.phase, state.as_str()) {
                        (Phase::Sent(job), "running") => cancel_job = Some(*job),
                        _ => close = true,
                    }
                }
            });
            return;
        }
        // the body scrolls (a long span or a small screen must not push Cancel / Send out of reach)
        let body_h = (ctx.content_rect().height() * 0.8 - 150.0).max(160.0);
        egui::ScrollArea::vertical().id_salt("lettering-body").max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.label(egui::RichText::new(tl!("Words (click to highlight)")).strong());
            egui::ScrollArea::vertical().id_salt("lettering-words").max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, w) in d.words.iter().enumerate() {
                        let on = d.heroes.get(i).copied().unwrap_or(false);
                        let r = ui.selectable_label(on, w);
                        elems.push((format!("letteringFx.word.{i}"), r.rect, w.clone()));
                        if r.clicked()
                            && let Some(h) = d.heroes.get_mut(i)
                        {
                            *h = !*h;
                        }
                    }
                });
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Style")).strong());
            if d.templates.is_empty() {
                ui.label(tl!("No templates found in the template bank."));
            }
            for t in d.templates.clone() {
                let selected = d.template.as_deref() == Some(t.id.as_str());
                let fill = if selected { app.tokens.accent.gamma_multiply(0.35) } else { ui.visuals().faint_bg_color };
                let card = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(&t.name).strong());
                    if !t.description.is_empty() {
                        ui.label(egui::RichText::new(&t.description).weak());
                    }
                });
                let r = card.response.interact(egui::Sense::click());
                elems.push((format!("letteringFx.template.{}", t.id), r.rect, t.name.clone()));
                if r.clicked() {
                    d.template = Some(t.id.clone());
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Words per line")).strong());
                let r = ui.add(egui::DragValue::new(&mut d.per_line).range(1..=12));
                elems.push(("letteringFx.perLine".into(), r.rect, "Words per line".into()));
            });
            let solo = ui.checkbox(&mut d.solo, tl!("Highlight words appear alone"));
            elems.push(("letteringFx.solo".into(), solo.rect, "Highlight words appear alone".into()));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Colors (the style's own unless changed)")).strong());
            for (i, (key, start)) in COLORS.iter().enumerate() {
                let label = color_label(i);
                ui.horizontal(|ui| {
                    ui.label(&label);
                    match d.colors.get_mut(i) {
                        Some(slot @ Some(_)) => {
                            if let Some(rgb) = slot.as_mut() {
                                let r = ui.color_edit_button_srgb(rgb);
                                elems.push((format!("letteringFx.color.{key}"), r.rect, label.clone()));
                            }
                            let x = ui.button(tl!("Reset"));
                            elems.push((format!("letteringFx.color.{key}.reset"), x.rect, "Reset".into()));
                            if x.clicked() {
                                *slot = None;
                            }
                        }
                        Some(slot) => {
                            let b = ui.button(tl!("Change"));
                            elems.push((format!("letteringFx.color.{key}"), b.rect, label.clone()));
                            if b.clicked() {
                                *slot = Some(*start);
                            }
                        }
                        None => {}
                    }
                });
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Fonts (the style's own unless changed)")).strong());
            for (i, key) in FONT_KEYS.iter().enumerate() {
                let label = font_label(i);
                let fams = d.fonts.clone().unwrap_or_default();
                let mut pick = d.font_picks.get(i).cloned().flatten();
                let mut start = false;
                ui.horizontal(|ui| {
                    ui.label(&label);
                    let mut reset = false;
                    match pick.as_mut() {
                        Some((family, style)) => {
                            let r = craft_ui::ComboBox::from_id_salt(("lettering-font", *key)).selected_text(family.clone()).width(170.0).show_ui(ui, |ui| {
                                for (name, styles) in fams.iter() {
                                    if ui.selectable_label(name == family, name).clicked() {
                                        *family = name.clone();
                                        if !styles.contains(style) {
                                            *style = styles.iter().find(|s| s.as_str() == "Regular").or_else(|| styles.first()).cloned().unwrap_or_default();
                                        }
                                    }
                                }
                            });
                            elems.push((format!("letteringFx.font.{key}"), r.response.rect, label.clone()));
                            let styles = fams.iter().find(|(n, _)| n == family).map(|(_, s)| s.clone()).unwrap_or_default();
                            let r =
                                craft_ui::ComboBox::from_id_salt(("lettering-font-style", *key)).selected_text(style.clone()).width(120.0).show_ui(ui, |ui| {
                                    for st in &styles {
                                        if ui.selectable_label(st == style, st).clicked() {
                                            *style = st.clone();
                                        }
                                    }
                                });
                            elems.push((format!("letteringFx.font.{key}.style"), r.response.rect, "Style".into()));
                            let x = ui.button(tl!("Reset"));
                            elems.push((format!("letteringFx.font.{key}.reset"), x.rect, "Reset".into()));
                            reset = x.clicked();
                        }
                        None => {
                            let b = ui.button(tl!("Change"));
                            elems.push((format!("letteringFx.font.{key}"), b.rect, label.clone()));
                            start = b.clicked();
                        }
                    }
                    if reset {
                        pick = None;
                    }
                });
                if start {
                    if d.fonts.is_none() {
                        d.fonts = Some(Arc::new(filmcraft_text::picker_families()));
                    }
                    pick = d.fonts.as_deref().and_then(|f| default_font(f));
                }
                if let Some(slot) = d.font_picks.get_mut(i) {
                    *slot = pick;
                }
            }
        });
        if let Some(p) = &d.problem {
            ui.colored_label(ui.visuals().error_fg_color, p);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let c = ui.button(tl!("Cancel"));
            elems.push(("letteringFx.close".into(), c.rect, "Cancel".into()));
            if c.clicked() {
                close = true;
            }
            let o =
                ui.add_enabled(d.template.is_some(), egui::Button::new(egui::RichText::new(tl!("Send")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
            elems.push(("letteringFx.send".into(), o.rect, "Send".into()));
            if o.clicked() {
                send = true;
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) && !matches!(d.phase, Phase::Sent(_)) {
        close = true;
    }
    if let Some(job) = cancel_job {
        let _ = app.session.execute("lettering.fx.cancel", json!({"job": job}));
    }
    if send && let Some(template) = d.template.clone() {
        let heroes: Vec<usize> = d.heroes.iter().enumerate().filter(|(_, h)| **h).map(|(i, _)| i).collect();
        let cores = cores_json(&d.colors);
        match app.session.execute(
            "lettering.fx.send",
            json!({"template": template, "heroes": heroes, "captions": d.captions, "cores": cores, "palavrasPorLinha": d.per_line, "destaqueSozinho": d.solo, "fontes": fontes_json(&d.font_picks)}),
        ) {
            Ok(v) => {
                d.problem = None;
                match v["job"].as_u64() {
                    Some(job) => d.phase = Phase::Sent(job),
                    None => d.problem = Some("the order was not started".into()),
                }
            }
            Err(e) => d.problem = Some(e.to_string()),
        }
    }
    ctx.data_mut(|m| m.insert_temp(draft_id(), if close { None } else { Some(d) }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_changed_fonts_are_sent() {
        assert!(fontes_json(&[None, None]).is_empty());
        let f = fontes_json(&[None, Some(("Host Grotesk".into(), "ExtraBold Italic".into()))]);
        assert_eq!(Value::Object(f), json!({"destaque": {"familia": "Host Grotesk", "estilo": "ExtraBold Italic"}}));
        let no_style = fontes_json(&[Some(("Inter".into(), String::new())), None]);
        assert_eq!(Value::Object(no_style), json!({"texto": {"familia": "Inter"}}));
    }

    #[test]
    fn the_offered_font_is_inter_regular_else_the_first() {
        let fams = vec![("Alfa".to_string(), vec!["Bold".to_string()]), ("Inter".to_string(), vec!["Bold".to_string(), "Regular".to_string()])];
        assert_eq!(default_font(&fams), Some(("Inter".into(), "Regular".into())));
        assert_eq!(default_font(&fams[..1]), Some(("Alfa".into(), "Bold".into())));
        assert_eq!(default_font(&[]), None);
    }

    #[test]
    fn only_the_changed_colours_are_sent_as_hex() {
        assert!(cores_json(&[None; 4]).is_empty());
        let c = cores_json(&[None, Some([0, 0, 0]), Some([255, 214, 10]), None]);
        assert_eq!(Value::Object(c), json!({"contorno": "#000000", "falada": "#ffd60a"}));
    }
}

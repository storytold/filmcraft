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
//! Automation ids: `letteringFx.word.<index>`, `letteringFx.template.<id>`, `letteringFx.send`,
//! `letteringFx.cancel` (closes, or cancels the running order), `letteringFx.close`.

use crate::widgets::CenteredDialog as _;
use serde_json::{Value, json};

use crate::FilmcraftApp;

#[derive(Clone, Debug)]
struct Template {
    id: String,
    name: String,
    description: String,
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
    templates: Vec<Template>,
    template: Option<String>,
    /// Why the menu is empty, or why the last send failed.
    problem: Option<String>,
    phase: Phase,
}

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
        m.insert_temp(draft_id(), Some(Draft { summary, captions, words, heroes: vec![false; n], templates, template, problem, phase: Phase::Editing }))
    });
    Ok(json!({"dialog": "letteringFx"}))
}

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
        ui.label(egui::RichText::new(tl!("Words (click to highlight)")).strong());
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
        match app.session.execute("lettering.fx.send", json!({"template": template, "heroes": heroes, "captions": d.captions})) {
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

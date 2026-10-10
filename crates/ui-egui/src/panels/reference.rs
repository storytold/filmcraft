//! Reference Monitor: a second view of the active sequence for comparing shots while grading. It
//! is either ganged to the Program monitor (follows the playhead) or parked on its own frame,
//! and shows the picture (Composite Video) or Lumetri Scopes of that frame.
//!
//! Automation ids: `reference.picture` (once a picture is shown), `reference.timecode`, `reference.stepBack`,
//! `reference.stepForward`, `reference.matchPlayhead`, `reference.gang`, `reference.display`,
//! `reference.display.<composite|scopes>`, `reference.scope.<kind>` (right-click menu in Scopes
//! mode), `reference.scopes.view.<kind>`. State: `ui.panels.reference`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_scopes::ScopeKind;
use filmcraft_time::{Tick, TimeDisplay, format_time};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::panels::panel_state::RefDisplay;
use crate::theme::Tokens;

/// The frame the Reference Monitor shows (None without a sequence).
pub fn frame(app: &FilmcraftApp) -> Option<i64> {
    let q = app.session.active_sequence()?;
    let rate = q.settings.frame_rate;
    let r = &app.ui.panels.reference;
    let t = if r.ganged { app.session.playhead() } else { Tick(r.time) };
    let last = rate.frame_at(q.duration()).max(1) - 1;
    Some(rate.frame_at(t).clamp(0, last))
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let Some(seq_id) = app.session.state.active_sequence else {
        crate::dock::placeholder(ui, rect, &t, tl!("(no sequences)"));
        return;
    };
    let Some(frame) = frame(app) else { return };
    let Some(q) = app.session.active_sequence().cloned() else { return };
    let rate = q.settings.frame_rate;
    let bar_h = 34.0;
    let area = Rect::from_min_max(rect.min + vec2(4.0, 4.0), pos2(rect.max.x - 4.0, rect.max.y - bar_h));
    let st = app.ui.panels.reference.clone();
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    match st.display {
        RefDisplay::Composite => {
            let (w, h) = (q.settings.width.max(1) as f32, q.settings.height.max(1) as f32);
            let pic = crate::panels::monitor::fit(area, w, h);
            ui.painter().rect_filled(pic, 0.0, t.monitor_bg);
            let ppp = ctx.pixels_per_point();
            let scale = crate::panels::monitor::quantize_scale((pic.width() * ppp / w).clamp(1.0 / 32.0, 1.0));
            let key = FrameKey { target: Target::Sequence(seq_id), frame, size: (scale * 1000.0) as u32, revision: app.session.revision, draft: false };
            let project = app.session.project.clone();
            app.frames.request(key, rate.tick_of(frame), scale, &project, 3);
            let tex = match app.frames.get(&key) {
                Some(img) => Some(app.texture_for(&ctx, "reference-monitor", key, &img)),
                None => app.texture_existing("reference-monitor").map(|x| x.0),
            };
            if let Some(tex) = tex {
                ui.painter().image(tex, pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                elems.push(("reference.picture".into(), pic, "reference frame".into()));
            }
        }
        RefDisplay::Scopes => {
            ui.painter().rect_filled(area, 0.0, Color32::BLACK);
            let sst = app.ui.panels.scopes.clone();
            match crate::panels::scopes::frame_signal(app, &ctx, "reference", frame, &sst, 3) {
                Some(sf) => crate::panels::scopes::draw(app, ui, area, &st.scopes, &sf, &sst, "reference.scopes"),
                None => crate::dock::placeholder(ui, area, &t, "…"),
            }
            elems.push(("reference.picture".into(), area, "reference scopes".into()));
            let resp = ui.interact(area, egui::Id::new("reference-area"), Sense::click());
            egui::Popup::context_menu(&resp).show(|ui| {
                for k in ScopeKind::ALL {
                    let on = st.scopes.contains(&k);
                    let r = ui.selectable_label(on, k.label());
                    elems.push((format!("reference.scope.{}", k.name()), r.rect, k.label().into()));
                    if r.clicked() {
                        let v = &mut app.ui.panels.reference.scopes;
                        if on {
                            if v.len() > 1 {
                                v.retain(|x| *x != k);
                            }
                        } else {
                            v.push(k);
                            v.sort_by_key(|x| ScopeKind::ALL.iter().position(|a| a == x));
                        }
                    }
                }
            });
        }
    }
    // ---- control bar: timecode · ◀ ▶ · Match Playhead · Gang · [Composite Video ▾]
    let bar = Rect::from_min_max(pos2(rect.min.x, area.max.y + 4.0), rect.max);
    let tc = format_time(rate.tick_of(frame), rate, q.settings.drop_frame, TimeDisplay::Timecode, 48000);
    let tr = ui.painter().text(pos2(bar.min.x + 10.0, bar.center().y), Align2::LEFT_CENTER, &tc, Tokens::timecode(), t.timecode);
    elems.push(("reference.timecode".into(), tr, tc.clone()));
    let mut step = 0i64;
    let mut x = tr.max.x + 16.0;
    let mut button = |ui: &mut egui::Ui, id: &str, label: &str, icon: Option<Icon>, on: bool, elems: &mut Vec<(String, Rect, String)>| -> bool {
        let galley_w = if icon.is_some() { 0.0 } else { ui.painter().layout_no_wrap(label.into(), Tokens::ui(11.5), t.text).size().x };
        let w = if icon.is_some() { 24.0 } else { galley_w + 16.0 };
        let r = Rect::from_min_size(pos2(x, bar.center().y - 11.0), vec2(w, 22.0));
        x = r.max.x + 4.0;
        let resp = ui.interact(r, egui::Id::new(("reference-btn", id.to_string())), Sense::click());
        if on {
            ui.painter().rect_filled(r, 4.0, t.pressed);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        match icon {
            Some(i) => icons::paint(ui.painter(), r.shrink(4.0), i, if on { t.icon_active } else { t.icon }),
            None => {
                ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.5), if on { t.tab_text_active } else { t.text });
            }
        }
        elems.push((id.to_string(), r, label.to_string()));
        resp.on_hover_text(label).clicked()
    };
    if button(ui, "reference.stepBack", tl!("Step Back 1 Frame"), Some(Icon::StepBack), false, &mut elems) {
        step = -1;
    }
    if button(ui, "reference.stepForward", tl!("Step Forward 1 Frame"), Some(Icon::StepFwd), false, &mut elems) {
        step = 1;
    }
    let matched = button(ui, "reference.matchPlayhead", tl!("Match Playhead"), None, false, &mut elems);
    let gang = button(ui, "reference.gang", tl!("Gang to Program"), None, st.ganged, &mut elems);
    // display dropdown at the right
    let dr = Rect::from_min_size(pos2(bar.max.x - 150.0, bar.center().y - 11.0), vec2(142.0, 22.0));
    let mut display = st.display;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(dr));
    let label = |d: RefDisplay| if d == RefDisplay::Composite { tl!("Composite Video") } else { tl!("Lumetri Scopes") };
    let cb = egui::ComboBox::from_id_salt("reference-display").selected_text(label(display)).width(134.0).show_ui(&mut child, |ui| {
        for d in [RefDisplay::Composite, RefDisplay::Scopes] {
            let r = ui.selectable_value(&mut display, d, label(d));
            elems.push((format!("reference.display.{}", if d == RefDisplay::Composite { "composite" } else { "scopes" }), r.rect, label(d).into()));
        }
    });
    elems.push(("reference.display".into(), cb.response.rect, label(display).into()));
    if st.ganged {
        ui.painter().rect_stroke(area, 0.0, Stroke::new(1.0, t.focus.gamma_multiply(0.5)), egui::StrokeKind::Inside);
    }
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    let r = &mut app.ui.panels.reference;
    r.display = display;
    if gang {
        r.ganged = !r.ganged;
        if !r.ganged {
            r.time = rate.tick_of(frame).0;
        }
    }
    if matched {
        r.ganged = false;
        r.time = app.session.playhead().0;
    }
    if step != 0 {
        r.ganged = false;
        r.time = rate.tick_of((frame + step).max(0)).0;
    }
}

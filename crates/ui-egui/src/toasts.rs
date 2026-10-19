//! Toasts: short notices in the lower right corner of the window, newest at the bottom, as Premiere
//! Pro shows them (an exported frame, a finished export queue, an auto-save error…). A toast goes
//! away by itself after a few seconds (an error stays longer) but never while the pointer rests on
//! one, or with its close button; one about a file offers to show that file in the file manager.
//!
//! Every toast is also an Events panel entry (`Session::toast`), so nothing is lost once it is gone.
//! Automation ids: `toast.N` (its text), `toast.N.close` and `toast.N.reveal`, N counting from the
//! oldest one shown.

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Toasts shown at once; the oldest gives way to a new one.
const MAX: usize = 4;
const WIDTH: f32 = 340.0;
/// Seconds a toast stays up; an error stays [`ERROR_SECONDS`].
const SECONDS: f64 = 6.0;
const ERROR_SECONDS: f64 = 10.0;
/// Seconds a toast takes to fade out at the end.
const FADE: f64 = 0.4;

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub message: String,
    pub error: bool,
    /// A file the toast is about (an exported frame), shown in the file manager on request.
    pub reveal: Option<String>,
    /// How many times in a row this message came in (a repeat refreshes the toast instead of
    /// stacking a copy).
    pub count: u32,
    /// When it went up (egui time); moved on while the pointer rests on the toasts.
    shown: f64,
}

impl Toast {
    fn seconds(&self) -> f64 {
        if self.error { ERROR_SECONDS } else { SECONDS }
    }
}

#[derive(Default)]
pub struct Toasts {
    /// Oldest first.
    pub items: Vec<Toast>,
    /// egui time of the last frame drawn with toasts up.
    last: Option<f64>,
}

impl Toasts {
    /// Put up a toast at egui time `now`.
    pub fn push(&mut self, message: impl Into<String>, error: bool, reveal: Option<String>, now: f64) {
        let message = message.into();
        if let Some(t) = self.items.last_mut().filter(|t| t.message == message && t.error == error && t.reveal == reveal) {
            t.count += 1;
            t.shown = now;
            return;
        }
        self.items.push(Toast { message, error, reveal, count: 1, shown: now });
        if self.items.len() > MAX {
            self.items.remove(0);
        }
    }

    /// Drop the toasts whose time is up at `now`; while `paused` (the pointer on them) their time
    /// does not run.
    fn tick(&mut self, now: f64, paused: bool) {
        let dt = self.last.map_or(0.0, |l| (now - l).max(0.0));
        if paused {
            for t in &mut self.items {
                t.shown += dt;
            }
        }
        self.items.retain(|t| now - t.shown < t.seconds());
        self.last = (!self.items.is_empty()).then_some(now);
    }
}

/// Draw the toasts above the status bar (`bottom`: its height) and handle their buttons.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context, bottom: f32) {
    let now = ctx.input(|i| i.time);
    let rect_id = egui::Id::new("toasts-rect");
    let paused = ctx.data(|m| m.get_temp::<egui::Rect>(rect_id)).zip(ctx.pointer_hover_pos()).is_some_and(|(r, p)| r.contains(p));
    app.toasts.tick(now, paused);
    if app.toasts.items.is_empty() {
        ctx.data_mut(|m| m.remove::<egui::Rect>(rect_id));
        return;
    }
    let tk = app.tokens;
    // a shade lighter than the panels, so the card stands out over a panel of the same colour
    let card_bg = tk.panel_bg.lerp_to_gamma(tk.text, 0.08);
    let mut close = None;
    let mut reveal = None;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let items = &app.toasts.items;
    let area = egui::Area::new(egui::Id::new("toasts"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -(bottom + 12.0)))
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing.y = 8.0;
            for (i, t) in items.iter().enumerate() {
                let left = t.seconds() - (now - t.shown);
                let alpha = if paused { 1.0 } else { (left / FADE).clamp(0.0, 1.0) as f32 };
                let fade = |c: egui::Color32| c.gamma_multiply(alpha);
                let accent = if t.error { tk.danger } else { tk.accent };
                let card = egui::Frame::new()
                    .fill(fade(card_bg))
                    .stroke(egui::Stroke::new(1.0, fade(tk.separator)))
                    .corner_radius(6)
                    .shadow(egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: egui::Color32::from_black_alpha((90.0 * alpha) as u8) })
                    .inner_margin(egui::Margin { left: 16, right: 8, top: 10, bottom: 10 })
                    .show(ui, |ui| {
                        ui.set_width(WIDTH - 24.0);
                        ui.horizontal_top(|ui| {
                            let (icon, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            icons::paint(ui.painter(), icon, Icon::Info, fade(accent));
                            let text = if t.count > 1 { format!("{} (×{})", t.message, t.count) } else { t.message.clone() };
                            let width = ui.available_width() - 26.0;
                            ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
                                ui.set_width(width);
                                ui.add(egui::Label::new(egui::RichText::new(text).font(Tokens::ui(12.0)).color(fade(tk.text))).wrap());
                            });
                            let (x, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(x, 3.0, fade(tk.hover));
                            }
                            icons::paint(ui.painter(), x.shrink(5.0), Icon::Close, fade(tk.text_dim));
                            elems.push((format!("toast.{i}.close"), x, tl!("Close").to_string()));
                            if resp.on_hover_text(tl!("Close")).clicked() {
                                close = Some(i);
                            }
                        });
                        if let Some(path) = &t.reveal {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add_space(22.0);
                                let label = tl!("Show in Folder");
                                let b = ui.add(egui::Button::new(egui::RichText::new(label).font(Tokens::ui(11.5)).color(fade(tk.text))));
                                elems.push((format!("toast.{i}.reveal"), b.rect, label.to_string()));
                                if b.clicked() {
                                    reveal = Some(path.clone());
                                }
                            });
                        }
                    });
                let r = card.response.rect;
                let stripe = egui::Rect::from_min_size(r.min, egui::vec2(3.0, r.height()));
                ui.painter().rect_filled(stripe, egui::CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, fade(accent));
                elems.push((format!("toast.{i}"), r, t.message.clone()));
            }
        });
    ctx.data_mut(|m| m.insert_temp(rect_id, area.response.rect));
    for (id, rect, label) in elems {
        app.auto.add(&id, rect, &label);
    }
    if let Some(path) = reveal {
        let shown = match app.hooks.open_path.as_mut() {
            Some(open) => open(&path, true),
            None => Err("no file manager".into()),
        };
        if let Err(e) = shown {
            let message = format!("{path}: {e}");
            app.toasts.push(message, true, None, now);
        }
    }
    if let Some(i) = close {
        app.toasts.items.remove(i);
    }
    // the fade and the time out need frames while nothing else moves
    ctx.request_repaint_after(std::time::Duration::from_millis(if paused { 250 } else { 50 }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_time_out_pause_under_the_pointer_and_keep_at_most_four() {
        let mut t = Toasts::default();
        t.push("a", false, None, 0.0);
        t.push("b", true, None, 0.0);
        t.tick(5.0, false);
        assert_eq!(t.items.len(), 2);
        // a message stays SECONDS, an error ERROR_SECONDS
        t.tick(7.0, false);
        assert_eq!(t.items.iter().map(|t| t.message.as_str()).collect::<Vec<_>>(), ["b"]);
        // the pointer on the toasts stops the clock
        t.tick(9.5, true);
        t.tick(12.0, true);
        // paused for 5 s in all, the error (up at 0 s for 10 s) is due at 15 s
        t.tick(14.9, false);
        assert_eq!(t.items.len(), 1);
        t.tick(15.1, false);
        assert!(t.items.is_empty());
        for (i, m) in ["1", "2", "3", "4", "5"].iter().enumerate() {
            t.push(*m, false, None, 20.0 + i as f64 * 0.1);
        }
        assert_eq!(t.items.iter().map(|t| t.message.as_str()).collect::<Vec<_>>(), ["2", "3", "4", "5"]);
    }

    #[test]
    fn a_repeated_message_counts_up_instead_of_stacking() {
        let mut t = Toasts::default();
        t.push("Audio during scrubbing off", false, None, 0.0);
        t.push("Audio during scrubbing off", false, None, 5.0);
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].count, 2);
        t.tick(10.0, false);
        assert_eq!(t.items.len(), 1, "the repeat restarted its time");
        t.push("Audio during scrubbing on", false, None, 10.0);
        assert_eq!(t.items.len(), 2);
    }
}

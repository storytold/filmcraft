//! Toasts: brief notices stacked at the bottom right of the window, above the status bar
//! (#2443). The engine raises them as [`filmcraft_engine::Event::Toast`] (`Session::toast`,
//! `Session::error_toast`); the UI adds its own with [`Toasts::push`]. An error toast carries a
//! red accent and stays longer than a plain one. Each goes away on its own, or when clicked.
//!
//! Automation ids: `toast.N` for the N-th toast shown (oldest first).

use std::time::Duration;

use egui::{Align2, Context, CornerRadius, Frame, Id, Label, Margin, Order, RichText, Sense, Stroke, vec2};

use crate::automation::Registry;
use crate::theme::Tokens;

/// How long a plain toast stays, in seconds.
const INFO_SECS: f64 = 4.5;
/// How long an error toast stays, in seconds: long enough to read what went wrong.
const ERROR_SECS: f64 = 8.0;
/// At most this many at once; an older one makes room for a new one.
const MAX_SHOWN: usize = 4;
/// The widest a toast grows before its text wraps.
const MAX_WIDTH: f32 = 380.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    /// UI clock time (seconds) the toast disappears at.
    pub until: f64,
}

/// The toasts on screen, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    /// Show `text` from `now` on. The same text twice in a row (a held key, a job that reports the
    /// same failure every tick) shows once, with a fresh timer.
    pub fn push(&mut self, now: f64, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.items.retain(|t| !(t.text == text && t.error == error));
        let life = if error { ERROR_SECS } else { INFO_SECS };
        self.items.push(Toast { text, error, until: now + life });
        while self.items.len() > MAX_SHOWN {
            self.items.remove(0);
        }
    }

    /// Drop the toasts whose time is up.
    pub fn expire(&mut self, now: f64) {
        self.items.retain(|t| t.until > now);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Draw the stack and drop what has expired or was clicked. `lift` is how far above the
    /// window's bottom edge the lowest toast sits (the status bar, and anything else parked in
    /// that corner).
    pub fn show(&mut self, ctx: &Context, t: &Tokens, auto: &mut Registry, lift: f32) {
        let now = ctx.input(|i| i.time);
        self.expire(now);
        if self.items.is_empty() {
            return;
        }
        // Wake up in time to take the oldest one down.
        if let Some(next) = self.items.iter().map(|t| t.until).reduce(f64::min) {
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).clamp(0.0, 1.0)));
        }
        let shadow = ctx.global_style().visuals.popup_shadow;
        let mut dismiss = None;
        egui::Area::new(Id::new("toasts")).anchor(Align2::RIGHT_BOTTOM, vec2(-12.0, -lift)).order(Order::Foreground).interactable(true).show(ctx, |ui| {
            ui.set_max_width(MAX_WIDTH);
            for (i, toast) in self.items.iter().enumerate() {
                let accent = if toast.error { t.danger } else { t.accent };
                let r = Frame::new()
                    .fill(t.panel_bg)
                    .stroke(Stroke::new(1.0, t.field_border))
                    .corner_radius(CornerRadius::same(6))
                    .shadow(shadow)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                            ui.painter().circle_filled(dot.center(), 4.0, accent);
                            ui.add(Label::new(RichText::new(&toast.text).font(Tokens::ui(12.0)).color(t.text)).wrap());
                        });
                    });
                let resp = r.response.interact(Sense::click());
                auto.add(&format!("toast.{i}"), resp.rect, &toast.text);
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    dismiss = Some(i);
                }
                if i + 1 < self.items.len() {
                    ui.add_space(6.0);
                }
            }
        });
        if let Some(i) = dismiss
            && i < self.items.len()
        {
            self.items.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_stay_longer_than_notes() {
        let mut t = Toasts::default();
        t.push(10.0, "Saved", false);
        t.push(10.0, "Disk full", true);
        t.expire(10.0 + INFO_SECS + 0.1);
        assert_eq!(t.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["Disk full"]);
        t.expire(10.0 + ERROR_SECS + 0.1);
        assert!(t.is_empty());
    }

    #[test]
    fn a_repeated_message_shows_once_with_a_fresh_timer() {
        let mut t = Toasts::default();
        t.push(0.0, "No cut points found", false);
        t.push(3.0, "No cut points found", false);
        assert_eq!(t.iter().count(), 1);
        t.expire(INFO_SECS + 1.0);
        assert_eq!(t.iter().count(), 1, "the second push restarted the clock");
    }

    #[test]
    fn the_stack_is_capped() {
        let mut t = Toasts::default();
        for i in 0..10 {
            t.push(0.0, format!("toast {i}"), i % 2 == 0);
        }
        assert_eq!(t.iter().count(), MAX_SHOWN);
        assert_eq!(t.iter().next().map(|t| t.text.as_str()), Some("toast 6"), "the oldest make room");
    }

    /// The toasts are drawn (and registered for automation) and go away when their time is up.
    #[test]
    fn toasts_are_drawn_and_expire() {
        let ctx = Context::default();
        let tokens = Tokens::for_kind(crate::theme::ThemeKind::Dark);
        let mut toasts = Toasts::default();
        let mut auto = Registry::default();
        toasts.push(1.0, "Project upgraded to schema 4", false);
        toasts.push(1.0, "Mask track failed: no features", true);
        let run = |ctx: &Context, toasts: &mut Toasts, auto: &mut Registry, time: f64| {
            auto.begin_frame();
            let raw =
                egui::RawInput { time: Some(time), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0))), ..Default::default() };
            // No renderer here: drop the pass output, clearing its texture deltas so epaint's drop
            // guard does not panic on the font texture.
            let mut out = ctx.run_ui(raw, |ctx| toasts.show(ctx, &tokens, auto, 32.0));
            out.textures_delta.clear();
        };
        run(&ctx, &mut toasts, &mut auto, 1.0);
        let ids: Vec<_> = auto.elements.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids, ["toast.0", "toast.1"]);
        assert!(auto.elements.iter().all(|e| e.rect[2] > 0.0 && e.rect[3] > 0.0), "each toast has a size: {:?}", auto.elements);
        run(&ctx, &mut toasts, &mut auto, 1.0 + INFO_SECS + 0.1);
        assert_eq!(toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["Mask track failed: no features"]);
        run(&ctx, &mut toasts, &mut auto, 1.0 + ERROR_SECS + 0.1);
        assert!(toasts.is_empty());
        assert!(auto.elements.is_empty());
    }
}

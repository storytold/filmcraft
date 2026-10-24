//! Keeps the window's input method on for a moment after the text that used it lets go.
//!
//! Turning the input method off makes winit on X11 destroy the window's input context and create
//! another. When that happens while the input method (fcitx, ibus) is still handling a key, as when
//! Esc ends the editing of a title or a field, the new context never receives a key again: typing
//! stops working in every field until FilmCraft is restarted. So the input method stays on until
//! no key is held and [`HOLD`] has passed since the last one, and is turned off then.

use egui::output::IMEOutput;

/// How long the input method stays on after the text lets go of it and the keys are released.
pub(crate) const HOLD: f64 = 0.3;

#[derive(Default)]
pub(crate) struct ImeHold {
    /// The input method the last text asked for, and when it last did (or a key was last held).
    last: Option<(IMEOutput, f64)>,
}

impl ImeHold {
    /// The input method to ask the window for, given what this frame asked for at time `now`
    /// (seconds) with `keys_down` held; and, while it is held on, in how many seconds to look
    /// again.
    pub(crate) fn apply(&mut self, ime: Option<IMEOutput>, now: f64, keys_down: bool) -> (Option<IMEOutput>, Option<f64>) {
        if let Some(ime) = ime {
            self.last = Some((ime, now));
            return (Some(ime), None);
        }
        let Some((held, mut since)) = self.last else { return (None, None) };
        if keys_down || !now.is_finite() || !since.is_finite() {
            since = now;
        }
        let left = HOLD - (now - since);
        if keys_down || left > 0.0 {
            self.last = Some((held, since));
            // no new composition is interrupted: nothing is typed into it any more
            (Some(IMEOutput { should_interrupt_composition: false, ..held }), Some(left.clamp(0.01, HOLD)))
        } else {
            self.last = None;
            (None, None)
        }
    }

    /// Hold the input method of this frame's output on, as [`ImeHold::apply`] decides.
    pub(crate) fn hold(&mut self, ctx: &egui::Context) {
        let (now, keys_down) = ctx.input(|i| (i.time, !i.keys_down.is_empty()));
        let ime = ctx.output(|o| o.ime);
        let (ime, again) = self.apply(ime, now, keys_down);
        ctx.output_mut(|o| o.ime = ime);
        if let Some(s) = again {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(s));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ime() -> IMEOutput {
        IMEOutput { purpose: Default::default(), rect: egui::Rect::ZERO, cursor_rect: egui::Rect::ZERO, should_interrupt_composition: true }
    }

    #[test]
    fn stays_on_while_a_key_is_held_and_a_moment_after() {
        let mut h = ImeHold::default();
        assert_eq!(h.apply(None, 0.0, false), (None, None), "never on: stays off");
        assert!(h.apply(Some(ime()), 1.0, false).0.is_some_and(|i| i.should_interrupt_composition));
        // Esc ends the edit: the text no longer asks, the key is still down
        let (on, again) = h.apply(None, 1.1, true);
        assert!(on.is_some_and(|i| !i.should_interrupt_composition), "held on, not restarted");
        assert!(again.is_some());
        assert!(h.apply(None, 2.0, true).0.is_some(), "as long as the key is held");
        // released at 2.0: on until HOLD after that
        assert!(h.apply(None, 2.0 + HOLD * 0.5, false).0.is_some());
        assert_eq!(h.apply(None, 2.0 + HOLD + 0.01, false), (None, None));
        assert_eq!(h.apply(None, 9.0, false), (None, None), "and stays off");
    }

    #[test]
    fn a_text_asking_again_takes_it_over() {
        let mut h = ImeHold::default();
        h.apply(Some(ime()), 0.0, false);
        let mut other = ime();
        other.rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(5.0, 5.0));
        assert_eq!(h.apply(Some(other), 0.1, false).0, Some(other));
        assert!(h.apply(None, 0.2, false).0.is_some_and(|i| i.rect == other.rect));
    }

    #[test]
    fn odd_clocks_do_not_hold_it_on_forever() {
        let mut h = ImeHold::default();
        h.apply(Some(ime()), f64::NAN, false);
        assert!(h.apply(None, 1.0, false).0.is_some());
        assert_eq!(h.apply(None, 1.0 + HOLD + 0.01, false), (None, None));
    }
}

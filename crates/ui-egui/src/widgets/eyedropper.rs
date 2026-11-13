//! The eyedropper button colour controls show beside their swatch, and the colour formats the
//! armed eyedropper writes.

use crate::icons::{self, Icon};
use crate::state::Eyedropper;
use crate::theme::Tokens;

/// Draws the eyedropper for a colour control; `armed` lights it up while the next click in the
/// Program monitor fills that colour. Returns the button's response.
pub fn button(ui: &mut egui::Ui, t: &Tokens, armed: bool, tooltip: &str) -> egui::Response {
    icons::button(ui, Icon::Eyedropper, 18.0, armed, t, tooltip)
}

/// Arms `target` in `slot`, or disarms it when it was already armed.
pub fn toggle(slot: &mut Option<Eyedropper>, target: Eyedropper) {
    *slot = if slot.as_ref() == Some(&target) { None } else { Some(target) };
}

/// `#rrggbb`, or `#rrggbbaa` when `alpha` and the colour is not fully opaque — what
/// `graphics.set` stores.
pub fn hex_of([r, g, b, a]: [u8; 4], alpha: bool) -> String {
    if alpha && a != 255 { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_of_writes_the_colour_a_graphics_property_stores() {
        assert_eq!(hex_of([255, 128, 0, 255], false), "#ff8000");
        assert_eq!(hex_of([255, 128, 0, 255], true), "#ff8000");
        assert_eq!(hex_of([255, 128, 0, 64], true), "#ff800040");
        assert_eq!(hex_of([0, 0, 0, 0], false), "#000000");
    }

    #[test]
    fn toggling_twice_leaves_no_armed_eyedropper() {
        let mut slot = None;
        let target = Eyedropper::Graphic { clip: 7, layer: 0, prop: "fill_color".into(), alpha: false };
        toggle(&mut slot, target.clone());
        assert_eq!(slot.as_ref(), Some(&target));
        toggle(&mut slot, target);
        assert_eq!(slot, None);
    }
}

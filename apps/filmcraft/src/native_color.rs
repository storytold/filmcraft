//! Nonblocking macOS custom-color selection through safe AppKit bindings.
//! The panel edits a draft; FilmCraft's Add/Save/Cancel buttons commit or discard it.
use filmcraft_ui_egui::widgets::color_picker::{NativeColorPicker, NativeColorSession, NativeColorState};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSColor, NSColorPanel, NSColorSpace};
use std::{cell::Cell, rc::Rc};

struct ColorSession {
    panel: Retained<NSColorPanel>,
    active: Rc<Cell<u64>>,
    id: u64,
    text_menu: crate::native_menu::NativeTextMenu,
}

impl std::fmt::Debug for ColorSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorSession").field("id", &self.id).finish_non_exhaustive()
    }
}

impl ColorSession {
    fn owns_panel(&self) -> bool {
        self.active.get() == self.id
    }
}

impl Drop for ColorSession {
    fn drop(&mut self) {
        // NSColorPanel is shared. An older draft must never close a newer draft's panel.
        if self.owns_panel() {
            if let Err(error) = (self.text_menu)(false) {
                log::warn!("Cannot restore FilmCraft Edit menu: {error}");
            }
            self.panel.orderOut(None);
        }
    }
}

fn color(rgb: [u8; 3]) -> Retained<NSColor> {
    let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

fn channel(value: f64) -> Result<u8, String> {
    if !value.is_finite() {
        return Err("The selected color has an invalid component".into());
    }
    Ok((value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

impl NativeColorSession for ColorSession {
    fn poll(&self) -> Result<NativeColorState, String> {
        if !self.owns_panel() {
            return Ok(NativeColorState { color: None, open: false });
        }
        (self.text_menu)(self.panel.isKeyWindow())?;
        let rgb = self.panel.color().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).ok_or("Cannot convert the selected color to sRGB")?;
        let rgb = [channel(rgb.redComponent())?, channel(rgb.greenComponent())?, channel(rgb.blueComponent())?];
        Ok(NativeColorState { color: Some(rgb), open: self.panel.isVisible() })
    }

    fn set_color(&self, rgb: [u8; 3]) -> Result<(), String> {
        if !self.owns_panel() {
            return Err("This color picker belongs to an earlier draft".into());
        }
        self.panel.setColor(&color(rgb));
        Ok(())
    }
}

pub fn picker(text_menu: crate::native_menu::NativeTextMenu) -> NativeColorPicker {
    let active = Rc::new(Cell::new(0_u64));
    Box::new(move |rgb| {
        let mtm = MainThreadMarker::new().ok_or("The macOS color picker must open on the main thread")?;
        let id = active.get().checked_add(1).ok_or("Too many color picker requests")?;
        // Complete fallible setup before changing AppKit's shared panel or its owner.
        text_menu(true)?;
        let panel = NSColorPanel::sharedColorPanel(mtm);
        panel.setShowsAlpha(false);
        panel.setContinuous(true);
        panel.setColor(&color(rgb));
        active.set(id);
        panel.makeKeyAndOrderFront(None);
        Ok(Rc::new(ColorSession { panel, active: active.clone(), id, text_menu: text_menu.clone() }))
    })
}

#[cfg(test)]
mod tests {
    use super::channel;

    #[test]
    fn srgb_channels_are_rounded_bounded_and_finite() {
        assert_eq!(channel(0.5).unwrap(), 128);
        assert_eq!(channel(-1.0).unwrap(), 0);
        assert_eq!(channel(2.0).unwrap(), 255);
        assert!(channel(f64::NAN).is_err());
        assert!(channel(f64::INFINITY).is_err());
        for c in 0..=255_u8 {
            assert_eq!(channel(f64::from(c) / 255.0).unwrap(), c);
        }
    }
}

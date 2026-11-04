//! Reusable opaque sRGB color editor with an optional native host picker.
//! Callers own dialogs, palettes and project commands.
//! The fallback reuses egui's established picker. Opaque label bytes stay in sRGB;
//! floating-point effect colors and their alpha handling are outside this editor.
use crate::{HostHooks, automation::Registry};
use egui::{Color32, Sense, Stroke, vec2};
use filmcraft_project::HexColor;
use serde::{Deserialize, Serialize};
use std::rc::Rc;

/// A native picker edits only a draft. Callers still own Apply/Cancel and project commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeColorState {
    pub color: Option<[u8; 3]>,
    pub open: bool,
}

/// Main-thread host session. Dropping the last reference closes its own panel.
pub trait NativeColorSession: std::fmt::Debug {
    fn poll(&self) -> Result<NativeColorState, String>;
    fn set_color(&self, color: [u8; 3]) -> Result<(), String>;
}

pub type NativeColorPicker = Box<dyn FnMut([u8; 3]) -> Result<Rc<dyn NativeColorSession>, String>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColorDraft {
    pub hex: String,
    #[serde(default = "default_rgb")]
    rgb: [u8; 3],
    #[serde(skip)]
    native: Option<Rc<dyn NativeColorSession>>,
    #[serde(skip)]
    native_requested: bool,
    #[serde(skip)]
    native_last: Option<[u8; 3]>,
    #[serde(skip)]
    native_error: Option<String>,
}

impl ColorDraft {
    pub fn new(rgb: [u8; 3]) -> Self {
        Self { hex: HexColor(rgb).hex().trim_start_matches('#').to_string(), rgb, native: None, native_requested: false, native_last: None, native_error: None }
    }

    /// End native editing when a retained palette draft returns to its list.
    pub fn close_native(&mut self) {
        self.native = None;
    }

    fn open_native(&mut self, hooks: &mut HostHooks) {
        let Some(open) = &mut hooks.pick_color else { return };
        self.close_native();
        self.native_requested = true;
        let rgb = self.preview_rgb();
        match open(rgb) {
            Ok(session) => {
                self.native = Some(session);
                self.native_last = Some(rgb);
                self.native_error = None;
            }
            Err(error) => self.native_error = Some(error),
        }
    }

    fn poll_native(&mut self) {
        let Some(session) = &self.native else { return };
        match session.poll() {
            Ok(state) => {
                if let Some(rgb) = state.color
                    && self.native_last != Some(rgb)
                {
                    self.hex = HexColor(rgb).hex().trim_start_matches('#').to_string();
                    self.rgb = rgb;
                    self.native_last = Some(rgb);
                }
                // A native panel can hide when the app deactivates. Keep its draft/session;
                // Apply/Cancel owns its lifetime and Choose Color can bring it back.
                if state.color.is_none() && !state.open {
                    self.close_native();
                }
            }
            Err(error) => {
                self.native_error = Some(error);
                self.close_native();
            }
        }
    }

    pub fn value(&self) -> Option<HexColor> {
        HexColor::parse(&self.hex)
    }

    /// An incomplete hex edit keeps the most recent valid selection visible.
    fn preview_rgb(&self) -> [u8; 3] {
        self.value().map_or(self.rgb, |color| color.0)
    }
}

fn default_rgb() -> [u8; 3] {
    [128, 128, 128]
}

pub fn show(ui: &mut egui::Ui, auto: &mut Registry, hooks: &mut HostHooks, prefix: &str, draft: &mut ColorDraft) {
    if hooks.pick_color.is_some() && !draft.native_requested {
        draft.open_native(hooks);
    }
    draft.poll_native();
    if hooks.pick_color.is_some() && draft.native_error.is_none() {
        ui.horizontal(|ui| {
            preview_and_hex(ui, auto, prefix, draft);
            let response = ui.button(tl!("Choose Color…"));
            auto.add_response(&format!("{prefix}.native"), &response, tl!("Choose color with system picker"));
            if response.clicked() {
                draft.open_native(hooks);
            }
        });
        // Keep observing focus even while AppKit hides its panel. Reactivating that
        // native window need not produce a focus event for eframe's main window.
        if draft.native.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        if draft.value().is_none() {
            ui.colored_label(ui.visuals().error_fg_color, tl!("Enter six hex digits, for example FF80C0."));
        }
        return;
    }
    if let Some(error) = &draft.native_error {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::fmt(tl!("System color picker unavailable: {error}"), &[("error", error)]));
    }
    // Match FilmCraft's other opaque sRGB controls without converting label bytes
    // through the linear floating-point representation used by effect parameters.
    let [r, g, b] = draft.preview_rgb();
    let mut color = Color32::from_rgb(r, g, b);
    let picker = ui.push_id(prefix, |ui| egui::color_picker::color_picker_color32(ui, &mut color, egui::color_picker::Alpha::Opaque));
    auto.add_response(&format!("{prefix}.picker"), &picker.response, tl!("Color picker"));
    if picker.inner {
        draft.rgb = [color.r(), color.g(), color.b()];
        draft.hex = HexColor(draft.rgb).hex().trim_start_matches('#').to_string();
    }
    hex_field(ui, auto, prefix, draft);
    if draft.value().is_none() {
        ui.colored_label(ui.visuals().error_fg_color, tl!("Enter six hex digits, for example FF80C0."));
    }
}

fn preview_and_hex(ui: &mut egui::Ui, auto: &mut Registry, prefix: &str, draft: &mut ColorDraft) {
    let [r, g, b] = draft.preview_rgb();
    let color = Color32::from_rgb(r, g, b);
    let (rect, _) = ui.allocate_exact_size(vec2(68.0, 48.0), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, color);
    ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, ui.visuals().text_color()), egui::StrokeKind::Inside);
    hex_field(ui, auto, prefix, draft);
}

fn hex_field(ui: &mut egui::Ui, auto: &mut Registry, prefix: &str, draft: &mut ColorDraft) {
    ui.label(tl!("Hex"));
    let response = super::accessible_name(
        ui.add(egui::TextEdit::singleline(&mut draft.hex).font(egui::TextStyle::Monospace).desired_width(58.0).char_limit(7)),
        tl!("Hex color"),
    );
    auto.add_response(&format!("{prefix}.hex"), &response, tl!("Hex color"));
    if response.changed() {
        if let Some(hex) = draft.hex.strip_prefix('#') {
            draft.hex = hex.to_string();
        }
        if let Some(color) = draft.value() {
            draft.rgb = color.0;
            if let Some(session) = &draft.native {
                if let Err(error) = session.set_color(color.0) {
                    draft.native_error = Some(error);
                    draft.close_native();
                } else {
                    draft.native_last = Some(color.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_drafts_keep_exact_srgb_bytes_without_hsv_conversion() {
        let draft: ColorDraft = serde_json::from_value(serde_json::json!({"hex":"12ABCD","hue":0.2,"saturation":1.0,"brightness":0.8})).unwrap();
        assert_eq!(draft.value(), Some(HexColor([18, 171, 205])));
        assert!(draft.native.is_none());
    }
}

//! Reusable opaque sRGB color editor. Callers own dialogs, palettes and project commands.
//! HSV operates on sRGB channel values, not linear-light channels: radial saturation
//! therefore matches the familiar selection wheel without an oversized white center.
use crate::automation::Registry;
use egui::{Color32, Sense, Stroke, vec2};
use filmcraft_project::HexColor;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColorDraft {
    pub hex: String,
    pub hue: f32,
    pub saturation: f32,
    pub brightness: f32,
}

impl ColorDraft {
    pub fn new(rgb: [u8; 3]) -> Self {
        let mut draft = Self { hex: HexColor(rgb).hex().trim_start_matches('#').to_string(), hue: 0.0, saturation: 0.0, brightness: 1.0 };
        draft.set_rgb(rgb);
        draft
    }

    fn set_rgb(&mut self, rgb: [u8; 3]) {
        let [r, g, b] = rgb.map(|v| f32::from(v) / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        if delta > 0.0 {
            let sector = if max == r {
                (g - b) / delta
            } else if max == g {
                (b - r) / delta + 2.0
            } else {
                (r - g) / delta + 4.0
            };
            self.hue = (sector / 6.0).rem_euclid(1.0);
        }
        if max > 0.0 {
            self.saturation = delta / max;
        }
        self.brightness = max;
    }

    fn update_hex(&mut self) {
        let color = hsv_color(self.hue, self.saturation, self.brightness);
        self.hex = HexColor([color.r(), color.g(), color.b()]).hex().trim_start_matches('#').to_string();
    }

    pub fn value(&self) -> Option<HexColor> {
        HexColor::parse(&self.hex)
    }
}

fn unit(value: f32) -> f32 {
    if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 }
}

fn hsv_color(hue: f32, saturation: f32, brightness: f32) -> Color32 {
    let h = unit(hue).rem_euclid(1.0) * 6.0;
    let v = unit(brightness);
    let chroma = v * unit(saturation);
    let x = chroma * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let rgb = match h as u8 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    let [r, g, b] = rgb.map(|channel| ((channel + v - chroma).clamp(0.0, 1.0) * 255.0).round() as u8);
    Color32::from_rgb(r, g, b)
}

pub fn show(ui: &mut egui::Ui, auto: &mut Registry, prefix: &str, draft: &mut ColorDraft) {
    draft.hue = unit(draft.hue);
    draft.saturation = unit(draft.saturation);
    draft.brightness = unit(draft.brightness);
    ui.horizontal(|ui| {
        wheel(ui, auto, prefix, draft);
        ui.vertical(|ui| {
            let color =
                draft.value().map(|c| Color32::from_rgb(c.0[0], c.0[1], c.0[2])).unwrap_or_else(|| hsv_color(draft.hue, draft.saturation, draft.brightness));
            let (rect, _) = ui.allocate_exact_size(vec2(68.0, 48.0), Sense::hover());
            ui.painter().rect_filled(rect, 4.0, color);
            ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, ui.visuals().text_color()), egui::StrokeKind::Inside);
            ui.label("Hex");
            let response = ui.add(egui::TextEdit::singleline(&mut draft.hex).font(egui::TextStyle::Monospace).desired_width(58.0).char_limit(7));
            auto.add_response(&format!("{prefix}.hex"), &response, "Hex color");
            if response.changed() {
                if let Some(hex) = draft.hex.strip_prefix('#') {
                    draft.hex = hex.to_string();
                }
                if let Some(color) = draft.value() {
                    draft.set_rgb(color.0);
                }
            }
        });
    });
    for (id, label, scale, suffix) in [("hue", "Hue", 360.0, "°"), ("saturation", "Saturation", 100.0, "%"), ("brightness", "Brightness", 100.0, "%")] {
        let field = match id {
            "hue" => &mut draft.hue,
            "saturation" => &mut draft.saturation,
            _ => &mut draft.brightness,
        };
        let mut value = *field * scale;
        // Format the display without rounding the stored value or an entered hex color.
        let response = ui.add(egui::Slider::new(&mut value, 0.0..=scale).text(label).suffix(suffix).custom_formatter(|v, _| format!("{v:.0}")));
        auto.add_response(&format!("{prefix}.{id}"), &response, label);
        if response.changed() {
            *field = value / scale;
            draft.update_hex();
        }
    }
    if draft.value().is_none() {
        ui.colored_label(ui.visuals().error_fg_color, "Enter six hex digits, for example FF80C0.");
    }
}

fn wheel(ui: &mut egui::Ui, auto: &mut Registry, prefix: &str, draft: &mut ColorDraft) {
    let (rect, response) = ui.allocate_exact_size(vec2(200.0, 200.0), Sense::click_and_drag());
    auto.add_response(&format!("{prefix}.wheel"), &response, "Color wheel");
    let center = rect.center();
    let radius = 94.0;
    let mut mesh = egui::Mesh::default();
    for ring in 0..=16_u32 {
        let saturation = ring as f32 / 16.0;
        for step in 0..=96_u32 {
            let h = step as f32 / 96.0;
            let angle = h * std::f32::consts::TAU;
            mesh.colored_vertex(center + vec2(angle.cos(), angle.sin()) * radius * saturation, hsv_color(h, saturation, 1.0));
            if ring > 0 && step > 0 {
                let current = ring * 97 + step;
                mesh.add_triangle(current - 98, current - 97, current);
                mesh.add_triangle(current - 98, current, current - 1);
            }
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let delta = pointer - center;
        if delta.length() > 0.0 {
            draft.hue = (delta.y.atan2(delta.x) / std::f32::consts::TAU).rem_euclid(1.0);
        }
        draft.saturation = (delta.length() / radius).clamp(0.0, 1.0);
        draft.update_hex();
    }
    let angle = draft.hue * std::f32::consts::TAU;
    let selected = center + vec2(angle.cos(), angle.sin()) * radius * draft.saturation;
    ui.painter().circle_stroke(selected, 5.0, Stroke::new(3.0, Color32::BLACK));
    ui.painter().circle_stroke(selected, 5.0, Stroke::new(1.0, Color32::WHITE));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wheel_uses_srgb_saturation_and_round_trips_channels() {
        assert_eq!(hsv_color(0.0, 0.5, 1.0), Color32::from_rgb(255, 128, 128));
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(17) {
                    let mut draft = ColorDraft::new([r, g, b]);
                    draft.update_hex();
                    assert_eq!(draft.value(), Some(HexColor([r, g, b])));
                }
            }
        }
    }
}

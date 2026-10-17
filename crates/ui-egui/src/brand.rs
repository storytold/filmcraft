//! Plain-text identity for this experimental derivative; no upstream brand artwork.

pub fn paint_wordmark(ui: &egui::Ui, left_center: egui::Pos2, height: f32, dark: bool) -> Option<egui::Rect> {
    let color = if dark { egui::Color32::WHITE } else { egui::Color32::BLACK };
    let galley = ui.painter().layout_no_wrap("DURATION LAB".into(), egui::FontId::proportional(height), color);
    let rect = egui::Rect::from_min_size(egui::pos2(left_center.x, left_center.y - height / 2.0), galley.size());
    ui.painter().galley(rect.min, galley, color);
    Some(rect)
}

//! Original theme favorite star artwork, MIT OR Apache-2.0.
pub(crate) fn star(ui: &mut egui::Ui, favorite: bool, visible: bool, accent: egui::Color32, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if visible {
        let color = if favorite { accent } else { ui.visuals().weak_text_color() };
        let center = rect.center();
        let points: Vec<_> = (0..10)
            .map(|i| {
                let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                center + egui::vec2(angle.cos(), angle.sin()) * if i % 2 == 0 { 7.0 } else { 3.0 }
            })
            .collect();
        if favorite {
            for (a, b) in points.iter().zip(points.iter().cycle().skip(1)).take(10) {
                ui.painter().add(egui::Shape::convex_polygon(vec![center, *a, *b], color, egui::Stroke::NONE));
            }
        } else {
            ui.painter().add(egui::Shape::closed_line(points, egui::Stroke::new(1.2, color)));
        }
    }
    response.on_hover_text(label)
}

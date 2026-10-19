//! The ArtCraft wordmark (first-party trademark, used with permission; see `docs/brand/` and
//! AGENTS.md §1.8), decoded once and kept as a texture.

const LOGO_LIGHT_INK: &[u8] = include_bytes!("../../../docs/brand/artcraft-logo-white.png");
const LOGO_DARK_INK: &[u8] = include_bytes!("../../../docs/brand/artcraft-logo.png");

/// The wordmark for a dark (`dark = true`) or light UI: light ink on dark themes. Its aspect
/// ratio is width / height of the returned texture.
pub fn wordmark(ctx: &egui::Context, dark: bool) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("artcraft-wordmark", dark));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(t);
    }
    let bytes = if dark { LOGO_LIGHT_INK } else { LOGO_DARK_INK };
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    // Large enough to stay sharp at 2x for the sizes we draw (≤ 320 pt wide).
    let img = img.resize(1024, 1024, image::imageops::FilterType::Lanczos3).to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    let tex = ctx.load_texture("artcraft-wordmark", color, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    Some(tex)
}

/// Draw the wordmark `height` points tall with its left edge at `left_center` (vertically centred).
/// Returns the drawn rect.
pub fn paint_wordmark(ui: &egui::Ui, left_center: egui::Pos2, height: f32, dark: bool) -> Option<egui::Rect> {
    let tex = wordmark(ui.ctx(), dark)?;
    let [w, h] = tex.size();
    let width = height * w as f32 / h.max(1) as f32;
    let r = egui::Rect::from_min_size(egui::pos2(left_center.x, left_center.y - height / 2.0), egui::vec2(width, height));
    ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    Some(r)
}

use super::{
    Affine, ClipId, FilmcraftApp, ItemKind, LayerView, Pos2, Rect, Vec2, eval_layer, layer_indices, layer_local_bounds, layer_matrix, pos2, selected_graphic,
};

pub(super) fn sp(a: &Affine, x: f32, y: f32) -> Pos2 {
    let v = a.apply(Vec2::new(x as f64, y as f64));
    pos2(v.x as f32, v.y as f32)
}

pub(super) fn screen_scale(a: &Affine) -> f32 {
    (a.a * a.d - a.b * a.c).abs().sqrt() as f32
}

/// Layers of the graphic clips visible at the playhead, front-most first. A layer whose
/// visibility is off is not in the picture, so it has no box and cannot be clicked either.
/// Sequence pixels → screen points on the monitor picture `pic`. The picture shows the frame at its
/// display aspect, so with non-square sequence pixels the two axes scale differently.
pub(crate) fn frame_to_screen(pic: Rect, frame: (u32, u32)) -> Affine {
    let (kx, ky) = (pic.width() as f64 / frame.0.max(1) as f64, pic.height() as f64 / frame.1.max(1) as f64);
    Affine::translate(pic.min.x as f64, pic.min.y as f64).then_apply(&Affine::scale(kx, ky))
}

pub fn visible_layers(app: &FilmcraftApp, pic: Rect, frame: (u32, u32)) -> Vec<LayerView> {
    let Some(seq) = app.session.active_sequence() else { return Vec::new() };
    let t = app.session.playhead();
    let screen = frame_to_screen(pic, frame);
    let mut out = Vec::new();
    for tr in seq.video_tracks.iter().rev() {
        if !tr.enabled {
            continue;
        }
        let Some(it) = tr.item_at(t) else { continue };
        if !it.enabled {
            continue;
        }
        let Some(ItemKind::Graphic { width, height, .. }) = app.session.project.item(it.item).map(|p| &p.kind) else { continue };
        let size = (*width, *height);
        let mt = it.source_time_at(t);
        let base = screen.then_apply(&filmcraft_render::motion_matrix(seq, it, size, None, mt));
        let idx = layer_indices(&it.effects);
        for (li, &ei) in idx.iter().enumerate().rev() {
            if let Some(spec) = eval_layer(&it.effects[ei], mt, size).filter(|s| s.enabled) {
                let local = layer_local_bounds(&spec);
                out.push(LayerView { clip: it.id, layer: li, to_screen: base.then_apply(&layer_matrix(&spec)), canvas_to_screen: base, spec, local });
            }
        }
    }
    out
}

/// Screen → graphic-canvas mapping for new layers: the selected graphic clip under the playhead
/// (its Motion applies), else the sequence frame.
pub(super) fn canvas_target(app: &FilmcraftApp, pic: Rect, frame: (u32, u32)) -> (Option<ClipId>, Affine) {
    let screen = frame_to_screen(pic, frame);
    let t = app.session.playhead();
    if let Some((c, it)) = selected_graphic(app)
        && it.range().contains(t)
        && let Some(seq) = app.session.active_sequence()
        && let Some(ItemKind::Graphic { width, height, .. }) = app.session.project.item(it.item).map(|p| &p.kind)
    {
        let m = screen.then_apply(&filmcraft_render::motion_matrix(seq, &it, (*width, *height), None, it.source_time_at(t)));
        return (Some(c), m);
    }
    (None, screen)
}

pub(super) fn to_canvas(m: &Affine, p: Pos2) -> Vec2 {
    m.inverse().map(|i| i.apply(Vec2::new(p.x as f64, p.y as f64))).unwrap_or_default()
}

/// The layer's anchor point on screen.
pub(super) fn anchor_screen(v: &LayerView) -> Pos2 {
    sp(&v.to_screen, v.spec.transform.anchor.x as f32, v.spec.transform.anchor.y as f32)
}

/// A screen offset in the units `m` maps to the screen (its translation left out).
pub(crate) fn unscale(m: &Affine, off: egui::Vec2) -> Vec2 {
    Affine { e: 0.0, f: 0.0, ..*m }.inverse().map(|i| i.apply(Vec2::new(off.x as f64, off.y as f64))).unwrap_or_default()
}

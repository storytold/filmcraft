use super::layout::Layout;
use super::waveforms::draw_waveform;
use super::{
    Align2, Arc, ClipId, Color32, DuplicateFrames, FilmcraftApp, FrameRate, HashMap, ItemId, Pos2, Rect, Sequence, Stroke, StrokeKind, ThumbnailMode, Tick,
    TimeRange, Tokens, TrackItem, TrackKind, duplicate_frames, label_color, pos2, vec2,
};

/// (session revision, sequence, project address)
pub(super) type DuplicatesKey = (u64, ItemId, usize);

pub(super) type DuplicateMarkers = Arc<HashMap<ClipId, DuplicateFrames>>;

/// The duplicate frame markers of sequence `seq_id`, worked out again on a background thread
/// after the project changes: a long sequence takes too long for a frame. Until the new markers
/// are ready, the last ones of the same sequence are shown.
pub(super) fn duplicate_markers(app: &mut FilmcraftApp, ctx: &egui::Context, seq_id: ItemId, seq: &Sequence) -> DuplicateMarkers {
    let key = (app.session.revision, seq_id, Arc::as_ptr(&app.session.project) as usize);
    let done = app.tl.duplicates_done.lock().unwrap_or_else(|e| e.into_inner()).take();
    // a job that finishes after a newer one leaves the newer markers in place
    if let Some((k, d)) = done
        && app.tl.duplicates.as_ref().is_none_or(|(have, _)| *have != key)
    {
        app.tl.duplicates = Some((k, d));
    }
    match &app.tl.duplicates {
        Some((k, d)) if *k == key => return d.clone(),
        _ => {}
    }
    if app.tl.duplicates_pending != Some(key) {
        app.tl.duplicates_pending = Some(key);
        let (project, seq, slot, ctx) = (app.session.project.clone(), seq.clone(), app.tl.duplicates_done.clone(), ctx.clone());
        let run = move || {
            // a panic in the comparison must not take the app down: it leaves no markers
            let d = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| duplicate_frames(&project, &seq))).unwrap_or_default();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, Arc::new(d.into_iter().map(|d| (d.clip, d)).collect())));
            ctx.request_repaint();
        };
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(run);
        #[cfg(target_arch = "wasm32")]
        run();
    }
    match &app.tl.duplicates {
        Some(((_, s, _), d)) if *s == seq_id => d.clone(),
        _ => Arc::default(),
    }
}

/// The through-edit mark: two small triangles pointing at the cut, centred on the track, with a
/// dashed white line through the clip body. Returns the mark's rect.
pub(super) fn paint_through_edit(p: &egui::Painter, x: f32, row: Rect) -> Rect {
    let cy = row.center().y.max(row.min.y + 10.0);
    let (w, h) = (4.0, 4.0);
    let col = Color32::from_white_alpha(230);
    let mut y = row.min.y + 2.0;
    while y < row.max.y - 2.0 {
        p.line_segment([pos2(x, y), pos2(x, (y + 3.0).min(row.max.y - 2.0))], Stroke::new(1.0, Color32::from_white_alpha(140)));
        y += 6.0;
    }
    p.add(egui::Shape::convex_polygon(vec![pos2(x - w, cy - h), pos2(x, cy), pos2(x - w, cy + h)], col, Stroke::new(0.5, Color32::BLACK)));
    p.add(egui::Shape::convex_polygon(vec![pos2(x + w, cy - h), pos2(x + w, cy + h), pos2(x, cy)], col, Stroke::new(0.5, Color32::BLACK)));
    Rect::from_center_size(pos2(x, cy), vec2(2.0 * w + 2.0, 2.0 * h + 2.0))
}

/// The duplicate frame stripes of one clip, between `ys`: its `ranges` (timeline ranges in order)
/// drawn `offset` later, as the clip is while it is dragged. Only the ranges in the `visible` time
/// and inside the painter's clip rect are drawn, and ranges a point or less apart are drawn as
/// one stripe; each stripe takes the automation id `timeline.duplicateFrames.<clip>.<n>`, `n`
/// being its first range.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_duplicate_stripes(
    app: &mut FilmcraftApp,
    p: &egui::Painter,
    layout: &Layout,
    ranges: &[TimeRange],
    offset: Tick,
    ys: (f32, f32),
    visible: (Tick, Tick),
    col: Color32,
    clip: ClipId,
) {
    let first = ranges.partition_point(|r| r.end() + offset < visible.0);
    let mut stripe: Option<(usize, f32, f32)> = None;
    let flush = |app: &mut FilmcraftApp, s: (usize, f32, f32)| {
        let r = Rect::from_min_max(pos2(s.1, ys.0), pos2(s.2.max(s.1 + 1.0), ys.1));
        let shown = r.intersect(p.clip_rect());
        if shown.width() > 0.0 && shown.height() > 0.0 {
            p.rect_filled(r, 0.0, col);
            app.auto.add(&format!("timeline.duplicateFrames.{}.{}", clip.0, s.0), shown, "Duplicate frames");
        }
    };
    for (i, range) in ranges.iter().enumerate().skip(first) {
        if range.start + offset > visible.1 {
            break;
        }
        let (x0, x1) = (layout.x_of(range.start + offset), layout.x_of(range.end() + offset));
        stripe = match stripe {
            Some((n, a, b)) if x0 <= b + 1.0 => Some((n, a, b.max(x1))),
            Some(s) => {
                flush(app, s);
                Some((i, x0, x1))
            }
            None => Some((i, x0, x1)),
        };
    }
    if let Some(s) = stripe {
        flush(app, s);
    }
}

fn lighten(c: Color32, f: f32) -> Color32 {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * f).round() as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

/// Whether a clip lies inside the sequence In/Out range (Premiere brightens those).
fn in_range(app: &FilmcraftApp, it: &TrackItem) -> bool {
    let Some(q) = app.session.active_sequence() else { return false };
    if q.mark_in.is_none() && q.mark_out.is_none() {
        return false;
    }
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + q.settings.frame_rate.frame_duration()).unwrap_or(Tick::MAX);
    it.start < b && it.end() > a
}

/// Most thumbnails one clip draws: a long clip far zoomed in only shows the visible ones anyway.
pub(super) const MAX_THUMBNAIL_TILES: usize = 256;

/// The video thumbnails of a clip whose picture area spans `x0..x1` (points), for frames `tile_w`
/// wide: the left edge of each tile and the offset into the clip (`dur` long, frames of `frame`)
/// whose picture it shows. Only tiles that meet `visible` (the panel's x range) are listed.
/// Continuous tiles sit on a grid anchored at the clip's head, so scrolling only reveals new tiles
/// and the others keep their frames. A clip too narrow for more than one tile shows its head.
pub fn thumbnail_tiles(mode: ThumbnailMode, x0: f32, x1: f32, tile_w: f32, visible: (f32, f32), dur: Tick, frame: Tick) -> Vec<(f32, Tick)> {
    let width = x1 - x0;
    if !(x0.is_finite() && width.is_finite() && width > 0.0 && tile_w.is_finite() && tile_w >= 1.0) {
        return Vec::new();
    }
    let last = (dur - frame).max(Tick::ZERO);
    let offset_at = |x: f32| Tick(((x - x0) as f64 / width as f64 * dur.0 as f64).round() as i64).clamp(Tick::ZERO, last);
    let shown = |x: f32| x < visible.1 && x + tile_w > visible.0;
    let head = (x0, Tick::ZERO);
    let tiles = match mode {
        ThumbnailMode::Head => vec![head],
        ThumbnailMode::HeadAndTail if width >= 2.0 * tile_w => vec![head, (x1 - tile_w, last)],
        ThumbnailMode::HeadAndTail => vec![head],
        ThumbnailMode::Continuous => {
            let first = ((visible.0 - x0).max(0.0) / tile_w).floor() as usize;
            let end = ((visible.1.min(x1) - x0) / tile_w).ceil().max(1.0) as usize;
            (first..end).take(MAX_THUMBNAIL_TILES).map(|k| x0 + k as f32 * tile_w).map(|x| (x, offset_at(x))).collect()
        }
    };
    tiles.into_iter().filter(|(x, _)| shown(*x)).collect()
}

/// [`thumbnail_tiles`] while the view zooms (#1094): Continuous tiles are laid out at the zoom
/// the gesture started at (`frozen` points a second) and stretched to where their frames are at
/// the live zoom `pps`, so every tile keeps its frame (cached) instead of asking for a new one at
/// each zoom step. Returns each tile's left edge, width and offset into the clip.
#[allow(clippy::too_many_arguments)]
pub fn zoomed_thumbnail_tiles(
    mode: ThumbnailMode,
    x0: f32,
    x1: f32,
    tile_w: f32,
    visible: (f32, f32),
    dur: Tick,
    frame: Tick,
    pps: f64,
    frozen: Option<f64>,
) -> Vec<(f32, f32, Tick)> {
    let r = match frozen {
        Some(f) if mode == ThumbnailMode::Continuous && f.is_finite() && f > 0.0 && pps.is_finite() && pps > 0.0 && f != pps => (pps / f) as f32,
        _ => return thumbnail_tiles(mode, x0, x1, tile_w, visible, dur, frame).into_iter().map(|(x, t)| (x, tile_w, t)).collect(),
    };
    let laid_out = |x: f32| x0 + (x - x0) / r;
    let tiles = thumbnail_tiles(mode, x0, laid_out(x1), tile_w, (laid_out(visible.0), laid_out(visible.1)), dur, frame);
    tiles.into_iter().map(|(x, t)| (x0 + (x - x0) * r, tile_w * r, t)).collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_clip(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    p: &egui::Painter,
    body: Rect,
    it: &TrackItem,
    kind: TrackKind,
    selected: bool,
    t: &Tokens,
    rate: FrameRate,
) {
    let base = label_color(app, it.label);
    let inr = in_range(app, it);
    let fill = if !it.enabled {
        // Keep the configured label hue when disabled, with an opaque darker fill (#1190).
        Color32::from_rgb(base.r() / 2, base.g() / 2, base.b() / 2)
    } else if inr {
        lighten(base, 0.12)
    } else if selected {
        lighten(base, 0.06)
    } else {
        base
    };
    p.rect_filled(body, 0.0, fill);
    // 1 pt lighter top edge
    p.line_segment([pos2(body.min.x, body.min.y + 0.5), pos2(body.max.x, body.min.y + 0.5)], Stroke::new(1.0, lighten(fill, 0.15)));
    let name_h = 16.0;
    let w = body.width();
    // video thumbnails, aspect-correct, below the name band: the head frame, head and tail, or
    // frames side by side across the clip (Show Video Thumbnails in the wrench menu)
    if kind == TrackKind::Video && app.ui.timeline.show_thumbnails && body.height() > 28.0 && w > 20.0 && it.enabled {
        let th = Rect::from_min_max(pos2(body.min.x + 1.0, body.min.y + name_h), pos2(body.max.x - 1.0, body.max.y - 1.0));
        let aspect = app.session.project.item(it.item).and_then(|pi| match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref().map(|v| v.width as f32 / v.height as f32),
            filmcraft_project::ItemKind::Sequence(s) => Some(s.settings.width as f32 / s.settings.height as f32),
            _ => None,
        });
        if let Some(aspect) = aspect.filter(|a| a.is_finite() && *a > 0.0) {
            let tw = th.height() * aspect;
            let cp = p.with_clip_rect(th.intersect(p.clip_rect()));
            let visible = (cp.clip_rect().min.x, cp.clip_rect().max.x);
            let mode = app.ui.timeline.thumbnail_mode;
            let (pps, frozen) = (app.ui.timeline.pps, app.tl.thumb_pps);
            for (x, w, offset) in zoomed_thumbnail_tiles(mode, th.min.x, th.max.x, tw, visible, it.duration, rate.frame_duration(), pps, frozen) {
                let mt = rate.snap(it.source_time_at(it.start + offset));
                // a multi-camera clip shows its angle's clip
                let (thumb_item, mt) = app
                    .session
                    .project
                    .sequence(it.item)
                    .and_then(|q| {
                        let ti = q.angle_video_track_index(it.multicam_angle(q)?)?;
                        let inner = q.video_tracks.get(ti)?.item_at(mt)?;
                        Some((inner.item, inner.source_time_at(mt)))
                    })
                    .unwrap_or((it.item, mt));
                if let Some((tex, _)) = app.thumbnail(ctx, thumb_item, mt, 160) {
                    let r = Rect::from_min_size(pos2(x, th.min.y), vec2(w, th.height()));
                    cp.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
        }
    }
    // waveform (audio): rectified, lower 40 %
    if kind == TrackKind::Audio && app.ui.timeline.show_waveforms && w > 3.0 {
        draw_waveform(app, p, body, it, Color32::from_rgb(fill.r() / 2, fill.g() / 2, fill.b() / 2));
    }
    let clip_p = p.with_clip_rect(body.shrink2(vec2(1.0, 0.0)).intersect(p.clip_rect()));
    // name band (View ▸ the wrench menu's Show Video Names / Show Audio Names)
    // a hidden name keeps the multicam angle and the speed, which say something the name does not
    let names = if kind == TrackKind::Video { app.ui.timeline.show_video_names } else { app.ui.timeline.show_audio_names };
    let mut label = if names { it.name.clone() } else { String::new() };
    if let Some(m) = it.multicam.filter(|m| m.enabled) {
        let cam = app.session.project.sequence(it.item).and_then(|q| q.cameras().cameras.get(m.angle as usize).map(|c| c.name.clone()));
        label = if names { format!("[MC{}] {}", m.angle + 1, cam.unwrap_or(label)) } else { format!("[MC{}]", m.angle + 1) };
    }
    if (it.speed - 1.0).abs() > 1e-6 || it.reverse {
        let speed = format!("[{}%]", ((if it.reverse { -1.0 } else { 1.0 }) * it.speed * 100.0).round());
        label = if label.is_empty() { speed } else { format!("{label} {speed}") };
    }
    if w > 24.0 && !label.is_empty() {
        clip_p.text(
            pos2(body.min.x + 6.0, body.min.y + 8.5),
            Align2::LEFT_CENTER,
            label,
            Tokens::ui(11.5),
            if it.enabled { Color32::from_rgb(0xd9, 0xd9, 0xd9) } else { t.text_faint },
        );
    }
    // fx badge right-aligned in the header (italic-ish "fx")
    if (it.has_standard_effects() || it.has_modified_intrinsics()) && w > 40.0 {
        let col = if it.has_standard_effects() { Color32::from_rgb(0xe8, 0xc5, 0x4a) } else { Color32::from_rgb(0x91, 0xa2, 0xac) };
        clip_p.text(
            pos2(body.max.x - 6.0, body.min.y + 8.5),
            Align2::RIGHT_CENTER,
            "fx",
            egui::FontId::new(11.0, egui::FontFamily::Name("semibold".into())),
            col,
        );
    }
    // corner triangles where the clip reaches the media's first/last frame (no handles)
    let media_dur = app.session.project.item(it.item).map(|i| i.duration());
    let no_head = it.source_in <= Tick::ZERO;
    let no_tail = media_dur.is_some_and(|d| d.0 > 0 && it.source_out() >= d - rate.frame_duration());
    let tri = 5.0;
    if no_head && w > 12.0 {
        clip_p.add(egui::Shape::convex_polygon(
            vec![body.min, body.min + vec2(tri, 0.0), body.min + vec2(0.0, tri)],
            Color32::from_white_alpha(220),
            Stroke::NONE,
        ));
    }
    if no_tail && w > 12.0 {
        let tr = pos2(body.max.x, body.min.y);
        clip_p.add(egui::Shape::convex_polygon(vec![tr, tr + vec2(0.0, tri), tr - vec2(tri, 0.0)], Color32::from_white_alpha(220), Stroke::NONE));
    }
    // 1 pt black separator at the clip's left edge
    p.line_segment([pos2(body.min.x + 0.5, body.min.y), pos2(body.min.x + 0.5, body.max.y)], Stroke::new(1.0, Color32::BLACK));
    if selected {
        p.rect_stroke(body.shrink(1.0), 0.0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Inside);
        p.rect_stroke(body, 0.0, Stroke::new(2.0, t.clip_selected_border), StrokeKind::Inside);
    }
}

/// 45° hatching over `r` (transitions, and the empty end of a nested sequence clip).
pub(super) fn paint_hatch(p: &egui::Painter, r: Rect) {
    let cp = p.with_clip_rect(r.intersect(p.clip_rect()));
    let step = 4.0;
    let mut x = r.min.x - r.height();
    while x < r.max.x {
        cp.line_segment([pos2(x, r.max.y), pos2(x + r.height(), r.min.y)], Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xd9, 0xd9, 0xd9, 110)));
        x += step;
    }
}

pub(super) fn draw_transition(p: &egui::Painter, r: Rect, trn: &filmcraft_project::Transition, selected: bool, t: &Tokens) {
    let cp = p.with_clip_rect(r.intersect(p.clip_rect()));
    cp.rect_filled(r, 0.0, Color32::from_black_alpha(90));
    paint_hatch(p, r);
    let audio = matches!(trn.effect.def().map(|d| d.kind), Some(filmcraft_project::EffectKind::AudioTransition));
    if audio {
        // crossing fade curves
        let n = 16;
        let a: Vec<Pos2> = (0..=n)
            .map(|i| {
                pos2(r.min.x + r.width() * i as f32 / n as f32, r.min.y + r.height() * (1.0 - ((i as f32 / n as f32) * std::f32::consts::FRAC_PI_2).sin()))
            })
            .collect();
        let b: Vec<Pos2> = (0..=n)
            .map(|i| {
                pos2(r.min.x + r.width() * i as f32 / n as f32, r.min.y + r.height() * (1.0 - ((i as f32 / n as f32) * std::f32::consts::FRAC_PI_2).cos()))
            })
            .collect();
        cp.add(egui::Shape::line(a, Stroke::new(1.0, Color32::WHITE)));
        cp.add(egui::Shape::line(b, Stroke::new(1.0, Color32::WHITE)));
    }
    cp.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(0xeb, 0xeb, 0xeb)), StrokeKind::Inside);
    if selected {
        cp.rect_stroke(r, 0.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
    }
    if r.width() > 34.0 && !audio {
        let name = trn.effect.def().map(|d| d.name).unwrap_or(&trn.effect.effect);
        let tr = Rect::from_min_size(r.min + vec2(2.0, 2.0), vec2((r.width() - 4.0).min(80.0), 13.0));
        cp.rect_filled(tr, 0.0, Color32::from_black_alpha(170));
        cp.text(pos2(tr.min.x + 3.0, tr.center().y), Align2::LEFT_CENTER, name, Tokens::ui(10.5), Color32::from_rgb(0xd9, 0xd9, 0xd9));
    }
}

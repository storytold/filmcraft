//! How much memory one CPU render of a frame holds at its peak, estimated from the project alone
//! (no decoding, no pixels).
//!
//! An export renders a batch of frames at once, one per worker. What bounds it so far is the size of
//! the *finished* frames (the exporter's in-flight budget), but the images that produce a
//! frame can be far larger: a 3840×2160 clip scaled by Motion to anything over half size is decoded,
//! colour managed, run through its effects and transformed as a full 4K float image (133 MB), several
//! times over, on every worker at once (#1443). [`frame_working_set`] lets the exporter size its
//! batch to that.
//!
//! The estimate follows `seq_canvas` track by track: the output canvas, plus the largest
//! single layer (layers are drawn one after another and handed back to the pool, so only one is
//! alive besides the canvas). A layer of media costs its decoded picture, the float image at the
//! decimation the renderer will choose (`decimation`), one working copy of it when something works
//! on it (effects, opacity masks, the second picture of a frame blend) and the layer placed on an
//! output-sized image unless it already is one. It is an upper bound for the common cases, not an
//! exact count: the decoder may hand back an already reduced picture.

use filmcraft_project::{ClipId, ItemId, ItemKind, Project, Sequence, TrackItem};
use filmcraft_time::Tick;

use filmcraft_geom::Affine;

use crate::{
    MAX_NEST_DEPTH, RenderOptions, decimation, identity_placement, interpolation_blend, layer_scale, motion_matrix, output_size, source_par, source_size,
};

/// Bytes of one pixel of a working image (premultiplied linear `f32` RGBA).
const IMAGE_PX: u64 = 16;

/// Bytes of one source pixel as decoded, at most (16-bit 4:2:0 Y'CbCr is 3, 8-bit RGBA 4).
const DECODED_PX: u64 = 4;

/// Bytes of one `w` x `h` working image (premultiplied linear `f32` RGBA), saturating.
pub fn image_bytes(w: usize, h: usize) -> u64 {
    u64::try_from(w).unwrap_or(u64::MAX).saturating_mul(u64::try_from(h).unwrap_or(u64::MAX)).saturating_mul(IMAGE_PX)
}

/// The estimated peak bytes of a CPU render of sequence `seq_id` at timeline `t` (see the module
/// docs). 0 when there is no such sequence.
pub fn frame_working_set(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions) -> u64 {
    project.sequence(seq_id).map_or(0, |seq| sequence_bytes(project, seq, t, opts))
}

fn sequence_bytes(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions) -> u64 {
    let (w, h) = output_size(seq, opts.scale);
    let canvas = image_bytes(w, h);
    if opts.depth > MAX_NEST_DEPTH {
        return canvas;
    }
    let mut peak = 0u64;
    for track in seq.video_tracks.iter().filter(|tr| tr.enabled) {
        if let Some(tr) = track.transitions.iter().find(|tr| tr.range().contains(t)) {
            // the picture below, both sides on output-sized images and their mix, besides the
            // larger side's own layer while it is drawn
            let side = |id: Option<ClipId>| id.and_then(|id| track.item(id)).filter(|i| i.enabled).map_or(0, |i| item_bytes(project, seq, i, t, opts));
            let sides = side(tr.from).max(side(tr.to));
            peak = peak.max(canvas.saturating_mul(3).saturating_add(sides));
            continue;
        }
        if let Some(item) = track.item_at(t).filter(|i| i.enabled) {
            peak = peak.max(item_bytes(project, seq, item, t, opts));
        }
    }
    canvas.saturating_add(peak)
}

/// One clip's layer at its peak, its output-sized placement included.
fn item_bytes(project: &Project, seq: &Sequence, item: &TrackItem, t: Tick, opts: RenderOptions) -> u64 {
    let (w, h) = output_size(seq, opts.scale);
    let placed = image_bytes(w, h);
    let (Some(pi), Some(src)) = (project.item(item.item), source_size(project, item.item)) else { return 0 };
    let mt = item.effect_time_at(t);
    let motion = motion_matrix(seq, item, src, source_par(project, item.item), mt);
    let want = layer_scale(&motion, opts.scale);
    let (sw, sh) = (src.0 as usize, src.1 as usize);
    match &pi.kind {
        ItemKind::Media(_) | ItemKind::Subclip { .. } => {
            let n = decimation(src.0 as f32, src.0 as f32 * want).max(1);
            let (lw, lh) = (sw.div_ceil(n), sh.div_ceil(n));
            let layer = image_bytes(lw, lh);
            let decoded = u64::from(src.0).saturating_mul(u64::from(src.1)).saturating_mul(DECODED_PX);
            // a working copy only for effects, opacity masks or a frame blend's second picture
            let worked = (opts.effects && item.has_standard_effects())
                || item.has_opacity_masks()
                || interpolation_blend(item, t, pi.frame_rate(), seq.settings.frame_rate).is_some();
            let copy = if worked { layer } else { 0 };
            // a layer that is the output already, placed as it is, is mixed (or kept) as it is
            let px = lw as f64 / f64::from(src.0.max(1));
            let s = f64::from(opts.scale);
            let m = Affine::scale(s, s).then_apply(&motion).then_apply(&Affine::scale(1.0 / px, 1.0 / px));
            let placed = if (lw, lh) == (w, h) && identity_placement(&m) { 0 } else { placed };
            decoded.saturating_add(layer).saturating_add(copy).saturating_add(placed)
        }
        ItemKind::Sequence(nested) => {
            let sub = RenderOptions { scale: want, depth: opts.depth.saturating_add(1), ..opts };
            // The nested render hands back its canvas as the layer. Its effects then work on a copy
            // of that canvas, after everything else the nested render held has gone back to the pool.
            let rendered = sequence_bytes(project, nested, item.source_time_at(t), sub);
            let (nw, nh) = output_size(nested, want);
            let worked = (opts.effects && item.has_standard_effects()) || item.has_opacity_masks();
            let with_copy = if worked { image_bytes(nw, nh).saturating_mul(2) } else { 0 };
            rendered.max(with_copy).saturating_add(placed)
        }
        ItemKind::Graphic { .. } => {
            let (gw, gh) = (((sw as f32 * want).ceil() as usize).max(1), ((sh as f32 * want).ceil() as usize).max(1));
            image_bytes(gw, gh).saturating_add(placed)
        }
        // the picture below, its adjusted copy and the opacity weight, all output-sized
        ItemKind::AdjustmentLayer { .. } => placed.saturating_mul(3),
    }
}

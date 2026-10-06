//! Frame plans for the GPU compositor.
//!
//! [`plan_frame`] resolves what is visible at a time into a list of layers the GPU can draw
//! directly: a decoded source frame (YUV planes or RGBA), the matrix from source pixels to output
//! pixels, an opacity and a blend mode (all 27 of [`Blend`] are composited by the GPU). A media
//! clip whose enabled standard effects all have a GPU implementation ([`crate::gpufx`]) carries
//! them as a [`LayerFx`]: their parameters evaluated at the frame's time, run by the GPU on the
//! clip's working image before Motion places it, in the CPU's order (effects → Motion → Opacity /
//! blend). Anything the shaders don't cover yet — other standard effects, masks, adjustment
//! layers, nested sequences, non-dissolve transitions — is rendered on the CPU for that layer (or
//! the whole frame) and handed over as a pre-composited image, so the GPU path is always exact
//! with respect to the CPU reference.

use std::sync::Arc;

use filmcraft_frame::VideoFrame;
use filmcraft_geom::Affine;
use filmcraft_media::FrameRequest;
use filmcraft_project::{ItemId, ItemKind, Project, Sequence, TrackItem};
use filmcraft_time::Tick;

use crate::gpufx::FxOp;
use crate::{Blend, RenderOptions, SourceProvider, motion_matrix, output_size};

/// One layer for the GPU, bottom to top.
#[derive(Clone)]
pub struct PlanLayer {
    pub frame: Arc<VideoFrame>,
    /// Maps frame pixels (0..w, 0..h) to output pixels — or, with [`fx`](Self::fx), working-image
    /// pixels (0..fx.size).
    pub matrix: Affine,
    pub opacity: f32,
    /// How the layer combines with what is under it ([`crate::blend::composite`]).
    pub blend: Blend,
    /// Standard effects to run on the frame before it is placed (None: draw the frame directly).
    pub fx: Option<Arc<LayerFx>>,
}

/// The GPU effect stage of a layer: the frame is decoded into a working image of `size` (the
/// frame box-decimated by `decimation`, as the CPU decodes it), `ops` run on it in order, and the
/// result is placed with the layer's matrix, opacity and blend mode.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerFx {
    pub size: (u32, u32),
    pub decimation: u32,
    pub ops: Vec<FxOp>,
}

impl PlanLayer {
    /// A layer drawing `frame` directly.
    pub fn new(frame: Arc<VideoFrame>, matrix: Affine, opacity: f32, blend: Blend) -> Self {
        Self { frame, matrix, opacity, blend, fx: None }
    }

    /// Size of the picture the matrix places: the working image with effects, else the frame.
    pub fn size(&self) -> (u32, u32) {
        match &self.fx {
            Some(fx) => fx.size,
            None => (self.frame.width, self.frame.height),
        }
    }
}

#[derive(Clone)]
pub enum FramePlan {
    /// Draw these layers over black.
    Layers { width: usize, height: usize, layers: Vec<PlanLayer> },
    /// The CPU produced the final image (fallback).
    Image(crate::Image),
}

fn cpu_frame(img: crate::Image) -> Arc<VideoFrame> {
    Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px))
}

fn simple_transition(id: &str) -> bool {
    matches!(id, "cross_dissolve" | "dip_to_black" | "dip_to_white" | "morph_cut")
}

/// The standard effects of a media clip if the GPU can draw it (any blend mode, no opacity masks,
/// every enabled standard effect unmasked and with a GPU implementation; empty without effects or
/// with effects off). None: the clip is rendered on the CPU.
fn gpu_chain<'a>(project: &Project, item: &'a TrackItem, opts: RenderOptions) -> Option<Vec<&'a filmcraft_project::EffectInstance>> {
    let is_media = project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Media(_) | ItemKind::Subclip { .. }));
    if !is_media || item.has_opacity_masks() {
        return None;
    }
    if !opts.effects {
        return Some(Vec::new());
    }
    let mut chain = Vec::new();
    for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e)) {
        if !e.enabled {
            continue;
        }
        let masked = e.masks.iter().any(|m| m.mode != filmcraft_project::MaskMode::None);
        if masked || !crate::gpufx::GPU_EFFECTS.contains(&e.effect.as_str()) {
            return None;
        }
        chain.push(e);
    }
    Some(chain)
}

/// Plan the frame at timeline `t`.
pub fn plan_frame(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> FramePlan {
    let Some(seq) = project.sequence(seq_id) else { return FramePlan::Image(crate::Image::new(1, 1)) };
    let (w, h) = output_size(seq, opts.scale);
    // HDR / wide-gamut sequences composite and convert on the CPU.
    if !seq.settings.color.is_plain() {
        return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
    }
    // Whole-frame fallback: adjustment layers or complex transitions anywhere at t.
    for tr in &seq.video_tracks {
        if !tr.enabled {
            continue;
        }
        if let Some(trn) = tr.transitions.iter().find(|x| x.range().contains(t))
            && !simple_transition(&trn.effect.effect)
        {
            return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
        }
        if let Some(it) = tr.item_at(t)
            && project.item(it.item).is_some_and(|p| matches!(p.kind, ItemKind::AdjustmentLayer { .. }))
        {
            return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
        }
    }
    let mut layers = Vec::new();
    for tr in &seq.video_tracks {
        if !tr.enabled {
            continue;
        }
        if let Some(trn) = tr.transitions.iter().find(|x| x.range().contains(t)) {
            // These dissolves are symmetric: Reverse (play B→A backwards) renders the same frames.
            let p = trn.progress(t) as f32;
            let a = trn.from.and_then(|id| tr.item(id)).filter(|i| i.enabled);
            let b = trn.to.and_then(|id| tr.item(id)).filter(|i| i.enabled);
            match trn.effect.effect.as_str() {
                "dip_to_black" | "dip_to_white" => {
                    let col = if trn.effect.effect == "dip_to_black" { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] };
                    layers.push(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(1, 1, col.to_vec())), Affine::scale(w as f64, h as f64), 1.0, Blend::Normal));
                    let (it, k) = if p < 0.5 { (a, 1.0 - p * 2.0) } else { (b, (p - 0.5) * 2.0) };
                    if let Some(it) = it {
                        push_item(project, seq, it, t, opts, sources, k, Some(Blend::Normal), &mut layers);
                    }
                }
                _ => {
                    // cross dissolve: A at full, B over it at p (premultiplied over == linear mix when A is opaque)
                    if let Some(it) = a {
                        push_item(project, seq, it, t, opts, sources, 1.0 - if b.is_none() { p } else { 0.0 }, Some(Blend::Normal), &mut layers);
                    }
                    if let Some(it) = b {
                        push_item(project, seq, it, t, opts, sources, p, Some(Blend::Normal), &mut layers);
                    }
                }
            }
            continue;
        }
        let Some(item) = tr.item_at(t) else { continue };
        if !item.enabled {
            continue;
        }
        push_item(project, seq, item, t, opts, sources, 1.0, None, &mut layers);
    }
    if opts.captions {
        for o in crate::caption_overlays(seq, t, w, h) {
            layers.push(PlanLayer::new(
                Arc::new(VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                Affine::translate(o.x as f64, o.y as f64),
                1.0,
                Blend::Normal,
            ));
        }
    }
    FramePlan::Layers { width: w, height: h, layers }
}

/// Push the layer(s) of `item`. `blend` overrides the item's own blend mode: inside a transition
/// the CPU reference mixes the clips and composites the result Normal, ignoring their modes.
#[allow(clippy::too_many_arguments)]
fn push_item(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    extra_opacity: f32,
    blend: Option<Blend>,
    out: &mut Vec<PlanLayer>,
) {
    // frame time (`ft`) vs. effect time (`mt`): they differ inside a frame hold without Hold Filters
    let ft = item.source_time_at(t);
    let mt = item.effect_time_at(t);
    let (op, own) = crate::opacity_blend(item, mt);
    let bl = blend.unwrap_or(own);
    // A multi-camera clip that only shows its angle (no effects, untransformed, same frame size)
    // draws the angle's clip directly: no CPU pass over the nested sequence.
    if let Some(ItemKind::Sequence(nested)) = project.item(item.item).map(|p| &p.kind)
        && let Some(angle) = item.multicam_angle(nested)
        && !(opts.effects && item.has_standard_effects())
        && (nested.settings.width, nested.settings.height) == (seq.settings.width, seq.settings.height)
        && near_identity(&motion_matrix(seq, item, (nested.settings.width, nested.settings.height), mt))
        && let Some(tr) = nested.angle_video_track_index(angle).and_then(|i| nested.video_tracks.get(i))
        && !tr.transitions.iter().any(|x| x.range().contains(ft))
        && tr.item_at(ft).is_none_or(|i| crate::opacity_blend(i, i.effect_time_at(ft)).1 != Blend::Dissolve)
    {
        // Inside the nested sequence the angle's clip is composited onto an empty canvas, where
        // every mode but Dissolve is Normal; the multicam clip's own mode then applies to it.
        if let Some(inner) = tr.item_at(ft).filter(|i| i.enabled) {
            push_item(project, nested, inner, ft, opts, sources, extra_opacity * op, Some(bl), out);
        }
        return;
    }
    // Graphic clips without standard effects: the layers are rasterised (cached) into one tight
    // image the GPU places as a layer.
    if !(opts.effects && item.has_standard_effects())
        && !item.has_opacity_masks()
        && project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
    {
        let Some(size) = crate::source_size(project, item.item) else { return };
        let (w, h) = output_size(seq, opts.scale);
        let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion_matrix(seq, item, size, mt));
        if let Some((img, x, y)) = crate::graphic_clip::render_graphic_tight(item, mt, size, &m, w, h) {
            out.push(PlanLayer::new(cpu_frame(img), Affine::translate(x as f64, y as f64), op * extra_opacity, bl));
        }
        return;
    }
    if let Some(chain) = gpu_chain(project, item, opts) {
        let Some(src) = sources.source(item.item) else { return };
        let Some(size) = crate::source_size(project, item.item) else { return };
        let motion = motion_matrix(seq, item, size, mt);
        let lin = ((motion.a * motion.a + motion.b * motion.b).sqrt()).max((motion.c * motion.c + motion.d * motion.d).sqrt());
        let want = (lin * opts.scale as f64).clamp(1.0 / 64.0, 1.0) as f32;
        let Ok(frame) = src.video_frame(FrameRequest { time: ft, scale: want }) else { return };
        let cs = crate::colorman::source_space(project, item.item, &frame);
        // The GPU path uploads only the Y'CbCr planes and drops the alpha plane (ProRes 4444,
        // yuva), so a layer carrying one is drawn on the CPU, which composites it correctly.
        let has_alpha_plane =
            matches!(&frame.data, filmcraft_frame::PixelData::Yuv8 { alpha: Some(_), .. } | filmcraft_frame::PixelData::Yuv16 { alpha: Some(_), .. });
        // log / HDR / wide-gamut media is converted on the CPU (below), and so are blended
        // in-between frames (Frame Blending / Optical Flow on speed-changed clips)
        let plain = !has_alpha_plane
            && !crate::colorman::needs_management(&seq.settings.color, cs, &frame)
            && crate::interpolation_blend(item, t, src.info().frame_rate()).is_none();
        if plain && !chain.is_empty() {
            // GPU effect stage: the working image the CPU would decode (`base_layer`), the
            // effects evaluated for it, placed as `item_layer` places it
            let n = crate::decimation(frame.width as f32, size.0 as f32 * want);
            let (lw, lh) = ((frame.width as usize / n).max(1), (frame.height as usize / n).max(1));
            let px_scale = lw as f32 / size.0.max(1) as f32;
            let tc = "";
            let cx = crate::effects::FxCtx {
                t: mt,
                px_scale,
                seconds: (t - item.start).seconds(),
                timecode: tc,
                clip_name: &item.name,
                project: Some(project),
                env: None,
                working: seq.settings.color.working,
            };
            let ops: Option<Vec<FxOp>> = chain.iter().map(|e| FxOp::eval(e, &cx, lw, lh).filter(FxOp::gpu_ok)).collect();
            if let Some(ops) = ops {
                let m = Affine::scale(opts.scale as f64, opts.scale as f64)
                    .then_apply(&motion)
                    .then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
                let fx = LayerFx { size: (lw as u32, lh as u32), decimation: n as u32, ops };
                out.push(PlanLayer { frame, matrix: m, opacity: op * extra_opacity, blend: bl, fx: Some(Arc::new(fx)) });
                return;
            }
        } else if plain {
            // Draft playback at reduced resolution: hand over planes box-filtered to the size
            // drawn instead of the full picture (a quarter at 1/2, a sixteenth at 1/4 of the
            // upload and sampling). The mean is taken over Y'CbCr codes, not linear light as
            // the shader's supersampling does, so this stays limited to the opt-in draft mode.
            let n = if filmcraft_media::cancel::draft() { crate::decimation(frame.width as f32, size.0 as f32 * want) } else { 1 };
            let frame = match frame.box_decimated(n) {
                Some(small) => Arc::new(small),
                None => frame,
            };
            let px_scale = frame.width as f64 / size.0.max(1) as f64;
            let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion).then_apply(&Affine::scale(1.0 / px_scale, 1.0 / px_scale));
            out.push(PlanLayer::new(frame, m, op * extra_opacity, bl));
            return;
        }
    }
    // CPU-rendered layer (standard effects): drawn by the GPU as a pre-rendered canvas image.
    let tc = filmcraft_time::format_time(t, seq.settings.frame_rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48_000);
    if let Some((img, op2, _)) = crate::item_layer(project, seq, item, t, opts, sources, &tc) {
        out.push(PlanLayer::new(cpu_frame(img), Affine::IDENTITY, op2 * extra_opacity, bl));
    }
}

fn near_identity(m: &Affine) -> bool {
    (m.a - 1.0).abs() < 1e-9 && (m.d - 1.0).abs() < 1e-9 && m.b.abs() < 1e-9 && m.c.abs() < 1e-9 && m.e.abs() < 1e-6 && m.f.abs() < 1e-6
}

/// A layer's working image with its effects applied, on the CPU (the reference for the GPU
/// effect stage).
pub fn effect_image(frame: &VideoFrame, fx: &LayerFx) -> crate::Image {
    let (w, h, px) = frame.to_linear_f32_decimated(fx.decimation.max(1) as usize);
    let mut img = crate::Image { w, h, px };
    for op in &fx.ops {
        op.apply(&mut img);
    }
    img
}

/// Execute a plan on the CPU (reference for the GPU compositor).
pub fn execute_cpu(plan: &FramePlan) -> crate::Image {
    match plan {
        FramePlan::Image(img) => img.clone(),
        FramePlan::Layers { width, height, layers } => {
            let mut canvas = crate::Image::new(*width, *height);
            for l in layers {
                let src = match &l.fx {
                    Some(fx) => effect_image(&l.frame, fx),
                    None => crate::Image { w: l.frame.width as usize, h: l.frame.height as usize, px: l.frame.to_linear_f32() },
                };
                let placed = if l.matrix == Affine::scale(*width as f64, *height as f64) && src.w == 1 && src.h == 1 {
                    crate::Image::filled(*width, *height, src.get(0, 0))
                } else {
                    src.transformed(*width, *height, &l.matrix)
                };
                crate::blend::composite(&mut canvas, &placed, l.opacity, l.blend);
            }
            canvas
        }
    }
}

//! Sequence evaluation and the CPU reference compositor.
//!
//! [`render_sequence`] turns (project, sequence, time, scale) into a premultiplied linear-light
//! image. Tracks composite bottom (V1) to top. Per item: fetch the source frame at the mapped media
//! time (already reduced for low-resolution playback), run standard effects, then the fixed effects
//! (Motion → Opacity/blend), then composite. Transitions combine outgoing/incoming layers.
//!
//! The same code renders monitors, thumbnails and exports, and is the oracle for the GPU path.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod audio;
pub mod audio_fx;
pub mod blend;
pub mod color_match;
pub mod colorman;
pub mod effects;
pub mod gpufx;
pub mod graphic_clip;
pub mod graphics;
pub mod image;
pub mod lumetri_presets;
pub mod luts;
pub mod mask;
pub mod mixer;
pub mod multicam;
pub mod offline;
pub mod plan;
pub mod preview;
pub mod remix;
pub mod scene;
pub mod track;
pub mod transitions;
pub mod vfx;

use std::sync::Arc;

use filmcraft_frame::{PixelData, VideoFrame, pool};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_media::{FrameRequest, SharedSource};
use filmcraft_project::{ItemId, ItemKind, ParamValue, Project, Sequence, TrackItem};
use filmcraft_time::{Tick, TimeDisplay, format_time};

pub use blend::Blend;
pub use image::Image;

/// Resolves project items to media sources (the engine owns the media pool).
pub trait SourceProvider: Sync {
    fn source(&self, item: ItemId) -> Option<SharedSource>;
}

impl<F: Fn(ItemId) -> Option<SharedSource> + Sync> SourceProvider for F {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        self(item)
    }
}

/// How many nested sequences deep the picture and the sound are followed. Deeper nests draw
/// nothing and are silent, which also ends a sequence that contains itself (only a damaged project
/// has one: the editor refuses the edit).
pub const MAX_NEST_DEPTH: u32 = 8;

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Output scale relative to the sequence frame size (1.0 = full, 0.5 = ½ resolution…).
    pub scale: f32,
    /// Skip standard effects (fast scrubbing / "Toggle Effects" off).
    pub effects: bool,
    /// Nesting depth guard (see [`MAX_NEST_DEPTH`]).
    pub depth: u32,
    /// Draw the sequence's visible caption tracks over the picture (Program monitor, burn-in on
    /// export). Never applies to nested sequences.
    pub captions: bool,
    /// Return the image in the sequence's working space (HDR exports, scopes) instead of
    /// converting it for an SDR monitor. Only matters for HDR / wide-gamut sequences.
    pub working_output: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { scale: 1.0, effects: true, depth: 0, captions: false, working_output: false }
    }
}

/// Output size for a sequence at a scale.
pub fn output_size(seq: &Sequence, scale: f32) -> (usize, usize) {
    (((seq.settings.width as f32 * scale).round() as usize).max(1), ((seq.settings.height as f32 * scale).round() as usize).max(1))
}

/// Render sequence `seq_id` at timeline time `t`.
pub fn render_sequence(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Image {
    let Some(seq) = project.sequence(seq_id) else { return Image::new(1, 1) };
    render_seq(project, seq, t, opts, sources)
}

fn render_seq(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Image {
    render_seq_tracks(project, seq, t, opts, sources, None)
}

/// The frame being composited. It stays unallocated until a layer has to be mixed into it: an
/// opaque bottom layer simply becomes the canvas, which saves a zero-filled image (33 MB at 1080p)
/// and the pass that mixes the layer over it.
struct Canvas {
    img: Option<Image>,
    w: usize,
    h: usize,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self { img: None, w, h }
    }
    fn is_empty(&self) -> bool {
        self.img.is_none()
    }
    /// The image, transparent black when nothing was drawn yet.
    fn get(&mut self) -> &mut Image {
        let (w, h) = (self.w, self.h);
        self.img.get_or_insert_with(|| Image::new(w, h))
    }
    fn set(&mut self, img: Image) {
        self.img = Some(img);
    }
    /// A copy of the current image.
    fn snapshot(&self) -> Image {
        self.img.clone().unwrap_or_else(|| Image::new(self.w, self.h))
    }
    fn into_image(self) -> Image {
        let (w, h) = (self.w, self.h);
        self.img.unwrap_or_else(|| Image::new(w, h))
    }
}

/// What one clip adds to the frame.
pub(crate) enum Layer {
    /// An image the size of the output.
    Full {
        image: Image,
        opacity: f32,
        blend: Blend,
        /// Every pixel has alpha exactly 1.
        opaque: bool,
    },
    /// The only part of the layer that is not transparent: a rectangle with its top-left corner at
    /// (`x`, `y`) of the output (the rest of the layer has alpha 0, which mixes nothing in).
    Region { image: Image, x: usize, y: usize, opacity: f32, blend: Blend },
}

impl Layer {
    /// The layer as an image the size of the output, with its opacity and blend mode.
    fn into_full(self, w: usize, h: usize) -> (Image, f32, Blend) {
        match self {
            Layer::Full { image, opacity, blend, .. } => (image, opacity, blend),
            Layer::Region { image, x, y, opacity, blend } => {
                let mut full = Image::new(w, h);
                // the part of the rectangle that lies inside the output
                let cols = image.w.min(w.saturating_sub(x));
                for ry in 0..image.h.min(h.saturating_sub(y)) {
                    let (from, to) = (ry * image.w * 4, ((y + ry) * w + x) * 4);
                    if let (Some(src), Some(dst)) = (image.px.get(from..from + cols * 4), full.px.get_mut(to..to + cols * 4)) {
                        dst.copy_from_slice(src);
                    }
                }
                (full, opacity, blend)
            }
        }
    }
}

/// Render a sequence, or only its video track `only` (a multi-camera angle; drawn even when the
/// track's output is off).
pub(crate) fn render_seq_tracks(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider, only: Option<usize>) -> Image {
    let (w, h) = output_size(seq, opts.scale);
    let mut canvas = Canvas::new(w, h);
    if opts.depth > MAX_NEST_DEPTH {
        return canvas.into_image();
    }
    let tc = format_time(t, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    for (ti, track) in seq.video_tracks.iter().enumerate() {
        // A cancelled frame job (playback moved on) stops here; its result is discarded.
        if filmcraft_media::cancel::cancelled() {
            return canvas.into_image();
        }
        if only.is_some_and(|o| o != ti) || (!track.enabled && only.is_none()) {
            continue;
        }
        // transition covering t?
        if let Some(tr) = track.transitions.iter().find(|tr| tr.range().contains(t)) {
            let a = tr.from.and_then(|id| track.item(id)).filter(|i| i.enabled);
            let b = tr.to.and_then(|id| track.item(id)).filter(|i| i.enabled);
            let la = a
                .and_then(|i| item_layer(project, seq, i, t, opts, sources, &tc))
                .map(|(img, op, _)| with_opacity(img, op))
                .unwrap_or_else(|| Image::new(w, h));
            let lb = b
                .and_then(|i| item_layer(project, seq, i, t, opts, sources, &tc))
                .map(|(img, op, _)| with_opacity(img, op))
                .unwrap_or_else(|| Image::new(w, h));
            let p = tr.progress(t) as f32;
            // Reverse plays the transition backwards (e.g. an iris closing on the outgoing clip)
            // while still going from A to B.
            let mixed = if tr.reverse {
                transitions::apply_scaled(&tr.effect, &lb, &la, 1.0 - p, opts.scale)
            } else {
                transitions::apply_scaled(&tr.effect, &la, &lb, p, opts.scale)
            };
            blend::composite(canvas.get(), &mixed, 1.0, Blend::Normal);
            continue;
        }
        let Some(item) = track.item_at(t) else { continue };
        if !item.enabled {
            continue;
        }
        let pi = project.item(item.item);
        if let Some(pi) = pi
            && matches!(pi.kind, ItemKind::AdjustmentLayer { .. })
        {
            if !opts.effects {
                continue;
            }
            let mt = item.effect_time_at(t);
            let mut adjusted = canvas.snapshot();
            let cx = effects::FxCtx {
                t: mt,
                px_scale: opts.scale,
                seconds: (t - item.start).seconds(),
                timecode: &tc,
                clip_name: &item.name,
                project: Some(project),
                env: None,
                working: seq.settings.color.working,
            };
            for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic)) {
                mask::apply_effect(&mut adjusted, e, &cx);
            }
            let (op, bl) = opacity_blend(item, mt);
            // Adjustment layer opacity (and opacity masks, in sequence pixels) mix adjusted over original.
            let mut out = canvas.snapshot();
            let mut adj = adjusted;
            adj.scale_alpha(op);
            mask::apply_opacity_masks(&mut adj, item, mt, opts.scale);
            // Motion moves / scales the adjustment layer's frame: it only applies inside it.
            if let Some(region) = adjustment_region(seq, item, project, mt, opts.scale, w, h) {
                let cov: Vec<f32> = region.px.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
                mask::scale_by(&mut adj, &cov);
            }
            blend::composite(&mut out, &adj, 1.0, bl);
            canvas.set(out);
            continue;
        }
        match item_layer_ex(project, seq, item, t, opts, sources, &tc, true) {
            Some(Layer::Full { image, opacity, blend: bl, opaque }) => {
                if canvas.is_empty() && opaque && opacity == 1.0 && bl == Blend::Normal && (image.w, image.h) == (w, h) {
                    // mixing an opaque layer over transparent black gives the layer itself
                    canvas.set(image);
                } else {
                    blend::composite(canvas.get(), &image, opacity, bl);
                    pool::recycle_f32(image.px);
                }
            }
            Some(Layer::Region { image, x, y, opacity, blend: bl }) => {
                blend::composite_at(canvas.get(), &image, x, y, opacity, bl);
                pool::recycle_f32(image.px);
            }
            None => {}
        }
    }
    let mut canvas = canvas.into_image();
    if opts.depth == 0 && !opts.working_output {
        colorman::to_display(&mut canvas, &seq.settings.color);
    }
    // (a nested sequence's captions are part of its picture: see `base_layer`)
    if opts.captions {
        for o in caption_overlays(seq, t, w, h) {
            o.composite_onto(&mut canvas.px, w, h);
        }
    }
    canvas
}

/// Coverage of an adjustment layer's (Motion-transformed) frame in output pixels, or `None` when
/// it covers the whole output.
fn adjustment_region(seq: &Sequence, item: &TrackItem, project: &Project, mt: Tick, scale: f32, w: usize, h: usize) -> Option<Image> {
    let size = source_size(project, item.item).unwrap_or((seq.settings.width, seq.settings.height));
    let motion = motion_matrix(seq, item, size, source_par(project, item.item), mt);
    let s = scale as f64;
    let (fw, fh) = (((size.0 as f64 * s).round() as usize).max(1), ((size.1 as f64 * s).round() as usize).max(1));
    let m = Affine::scale(s, s).then_apply(&motion).then_apply(&Affine::scale(1.0 / s, 1.0 / s));
    let full = fw == w
        && fh == h
        && (m.a - 1.0).abs() < 1e-9
        && (m.d - 1.0).abs() < 1e-9
        && m.b.abs() < 1e-12
        && m.c.abs() < 1e-12
        && m.e.abs() < 1e-9
        && m.f.abs() < 1e-9;
    if full {
        return None;
    }
    Some(Image::filled(fw, fh, [1.0; 4]).transformed(w, h, &m))
}

/// Rendered captions of the visible caption tracks at `t` for a `w`×`h` output.
pub fn caption_overlays(seq: &Sequence, t: Tick, w: usize, h: usize) -> Vec<filmcraft_captions::burn::Overlay> {
    if seq.caption_tracks.is_empty() {
        return Vec::new();
    }
    filmcraft_captions::burn::sequence_overlays(seq, t, w, h)
}

fn with_opacity(mut img: Image, op: f32) -> Image {
    img.scale_alpha(op);
    img
}

pub(crate) fn opacity_blend(item: &TrackItem, mt: Tick) -> (f32, Blend) {
    match item.effect("opacity") {
        Some(e) if e.enabled => {
            let op = (e.f64_at("opacity", mt) / 100.0).clamp(0.0, 1.0) as f32;
            let bl = match e.param("blend").map(|p| &p.value) {
                Some(ParamValue::Choice(c)) => Blend::from_index(*c),
                _ => Blend::Normal,
            };
            (op, bl)
        }
        _ => (1.0, Blend::Normal),
    }
}

/// Size of an item's source at full resolution.
pub fn source_size(project: &Project, item: ItemId) -> Option<(u32, u32)> {
    project.source_size(item)
}

/// Pixel aspect ratio of an item's source, for [`motion_matrix`]: Interpret Footage's override,
/// else the media's; `None` for items drawn in sequence pixels (graphics, adjustment layers).
pub fn source_par(project: &Project, item: ItemId) -> Option<(u32, u32)> {
    project.source_par(item)
}

/// The Motion transform of an item at media time `mt`, mapping full-res source pixels to
/// full-res sequence pixels.
///
/// `src` is the source's size in its own (storage) pixels and `src_par` their pixel aspect ratio
/// (`None`: the sequence's, for graphics and adjustment layers). Motion works in square display
/// units, as Premiere's does: the Anchor Point is in source pixels and the Position in sequence
/// pixels, but Scale and Rotation see the picture at its display aspect, so a 1440 x 1080 clip with
/// 4:3 pixels covers a 1920 x 1080 square-pixel frame at 100 %, and a square-pixel clip in a 4:3
/// sequence is narrowed to its display width. Scale to Frame Size fits the display size too.
pub fn motion_matrix(seq: &Sequence, item: &TrackItem, src: (u32, u32), src_par: Option<(u32, u32)>, mt: Tick) -> Affine {
    let (sw, sh) = (seq.settings.width as f64, seq.settings.height as f64);
    let seq_par = filmcraft_project::sane_par(seq.settings.par);
    let src_par = src_par.map_or(seq_par, filmcraft_project::sane_par);
    let (p, q) = (filmcraft_project::par_ratio(src_par), filmcraft_project::par_ratio(seq_par));
    let mut pos = Vec2::new(sw / 2.0, sh / 2.0);
    let mut anchor = Vec2::new(src.0 as f64 / 2.0, src.1 as f64 / 2.0);
    let mut scale = Vec2::new(1.0, 1.0);
    let mut rot = 0.0;
    if let Some(m) = item.effect("motion").filter(|m| m.enabled) {
        let p = m.vec2_at("position", mt);
        if !p.x.is_nan() && m.param("position").is_some() {
            pos = p;
        }
        let a = m.vec2_at("anchor", mt);
        if !a.x.is_nan() && m.param("anchor").is_some() {
            anchor = a;
        }
        let s = m.f64_at("scale", mt) / 100.0;
        let uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
        let swid = if uniform { s } else { m.f64_at("scale_width", mt) / 100.0 };
        scale = Vec2::new(swid, s);
        rot = m.f64_at("rotation", mt);
    }
    if item.scale_to_frame {
        // the display size fitted in the frame's display size
        let fit = (sw * q / (src.0.max(1) as f64 * p)).min(sh / src.1.max(1) as f64);
        scale = scale * fit;
    }
    // source px → (× p) display units → Scale, Rotation → (÷ q) sequence px. Without rotation the
    // two stretches merge into one horizontal factor, exactly 1 when the ratios are the same.
    if rot == 0.0 {
        let r = if src_par == seq_par { 1.0 } else { p / q };
        return Affine::motion(pos, Vec2::new(scale.x * r, scale.y), rot, anchor);
    }
    Affine::translate(pos.x, pos.y).then_apply(&Affine::scale(1.0 / q, 1.0)).then_apply(&Affine::motion(
        Vec2::ZERO,
        Vec2::new(scale.x * p, scale.y),
        rot,
        anchor,
    ))
}

/// Render one track item's layer at timeline time `t` into a canvas-sized image.
/// Returns (layer, opacity, blend).
pub(crate) fn item_layer(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    tc: &str,
) -> Option<(Image, f32, Blend)> {
    let (w, h) = output_size(seq, opts.scale);
    item_layer_ex(project, seq, item, t, opts, sources, tc, false).map(|l| l.into_full(w, h))
}

/// Whether a placement matrix leaves the layer exactly where it is.
fn identity_placement(m: &Affine) -> bool {
    (m.a - 1.0).abs() < 1e-9 && (m.d - 1.0).abs() < 1e-9 && m.b == 0.0 && m.c == 0.0 && m.e.abs() < 1e-9 && m.f.abs() < 1e-9
}

/// [`item_layer`], which may also hand back only the non-transparent rectangle of a layer
/// (`allow_region`): for a clip of plain media (no effects, no masks, no speed blending, no colour
/// management, placed as it is) whose frame carries an alpha plane, only the part where alpha is not
/// zero is converted and mixed, the rest of the layer being transparent anyway; and a frame
/// without alpha is known to be opaque.
#[allow(clippy::too_many_arguments)]
pub(crate) fn item_layer_ex(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    tc: &str,
    allow_region: bool,
) -> Option<Layer> {
    let (w, h) = output_size(seq, opts.scale);
    // `mt`: where effects and Motion/Opacity are evaluated (differs from the shown frame's media
    // time inside a frame hold without Hold Filters).
    let mt = item.effect_time_at(t);
    let src_size = source_size(project, item.item)?;
    let motion = motion_matrix(seq, item, src_size, source_par(project, item.item), mt);
    // How many output pixels one source pixel covers → request a reduced frame when possible.
    let lin = ((motion.a * motion.a + motion.b * motion.b).sqrt()).max((motion.c * motion.c + motion.d * motion.d).sqrt());
    let want = (lin * opts.scale as f64).clamp(1.0 / 64.0, 1.0) as f32;
    let pi = project.item(item.item)?;
    if matches!(pi.kind, ItemKind::Graphic { .. }) && !(item.has_opacity_masks() || opts.effects && item.has_standard_effects()) {
        // vectors straight to the output: no resampling, crisp at any Motion scale
        let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion);
        let mut canvas = Image::new(w, h);
        graphic_clip::render_graphic(item, mt, src_size, &m, &mut canvas);
        let (op, bl) = opacity_blend(item, mt);
        return Some(Layer::Full { image: canvas, opacity: op, blend: bl, opaque: false });
    }
    // plain media: nothing but the frame itself reaches the output
    let plain_media = matches!(pi.kind, ItemKind::Media(_) | ItemKind::Subclip { .. })
        && !(opts.effects && item.has_standard_effects())
        && !item.effect("opacity").is_some_and(|e| e.enabled && !e.masks.is_empty());
    let mut fetched = None;
    if allow_region && plain_media {
        let (src, frame, n) = media_frame(item, t, sources, want, src_size)?;
        let (lw, lh) = frame.decimated_size(n);
        let px_scale = lw as f32 / src_size.0.max(1) as f32;
        let m =
            Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion).then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
        if (lw, lh) == (w, h) && identity_placement(&m) && interpolation_blend(item, t, src.info().frame_rate()).is_none() {
            let cs = colorman::source_space(project, item.item, &frame);
            if !colorman::needs_management(&seq.settings.color, cs, &frame) {
                let (op, bl) = opacity_blend(item, mt);
                return Some(plain_layer(&frame, n, op, bl));
            }
        }
        // anything else takes the general path below, with the frame already fetched
        fetched = Some((src, frame, n));
    }
    let mut layer = match fetched {
        Some((src, frame, n)) => media_image(project, seq, item, t, &src, &frame, n, want),
        None => base_layer(project, seq, item, t, opts, sources, want)?,
    };
    let px_scale = layer.w as f32 / src_size.0.max(1) as f32;
    // layer px → source px → sequence px → output px
    let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion).then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
    if opts.effects {
        let env = vfx::ItemEnv { project, seq, item, t, opts, sources, want, layer_size: (layer.w, layer.h), layer_to_output: m, tc };
        let cx = effects::FxCtx {
            t: mt,
            px_scale,
            seconds: (t - item.start).seconds(),
            timecode: tc,
            clip_name: &item.name,
            project: Some(project),
            env: Some(&env),
            working: seq.settings.color.working,
        };
        for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e)) {
            if filmcraft_media::cancel::cancelled() {
                return None;
            }
            mask::apply_effect(&mut layer, e, &cx);
        }
    }
    mask::apply_opacity_masks(&mut layer, item, mt, px_scale);
    let placed = if layer.w == w && layer.h == h && identity_placement(&m) { layer } else { layer.transformed(w, h, &m) };
    let (op, bl) = opacity_blend(item, mt);
    Some(Layer::Full { image: placed, opacity: op, blend: bl, opaque: false })
}

/// The layer of a plain media frame (see [`item_layer_ex`]): only the part with alpha when the frame
/// has an alpha plane, the whole picture otherwise (opaque when it has no alpha at all).
fn plain_layer(frame: &VideoFrame, n: usize, opacity: f32, blend: Blend) -> Layer {
    match frame.alpha_region(n) {
        Some(r) => {
            let (r, px) = frame.to_linear_f32_region(n, None, r);
            Layer::Region { image: Image { w: r.w, h: r.h, px }, x: r.x, y: r.y, opacity, blend }
        }
        None => {
            let (w, h, px) = frame.to_linear_f32_decimated(n);
            let opaque = matches!(frame.data, PixelData::Yuv8 { alpha: None, .. } | PixelData::Yuv16 { alpha: None, .. });
            Layer::Full { image: Image { w, h, px }, opacity, blend, opaque }
        }
    }
}

/// The source frame of a media clip at timeline `t` (at `want` of its size), and the decimation to
/// convert it with.
fn media_frame(item: &TrackItem, t: Tick, sources: &dyn SourceProvider, want: f32, src_size: (u32, u32)) -> Option<(SharedSource, Arc<VideoFrame>, usize)> {
    let src = sources.source(item.item)?;
    let frame = src.video_frame(FrameRequest { time: video_source_time(item, t, src.info().frame_rate()), scale: want }).ok()?;
    let n = decimation(frame.width as f32, src_size.0 as f32 * want);
    Some((src, frame, n))
}

/// The media time to ask a source for the video frame a clip shows at timeline `t`. A reversed
/// clip's source time is the instant just before its mirrored position; readers that round the
/// request to the nearest unit of a coarse timescale (MP4) land on the next frame from there,
/// showing one frame past the clip's source range and never its In (#318). Snapped down to the
/// start of the `media_rate` frame that contains it, every reader shows that frame.
pub(crate) fn video_source_time(item: &TrackItem, t: Tick, media_rate: filmcraft_time::FrameRate) -> Tick {
    let ft = item.source_time_at(t);
    if !item.reverse || item.frame_hold.is_some() || media_rate.frame_duration().0 <= 0 {
        return ft;
    }
    media_rate.tick_of(media_rate.frame_at(ft)).clamp(item.source_in.min(ft), ft)
}

/// The linear image of a media clip's `frame`, colour managed, and blended with the next frame when
/// the clip's speed asks for it.
#[allow(clippy::too_many_arguments)]
fn media_image(project: &Project, seq: &Sequence, item: &TrackItem, t: Tick, src: &SharedSource, frame: &VideoFrame, n: usize, want: f32) -> Image {
    let img = colorman::decode(project, item.item, frame, n, &seq.settings.color);
    match interpolation_blend(item, t, src.info().frame_rate()) {
        Some((next_time, wgt)) => match src.video_frame(FrameRequest { time: next_time, scale: want }) {
            Ok(f2) if f2.width == frame.width && f2.height == frame.height => {
                let b = colorman::decode(project, item.item, &f2, n, &seq.settings.color);
                img.lerp(&b, wgt)
            }
            _ => img,
        },
        None => img,
    }
}

/// A track item's picture at timeline `t` before any effect (decoded, colour managed, frame
/// blended; nested sequences rendered; graphics rasterised), at `want` of its source size.
pub(crate) fn base_layer(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    want: f32,
) -> Option<Image> {
    // `ft`: the media time of the frame shown
    let ft = item.source_time_at(t);
    let mt = item.effect_time_at(t);
    let src_size = source_size(project, item.item)?;
    let pi = project.item(item.item)?;
    Some(match &pi.kind {
        ItemKind::Media(_) | ItemKind::Subclip { .. } => {
            let (src, frame, n) = media_frame(item, t, sources, want, src_size)?;
            media_image(project, seq, item, t, &src, &frame, n, want)
        }
        ItemKind::Sequence(nested) => {
            let sub = RenderOptions { scale: want, effects: opts.effects, depth: opts.depth + 1, captions: false, working_output: true };
            match item.multicam_angle(nested) {
                // a multi-camera clip shows its angle's track only (nothing for an audio-only angle)
                Some(angle) => match nested.angle_video_track_index(angle) {
                    Some(ti) => render_seq_tracks(project, nested, ft, sub, sources, Some(ti)),
                    None => {
                        let (nw, nh) = output_size(nested, want);
                        Image::new(nw, nh)
                    }
                },
                // A nested sequence shows its captions wherever it is nested, as part of its
                // picture (Premiere does the same), whether or not the outer sequence shows its own.
                None => render_seq(project, nested, ft, RenderOptions { captions: true, ..sub }, sources),
            }
        }
        ItemKind::AdjustmentLayer { .. } => return None,
        ItemKind::Graphic { .. } => {
            // standard effects work on the graphic at source resolution, then Motion places it
            let (gw, gh) = (((src_size.0 as f32 * want).ceil() as usize).max(1), ((src_size.1 as f32 * want).ceil() as usize).max(1));
            let mut img = Image::new(gw, gh);
            graphic_clip::render_graphic(item, mt, src_size, &Affine::scale(want as f64, want as f64), &mut img);
            img
        }
    })
}

/// The layer of one clip at timeline `t` with its effects applied, on a canvas the size of the
/// sequence output (transparent elsewhere), before opacity and blending. Used by Apply Match.
pub fn render_clip(
    project: &Project,
    seq_id: ItemId,
    clip: filmcraft_project::ClipId,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
) -> Option<Image> {
    let seq = project.sequence(seq_id)?;
    let (_, item) = seq.find_item(clip)?;
    let tc = format_time(t, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    item_layer(project, seq, item, t, opts, sources, &tc).map(|(img, _, _)| img)
}

/// Frame Blending / Optical Flow on a speed-changed clip: the later source frame to mix in and
/// its weight (0..1) at timeline `t`, or None when the exact media time falls on a source frame
/// (or the clip plays at 100 %, is frame-held, or uses Frame Sampling).
///
/// TODO(optical flow): motion-compensated interpolation; Optical Flow renders as Frame Blending.
pub fn interpolation_blend(item: &TrackItem, t: Tick, src_rate: filmcraft_time::FrameRate) -> Option<(Tick, f32)> {
    if item.time_interpolation == filmcraft_project::TimeInterpolation::FrameSampling || item.frame_hold.is_some() {
        return None;
    }
    if (item.speed.abs() - 1.0).abs() < 1e-9 {
        return None;
    }
    let mt = item.source_time_at(t);
    if src_rate.frame_duration().0 <= 0 {
        return None;
    }
    // position in source frames from the media origin
    let f = src_rate.frame_at(mt);
    let f0 = src_rate.tick_of(f);
    let frac = (mt - f0).0 as f64 / (src_rate.tick_of(f + 1) - f0).0.max(1) as f64;
    if frac <= 1e-6 {
        return None;
    }
    // the mix depends only on the media position, so reversed clips blend the same pair
    Some((src_rate.tick_of(f + 1), frac.clamp(0.0, 1.0) as f32))
}

/// Largest power-of-two box decimation that keeps at least `target_w` pixels of width.
pub(crate) fn decimation(have_w: f32, target_w: f32) -> usize {
    let mut n = 1usize;
    while n < 16 && have_w / (n as f32 * 2.0) >= target_w.max(1.0) {
        n *= 2;
    }
    n
}

/// Render a single project item (e.g. for the Source monitor) at media time `t`.
pub fn render_item(project: &Project, item: ItemId, t: Tick, scale: f32, sources: &dyn SourceProvider) -> Option<Image> {
    let pi = project.item(item)?;
    match &pi.kind {
        ItemKind::Sequence(s) => Some(render_seq(project, s, t, RenderOptions { scale, ..Default::default() }, sources)),
        _ => {
            let src = sources.source(item)?;
            let f = src.video_frame(FrameRequest { time: t, scale }).ok()?;
            let full_w = src.info().video.as_ref().map_or(f.width, |v| v.width) as f32;
            let n = decimation(f.width as f32, full_w * scale);
            // the Source monitor shows media as SDR Rec. 709 (log/HDR tone mapped per its colour space)
            Some(colorman::decode(project, item, &f, n, &filmcraft_color::ColorPipeline::REC709))
        }
    }
}

/// A shared, clonable source map for tests and simple hosts.
#[derive(Default, Clone)]
pub struct SourceMap(pub std::collections::HashMap<ItemId, SharedSource>);

impl SourceProvider for SourceMap {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        self.0.get(&item).cloned()
    }
}

pub fn arc_source(s: impl filmcraft_media::MediaSource + 'static) -> SharedSource {
    Arc::new(s)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lumetri_hdr_tests;

#[cfg(test)]
#[path = "adjustment_tests.rs"]
mod adjustment_tests;

#[cfg(test)]
#[path = "mixer_tests.rs"]
mod mixer_tests;

#[cfg(test)]
mod nest_tests;

#[cfg(test)]
#[path = "preview_tests.rs"]
mod preview_tests;

#[cfg(test)]
#[path = "region_tests.rs"]
mod region_tests;

#[cfg(test)]
#[path = "par_tests.rs"]
mod par_tests;

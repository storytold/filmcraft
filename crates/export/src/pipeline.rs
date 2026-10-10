//! The per-frame picture pipeline shared by every export format: render the sequence at the
//! output frame rate, fit it into the output frame (scale to fit / fill / stretch, pixel aspect),
//! apply the Export effects (image, name and timecode overlays, video limiter) and convert to the
//! encoder's input (straight sRGB RGBA8, or encoded R'G'B' floats for HDR).

use std::sync::Arc;

use filmcraft_geom::Vec2;
use filmcraft_project::{ItemId, Project};
use filmcraft_render::{Image, RenderOptions, SourceProvider};
use filmcraft_time::FrameRate;
use rayon::prelude::*;

use crate::settings::{ExportEffects, Placement, Scaling, TextOverlay};
use crate::{ExportError, ExportSettings, Format, FrameRenderer, GpuRendering, Result, build_frame_renderer, note_gpu_fallback, note_gpu_frame};

/// Where the rendered picture lands in the output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub w: u32,
    pub h: u32,
    /// Render scale (relative to the sequence frame size).
    pub render_scale: f32,
    /// Size of the picture in the output frame (None = as rendered).
    pub content: Option<(usize, usize)>,
    /// Top-left of the picture in the output frame (negative = cropped).
    pub offset: (i64, i64),
}

impl Geometry {
    pub fn new(settings: &ExportSettings, seq_w: u32, seq_h: u32, w: u32, h: u32) -> Self {
        let (sw, sh) = (seq_w.max(1) as f32, seq_h.max(1) as f32);
        if settings.frame_size.is_none() && settings.pixel_aspect.is_none() && !settings.max_render_quality {
            // the sequence size times `scale`, rendered directly (even-size crop/pad at the edge)
            return Geometry { w, h, render_scale: w as f32 / sw, content: None, offset: (0, 0) };
        }
        let par = settings.pixel_aspect.map(|(n, d)| n.max(1) as f32 / d.max(1) as f32).unwrap_or(1.0);
        let disp_w = w as f32 * par;
        let (fx, fy) = (disp_w / sw, h as f32 / sh);
        let (sx, sy) = match settings.scaling {
            Scaling::ScaleToFit => (fx.min(fy), fx.min(fy)),
            Scaling::ScaleToFill => (fx.max(fy), fx.max(fy)),
            Scaling::StretchToFill => (fx, fy),
        };
        let cw = (sw * sx / par).round().max(1.0) as usize;
        let ch = (sh * sy).round().max(1.0) as usize;
        let render_scale = if settings.max_render_quality { 1.0f32.max(sy).max(sx / par) } else { sy.max(sx / par) }.clamp(0.01, 4.0);
        Geometry { w, h, render_scale, content: Some((cw, ch)), offset: ((w as i64 - cw as i64) / 2, (h as i64 - ch as i64) / 2) }
    }
}

/// GPU renderers shared by the export workers: a worker takes a free one for a frame and puts it
/// back (a single renderer behind a mutex made the workers queue, GPU export ~20 % slower than CPU).
struct RendererPool {
    renderers: std::sync::Mutex<Vec<Box<dyn FrameRenderer>>>,
    available: std::sync::Condvar,
    /// A renderer panicked: the pool is retired and the rest of the export renders on the CPU.
    failed: std::sync::atomic::AtomicBool,
}

impl RendererPool {
    /// `None` for an empty list (a pool nobody could take from).
    fn new(renderers: Vec<Box<dyn FrameRenderer>>) -> Option<Self> {
        (!renderers.is_empty()).then(|| Self {
            renderers: std::sync::Mutex::new(renderers),
            available: std::sync::Condvar::new(),
            failed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Run `f` with a free renderer, waiting for one if all are busy. `None` once a renderer has
    /// panicked: the panicking renderer is dropped (its state can't be trusted) and so are the
    /// others as they come back, so the caller falls back to the CPU for the rest of the export.
    fn with<R>(&self, f: impl FnOnce(&mut dyn FrameRenderer) -> R) -> Option<R> {
        use std::sync::atomic::Ordering;
        let mut free = self.renderers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut r = loop {
            if self.failed.load(Ordering::SeqCst) {
                return None;
            }
            match free.pop() {
                Some(r) => break r,
                None => free = self.available.wait(free).unwrap_or_else(std::sync::PoisonError::into_inner),
            }
        };
        drop(free);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *r)));
        let mut free = self.renderers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if res.is_err() {
            // (the panic hook has logged the panic itself)
            self.failed.store(true, Ordering::SeqCst);
            free.clear();
            drop(free);
            drop(r);
        } else if self.failed.load(Ordering::SeqCst) {
            drop(free);
            drop(r);
        } else {
            free.push(r);
            drop(free);
        }
        // waiters re-check `failed` (or take the renderer that came back)
        self.available.notify_all();
        res.ok()
    }
}

/// The immutable part of an export: everything needed to produce one output frame.
pub(crate) struct Pipeline {
    pub project: Arc<Project>,
    /// The registered frame renderers (GPU compositors) when the setting allows and factories
    /// provide them; pooled because `frame` runs on several export threads at once.
    renderer: Option<RendererPool>,
    pub seq: ItemId,
    /// Output frame rate (frame `f` is at `rate.tick_of(f)`).
    pub rate: FrameRate,
    seq_rate: FrameRate,
    pub w: u32,
    pub h: u32,
    geom: Geometry,
    opts: RenderOptions,
    pub hdr_out: bool,
    /// Keep straight alpha in the RGBA8 output instead of flattening over black.
    alpha: bool,
    out_tf: Option<filmcraft_color::OutputTransform>,
    effects: ExportEffects,
    overlay: Option<Image>,
    start_tc: i64,
    drop_frame: bool,
    /// Free the pool's float images when the export ends: a standalone export, not one part of a
    /// batch (`ExportSettings::part_of_batch`).
    trim_pool: bool,
}

impl Drop for Pipeline {
    /// The export is over, however it ended (done, failed, cancelled, dropped half way). The float
    /// images its frames left on the `filmcraft_frame::pool` float shelf are the size of its
    /// frames, which nothing else asks for, so free them instead of holding up to 320 MiB idle. A
    /// job still rendering at that moment allocates a few images again; the pool is only a cache.
    /// Parts of a larger job (render-preview segments, proxies) keep them for the next part.
    fn drop(&mut self) {
        if self.trim_pool {
            filmcraft_frame::pool::trim_f32();
        }
    }
}

impl Pipeline {
    pub fn new(project: Arc<Project>, seq: ItemId, settings: &ExportSettings, hdr_out: bool) -> Result<Self> {
        let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
        q.settings.validate().map_err(ExportError::Unsupported)?;
        settings.validate()?;
        let r = settings.resolve(q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate);
        filmcraft_project::validate_frame_size(r.width, r.height).map_err(ExportError::Unsupported)?;
        let geom = Geometry::new(settings, q.settings.width, q.settings.height, r.width, r.height);
        if let Some((w, h)) = geom.content {
            let w = u32::try_from(w).map_err(|_| ExportError::Unsupported("scaled picture width is too large".into()))?;
            let h = u32::try_from(h).map_err(|_| ExportError::Unsupported("scaled picture height is too large".into()))?;
            filmcraft_project::validate_frame_size(w, h).map_err(ExportError::Unsupported)?;
        }
        let (w, h) = filmcraft_render::output_size(q, geom.render_scale);
        let w = u32::try_from(w).map_err(|_| ExportError::Unsupported("rendered picture width is too large".into()))?;
        let h = u32::try_from(h).map_err(|_| ExportError::Unsupported("rendered picture height is too large".into()))?;
        filmcraft_project::validate_frame_size(w, h).map_err(ExportError::Unsupported)?;
        let pipe = q.settings.color;
        let out_tf = hdr_out.then(|| filmcraft_color::OutputTransform::new(&pipe, pipe.working.output_space()));
        let opts = RenderOptions { scale: geom.render_scale, captions: settings.burn_captions, working_output: hdr_out, ..Default::default() };
        let effects = settings.effects.clone();
        let overlay = if effects.image_overlay.enabled && !effects.image_overlay.path.is_empty() {
            Some(load_overlay(&effects.image_overlay.path, r.width as f32 * effects.image_overlay.size_percent.clamp(0.5, 100.0) / 100.0)?)
        } else {
            None
        };
        let renderer = if settings.gpu_rendering == GpuRendering::Off {
            None
        } else {
            // Four renderers: as fast as one per worker (measured on 16 threads, where more only
            // contend for memory bandwidth in the read-back), with a quarter of the GPU memory.
            let count = rayon::current_num_threads().clamp(2, 4);
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                if let Some(r) = build_frame_renderer() {
                    list.push(r);
                } else {
                    break;
                }
            }
            RendererPool::new(list)
        };
        Ok(Pipeline {
            renderer,
            seq_rate: q.settings.frame_rate,
            start_tc: q.start_timecode,
            drop_frame: q.settings.drop_frame,
            trim_pool: !settings.part_of_batch,
            project: project.clone(),
            seq,
            rate: r.rate,
            w: r.width,
            h: r.height,
            geom,
            opts,
            hdr_out,
            alpha: settings.alpha && matches!(settings.format, Format::PngSequence | Format::TiffSequence),
            out_tf,
            effects,
            overlay,
        })
    }

    /// Output frame `f` as straight sRGB RGBA8 at the output size, or for HDR exports the encoded
    /// R'G'B' floats (3 per pixel).
    pub fn frame(&self, f: i64, sources: &dyn SourceProvider) -> (Vec<u8>, Vec<f32>) {
        let t = self.rate.tick_of(f);
        // The registered GPU renderer goes first; `None` (no adapter, a plan the GPU cannot
        // draw, any internal error) falls back to the CPU reference renderer below. A frame the
        // planner hands back as one CPU image anyway (an adjustment layer, a complex transition,
        // HDR) does not take a pooled renderer: it would render on the CPU inside the pool, as
        // many frames at a time as there are renderers instead of one per export thread.
        let pool = self.renderer.as_ref().filter(|_| !filmcraft_render::plan::is_cpu_frame(&self.project, self.seq, t));
        let gpu = match pool {
            Some(pool) => {
                let asked = web_time::Instant::now();
                pool.with(|r| {
                    crate::note_lock_wait(asked.elapsed());
                    r.render(&self.project, self.seq, t, self.opts, sources)
                })
                .flatten()
            }
            None => None,
        };
        let img = match gpu {
            Some(img) => {
                note_gpu_frame();
                img
            }
            None => {
                if self.renderer.is_some() {
                    note_gpu_fallback();
                }
                filmcraft_render::render_sequence(&self.project, self.seq, t, self.opts, sources)
            }
        };
        let mut img = self.place(img);
        self.overlays(&mut img, t);
        let lim = &self.effects.video_limiter;
        if let Some(tf) = self.out_tf.as_ref().filter(|_| self.hdr_out) {
            let mut out = vec![0f32; img.w * img.h * 3];
            out.par_chunks_mut(img.w * 3).zip(img.px.par_chunks(img.w * 4)).for_each(|(o, s)| {
                for (o, p) in o.as_chunks_mut::<3>().0.iter_mut().zip(s.as_chunks::<4>().0) {
                    o.copy_from_slice(&tf.encode([p[0], p[1], p[2]]));
                }
            });
            if lim.enabled {
                limit_f32(&mut out, lim.min_percent, lim.max_percent);
            }
            // the float image is not needed any more: its buffer serves the next frame's layers
            filmcraft_frame::pool::recycle_f32(img.px);
            return (Vec::new(), out);
        }
        let mut rgba = if self.alpha { img.to_rgba8() } else { img.over_black_rgba8() };
        // the float image is not needed any more: its buffer serves the next frame's layers
        filmcraft_frame::pool::recycle_f32(img.px);
        if lim.enabled {
            limit_rgba8(&mut rgba, lim.min_percent, lim.max_percent);
        }
        (rgba, Vec::new())
    }

    /// Fit the rendered picture into the output frame.
    fn place(&self, img: Image) -> Image {
        let (w, h) = (self.w as usize, self.h as usize);
        let img = match self.geom.content {
            Some((cw, ch)) if (cw, ch) != (img.w, img.h) => resample(&img, cw, ch),
            _ => img,
        };
        if img.w == w && img.h == h && self.geom.offset == (0, 0) {
            return img;
        }
        let mut out = Image::new(w, h);
        let (ox, oy) = self.geom.offset;
        let x0 = ox.max(0) as usize;
        let x1 = (ox + img.w as i64).clamp(0, w as i64) as usize;
        if x1 <= x0 {
            return out;
        }
        out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            let sy = y as i64 - oy;
            if sy < 0 || sy >= img.h as i64 {
                return;
            }
            let sx0 = (x0 as i64 - ox) as usize;
            let src = &img.px[(sy as usize * img.w + sx0) * 4..(sy as usize * img.w + sx0 + (x1 - x0)) * 4];
            row[x0 * 4..x1 * 4].copy_from_slice(src);
        });
        out
    }

    fn overlays(&self, img: &mut Image, t: filmcraft_time::Tick) {
        let fx = &self.effects;
        if let Some(ov) = &self.overlay {
            let o = &fx.image_overlay;
            let (x, y) = anchor(o.placement, o.offset, img.w as f32, img.h as f32, ov.w as f32, ov.h as f32);
            composite(img, ov, x.round() as i64, y.round() as i64, o.opacity / 100.0);
        }
        if fx.name_overlay.enabled && !fx.name_overlay.text.trim().is_empty() {
            self.text(img, &fx.name_overlay, fx.name_overlay.text.trim());
        }
        if fx.timecode_overlay.enabled {
            let frame = self.seq_rate.frame_at(t) + self.start_tc;
            let tc = filmcraft_time::format_timecode_frames(frame, self.seq_rate, self.drop_frame);
            let text = if fx.timecode_overlay.text.trim().is_empty() { tc } else { format!("{} {tc}", fx.timecode_overlay.text.trim()) };
            self.text(img, &fx.timecode_overlay, &text);
        }
    }

    /// White text on a translucent black box (the Timecode / Clip Name burn-in look).
    fn text(&self, img: &mut Image, o: &TextOverlay, text: &str) {
        let px = (img.h as f32 * o.size_percent.clamp(1.0, 50.0) / 100.0).max(6.0);
        let family = filmcraft_text::fonts::DEFAULT_FAMILY;
        let st = filmcraft_text::TextStyle { family: family.into(), style: "Regular".into(), size: px, ..Default::default() };
        let l = filmcraft_text::layout(text, &st, &filmcraft_text::ParagraphStyle::default());
        let pad = px * 0.25;
        let (bw, bh) = (l.bounds[2] - l.bounds[0] + 2.0 * pad, px * 1.1 + 2.0 * pad);
        let (x, y) = anchor(o.placement, o.offset, img.w as f32, img.h as f32, bw, bh);
        let centre = Vec2::new((x + bw / 2.0) as f64, (y + bh / 2.0) as f64);
        let op = (o.opacity / 100.0).clamp(0.0, 1.0);
        if op >= 0.999 {
            filmcraft_render::graphics::burn_text(img, text, family, centre, px, 0.6);
            return;
        }
        // draw on a transparent layer, then composite it at the overlay's opacity
        let mut layer = Image::new(img.w, img.h);
        filmcraft_render::graphics::burn_text(&mut layer, text, family, centre, px, 0.6);
        composite(img, &layer, 0, 0, op);
    }
}

/// Top-left of a `bw`×`bh` box placed in a `w`×`h` frame (3 % margin) plus `offset`.
fn anchor(p: Placement, offset: (f32, f32), w: f32, h: f32, bw: f32, bh: f32) -> (f32, f32) {
    let (ax, ay) = p.anchor();
    let m = 0.03 * w.min(h);
    (m + ax * (w - 2.0 * m - bw) + offset.0, m + ay * (h - 2.0 * m - bh) + offset.1)
}

/// `src` (premultiplied) over `dst` at (`x`, `y`) with `opacity`.
fn composite(dst: &mut Image, src: &Image, x: i64, y: i64, opacity: f32) {
    let op = opacity.clamp(0.0, 1.0);
    if op <= 0.0 {
        return;
    }
    let w = dst.w;
    let xs = x.max(0)..(x + src.w as i64).min(w as i64);
    if xs.is_empty() {
        return;
    }
    dst.px.par_chunks_mut(w * 4).enumerate().for_each(|(dy, row)| {
        let sy = dy as i64 - y;
        if sy < 0 || sy >= src.h as i64 {
            return;
        }
        for dx in xs.clone() {
            let s = &src.px[(sy as usize * src.w + (dx - x) as usize) * 4..][..4];
            if s[3] <= 0.0 {
                continue;
            }
            let d = &mut row[dx as usize * 4..dx as usize * 4 + 4];
            let k = 1.0 - s[3] * op;
            for c in 0..4 {
                d[c] = s[c] * op + d[c] * k;
            }
        }
    });
}

/// Load a PNG / JPEG overlay as premultiplied linear light, `width` pixels wide.
fn load_overlay(path: &str, width: f32) -> Result<Image> {
    let bytes = std::fs::read(path).map_err(|e| ExportError::Io(format!("image overlay {path}: {e}")))?;
    let rgba = image::load_from_memory(&bytes).map_err(|e| ExportError::Unsupported(format!("image overlay {path}: {e}")))?.to_rgba8();
    let (iw, ih) = (rgba.width() as usize, rgba.height() as usize);
    let mut img = Image::new(iw, ih);
    for (o, p) in img.px.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_raw().as_chunks::<4>().0) {
        let a = p[3] as f32 / 255.0;
        for c in 0..3 {
            o[c] = filmcraft_color::srgb_to_linear(p[c] as f32 / 255.0) * a;
        }
        o[3] = a;
    }
    let nw = width.round().max(1.0) as usize;
    let nh = ((ih as f32 * nw as f32 / iw.max(1) as f32).round() as usize).max(1);
    Ok(if (nw, nh) == (iw, ih) { img } else { resample(&img, nw, nh) })
}

/// Separable resample of a premultiplied image with a triangle filter widened by the reduction
/// factor (area-correct when downscaling, bilinear when enlarging).
pub(crate) fn resample(src: &Image, nw: usize, nh: usize) -> Image {
    let tmp = resample_axis(src, nw, true);
    resample_axis(&tmp, nh, false)
}

fn weights(n_src: usize, n_dst: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = n_src as f32 / n_dst as f32;
    let support = scale.max(1.0);
    (0..n_dst)
        .map(|i| {
            let c = (i as f32 + 0.5) * scale - 0.5;
            let lo = ((c - support).ceil() as i64).max(0) as usize;
            let hi = ((c + support).floor() as i64).min(n_src as i64 - 1).max(lo as i64) as usize;
            let mut ws: Vec<f32> = (lo..=hi).map(|j| (1.0 - (j as f32 - c).abs() / support).max(0.0)).collect();
            let sum: f32 = ws.iter().sum();
            if sum > 0.0 {
                ws.iter_mut().for_each(|w| *w /= sum);
            } else {
                ws.iter_mut().for_each(|w| *w = 0.0);
                if let Some(w) = ws.first_mut() {
                    *w = 1.0;
                }
            }
            (lo, ws)
        })
        .collect()
}

fn resample_axis(src: &Image, n: usize, horizontal: bool) -> Image {
    let (w, h) = if horizontal { (n, src.h) } else { (src.w, n) };
    if (horizontal && n == src.w) || (!horizontal && n == src.h) {
        return src.clone();
    }
    let ws = weights(if horizontal { src.w } else { src.h }, n);
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (lo, k) = if horizontal { &ws[x] } else { &ws[y] };
            let mut acc = [0f32; 4];
            for (j, wt) in k.iter().enumerate() {
                let (sx, sy) = if horizontal { (lo + j, y) } else { (x, lo + j) };
                let p = &src.px[(sy * src.w + sx) * 4..][..4];
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            row[x * 4..x * 4 + 4].copy_from_slice(&acc);
        }
    });
    out
}

/// Keep BT.709 luma of display-encoded RGBA8 inside [min, max] percent: above the ceiling the
/// pixel is scaled down, below the floor it is lifted (hue kept), then channels are clipped.
pub fn limit_rgba8(rgba: &mut [u8], min_percent: f32, max_percent: f32) {
    let (lo, hi) = (min_percent.clamp(0.0, 100.0) / 100.0, max_percent.clamp(0.0, 100.0) / 100.0);
    rgba.par_chunks_mut(4 * 1024).for_each(|chunk| {
        for p in chunk.as_chunks_mut::<4>().0 {
            let mut c = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
            limit_px(&mut c, lo, hi);
            for i in 0..3 {
                p[i] = (c[i] * 255.0).floor().clamp(0.0, 255.0) as u8;
            }
        }
    });
}

fn limit_f32(rgb: &mut [f32], min_percent: f32, max_percent: f32) {
    let (lo, hi) = (min_percent.clamp(0.0, 100.0) / 100.0, max_percent.clamp(0.0, 100.0) / 100.0);
    rgb.par_chunks_mut(3 * 1024).for_each(|chunk| {
        for p in chunk.as_chunks_mut::<3>().0 {
            let mut c = [p[0], p[1], p[2]];
            limit_px(&mut c, lo, hi);
            p.copy_from_slice(&c);
        }
    });
}

fn limit_px(c: &mut [f32; 3], lo: f32, hi: f32) {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    if y > hi && y > 0.0 {
        let k = hi / y;
        c.iter_mut().for_each(|v| *v *= k);
    } else if y < lo {
        let d = lo - y;
        c.iter_mut().for_each(|v| *v += d);
    }
    c.iter_mut().for_each(|v| *v = v.clamp(0.0, hi.max(lo)));
}

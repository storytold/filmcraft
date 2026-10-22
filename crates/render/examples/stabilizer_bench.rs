//! Synthetic Warp Stabilizer analysis timings; excludes media decode and does not imply UHD speedups.
//! Run with `RAYON_NUM_THREADS=8 cargo run --release -p filmcraft-render --example stabilizer_bench`.
//! Subspace measurements are bounded synthetic CPU evidence only; they make no UHD or Adobe speedup claim.
//! `--subspace-bench` covers normal/detailed × serial/fast Method3, and `--write-frames` emits
//! a target/subspace-warp-smoke input/output PPM sequence for native/ffmpeg smoke only.
use std::env;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

use filmcraft_geom::{Affine, Vec2};
use filmcraft_media::{FrameRequest, SharedSource};
use filmcraft_project::{EffectInstance, ParamValue, TrackMethod, find_effect};
use filmcraft_render::Image;
use filmcraft_render::effects::{self, FxCtx};
use filmcraft_render::track::{Gray, MAX_FEATURES, Prepared, features, fit_robust, track_step};
use filmcraft_render::vfx::{FxEnv, clear_stabilizer_cache, stabilizer_path};
use filmcraft_time::Tick;

struct Clip {
    base: Gray,
    reads: AtomicUsize,
}
impl Clip {
    fn image(&self, frame: usize, scale: f32) -> Image {
        let (w, h) = ((960.0 * scale).round() as usize, (540.0 * scale).round() as usize);
        let mut img = Image::new(w, h);
        let (dx, dy) = shake(frame);
        for (i, p) in img.px.chunks_exact_mut(4).enumerate() {
            let v = self.base.sample((i % w) as f32 / scale + dx, (i / w) as f32 / scale + dy);
            let v = filmcraft_color::srgb_to_linear(v);
            p.copy_from_slice(&[v, v, v, 1.0]);
        }
        img
    }
}
fn shake(frame: usize) -> (f32, f32) {
    let f = frame as f32;
    (3.0 * (f * 1.7).sin() + f * 0.15, 2.0 * (f * 2.1).cos())
}
impl FxEnv for Clip {
    fn frame(&self, _: f64, _: &EffectInstance) -> Option<Image> {
        None
    }
    fn source_frame(&self, dt: f64, scale: f32) -> Option<Image> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Some(self.image((dt * 24.0).round().max(0.0) as usize, scale))
    }
    fn track(&self, _: usize) -> Option<Image> {
        None
    }
    fn layer_to_output(&self) -> Affine {
        Affine::IDENTITY
    }
    fn clip_seconds(&self) -> f64 {
        2.0
    }
    fn clip_offset(&self) -> f64 {
        0.0
    }
    fn frame_rate(&self) -> f64 {
        24.0
    }
    fn media_timecode(&self) -> String {
        String::new()
    }
    fn file_name(&self) -> String {
        String::new()
    }
    fn sequence_name(&self) -> String {
        String::new()
    }
    fn sequence_size(&self) -> (u32, u32) {
        (960, 540)
    }
    fn source_size(&self) -> (u32, u32) {
        (960, 540)
    }
    fn clip_key(&self) -> u64 {
        0x4641_5354_4245_4e43
    }
}

/// A product-decoder-backed clip used by `--real-diagnostic`.
///
/// The environment follows the same contract as the compositor: `now` is the clip-local
/// timeline position and `source_frame(dt, ..)` asks for `now + dt` in media time. Keeping this
/// wrapper in the example makes the diagnostic exercise the Matroska demuxer, H.264 decoder, and
/// the actual Warp Stabilizer effect instead of a second synthetic decoder.
struct RealClip {
    source: SharedSource,
    name: String,
    width: usize,
    height: usize,
    fps: f64,
    seconds: f64,
    now: f64,
    key: u64,
}

impl RealClip {
    fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let source = filmcraft_media::open_path(path, &filmcraft_codecs::openers())?;
        let video = source.info().video.as_ref().ok_or("real diagnostic source has no video stream")?;
        let width = usize::try_from(video.width)?;
        let height = usize::try_from(video.height)?;
        let fps = video.frame_rate.as_f64();
        let seconds = source.info().duration.seconds();
        if width == 0 || height == 0 || !fps.is_finite() || fps <= 0.0 || !seconds.is_finite() || seconds <= 0.0 {
            return Err("real diagnostic source has invalid video geometry, frame rate, or duration".into());
        }
        let mut key = 0xcbf2_9ce4_8422_2325u64;
        for byte in path.to_string_lossy().as_bytes() {
            key = (key ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
        Ok(Self { source, name: path.display().to_string(), width, height, fps, seconds, now: 0.0, key })
    }

    fn frame_at(&self, seconds: f64, scale: f32) -> Result<(Image, f64), Box<dyn std::error::Error>> {
        if !seconds.is_finite() || !scale.is_finite() || scale <= 0.0 {
            return Err("real diagnostic frame request is non-finite".into());
        }
        let frame = self.source.video_frame(FrameRequest { time: Tick::from_seconds_f64(seconds.max(0.0)), scale: scale.clamp(1.0 / 64.0, 1.0) })?;
        let pts = frame.pts.seconds();
        let mut decimation = 1;
        let target_width = self.width as f32 * scale.clamp(1.0 / 64.0, 1.0);
        while decimation < 16 && frame.width as f32 / (decimation as f32 * 2.0) >= target_width.max(1.0) {
            decimation *= 2;
        }
        let (w, h, px) = frame.to_linear_f32_decimated(decimation);
        Ok((Image { w, h, px }, pts))
    }

    fn image_at(&self, seconds: f64, scale: f32) -> Option<Image> {
        self.frame_at(seconds, scale).ok().map(|(image, _)| image)
    }

    fn at(&self, now: f64) -> Self {
        Self { now, ..self.clone_for_env() }
    }

    fn clone_for_env(&self) -> Self {
        Self {
            source: Arc::clone(&self.source),
            name: self.name.clone(),
            width: self.width,
            height: self.height,
            fps: self.fps,
            seconds: self.seconds,
            now: self.now,
            key: self.key,
        }
    }
}

impl FxEnv for RealClip {
    fn frame(&self, dt: f64, _: &EffectInstance) -> Option<Image> {
        self.image_at(self.now + dt, 1.0)
    }

    fn source_frame(&self, dt: f64, scale: f32) -> Option<Image> {
        self.image_at(self.now + dt, scale)
    }

    fn track(&self, _: usize) -> Option<Image> {
        None
    }

    fn layer_to_output(&self) -> Affine {
        Affine::IDENTITY
    }

    fn clip_seconds(&self) -> f64 {
        self.seconds
    }

    fn clip_offset(&self) -> f64 {
        self.now
    }

    fn frame_rate(&self) -> f64 {
        self.fps
    }

    fn media_timecode(&self) -> String {
        String::new()
    }

    fn file_name(&self) -> String {
        self.name.clone()
    }

    fn sequence_name(&self) -> String {
        "stabilizer-real-diagnostic".to_string()
    }

    fn sequence_size(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    fn source_size(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    fn clip_key(&self) -> u64 {
        self.key
    }
}
fn gray(img: &Image) -> Gray {
    Gray::new(img.w, img.h, img.px.chunks_exact(4).map(|p| filmcraft_color::linear_to_srgb(p[0])).collect())
}
const SUBSPACE_WIDTH: usize = 128;
const SUBSPACE_HEIGHT: usize = 80;
const SUBSPACE_FRAMES: usize = 64;
const SUBSPACE_FPS: f64 = 24.0;

#[derive(Clone, Copy)]
struct SubspaceScene {
    width: usize,
    height: usize,
    frames: usize,
    fps: f64,
}

impl SubspaceScene {
    fn new() -> Self {
        Self { width: SUBSPACE_WIDTH, height: SUBSPACE_HEIGHT, frames: SUBSPACE_FRAMES, fps: SUBSPACE_FPS }
    }

    fn clip_seconds(self) -> f64 {
        ((self.frames.saturating_sub(1) as f64) + 0.5) / self.fps
    }

    fn frame_at(self, seconds: f64) -> usize {
        if self.frames == 0 || !seconds.is_finite() {
            return 0;
        }
        (seconds * self.fps).round().clamp(0.0, self.frames.saturating_sub(1) as f64) as usize
    }

    /// A camera path with a depth-dependent translation and a row-dependent wobble.  Frame zero
    /// is the static ground truth, so image comparisons do not rely on an echoed input transform.
    fn displacement(self, frame: usize, y: f64, depth: f64) -> (f64, f64) {
        if frame == 0 {
            return (0.0, 0.0);
        }
        let f = frame as f64;
        let camera_x = 2.4 * (f * 0.21).sin() + 0.7 * (f * 0.07).cos() + 0.04 * f;
        let camera_y = 1.7 * (f * 0.17).cos() + 0.35 * (f * 0.37).sin();
        let row = y / self.height.max(1) as f64;
        let rolling_x = 0.5 * (f * 0.43 + row * 6.0).sin() + 0.18 * (f * 0.13 - row * 8.0).cos();
        let rolling_y = 0.25 * (f * 0.31 + row * 5.0).cos();
        (depth * (camera_x + rolling_x), depth * (camera_y + rolling_y))
    }

    fn near_region(self, x: f64, y: f64) -> bool {
        let u = x / self.width.max(1) as f64;
        let v = y / self.height.max(1) as f64;
        (0.16..0.62).contains(&u) && (0.22..0.82).contains(&v) && ((u - 0.39) * 2.4).sin() > -0.85
    }

    fn texture(self, x: f64, y: f64, near: bool) -> [f32; 3] {
        let phase = if near { 0.73 } else { 0.21 };
        let r = 0.42 + 0.25 * (x * 0.17 + y * 0.031 + phase).sin() + 0.16 * ((x + y) * 0.071).cos();
        let g = 0.36 + 0.27 * (x * 0.047 - y * 0.19 + phase * 2.0).cos() + 0.14 * ((x - y) * 0.113).sin();
        let b = 0.48 + 0.22 * ((x + y) * 0.083 + phase).sin() + 0.14 * (x * 0.11 - y * 0.057).cos();
        [r.clamp(0.03, 0.98) as f32, g.clamp(0.03, 0.98) as f32, b.clamp(0.03, 0.98) as f32]
    }
    fn far_marker(self) -> (f64, f64) {
        (110.0, 12.0)
    }

    fn near_markers(self) -> [(f64, f64); 3] {
        [(30.0, 24.0), (62.0, 24.0), (46.0, 60.0)]
    }

    fn marker_at(self, x: f64, y: f64, near: bool) -> bool {
        if near {
            self.near_markers().into_iter().any(|(mx, my)| (x - mx).hypot(y - my) <= 2.25)
        } else {
            let (mx, my) = self.far_marker();
            (x - mx).hypot(y - my) <= 2.25
        }
    }

    fn render_at(self, seconds: f64, scale: f32) -> Image {
        let scale = if scale.is_finite() { scale.clamp(0.125, 1.0) } else { 1.0 };
        let w = ((self.width as f32 * scale).round() as usize).max(1);
        let h = ((self.height as f32 * scale).round() as usize).max(1);
        let inv_scale = 1.0 / scale as f64;
        let frame = self.frame_at(seconds);
        let mut image = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let sx = (x as f64 + 0.5) * inv_scale;
                let sy = (y as f64 + 0.5) * inv_scale;
                let (fx, fy) = self.displacement(frame, sy, 0.45);
                let mut far = self.texture(sx + fx, sy + fy, false);
                let mut far_a = (0.72 + 0.14 * (sx * 0.08 + sy * 0.11).sin()).clamp(0.35, 0.92);
                if self.marker_at(sx + fx, sy + fy, false) {
                    far = [0.04, 0.86, 0.96];
                    far_a = 0.96;
                }
                let (nx, ny) = self.displacement(frame, sy, 1.0);
                let wx = sx + nx;
                let wy = sy + ny;
                let near = self.near_region(wx, wy);
                let marker = self.marker_at(wx, wy, true);
                let mut near_a = if near { (0.45 + 0.22 * (wx * 0.09 - wy * 0.07).cos()).clamp(0.24, 0.78) } else { 0.0 };
                let mut near_rgb = self.texture(wx, wy, true);
                if marker {
                    near_rgb = [0.96, 0.04, 0.84];
                    near_a = 0.96;
                }
                let one_minus_near = 1.0 - near_a;
                let out_a = near_a + far_a * one_minus_near;
                let i = (y * w + x) * 4;
                image.px[i..i + 4].copy_from_slice(&[
                    (near_rgb[0] as f64 * near_a + far[0] as f64 * far_a * one_minus_near) as f32,
                    (near_rgb[1] as f64 * near_a + far[1] as f64 * far_a * one_minus_near) as f32,
                    (near_rgb[2] as f64 * near_a + far[2] as f64 * far_a * one_minus_near) as f32,
                    out_a as f32,
                ]);
            }
        }
        image
    }
}

#[derive(Clone, Copy)]
struct SubspaceClip {
    scene: SubspaceScene,
    now: f64,
    key: u64,
}

impl SubspaceClip {
    fn image(&self, frame: usize) -> Image {
        self.scene.render_at(frame as f64 / self.scene.fps, 1.0)
    }
}

impl FxEnv for SubspaceClip {
    fn frame(&self, dt: f64, _: &EffectInstance) -> Option<Image> {
        Some(self.scene.render_at(self.now + dt, 1.0))
    }

    fn source_frame(&self, dt: f64, scale: f32) -> Option<Image> {
        Some(self.scene.render_at(self.now + dt, scale))
    }

    fn track(&self, _: usize) -> Option<Image> {
        None
    }

    fn layer_to_output(&self) -> Affine {
        Affine::IDENTITY
    }

    fn clip_seconds(&self) -> f64 {
        self.scene.clip_seconds()
    }

    fn clip_offset(&self) -> f64 {
        self.now
    }

    fn frame_rate(&self) -> f64 {
        self.scene.fps
    }

    fn media_timecode(&self) -> String {
        String::new()
    }

    fn file_name(&self) -> String {
        "synthetic-subspace".to_string()
    }

    fn sequence_name(&self) -> String {
        "synthetic-subspace".to_string()
    }

    fn sequence_size(&self) -> (u32, u32) {
        (self.scene.width as u32, self.scene.height as u32)
    }

    fn source_size(&self) -> (u32, u32) {
        (self.scene.width as u32, self.scene.height as u32)
    }

    fn clip_key(&self) -> u64 {
        self.key
    }
}

fn set_choice(effect: &mut EffectInstance, id: &str, value: u32) -> Result<(), String> {
    let Some(param) = effect.params.get_mut(id) else {
        return Err(format!("effect {} has no choice parameter {id}", effect.effect));
    };
    param.value = ParamValue::Choice(value);
    Ok(())
}

fn set_bool(effect: &mut EffectInstance, id: &str, value: bool) -> Result<(), String> {
    let Some(param) = effect.params.get_mut(id) else {
        return Err(format!("effect {} has no boolean parameter {id}", effect.effect));
    };
    param.value = ParamValue::Bool(value);
    Ok(())
}

fn warp_effect(method: u32, framing: u32, detailed: bool, fast: bool) -> Result<EffectInstance, String> {
    let Some(def) = find_effect("warp_stabilizer") else {
        return Err("warp_stabilizer effect is not registered".to_string());
    };
    let mut effect = def.instance();
    set_choice(&mut effect, "method", method)?;
    set_choice(&mut effect, "result", 1)?;
    set_choice(&mut effect, "framing", framing)?;
    set_bool(&mut effect, "detailed", detailed)?;
    set_bool(&mut effect, "fast_analysis", fast)?;
    Ok(effect)
}

fn apply_warp(clip: &SubspaceClip, effect: &EffectInstance, input: &Image) -> Image {
    let mut output = input.clone();
    let seconds = clip.now;
    let context = FxCtx {
        t: Tick::from_seconds_f64(seconds),
        px_scale: 1.0,
        seconds,
        timecode: "",
        clip_name: "synthetic-subspace",
        project: None,
        env: Some(clip),
        frame_rate: clip.scene.fps,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    effects::apply(&mut output, effect, &context);
    output
}

fn timed_warp(clip: &SubspaceClip, effect: &EffectInstance, frame: usize) -> (f64, Image) {
    let clip = SubspaceClip { now: frame as f64 / clip.scene.fps, ..*clip };
    let input = clip.image(frame);
    let start = Instant::now();
    let output = apply_warp(&clip, effect, &input);
    (start.elapsed().as_secs_f64() * 1000.0, output)
}

fn patch_error(a: &Image, b: &Image, x: usize, y: usize, radius: usize) -> f64 {
    if (a.w, a.h) != (b.w, b.h) {
        return f64::INFINITY;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    let x0 = x.saturating_sub(radius);
    let y0 = y.saturating_sub(radius);
    let x1 = (x.saturating_add(radius + 1)).min(a.w);
    let y1 = (y.saturating_add(radius + 1)).min(a.h);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let pa = a.get(xx, yy);
            let pb = b.get(xx, yy);
            for channel in 0..4 {
                sum += (pa[channel] - pb[channel]).abs() as f64;
            }
            count += 4;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn local_jitter(frames: &[Image], points: &[(usize, usize)]) -> f64 {
    let Some(first) = frames.first() else { return 0.0 };
    let mut sum = 0.0;
    let mut count = 0usize;
    for frame in frames.iter().skip(1) {
        for &(x, y) in points {
            sum += patch_error(first, frame, x, y, 3);
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn alpha_span(image: &Image) -> f32 {
    let mut min = 1.0f32;
    let mut max = 0.0f32;
    for pixel in image.px.chunks_exact(4) {
        min = min.min(pixel[3]);
        max = max.max(pixel[3]);
    }
    max - min
}

fn finite_image(image: &Image) -> bool {
    image.px.iter().all(|v| v.is_finite())
}

fn premultiplied_alpha(image: &Image) -> bool {
    image.px.chunks_exact(4).all(|pixel| {
        let alpha = pixel[3];
        alpha.is_finite()
            && (0.0..=1.0).contains(&alpha)
            && pixel[..3].iter().all(|channel| channel.is_finite() && *channel >= -1e-5 && *channel <= alpha + 1e-3)
            && (alpha > 1e-5 || pixel[..3].iter().all(|channel| channel.abs() <= 1e-3))
    })
}

fn alpha_max(image: &Image) -> f32 {
    image.px.chunks_exact(4).map(|pixel| pixel[3]).fold(0.0, f32::max)
}

fn visible_alpha_count(image: &Image) -> usize {
    image.px.chunks_exact(4).filter(|pixel| pixel[3] > 0.05).count()
}
fn marker_score(pixel: &[f32], near: bool) -> f64 {
    let alpha = pixel[3] as f64;
    if alpha <= 0.05 {
        return 0.0;
    }
    let r = pixel[0] as f64 / alpha;
    let g = pixel[1] as f64 / alpha;
    let b = pixel[2] as f64 / alpha;
    let target = if near { [0.96, 0.04, 0.84] } else { [0.04, 0.86, 0.96] };
    let distance_sq = (r - target[0]).powi(2) + (g - target[1]).powi(2) + (b - target[2]).powi(2);
    // Texture channels are bounded below these marker extrema (r <= .83, b <= .84).
    // A .10 RGB radius excludes texture while retaining composited marker interiors.
    const MATCH_RADIUS: f64 = 0.10;
    (1.0 - distance_sq / (MATCH_RADIUS * MATCH_RADIUS)).max(0.0)
}

fn marker_centroid(image: &Image, target: (f64, f64), near: bool) -> Option<(f64, f64)> {
    let radius = 16isize;
    let cx = target.0.round() as isize;
    let cy = target.1.round() as isize;
    let x0 = (cx - radius).max(0);
    let y0 = (cy - radius).max(0);
    let x1 = (cx + radius + 1).min(image.w as isize);
    let y1 = (cy + radius + 1).min(image.h as isize);
    let mut weight = 0.0;
    let mut x_sum = 0.0;
    let mut y_sum = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y as usize * image.w + x as usize) * 4;
            let score = marker_score(&image.px[i..i + 4], near);
            weight += score;
            x_sum += score * (x as f64 + 0.5);
            y_sum += score * (y as f64 + 0.5);
        }
    }
    (weight > 0.5).then_some((x_sum / weight, y_sum / weight))
}

fn marker_positions(image: &Image, scene: SubspaceScene) -> Option<[(f64, f64); 4]> {
    let mut positions = [(0.0, 0.0); 4];
    positions[0] = marker_centroid(image, scene.far_marker(), false)?;
    for (index, target) in scene.near_markers().into_iter().enumerate() {
        positions[index + 1] = marker_centroid(image, target, true)?;
    }
    Some(positions)
}

fn marker_reference_error(image: &Image, scene: SubspaceScene) -> Option<f64> {
    let positions = marker_positions(image, scene)?;
    let mut targets = [scene.far_marker(); 4];
    for (index, target) in scene.near_markers().into_iter().enumerate() {
        targets[index + 1] = target;
    }
    let mut max_error: f64 = 0.0;
    for (observed, target) in positions.into_iter().zip(targets) {
        max_error = max_error.max((observed.0 - target.0).hypot(observed.1 - target.1));
    }
    Some(max_error)
}

/// Measure relative near-marker motion against the visible frame-zero reference.  Anchoring near
/// points to that measured reference makes this a consumer-visible parallax residual rather than a
/// comparison against analytic spacing or correction matrices.
fn marker_error(frames: &[Image], scene: SubspaceScene) -> f64 {
    let Some(reference) = frames.first().and_then(|image| marker_positions(image, scene)) else {
        return f64::INFINITY;
    };
    let mut sum = 0.0;
    let mut count = 0usize;
    for image in frames.iter().skip(1) {
        let Some(observed) = marker_positions(image, scene) else { return f64::INFINITY };
        for index in 1..reference.len() {
            let expected = (reference[index].0 - reference[0].0, reference[index].1 - reference[0].1);
            let relative = (observed[index].0 - observed[0].0, observed[index].1 - observed[0].1);
            sum += (relative.0 - expected.0).hypot(relative.1 - expected.1);
            count += 1;
        }
    }
    if count == frames.len().saturating_sub(1) * (reference.len() - 1) { sum / count.max(1) as f64 } else { f64::INFINITY }
}

fn marker_strength(frames: &[Image], scene: SubspaceScene) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for image in frames {
        let far = marker_centroid(image, scene.far_marker(), false);
        if far.is_some() {
            let mut best: f64 = 0.0;
            let (fx, fy) = scene.far_marker();
            let radius = 16isize;
            let cx = fx.round() as isize;
            let cy = fy.round() as isize;
            let x0 = (cx - radius).max(0);
            let y0 = (cy - radius).max(0);
            let x1 = (cx + radius + 1).min(image.w as isize);
            let y1 = (cy + radius + 1).min(image.h as isize);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y as usize * image.w + x as usize) * 4;
                    best = best.max(marker_score(&image.px[i..i + 4], false));
                }
            }
            sum += best;
            count += 1;
        }
        for target in scene.near_markers() {
            if marker_centroid(image, target, true).is_some() {
                let radius = 16isize;
                let cx = target.0.round() as isize;
                let cy = target.1.round() as isize;
                let x0 = (cx - radius).max(0);
                let y0 = (cy - radius).max(0);
                let x1 = (cx + radius + 1).min(image.w as isize);
                let y1 = (cy + radius + 1).min(image.h as isize);
                let mut best: f64 = 0.0;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let i = (y as usize * image.w + x as usize) * 4;
                        best = best.max(marker_score(&image.px[i..i + 4], true));
                    }
                }
                sum += best;
                count += 1;
            }
        }
    }
    if count == frames.len() * (scene.near_markers().len() + 1) { sum / count.max(1) as f64 } else { f64::INFINITY }
}

fn write_ppm(path: &Path, image: &Image) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::create(path)?;
    write!(file, "P6\n{} {}\n255\n", image.w, image.h)?;
    for pixel in image.px.chunks_exact(4) {
        let alpha = pixel[3].clamp(0.0, 1.0);
        let rgb =
            if alpha > 1e-5 { [(pixel[0] / alpha).clamp(0.0, 1.0), (pixel[1] / alpha).clamp(0.0, 1.0), (pixel[2] / alpha).clamp(0.0, 1.0)] } else { [0.0; 3] };
        let bytes = [
            (filmcraft_color::linear_to_srgb(rgb[0]) * 255.0).round().clamp(0.0, 255.0) as u8,
            (filmcraft_color::linear_to_srgb(rgb[1]) * 255.0).round().clamp(0.0, 255.0) as u8,
            (filmcraft_color::linear_to_srgb(rgb[2]) * 255.0).round().clamp(0.0, 255.0) as u8,
        ];
        file.write_all(&bytes)?;
    }
    Ok(())
}

fn image_mean_abs(a: &Image, b: &Image) -> f64 {
    if (a.w, a.h) != (b.w, b.h) || a.px.len() != b.px.len() {
        return f64::INFINITY;
    }
    let mut sum = 0.0;
    for (lhs, rhs) in a.px.iter().zip(&b.px) {
        sum += f64::from((*lhs - *rhs).abs());
    }
    sum / a.px.len().max(1) as f64
}

fn percentile(mut values: Vec<f64>, fraction: f64) -> f64 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return f64::INFINITY;
    }
    values.sort_by(f64::total_cmp);
    let position = ((values.len() - 1) as f64 * fraction.clamp(0.0, 1.0)).round() as usize;
    values[position.min(values.len() - 1)]
}

fn affine_parameters(m: Affine) -> [f64; 4] {
    let scale = m.a.hypot(m.b).max(1.0e-12);
    [m.e, m.f, m.b.atan2(m.a), scale.ln()]
}

fn wrap_angle_delta(value: f64) -> f64 {
    const PI: f64 = std::f64::consts::PI;
    const TAU: f64 = 2.0 * PI;
    (value + PI).rem_euclid(TAU) - PI
}

/// Measure consecutive-frame steps of a camera path directly. Reported pixel deltas are in the
/// path's own resolution: analysis frames are downscaled by `scale`, so multiplying by the
/// `1/scale` factor converts them to source pixels for comparison with the rendered diagnostics.
fn path_step_continuity(steps: &[Affine], scale: f64, label: &str) {
    let mut translations = Vec::new();
    let mut rotations = Vec::new();
    let mut scales = Vec::new();
    let mut worst_translation = (0usize, 0.0);
    let mut worst_rotation = (0usize, 0.0);
    let mut worst_scale = (0usize, 0.0);
    for (index, pair) in steps.windows(2).enumerate() {
        let previous = affine_parameters(pair[0]);
        let current = affine_parameters(pair[1]);
        let translation = (current[0] - previous[0]).hypot(current[1] - previous[1]) / scale;
        let rotation = wrap_angle_delta(current[2] - previous[2]).abs();
        let scale_delta = (current[3] - previous[3]).abs();
        translations.push(translation);
        rotations.push(rotation);
        scales.push(scale_delta);
        if translation > worst_translation.1 {
            worst_translation = (index + 1, translation);
        }
        if rotation > worst_rotation.1 {
            worst_rotation = (index + 1, rotation);
        }
        if scale_delta > worst_scale.1 {
            worst_scale = (index + 1, scale_delta);
        }
    }
    println!(
        "real {label}_continuity steps={} translation_px p95={:.5} max={:.5}@{} rotation_rad p95={:.6} max={:.6}@{} log_scale p95={:.6} max={:.6}@{}",
        steps.len(),
        percentile(translations, 0.95),
        worst_translation.1,
        worst_translation.0,
        percentile(rotations, 0.95),
        worst_rotation.1,
        worst_rotation.0,
        percentile(scales, 0.95),
        worst_scale.1,
        worst_scale.0,
    );
    for index in 1..steps.len() {
        if index <= 8 || (118..=138).contains(&index) {
            let previous = affine_parameters(steps[index - 1]);
            let current = affine_parameters(steps[index]);
            println!(
                "real {label}_step frame={index} translation_px={:.5} rotation_rad={:.6} log_scale={:.6}",
                (current[0] - previous[0]).hypot(current[1] - previous[1]) / scale,
                wrap_angle_delta(current[2] - previous[2]),
                current[3] - previous[3],
            );
        }
    }
}

fn path_continuity(path: &filmcraft_render::vfx::StabilizerPath) {
    path_step_continuity(&path.steps, 1.0, "transform");
}

fn real_effect_context(clip: &RealClip, image_width: usize) -> FxCtx<'_> {
    FxCtx {
        t: Tick::from_seconds_f64(clip.now),
        px_scale: image_width as f32 / clip.width.max(1) as f32,
        seconds: clip.now,
        timecode: "",
        clip_name: &clip.name,
        project: None,
        env: Some(clip),
        frame_rate: clip.fps,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

fn run_real_diagnostic(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    const METRIC_SCALE: f32 = 0.125;
    const SAMPLE_SCALE: f32 = 0.25;
    const MAX_DIAGNOSTIC_FRAMES: usize = 20_000;
    let base = RealClip::open(path)?;
    let count = (base.seconds * base.fps).ceil();
    if !count.is_finite() || count <= 0.0 || count > MAX_DIAGNOSTIC_FRAMES as f64 {
        return Err(format!("real diagnostic frame count is outside 1..={MAX_DIAGNOSTIC_FRAMES}: {count}").into());
    }
    let count = count as usize;
    // Retain full-clip tracking, factorization and crop context, but inspect the reported
    // startup problem over the first three seconds rather than rendering the whole clip.
    let measured_count = count.min((3.0 * base.fps).ceil() as usize);
    let video = base.source.info().video.as_ref().ok_or("real diagnostic source lost its video stream")?;
    println!(
        "real media path={} size={}x{} fps={}/{} ({:.9}) duration={:.6}s frames={} codec={} container={}",
        base.name,
        base.width,
        base.height,
        video.frame_rate.num,
        video.frame_rate.den,
        base.fps,
        base.seconds,
        count,
        video.codec,
        base.source.info().container,
    );

    // Warm the settings-independent camera path once. Method 3 computes the same adjacent
    // tracking steps while retaining trajectories, so this path is the transform continuity
    // oracle without exposing a second private plan representation from the library.
    clear_stabilizer_cache();
    let path_oracle = stabilizer_path(&base, 0.0, TrackMethod::PositionScaleRotation, false, true).ok_or("real diagnostic analysis failed")?;
    path_continuity(&path_oracle);

    let subspace_effect = find_effect("warp_stabilizer").ok_or("warp_stabilizer effect is not registered")?.instance();
    let mut global_effect = subspace_effect.clone();
    set_choice(&mut global_effect, "method", 1)?;
    let sample_seconds = [0.0, 1.0 / base.fps, 2.0 / base.fps, 0.5, 1.0, 119.0 / base.fps, 120.0 / base.fps, 121.0 / base.fps, 179.0 / base.fps];
    let sample_frames: Vec<usize> =
        sample_seconds.iter().map(|seconds| (seconds * base.fps).round().clamp(0.0, count.saturating_sub(1) as f64) as usize).collect();
    let root = Path::new("target/subspace-warp-real");
    let input_dir = root.join("input");
    let subspace_dir = root.join("subspace");
    let global_dir = root.join("global");
    fs::create_dir_all(&input_dir)?;
    fs::create_dir_all(&subspace_dir)?;
    fs::create_dir_all(&global_dir)?;

    let mut previous_input = None;
    let mut previous_subspace = None;
    let mut previous_global = None;
    let mut input_deltas = Vec::with_capacity(measured_count.saturating_sub(1));
    let mut subspace_deltas = Vec::with_capacity(measured_count.saturating_sub(1));
    let mut global_deltas = Vec::with_capacity(measured_count.saturating_sub(1));
    let mut mesh_vs_global = Vec::with_capacity(measured_count);
    let mut pts_errors = Vec::with_capacity(measured_count);
    let mut pts_deltas = Vec::with_capacity(measured_count.saturating_sub(1));
    let mut repeated_pairs = 0usize;
    let mut previous_pts = None;
    for frame_index in 0..measured_count {
        let seconds = frame_index as f64 / base.fps;
        let clip = base.at(seconds);
        let (input, pts) = clip.frame_at(seconds, METRIC_SCALE)?;
        if let Some(previous) = previous_input.as_ref() {
            let delta = image_mean_abs(previous, &input);
            if delta <= 1.0e-7 {
                repeated_pairs = repeated_pairs.saturating_add(1);
            }
            input_deltas.push(delta);
        }
        if let Some(previous) = previous_pts {
            pts_deltas.push(pts - previous);
        }
        pts_errors.push(pts - seconds);

        let context = real_effect_context(&clip, input.w);
        let mut subspace = input.clone();
        effects::apply(&mut subspace, &subspace_effect, &context);
        let mut global = input.clone();
        effects::apply(&mut global, &global_effect, &context);
        if let Some(previous) = previous_subspace.as_ref() {
            subspace_deltas.push(image_mean_abs(previous, &subspace));
        }
        if let Some(previous) = previous_global.as_ref() {
            global_deltas.push(image_mean_abs(previous, &global));
        }
        mesh_vs_global.push(image_mean_abs(&subspace, &global));

        if sample_frames.contains(&frame_index) {
            let sample_name = format!("frame-{frame_index:05}");
            let (sample_input, sample_pts) = clip.frame_at(seconds, SAMPLE_SCALE)?;
            let sample_context = real_effect_context(&clip, sample_input.w);
            let mut sample_subspace = sample_input.clone();
            effects::apply(&mut sample_subspace, &subspace_effect, &sample_context);
            let mut sample_global = sample_input.clone();
            effects::apply(&mut sample_global, &global_effect, &sample_context);
            write_ppm(&input_dir.join(format!("{sample_name}.ppm")), &sample_input)?;
            write_ppm(&subspace_dir.join(format!("{sample_name}.ppm")), &sample_subspace)?;
            write_ppm(&global_dir.join(format!("{sample_name}.ppm")), &sample_global)?;
            println!("real sample frame={} requested={seconds:.6}s reported_pts={sample_pts:.6}s", frame_index);
        }
        previous_input = Some(input);
        previous_subspace = Some(subspace);
        previous_global = Some(global);
        previous_pts = Some(pts);
    }
    let input_min_pts = pts_deltas.iter().copied().fold(f64::INFINITY, f64::min);
    let input_max_pts = pts_deltas.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let input_min_error = pts_errors.iter().copied().fold(f64::INFINITY, f64::min);
    let input_max_error = pts_errors.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let input_worst = input_deltas.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, 0.0));
    let subspace_worst = subspace_deltas.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, 0.0));
    let global_worst = global_deltas.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, 0.0));
    println!(
        "real input_continuity decoded_frames={} repeated_pairs={} reported_pts_delta_ms min={:.3} max={:.3} requested_error_ms min={:.3} max={:.3} mean_abs max={:.6}@{} p95={:.6}",
        measured_count,
        repeated_pairs,
        input_min_pts * 1000.0,
        input_max_pts * 1000.0,
        input_min_error * 1000.0,
        input_max_error * 1000.0,
        input_worst.1,
        input_worst.0.saturating_add(1),
        percentile(input_deltas, 0.95),
    );
    if measured_count > 1 && pts_deltas.iter().all(|delta| delta.abs() <= f64::EPSILON) {
        println!("real timing unavailable: reported PTS is constant across changing requested frames; use an external timing oracle");
    }
    println!(
        "real mesh_continuity proxy=rendered_method3_or_global frame_delta mean_abs max={:.6}@{} p95={:.6} global_similarity max={:.6}@{} p95={:.6} mesh_vs_global mean_abs max={:.6} p95={:.6}",
        subspace_worst.1,
        subspace_worst.0.saturating_add(1),
        percentile(subspace_deltas, 0.95),
        global_worst.1,
        global_worst.0.saturating_add(1),
        percentile(global_deltas, 0.95),
        mesh_vs_global.iter().copied().fold(0.0, f64::max),
        percentile(mesh_vs_global, 0.95),
    );
    println!("real diagnostic samples written under {}", root.display());
    Ok(())
}

fn run_subspace_modes(write_frames: bool, benchmark: bool) -> Result<(), Box<dyn std::error::Error>> {
    let scene = SubspaceScene::new();
    let mode_list = if benchmark { vec![(false, false), (false, true), (true, false), (true, true)] } else { vec![(false, false)] };
    let sample_frames = [0usize, 8, 16, 24, 32, 40, 48, 56];
    let near_points = [(30, 24), (62, 24), (46, 60)];
    let far_points = [(110, 12), (96, 14), (106, 68)];
    let all_points = [near_points.as_slice(), far_points.as_slice()].concat();

    for (mode_index, &(detailed, fast)) in mode_list.iter().enumerate() {
        let base = SubspaceClip { scene, now: 0.0, key: 0x5355_4253_5041_4345 ^ mode_index as u64 };
        let global_effect = warp_effect(1, 0, detailed, fast)?;
        let subspace_effect = warp_effect(3, 0, detailed, fast)?;
        clear_stabilizer_cache();
        let (global_cold_ms, global_cold_frame) = timed_warp(&base, &global_effect, 32);
        black_box(&global_cold_frame);
        let (global_warm_ms, global_warm_frame) = timed_warp(&base, &global_effect, 32);
        black_box(&global_warm_frame);
        clear_stabilizer_cache();
        let (subspace_cold_ms, subspace_cold_frame) = timed_warp(&base, &subspace_effect, 32);
        black_box(&subspace_cold_frame);
        let (subspace_warm_ms, subspace_warm_frame) = timed_warp(&base, &subspace_effect, 32);
        black_box(&subspace_warm_frame);

        let raw_frames: Vec<_> = sample_frames.iter().map(|&frame| base.image(frame)).collect();
        let reference_frame = raw_frames.first().ok_or("synthetic sample has no reference frame")?;
        let reference_error = marker_reference_error(reference_frame, scene).ok_or("synthetic marker reference frame is not measurable")?;
        if reference_error > 0.75 {
            return Err(format!("synthetic marker reference is {reference_error:.3}px from fixture truth").into());
        }
        let global_frames: Vec<_> = sample_frames
            .iter()
            .map(|&frame| {
                let clip = SubspaceClip { now: frame as f64 / scene.fps, ..base };
                apply_warp(&clip, &global_effect, &clip.image(frame))
            })
            .collect();
        let subspace_frames: Vec<_> = sample_frames
            .iter()
            .map(|&frame| {
                let clip = SubspaceClip { now: frame as f64 / scene.fps, ..base };
                apply_warp(&clip, &subspace_effect, &clip.image(frame))
            })
            .collect();
        let raw_jitter = local_jitter(&raw_frames, &all_points);
        let global_jitter = local_jitter(&global_frames, &all_points);
        let subspace_jitter = local_jitter(&subspace_frames, &all_points);
        let raw_near = local_jitter(&raw_frames, &near_points);
        let global_near = local_jitter(&global_frames, &near_points);
        let subspace_near = local_jitter(&subspace_frames, &near_points);
        let raw_markers = marker_error(&raw_frames, scene);
        let global_markers = marker_error(&global_frames, scene);
        let subspace_markers = marker_error(&subspace_frames, scene);
        let raw_marker_strength = marker_strength(&raw_frames, scene);
        let global_marker_strength = marker_strength(&global_frames, scene);
        let subspace_marker_strength = marker_strength(&subspace_frames, scene);
        let reduction = |before: f64, after: f64| if before > 1e-9 { 1.0 - after / before } else { 0.0 };
        println!(
            "subspace detailed={detailed} fast={fast} frames={} size={}x{} cold_ms global={global_cold_ms:.3} subspace={subspace_cold_ms:.3} warm_ms global={global_warm_ms:.3} subspace={subspace_warm_ms:.3}",
            scene.frames, scene.width, scene.height
        );
        println!(
            "local_jitter rgba mean raw={raw_jitter:.6} global_similarity={global_jitter:.6} subspace_warp={subspace_jitter:.6} near raw={raw_near:.6} global={global_near:.6} subspace={subspace_near:.6} global_reduction={:.1}% subspace_reduction={:.1}% marker_relative_jitter raw={raw_markers:.4}px global_similarity={global_markers:.4}px subspace_warp={subspace_markers:.4}px marker_strength raw={raw_marker_strength:.4} global_similarity={global_marker_strength:.4} subspace_warp={subspace_marker_strength:.4}",
            reduction(raw_near, global_near) * 100.0,
            reduction(raw_near, subspace_near) * 100.0,
        );
        if !raw_marker_strength.is_finite()
            || !global_marker_strength.is_finite()
            || !subspace_marker_strength.is_finite()
            || subspace_marker_strength < raw_marker_strength * 0.70
        {
            return Err(format!(
                "Subspace Warp lost marker contrast: raw={raw_marker_strength:.4} global={global_marker_strength:.4} subspace={subspace_marker_strength:.4}"
            )
            .into());
        }
        if !raw_markers.is_finite() || !global_markers.is_finite() || !subspace_markers.is_finite() || subspace_markers >= global_markers * 0.95 {
            return Err(format!(
                "Subspace Warp did not beat global similarity on relative markers: raw={raw_markers:.4} global={global_markers:.4} subspace={subspace_markers:.4}"
            )
            .into());
        }
        let finite = raw_frames.iter().chain(&global_frames).chain(&subspace_frames).all(|image| finite_image(image) && premultiplied_alpha(image));
        if !finite {
            return Err("synthetic Subspace Warp produced a non-finite or invalid premultiplied frame".into());
        }
        if mode_index == 0 {
            for framing in 0..=3 {
                let effect = warp_effect(3, framing, detailed, fast)?;
                let clip = SubspaceClip { now: 32.0 / scene.fps, ..base };
                let input = clip.image(32);
                let (_, output) = timed_warp(&base, &effect, 32);
                let valid = finite_image(&output) && premultiplied_alpha(&output);
                println!("subspace border_choice={framing} finite={valid} alpha_span={:.6}", alpha_span(&output));
                if !valid || alpha_max(&output) < alpha_max(&input) * 0.5 || visible_alpha_count(&output) < visible_alpha_count(&input) / 8 {
                    return Err(format!("Subspace Warp border choice {framing} lost source alpha or finite output").into());
                }
            }
        }
        if write_frames && mode_index == 0 {
            let root = Path::new("target/subspace-warp-smoke");
            let input_dir = root.join("input");
            let output_dir = root.join("output");
            fs::create_dir_all(&input_dir)?;
            fs::create_dir_all(&output_dir)?;
            let output_effect = warp_effect(3, 0, detailed, fast)?;
            for frame in 0..scene.frames {
                let clip = SubspaceClip { now: frame as f64 / scene.fps, ..base };
                let input = clip.image(frame);
                let output = apply_warp(&clip, &output_effect, &input);
                write_ppm(&input_dir.join(format!("frame-{frame:04}.ppm")), &input)?;
                write_ppm(&output_dir.join(format!("frame-{frame:04}.ppm")), &output)?;
            }
            println!("wrote {} input/output frames under {}", scene.frames, root.display());
        }
    }
    Ok(())
}

fn median_ms(mut f: impl FnMut()) -> f64 {
    f();
    let mut times = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        f();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    times.get(2).copied().unwrap_or(0.0)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--real-diagnostic") {
        let path = args
            .iter()
            .position(|arg| arg == "--real-diagnostic")
            .and_then(|p| args.get(p + 1))
            .map(PathBuf::from)
            .ok_or("--real-diagnostic requires a media path")?;
        return run_real_diagnostic(&path);
    }
    let subspace_only = args.iter().any(|arg| arg == "--subspace-only");
    let subspace_benchmark = args.iter().any(|arg| arg == "--subspace-bench");
    let write_frames = args.iter().any(|arg| arg == "--write-frames");
    let base = Gray::new(
        960,
        540,
        (0..960 * 540)
            .map(|i| {
                let (x, y) = ((i % 960) as f32, (i / 960) as f32);
                (0.5 + 0.15 * (x * 0.19).sin() + 0.15 * (y * 0.23).cos() + 0.15 * ((x + y) * 0.11).sin()).clamp(0.0, 1.0)
            })
            .collect(),
    );
    let clip = Clip { base, reads: AtomicUsize::new(0) };
    if !subspace_only {
        println!("Rayon threads: {}", rayon::current_num_threads());
        for detailed in [false, true] {
            let scale = if detailed { 1.0 } else { 0.5 };
            let img = clip.image(0, scale);
            let a = Prepared::new(gray(&img));
            let b = Prepared::new(gray(&clip.image(1, scale)));
            let (w, h) = (img.w as f64, img.h as f64);
            let region = [Vec2::new(8.0, 8.0), Vec2::new(w - 8.0, 8.0), Vec2::new(w - 8.0, h - 8.0), Vec2::new(8.0, h - 8.0)];
            let detect = median_ms(|| {
                black_box(features(a.base(), &region, MAX_FEATURES, 5.0));
            });
            let prepare = median_ms(|| {
                black_box(Prepared::new(gray(&img)));
            });
            let track = median_ms(|| {
                black_box(track_step(&a, &b, &region, TrackMethod::PositionScaleRotation));
            });
            let src: Vec<_> = features(a.base(), &region, MAX_FEATURES, 5.0).iter().map(|p| Vec2::new(p[0] as f64, p[1] as f64)).collect();
            let dst: Vec<_> = src.iter().map(|p| *p + Vec2::new(1.0, 2.0)).collect();
            let fit = median_ms(|| {
                black_box(fit_robust(TrackMethod::PositionScaleRotation, &src, &dst));
            });
            println!("{}x{} prepare={prepare:.3}ms detect={detect:.3}ms track+detect+fit={track:.3}ms fit={fit:.3}ms", img.w, img.h);
            let mut reference = None;
            for fast in [false, true] {
                clear_stabilizer_cache();
                clip.reads.store(0, Ordering::Relaxed);
                let start = Instant::now();
                let path = stabilizer_path(&clip, 0.0, TrackMethod::PositionScaleRotation, detailed, fast).ok_or("analysis failed")?;
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                let warm_start = Instant::now();
                let warm_path = stabilizer_path(&clip, 0.0, TrackMethod::PositionScaleRotation, detailed, fast).ok_or("warm analysis failed")?;
                let warm_ms = warm_start.elapsed().as_secs_f64() * 1000.0;
                if warm_path.steps != path.steps {
                    return Err("Warm analysis changed camera motion".into());
                }
                let mut worst = 0.0f64;
                for (k, step) in path.steps.iter().enumerate() {
                    let a = shake(k);
                    let b = shake(k + 1);
                    let expected = Vec2::new((a.0 - b.0) as f64 * scale as f64, (a.1 - b.1) as f64 * scale as f64);
                    let p = Vec2::new(w / 2.0, h / 2.0);
                    worst = worst.max((step.apply(p) - p - expected).length());
                }
                let checksum = path
                    .steps
                    .iter()
                    .flat_map(|s| [s.a, s.b, s.c, s.d, s.e, s.f])
                    .fold(0xcbf29ce484222325u64, |h, v| (h ^ v.to_bits()).wrapping_mul(0x100000001b3));
                println!(
                    "analysis detailed={detailed} fast={fast} frames=48 elapsed={ms:.3}ms cold_elapsed={ms:.3}ms warm_elapsed={warm_ms:.3}ms reads={} worst_center_error={worst:.6}px checksum={checksum:016x}",
                    clip.reads.load(Ordering::Relaxed)
                );
                if worst > 0.1 {
                    return Err(format!("tracking error {worst} exceeds 0.1 analysis pixels").into());
                }
                if let Some(steps) = &reference {
                    if steps != &path.steps {
                        return Err("Fast Analysis changed camera motion".into());
                    }
                } else {
                    reference = Some(path.steps.clone());
                }
            }
        }
    }
    run_subspace_modes(write_frames, subspace_benchmark)?;
    Ok(())
}

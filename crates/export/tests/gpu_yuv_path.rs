//! The GPU YUV export path through the real export pipeline, with a "perfect" frame renderer (the
//! CPU reference renderer behind both `render` and `render_yuv420`): the YUV path must give the
//! very same file as GPU rendering Off, take every eligible frame, never take an ineligible
//! export, and cope with a renderer that declines some frames. Its own test binary: the renderer
//! registry is process-wide.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_export::{ExportSettings, Format, FrameRenderer, GpuRendering, Progress, export, gpu_render_stats, register_frame_renderer};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::{RenderOptions, SourceMap, SourceProvider};
use filmcraft_time::{FrameRate, Tick, TimeRange};

/// 0: YUV for every frame, 1: never, 2: every other frame.
static MODE: AtomicU8 = AtomicU8::new(0);
/// Times the pipeline asked the renderer for a 4:2:0 frame: what eligibility decided, whatever
/// the renderer answered.
static YUV_CALLS: AtomicU64 = AtomicU64::new(0);
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The pictures the fake renderer hands out, keyed by (tick, render scale bits): the CPU reference
/// render and its 4:2:0 conversion, computed before each export ([`prepare`]) so the renderer does
/// no work inside the export's renderer pool. (A renderer that runs rayon work inside the pool can
/// deadlock it on main, the issue #861 addresses; that is not what these tests are about.)
type Cache = HashMap<(i64, u32), (filmcraft_render::Image, filmcraft_frame::VideoFrame)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn yuv_of(img: &filmcraft_render::Image) -> filmcraft_frame::VideoFrame {
    let rgba = img.over_black_rgba8();
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    filmcraft_export::rgba_to_yuv420_8(&rgba, img.w, img.h, &mut y, &mut u, &mut v);
    filmcraft_frame::VideoFrame {
        width: img.w as u32,
        height: img.h as u32,
        data: filmcraft_frame::PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: filmcraft_frame::Chroma::C420, alpha: None },
        color: Default::default(),
        par: (1, 1),
        pts: Tick::ZERO,
    }
}

/// Fill the cache with every frame of the sequence at full and half scale (the render scales the
/// exports below ask for).
fn prepare(p: &Project, seq: ItemId, m: &SourceMap) {
    let mut c = Cache::new();
    for scale in [1.0f32, 0.5] {
        let opts = RenderOptions { scale, ..RenderOptions::default() };
        for f in 0..24 {
            let t = FrameRate::FPS_24.tick_of(f);
            let img = filmcraft_render::render_sequence(p, seq, t, opts, m);
            let yuv = yuv_of(&img);
            c.insert((t.0, scale.to_bits()), (img, yuv));
        }
    }
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some(c);
}

fn cached(t: Tick, opts: RenderOptions) -> Option<(filmcraft_render::Image, filmcraft_frame::VideoFrame)> {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner).as_ref()?.get(&(t.0, opts.scale.to_bits())).cloned()
}

/// A renderer that is exactly the CPU reference (from the cache; a miss declines, so the CPU
/// renders that frame), for both `render` and `render_yuv420`.
struct Perfect(u64);

impl FrameRenderer for Perfect {
    fn render(&mut self, _: &Project, _: ItemId, t: Tick, opts: RenderOptions, _: &dyn SourceProvider) -> Option<filmcraft_render::Image> {
        cached(t, opts).map(|(img, _)| img)
    }
    fn render_yuv420(&mut self, _: &Project, _: ItemId, t: Tick, opts: RenderOptions, _: &dyn SourceProvider) -> Option<filmcraft_frame::VideoFrame> {
        self.0 += 1;
        YUV_CALLS.fetch_add(1, Ordering::SeqCst);
        match MODE.load(Ordering::SeqCst) {
            1 => return None,
            2 if self.0.is_multiple_of(2) => return None,
            _ => {}
        }
        cached(t, opts).map(|(_, yuv)| yuv)
    }
}

fn project() -> (Arc<Project>, ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("x");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), 320, 180, rate, rate.tick_of(24));
    let info = g.info().clone();
    let clip = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-gpu-yuv-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

fn settings(name: &str, gpu: GpuRendering) -> ExportSettings {
    ExportSettings { format: Format::H264, path: tmp(name), include_audio: false, bitrate_kbps: 2000, gpu_rendering: gpu, ..Default::default() }
}

/// Export and report how many frames took the YUV path.
fn run(s: &ExportSettings) -> u64 {
    let (p, seq, m) = project();
    prepare(&p, seq, &m);
    let before = gpu_render_stats().yuv_frames;
    export(&p, seq, s, &m, &Progress::default()).unwrap();
    gpu_render_stats().yuv_frames - before
}

fn setup() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    register_frame_renderer(|| Some(Box::new(Perfect(0)) as Box<dyn FrameRenderer>));
    g
}

#[test]
fn eligible_export_takes_every_frame_and_gives_the_same_file() {
    let _g = setup();
    MODE.store(0, Ordering::SeqCst);
    let off = settings("off.mp4", GpuRendering::Off);
    assert_eq!(run(&off), 0);
    let auto = settings("auto.mp4", GpuRendering::Auto);
    assert_eq!(run(&auto), 24, "every frame through render_yuv420");
    assert_eq!(std::fs::read(&off.path).unwrap(), std::fs::read(&auto.path).unwrap(), "same bytes as the CPU path");
}

#[test]
fn alternating_renderer_gives_the_same_file() {
    let _g = setup();
    MODE.store(2, Ordering::SeqCst);
    let off = settings("alt-off.mp4", GpuRendering::Off);
    run(&off);
    let auto = settings("alt-auto.mp4", GpuRendering::Auto);
    let n = run(&auto);
    assert!(n > 0 && n < 24, "some frames YUV, some RGBA: {n}");
    assert_eq!(std::fs::read(&off.path).unwrap(), std::fs::read(&auto.path).unwrap());
    MODE.store(0, Ordering::SeqCst);
}

#[test]
fn ineligible_exports_never_take_the_yuv_path() {
    let _g = setup();
    MODE.store(0, Ordering::SeqCst);
    let mut cases: Vec<(&str, ExportSettings)> = Vec::new();
    let mut s = settings("limiter.mp4", GpuRendering::Auto);
    s.effects.video_limiter.enabled = true;
    cases.push(("video limiter", s));
    let mut s = settings("timecode.mp4", GpuRendering::Auto);
    s.effects.timecode_overlay.enabled = true;
    cases.push(("timecode overlay", s));
    let mut s = settings("name.mp4", GpuRendering::Auto);
    s.effects.name_overlay.enabled = true;
    s.effects.name_overlay.text = "x".into();
    cases.push(("name overlay", s));
    let mut s = settings("letterbox.mp4", GpuRendering::Auto);
    s.frame_size = Some((320, 240));
    cases.push(("letterbox frame size", s));
    let mut s = settings("pillarbox.mp4", GpuRendering::Auto);
    s.frame_size = Some((240, 180));
    cases.push(("scaled into another aspect (pillarbox)", s));
    let overlay = tmp("overlay.png");
    image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 200])).save(&overlay).unwrap();
    let mut s = settings("image-overlay.mp4", GpuRendering::Auto);
    s.effects.image_overlay.enabled = true;
    s.effects.image_overlay.path = overlay;
    cases.push(("image overlay", s));
    cases.push(("ProRes (RGBA encoder)", ExportSettings { format: Format::ProRes, path: tmp("prores.mov"), ..settings("x", GpuRendering::Auto) }));
    cases.push(("GPU rendering Off", settings("off2.mp4", GpuRendering::Off)));
    for (what, s) in &cases {
        let calls = YUV_CALLS.load(Ordering::SeqCst);
        assert_eq!(run(s), 0, "{what}");
        assert_eq!(YUV_CALLS.load(Ordering::SeqCst) - calls, 0, "{what}: the pipeline must not even ask the renderer for 4:2:0");
    }
    MODE.store(1, Ordering::SeqCst);
    assert_eq!(run(&settings("declined.mp4", GpuRendering::Auto)), 0, "renderer returns None");
    MODE.store(0, Ordering::SeqCst);
}

/// Exports that change size without letterboxing (the render itself is scaled) and two-pass
/// exports are eligible: every frame is asked for as 4:2:0 and the file equals GPU Rendering Off.
#[test]
fn scaled_and_two_pass_exports_take_the_path_and_give_the_same_file() {
    let _g = setup();
    MODE.store(0, Ordering::SeqCst);
    let mut cases: Vec<(&str, ExportSettings, ExportSettings, u64)> = Vec::new();
    let (mut off, mut auto) = (settings("half-off.mp4", GpuRendering::Off), settings("half-auto.mp4", GpuRendering::Auto));
    off.frame_size = Some((160, 90));
    auto.frame_size = Some((160, 90));
    cases.push(("half size (render scale 0.5)", off, auto, 24));
    let (mut off, mut auto) = (settings("2pass-off.mp4", GpuRendering::Off), settings("2pass-auto.mp4", GpuRendering::Auto));
    off.bitrate_mode = filmcraft_export::BitrateMode::Vbr2Pass;
    auto.bitrate_mode = filmcraft_export::BitrateMode::Vbr2Pass;
    cases.push(("two-pass VBR (both passes)", off, auto, 48));
    for (what, off, auto, want) in &cases {
        run(off);
        let calls = YUV_CALLS.load(Ordering::SeqCst);
        assert_eq!(run(auto), *want, "{what}: frames through the 4:2:0 path");
        assert_eq!(YUV_CALLS.load(Ordering::SeqCst) - calls, *want, "{what}: asked for 4:2:0");
        assert_eq!(std::fs::read(&off.path).unwrap(), std::fs::read(&auto.path).unwrap(), "{what}: same bytes as the CPU path");
    }
}

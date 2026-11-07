//! End-to-end GPU export parity: the registered `filmcraft-gpu` frame renderer drives the full
//! export pipeline (`filmcraft_export::export` → PNG sequence) and its frames must match the
//! `GpuRendering::Off` (CPU reference) frames within the GPU-vs-CPU parity tolerance — p99 of the
//! per-pixel max RGB difference over black ≤ 6 and mean < 1.5 (the golden-test criterion).

use filmcraft_export::{ExportSettings, Format, FrameRenderer, GpuRenderStats, GpuRendering, Progress, export, register_frame_renderer};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{TICKS_PER_SECOND, TimeRange};
use std::sync::Arc;

/// The same wrapper the app and CLI register: `filmcraft-gpu`'s renderer behind
/// `filmcraft_export`'s trait.
struct GpuFrameRenderer(filmcraft_gpu::ExportRenderer);

/// Pictures the renderers decoded in compute shaders (ProRes, DNx), over every export.
static INTRA_DECODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        let before = self.0.intra_decodes().0;
        let img = self.0.render(project, seq, t, opts, sources);
        INTRA_DECODES.fetch_add(self.0.intra_decodes().0 - before, std::sync::atomic::Ordering::Relaxed);
        img
    }

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        self.0.caps()
    }
}

/// The colour matte on V1, and with `adjusted` an adjustment layer over it on V2 (Proc Amp and
/// Gaussian Blur, opaque, Normal, full frame: the kind the GPU runs as a `PlanStep::Adjust`).
fn project_with(width: u32, height: u32, adjusted: bool) -> (Arc<filmcraft_project::Project>, filmcraft_project::ItemId, SourceMap) {
    let mut p = filmcraft_project::Project::new("x");
    let g = GeneratorSource::new(
        Generator::ColorMatte { color: [0.2, 0.5, 0.9, 1.0] },
        320,
        180,
        filmcraft_time::FrameRate::FPS_24,
        filmcraft_time::Tick(2 * TICKS_PER_SECOND),
    );
    let info = g.info().clone();
    let red = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let tracks = if adjusted { 2 } else { 1 };
    let seq = p.new_sequence("s", SequenceSettings { width, height, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, tracks, 1, None);
    let v = p
        .make_track_item(
            red,
            TrackKind::Video,
            filmcraft_time::Tick::ZERO,
            TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(2)),
            filmcraft_time::FrameRate::FPS_24,
        )
        .unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    if adjusted {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let a = p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width, height, rate, duration: rate.tick_of(2) }, None);
        let mut ti =
            p.make_track_item(a, TrackKind::Video, filmcraft_time::Tick::ZERO, TimeRange::new(filmcraft_time::Tick::ZERO, rate.tick_of(2)), rate).unwrap();
        for (id, param, v) in [("proc_amp", "brightness", 25.0), ("gaussian_blur", "blurriness", 6.0)] {
            let mut e = filmcraft_project::find_effect(id).unwrap().instance();
            e.param_mut(param).unwrap().value = filmcraft_project::ParamValue::Float(v);
            ti.effects.insert(0, e);
        }
        p.sequence_mut(seq).unwrap().video_tracks[1].items.push(ti);
    }
    let mut m = SourceMap::default();
    m.0.insert(red, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn png(dir: &str, name: &str) -> Vec<u8> {
    // image_sequence_path flattens the path stem: <stem><index>.png in the temp dir itself.
    let index: u64 = name.trim_start_matches("img").trim_end_matches(".png").parse().unwrap_or(0);
    std::fs::read(std::env::temp_dir().join(format!("fc-gpu-export-{dir}{index:03}.png"))).unwrap()
}

fn export_png(dir: &str, gpu: GpuRendering, size: (u32, u32)) {
    export_project_png(dir, gpu, size, false);
}

fn export_project_png(dir: &str, gpu: GpuRendering, (w, h): (u32, u32), adjusted: bool) {
    let (p, seq, m) = project_with(w, h, adjusted);
    let mut s = ExportSettings {
        format: Format::PngSequence,
        path: std::env::temp_dir().join(format!("fc-gpu-export-{dir}")).to_string_lossy().into_owned(),
        ..Default::default()
    };
    s.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(2)));
    s.gpu_rendering = gpu;
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
}

fn assert_parity(cpu_png: &[u8], gpu_png: &[u8], what: &str) {
    let a = image::load_from_memory(cpu_png).unwrap().to_rgba8();
    let b = image::load_from_memory(gpu_png).unwrap().to_rgba8();
    assert_eq!(a.dimensions(), b.dimensions(), "{what}: size mismatch");
    let d: Vec<u32> = a
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_raw().as_chunks::<4>().0)
        .map(|(x, y)| (0..3).map(|k| (x[k] as u32).abs_diff(y[k] as u32)).max().unwrap_or(0))
        .collect();
    let mut d = d;
    d.sort_unstable();
    let p99 = d[d.len() * 99 / 100];
    let mean = d.iter().sum::<u32>() as f64 / d.len() as f64;
    eprintln!("{what}: GPU vs CPU p99 {p99}, mean {mean:.3}");
    assert!(p99 <= 6 && mean < 1.5, "{what}: GPU parity p99 {p99}, mean {mean:.3}");
}

/// The tests of this file share process-wide registries and counters: one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn gpu_export_matches_cpu_and_counts() {
    let _serial = serial();
    let Some(_) = filmcraft_gpu::ExportRenderer::new() else {
        eprintln!("SKIPPED (gpu export pipeline): no adapter");
        return;
    };
    // Clean registry first: Auto with no factory must be byte-equal to Off.
    export_png("off-cpu", GpuRendering::Off, (320, 180));
    export_png("off-auto", GpuRendering::Auto, (320, 180));
    assert_eq!(png("off-cpu", "img000.png"), png("off-auto", "img000.png"), "Auto with no registered renderer must be byte-equal to Off");

    register_frame_renderer(|| Some(Box::new(GpuFrameRenderer(filmcraft_gpu::ExportRenderer::new().expect("adapter present")))));
    let sizes = [(320u32, 180u32), (17, 13), (1, 1), (257, 129)];
    for (i, size) in sizes.into_iter().enumerate() {
        export_png("cpu", GpuRendering::Off, size);
        export_png("gpu", GpuRendering::Auto, size);
        for f in ["000", "001"] {
            assert_parity(&png("cpu", &format!("img{f}.png")), &png("gpu", &format!("img{f}.png")), &format!("{:?} frame {f}", size));
        }
        let _ = i;
    }
    let GpuRenderStats { frames, .. } = filmcraft_export::gpu_render_stats();
    assert!(frames >= 8, "gpu frames counted across the parity exports: {frames}");

    // An adjustment layer is composited by the GPU too (a `PlanStep::Adjust`), not handed to the
    // CPU renderer, and matches the CPU export.
    let composited = filmcraft_gpu::export_renderer::timing::get().frames;
    let GpuRenderStats { fallbacks, .. } = filmcraft_export::gpu_render_stats();
    export_project_png("adj-cpu", GpuRendering::Off, (320, 180), true);
    export_project_png("adj-gpu", GpuRendering::Auto, (320, 180), true);
    for f in ["000", "001"] {
        assert_parity(&png("adj-cpu", &format!("img{f}.png")), &png("adj-gpu", &format!("img{f}.png")), &format!("adjustment layer frame {f}"));
    }
    assert_ne!(png("adj-cpu", "img000.png"), png("off-cpu", "img000.png"), "the adjustment layer changes the picture");
    let gpu = filmcraft_gpu::export_renderer::timing::get().frames - composited;
    assert_eq!(gpu, 2, "both frames with the adjustment layer composited on the GPU");
    assert_eq!(filmcraft_export::gpu_render_stats().fallbacks, fallbacks, "no frame with the adjustment layer fell back to the CPU");

    // (the frame renderer registry is process-wide: the coded-media exports run here, after it)
    coded_media_exports_decode_on_the_gpu();
    gpu_rendered_ten_bit_ramp_keeps_its_luma_codes();
}

const RAMP_W: u32 = 1024;
const RAMP_H: u32 = 128;

/// A 10-bit 4:2:2 picture: luma 64..=940 from left to right, every row alike, neutral chroma.
struct Ramp(filmcraft_media::MediaInfo);

impl MediaSource for Ramp {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.0
    }
    fn video_frame(&self, _: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        let row = (0..RAMP_W).map(|x| (64 + 876 * x / (RAMP_W - 1)) as u16);
        let y: Vec<u16> = (0..RAMP_H).flat_map(|_| row.clone()).collect();
        let c = Arc::new(vec![512u16; (RAMP_W / 2 * RAMP_H) as usize]);
        let data = filmcraft_frame::PixelData::Yuv16 { planes: [Arc::new(y), c.clone(), c], chroma: filmcraft_frame::Chroma::C422, bits: 10, alpha: None };
        Ok(Arc::new(filmcraft_frame::VideoFrame {
            width: RAMP_W,
            height: RAMP_H,
            data,
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        }))
    }
    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Err(filmcraft_media::MediaError::NoStream("audio"))
    }
}

/// Distinct 10-bit luma codes on the middle row of the first frame of the file at `path`.
fn luma_codes(path: &str) -> usize {
    let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(path, bytes).unwrap();
    let f = src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
    let (w, row) = (f.width as usize, f.height as usize / 2);
    let filmcraft_frame::PixelData::Yuv16 { planes, bits, .. } = &f.data else { panic!("{path}: unexpected frame {:?}", f.data) };
    let mut codes: Vec<u32> = planes[0][row * w..(row + 1) * w].iter().map(|&v| u32::from(v) << 10 >> bits).collect();
    codes.sort_unstable();
    codes.dedup();
    codes.len()
}

/// A 10-bit ramp rendered by the GPU and exported to ProRes HQ keeps its luma codes: the GPU
/// renderer hands the encoder 16-bit floats, not an RGBA8 picture (about 256 of the 877 codes).
/// Needs the GPU frame renderer registered (the registry is process-wide).
fn gpu_rendered_ten_bit_ramp_keeps_its_luma_codes() {
    let r = filmcraft_time::FrameRate::FPS_24;
    let g = GeneratorSource::new(Generator::ColorMatte { color: [0.0; 4] }, RAMP_W, RAMP_H, r, filmcraft_time::Tick(TICKS_PER_SECOND));
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("ramp");
    let item = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let seq = p.new_sequence("s", SequenceSettings { width: RAMP_W, height: RAMP_H, frame_rate: r, ..Default::default() }, 1, 0, None);
    let ti = p.make_track_item(item, TrackKind::Video, filmcraft_time::Tick::ZERO, TimeRange::new(filmcraft_time::Tick::ZERO, r.tick_of(2)), r).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut m = SourceMap::default();
    m.0.insert(item, Arc::new(Ramp(info)));

    let dir = std::env::temp_dir().join(format!("fc-gpu-ramp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ramp-hq.mov").to_string_lossy().into_owned();
    let s = ExportSettings {
        format: Format::ProRes,
        prores_profile: "hq".into(),
        path: path.clone(),
        include_audio: false,
        gpu_rendering: GpuRendering::Auto,
        ..Default::default()
    };
    let frames = filmcraft_gpu::export_renderer::timing::get().frames;
    export(&Arc::new(p), seq, &s, &m, &Progress::default()).unwrap();
    assert!(filmcraft_gpu::export_renderer::timing::get().frames > frames, "the ramp was rendered on the GPU");
    let levels = luma_codes(&path);
    assert!(levels > 850, "GPU-rendered ramp in ProRes HQ: {levels} of 877 luma codes");
}

/// A MOV of `frames` (VC-3 or ProRes, one per frame at 24 fps).
fn mov(entry: filmcraft_isobmff::SampleEntry, frames: &[Vec<u8>]) -> Arc<[u8]> {
    let mut w =
        filmcraft_isobmff::Mp4Writer::new(std::io::Cursor::new(Vec::new()), filmcraft_isobmff::WriterOptions::new(filmcraft_isobmff::Brand::Mov)).unwrap();
    let t = w.add_track(filmcraft_isobmff::TrackConfig::new(entry, 24)).unwrap();
    for f in frames {
        w.write_sample(t, filmcraft_isobmff::WriteSample { data: f, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    }
    Arc::from(w.finish().unwrap().into_inner())
}

/// Luma with gradients, a checkerboard and noise, smooth chroma (`cw` wide), at `bits`. (CPU
/// and GPU rendering reconstruct subsampled chroma differently at hard chroma edges; that is not
/// what these exports compare.)
fn busy(w: usize, h: usize, cw: usize, bits: u32, seed: u32) -> [Vec<u16>; 3] {
    let s = bits - 8;
    let mut x = seed.wrapping_mul(2654435761) | 1;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x >> 28
    };
    let mut y = vec![0u16; w * h];
    for (i, v) in y.iter_mut().enumerate() {
        let (px, py) = (i % w, i / w);
        let chk = if (px / 8 + py / 8) % 2 == 0 { 40 } else { 0 };
        *v = (((20 + (px * 3 + py) % 150 + chk + rnd() as usize) as u32).min(235) << s) as u16;
    }
    let c =
        |k: i32| (0..cw * h).map(|i| (((128 + (i % cw) as i32 * k * 40 / cw as i32 - (i / cw) as i32 * 30 / h as i32) as u32) << s) as u16).collect::<Vec<_>>();
    [y, c(5), c(-4)]
}

/// The sequence `media` (a MOV) makes, opened afresh: every export decodes it itself.
fn clip_project(media: &Arc<[u8]>, w: u32, h: u32, frames: i64) -> (Arc<filmcraft_project::Project>, filmcraft_project::ItemId, SourceMap) {
    let src = filmcraft_codecs::open_bytes("clip.mov", media.clone()).unwrap();
    let info = src.info().clone();
    let mut p = filmcraft_project::Project::new("x");
    let item = p.add_item(
        "clip",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::File { path: "clip.mov".into() },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let rate = filmcraft_time::FrameRate::FPS_24;
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v =
        p.make_track_item(item, TrackKind::Video, filmcraft_time::Tick::ZERO, TimeRange::new(filmcraft_time::Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(item, src);
    (Arc::new(p), seq, m)
}

fn export_clip_png(dir: &str, gpu: GpuRendering, media: &Arc<[u8]>, (w, h): (u32, u32)) {
    let (p, seq, m) = clip_project(media, w, h, 3);
    let mut s = ExportSettings {
        format: Format::PngSequence,
        path: std::env::temp_dir().join(format!("fc-gpu-export-{dir}")).to_string_lossy().into_owned(),
        ..Default::default()
    };
    s.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(3)));
    s.gpu_rendering = gpu;
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
}

/// ProRes and DNx media export through the GPU renderer decoded in its compute shaders, as the
/// monitors decode them, and match the CPU export; a frame the GPU finds corrupt is rendered on
/// the CPU instead of being exported broken.
fn coded_media_exports_decode_on_the_gpu() {
    use filmcraft_dnx::{AlphaCoding, Profile};
    use std::sync::atomic::Ordering;
    let (w, h) = (320u32, 180u32);
    let (wu, hu) = (w as usize, h as usize);
    let dnx = |profile: Profile, bits: u8, rgb: bool, alpha: Option<AlphaCoding>| -> Vec<Vec<u8>> {
        let cfg = filmcraft_dnx::EncoderConfig { bit_depth: bits, alpha, ..filmcraft_dnx::EncoderConfig::new(profile, w, h) };
        let chroma = cfg.chroma();
        let mut enc = filmcraft_dnx::Encoder::with_config(cfg).unwrap();
        (0..3)
            .map(|k| {
                let mut f = filmcraft_dnx::Frame::new(w, h, chroma, bits, alpha.is_some());
                let cw = f.chroma_width() as usize;
                [f.y, f.cb, f.cr] = busy(wu, hu, cw, u32::from(bits), k + 1);
                f.rgb = rgb;
                if let Some(a) = f.alpha.as_mut() {
                    let max = (1u32 << bits) - 1;
                    for (i, v) in a.iter_mut().enumerate() {
                        *v = if (i % wu) < wu / 2 { max as u16 } else { ((i % wu) as u32 * max / wu as u32) as u16 };
                    }
                }
                enc.encode(&f).unwrap()
            })
            .collect()
    };
    let prores: Vec<Vec<u8>> = {
        let mut enc = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, w, h);
        (0..3)
            .map(|k| {
                let mut f = filmcraft_prores::Frame::new(w, h, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
                [f.y, f.cb, f.cr] = busy(wu, hu, wu / 2, 10, k + 1);
                enc.encode(&f).unwrap()
            })
            .collect()
    };
    let dnx_entry = || filmcraft_isobmff::SampleEntry::dnx(filmcraft_isobmff::FourCc(*b"AVdh"), w as u16, h as u16);
    let mut corrupt = dnx(Profile::Hqx, 10, false, None);
    // garbage over the middle of a scan line of the second frame: the layout parses, the GPU
    // finds the entropy-coded data broken (and the CPU decoder rejects it)
    let l = filmcraft_dnx::frame_layout(&corrupt[1]).unwrap();
    let r = l.rows[4].data;
    for b in &mut corrupt[1][r.0 as usize + 16..(r.0 + r.1 / 2) as usize] {
        *b = 0xff;
    }
    assert!(filmcraft_dnx::decode_frame(&corrupt[1]).is_err());
    let cases = [
        ("prores-hq", mov(filmcraft_isobmff::SampleEntry::prores(filmcraft_isobmff::FourCc(*b"apch"), w as u16, h as u16), &prores), 3),
        ("dnxhr-hqx", mov(dnx_entry(), &dnx(Profile::Hqx, 10, false, None)), 3),
        ("dnxhr-444-rgb", mov(dnx_entry(), &dnx(Profile::R444, 12, true, None)), 3),
        ("dnxhr-hq-alpha", mov(dnx_entry(), &dnx(Profile::Hq, 8, false, Some(AlphaCoding::Lossless))), 3),
        // the corrupt frame is decoded on the GPU too, found broken and rendered again on the CPU
        ("dnxhr-corrupt", mov(dnx_entry(), &corrupt), 3),
    ];
    for (name, media, decodes) in cases {
        let before = INTRA_DECODES.load(Ordering::Relaxed);
        export_clip_png(&format!("{name}-cpu"), GpuRendering::Off, &media, (w, h));
        export_clip_png(&format!("{name}-gpu"), GpuRendering::Auto, &media, (w, h));
        for f in ["000", "001", "002"] {
            assert_parity(
                &png(&format!("{name}-cpu"), &format!("img{f}.png")),
                &png(&format!("{name}-gpu"), &format!("img{f}.png")),
                &format!("{name} frame {f}"),
            );
        }
        let decoded = INTRA_DECODES.load(Ordering::Relaxed) - before;
        assert!(decoded >= decodes, "{name}: {decoded} pictures decoded on the GPU");
    }
}

/// PSNR in dB of two RGBA8 pictures over all four channels (infinite when equal).
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let se: f64 = a.iter().zip(b).map(|(&x, &y)| f64::from(x.abs_diff(y)).powi(2)).sum();
    if se == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / (se / a.len() as f64)).log10() }
}

/// Export `media` (3 frames) to `format` as `name` with `hardware`, and decode what was written
/// (every frame must decode).
fn export_codec(media: &Arc<[u8]>, name: &str, format: Format, profile: &str, alpha: bool, hardware: filmcraft_export::HardwareEncoding) -> Vec<Vec<u8>> {
    let (w, h) = (320u32, 180u32);
    let (p, seq, m) = clip_project(media, w, h, 3);
    let path = std::env::temp_dir().join(format!("fc-gpu-enc-{}-{name}.mov", std::process::id())).to_string_lossy().into_owned();
    let mut s = ExportSettings { format, path: path.clone(), include_audio: false, alpha, hardware_encoding: hardware, ..Default::default() };
    match format {
        Format::ProRes => s.prores_profile = profile.into(),
        _ => s.dnx_profile = profile.into(),
    }
    s.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(3)));
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert_eq!(r.frames, 3, "{name}: frame count");
    let src = filmcraft_codecs::open_bytes(&path, std::fs::read(&path).unwrap().into()).unwrap();
    (0..3).map(|i| src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_24.tick_of(i))).unwrap().to_rgba8().to_vec()).collect()
}

/// The GPU encoders under Hardware encoding: Auto write files that parse, decode and look like
/// the CPU encoder's (within 0.1 dB against the picture that went in), and do the encoding.
#[test]
fn gpu_encoders_export_like_the_cpu() {
    let _serial = serial();
    use filmcraft_export::HardwareEncoding::{Auto, Off};
    let (Some(_), Some(_)) = (filmcraft_gpu::ProResGpuEncoder::new(), filmcraft_gpu::DnxGpuEncoder::new()) else {
        eprintln!("SKIPPED (gpu encoders): no adapter");
        return;
    };
    filmcraft_export::register_prores_backend(|| filmcraft_gpu::ProResGpuEncoder::new().map(|e| Box::new(e) as _));
    filmcraft_export::register_dnx_backend(|| filmcraft_gpu::DnxGpuEncoder::new().map(|e| Box::new(e) as _));
    let (w, h) = (320u32, 180u32);
    let (wu, hu) = (w as usize, h as usize);
    let prores = |profile: filmcraft_prores::Profile, alpha: bool| -> Arc<[u8]> {
        let mut cfg = filmcraft_prores::EncoderConfig::new(profile, w, h);
        cfg.encode_alpha = alpha;
        let chroma = profile.chroma();
        let mut enc = filmcraft_prores::Encoder::with_config(cfg);
        let frames: Vec<Vec<u8>> = (0..3)
            .map(|k| {
                let mut f = filmcraft_prores::Frame::new(w, h, chroma, 10, alpha);
                let cw = f.chroma_width() as usize;
                [f.y, f.cb, f.cr] = busy(wu, hu, cw, 10, k + 1);
                if let Some(a) = f.alpha.as_mut() {
                    for (i, v) in a.iter_mut().enumerate() {
                        *v = ((i % wu) as u32 * 1023 / wu as u32) as u16;
                    }
                }
                enc.encode(&f).unwrap()
            })
            .collect();
        let fourcc = filmcraft_isobmff::FourCc(profile.fourcc());
        mov(filmcraft_isobmff::SampleEntry::prores(fourcc, w as u16, h as u16), &frames)
    };
    let dnx_src = {
        let cfg = filmcraft_dnx::EncoderConfig { bit_depth: 10, ..filmcraft_dnx::EncoderConfig::new(filmcraft_dnx::Profile::Hqx, w, h) };
        let mut enc = filmcraft_dnx::Encoder::with_config(cfg).unwrap();
        let frames: Vec<Vec<u8>> = (0..3)
            .map(|k| {
                let mut f = filmcraft_dnx::Frame::new(w, h, filmcraft_dnx::ChromaFormat::Yuv422, 10, false);
                let cw = f.chroma_width() as usize;
                [f.y, f.cb, f.cr] = busy(wu, hu, cw, 10, k + 1);
                enc.encode(&f).unwrap()
            })
            .collect();
        mov(filmcraft_isobmff::SampleEntry::dnx(filmcraft_isobmff::FourCc(*b"AVdh"), w as u16, h as u16), &frames)
    };
    let cases = [
        ("prores-hq", prores(filmcraft_prores::Profile::Hq, false), Format::ProRes, "hq", false),
        ("prores-4444-alpha", prores(filmcraft_prores::Profile::P4444, true), Format::ProRes, "4444", true),
        ("dnxhr-hqx", dnx_src, Format::DnxHr, "hqx", false),
    ];
    for (name, media, format, profile, alpha) in cases {
        let before = filmcraft_export::gpu_render_stats();
        let cpu = export_codec(&media, &format!("{name}-cpu"), format, profile, alpha, Off);
        assert_eq!(filmcraft_export::gpu_render_stats().encoded, before.encoded, "{name}: Off encodes on the CPU");
        let gpu = export_codec(&media, &format!("{name}-gpu"), format, profile, alpha, Auto);
        let st = filmcraft_export::gpu_render_stats();
        assert!(st.encoded >= before.encoded + 3, "{name}: {} pictures encoded on the GPU", st.encoded - before.encoded);
        let src = filmcraft_codecs::open_bytes("clip.mov", media.clone()).unwrap();
        for i in 0..3 {
            let want = src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_24.tick_of(i))).unwrap().to_rgba8();
            let (c, g) = (psnr(&want, &cpu[i as usize]), psnr(&want, &gpu[i as usize]));
            eprintln!("{name} frame {i}: CPU {c:.2} dB, GPU {g:.2} dB against the source");
            assert!((c - g).abs() <= 0.1, "{name} frame {i}: CPU {c:.2} dB, GPU {g:.2} dB");
        }
    }
}

/// What the paired GPU export of the tests below does: 0 no pair (the other tests see the
/// registries as they left them), 1 a pair whose renderers read every frame back, 2 a pair whose
/// frames stay on the GPU.
static PAIR_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Sets [`PAIR_MODE`] and puts it back to 0 when dropped (also when a test fails).
struct PairMode;

impl PairMode {
    fn set(mode: u8) -> PairMode {
        PAIR_MODE.store(mode, std::sync::atomic::Ordering::SeqCst);
        PairMode
    }
}

impl Drop for PairMode {
    fn drop(&mut self) {
        PAIR_MODE.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A renderer of the shipped pair whose frames are read back (mode 1): its resident methods are
/// the trait's defaults.
struct ReadBack(Box<dyn FrameRenderer>);

impl FrameRenderer for ReadBack {
    fn render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        self.0.render(project, seq, t, opts, sources)
    }

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        self.0.caps()
    }
}

/// The pair the apps register (`filmcraft_gpu_export::gpu_pair`), its renderers wrapped in
/// [`ReadBack`] in mode 1.
struct Pair(Box<dyn filmcraft_export::GpuPair>);

impl filmcraft_export::GpuPair for Pair {
    fn renderer(&self) -> Option<Box<dyn FrameRenderer>> {
        let r = self.0.renderer()?;
        Some(if PAIR_MODE.load(std::sync::atomic::Ordering::SeqCst) == 2 { r } else { Box::new(ReadBack(r)) })
    }
    fn prores_backend(&self) -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
        self.0.prores_backend()
    }
    fn dnx_backend(&self) -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
        self.0.dnx_backend()
    }
}

fn register_pair() {
    filmcraft_export::register_gpu_pair(|| {
        if PAIR_MODE.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            return None;
        }
        filmcraft_gpu_export::gpu_pair().map(|p| Box::new(Pair(p)) as Box<dyn filmcraft_export::GpuPair>)
    });
}

/// A ProRes 4444 clip with an alpha ramp (3 busy frames, 320×180).
fn alpha_clip() -> Arc<[u8]> {
    let (w, h) = (320u32, 180u32);
    let (wu, hu) = (w as usize, h as usize);
    let profile = filmcraft_prores::Profile::P4444;
    let mut enc = filmcraft_prores::Encoder::new(profile, w, h);
    let frames: Vec<Vec<u8>> = (0..3)
        .map(|k| {
            let mut f = filmcraft_prores::Frame::new(w, h, profile.chroma(), 10, true);
            [f.y, f.cb, f.cr] = busy(wu, hu, wu, 10, k + 7);
            if let Some(a) = f.alpha.as_mut() {
                for (i, v) in a.iter_mut().enumerate() {
                    *v = if i % wu < wu / 3 { 1023 } else { ((i % wu) as u32 * 1023 / wu as u32) as u16 };
                }
            }
            enc.encode(&f).unwrap()
        })
        .collect();
    mov(filmcraft_isobmff::SampleEntry::prores(filmcraft_isobmff::FourCc(profile.fourcc()), w as u16, h as u16), &frames)
}

/// One export of the resident tests: the settings, and the frames' coded samples.
struct Resident {
    path: String,
    frames: Vec<Vec<u8>>,
}

/// Export `media` (3 frames) as `name` with `s`'s format and profile, GPU rendering and encoding,
/// scaled into a 256×160 frame (rendered at full size, so resampled, and letterboxed), the video
/// limiter on; the coded samples, each checked to parse.
fn resident_export(media: &Arc<[u8]>, name: &str, s: &ExportSettings) -> Resident {
    resident_export_at(media, name, s, (256, 160))
}

/// [`resident_export`] into a frame of `size`.
fn resident_export_at(media: &Arc<[u8]>, name: &str, s: &ExportSettings, size: (u32, u32)) -> Resident {
    let (p, seq, m) = clip_project(media, 320, 180, 3);
    let ext = if s.format.is_image_sequence() { "" } else { ".mov" };
    let path = std::env::temp_dir().join(format!("fc-gpu-resident-{}-{name}{ext}", std::process::id())).to_string_lossy().into_owned();
    let mut s = ExportSettings { path: path.clone(), include_audio: false, gpu_rendering: GpuRendering::Auto, ..s.clone() };
    s.hardware_encoding = filmcraft_export::HardwareEncoding::Auto;
    s.frame_size = Some(size);
    s.max_render_quality = true;
    s.effects.video_limiter = filmcraft_export::settings::VideoLimiter { enabled: true, min_percent: 8.0, max_percent: 88.0 };
    s.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(3)));
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(r.frames, 3, "{name}: frame count");
    if s.format.is_image_sequence() {
        return Resident { path, frames: Vec::new() };
    }
    let data = std::fs::read(&path).unwrap();
    let file = filmcraft_isobmff::open(data.as_slice()).unwrap();
    let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
    let frames: Vec<Vec<u8>> = (0..file.tracks[vt].samples.len()).map(|i| file.read_sample(data.as_slice(), vt, i).unwrap().to_vec()).collect();
    assert_eq!(frames.len(), 3, "{name}: samples");
    for (i, f) in frames.iter().enumerate() {
        let parses = match s.video_format() {
            Format::ProRes => filmcraft_prores::layout::frame_layout(f).is_ok(),
            _ => filmcraft_dnx::frame_layout(f).is_ok(),
        };
        assert!(parses, "{name} frame {i}: the frame parses");
    }
    Resident { path, frames }
}

/// The decoded frames of an export (straight RGBA8, every frame must decode).
fn decoded(r: &Resident) -> Vec<Vec<u8>> {
    let src = filmcraft_codecs::open_bytes(&r.path, std::fs::read(&r.path).unwrap().into()).unwrap();
    (0..3).map(|i| src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_24.tick_of(i))).unwrap().to_rgba8().to_vec()).collect()
}

/// The export's frames as a PNG sequence (the reference picture the encoders' outputs are measured
/// against: rendered and post-processed the same way, not coded).
fn reference_frames(media: &Arc<[u8]>, name: &str, alpha: bool) -> Vec<Vec<u8>> {
    reference_frames_at(media, name, alpha, (256, 160))
}

/// [`reference_frames`] in a frame of `size`.
fn reference_frames_at(media: &Arc<[u8]>, name: &str, alpha: bool, size: (u32, u32)) -> Vec<Vec<u8>> {
    let s = ExportSettings { format: Format::PngSequence, alpha, ..Default::default() };
    let r = resident_export_at(media, &format!("{name}-png"), &s, size);
    (0..3)
        .map(|i| {
            let file = std::path::PathBuf::from(format!("{}{i:03}.png", r.path));
            image::load_from_memory(&std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))).unwrap().to_rgba8().into_raw()
        })
        .collect()
}

/// With a GPU pair registered, a ProRes / DNxHR export with scaling and the video limiter keeps
/// every frame on the GPU from the renderer into the encoder (the counter says so for every frame),
/// writes frames that parse and decode, and looks like the read-back export of the same pair:
/// against the same reference picture, its PSNR is within 0.1 dB of the read-back one's.
#[test]
fn resident_export_matches_the_read_back_export() {
    let _serial = serial();
    if filmcraft_gpu::ExportDevice::shared().is_none() {
        eprintln!("SKIPPED (resident export): no adapter");
        return;
    }
    register_pair();
    let media = alpha_clip();
    let settings = |format, profile: &str, alpha| {
        let mut s = ExportSettings { format, alpha, ..Default::default() };
        match format {
            Format::ProRes => s.prores_profile = profile.into(),
            _ => s.dnx_profile = profile.into(),
        }
        s
    };
    let interlaced = ExportSettings { field_order: filmcraft_export::settings::FieldOrder::UpperFirst, ..settings(Format::ProRes, "hq", false) };
    let cases = [
        ("prores-hq", settings(Format::ProRes, "hq", false)),
        ("prores-4444-alpha", settings(Format::ProRes, "4444", true)),
        ("dnxhr-hqx", settings(Format::DnxHr, "hqx", false)),
        // 8-bit DNxHR: the 8-bit picture's table, and lossless alpha
        ("dnxhr-hq-alpha", settings(Format::DnxHr, "hq", true)),
        ("prores-hq-interlaced", interlaced),
    ];
    for (name, s) in cases {
        let reference = {
            let _pair = PairMode::set(1);
            reference_frames(&media, name, s.alpha)
        };
        let before = filmcraft_export::gpu_render_stats();
        let read_back = {
            let _pair = PairMode::set(1);
            resident_export(&media, &format!("{name}-read-back"), &s)
        };
        let mid = filmcraft_export::gpu_render_stats();
        assert_eq!(mid.resident, before.resident, "{name}: the read-back export keeps no frame on the GPU");
        assert!(mid.encoded >= before.encoded + 3, "{name}: the read-back export encodes on the GPU");
        let resident = {
            let _pair = PairMode::set(2);
            resident_export(&media, &format!("{name}-resident"), &s)
        };
        let after = filmcraft_export::gpu_render_stats();
        assert_eq!(after.resident - mid.resident, 3, "{name}: every frame stayed on the GPU ({after:?})");
        assert_eq!(after.resident_fallbacks, mid.resident_fallbacks, "{name}: no frame was read back");
        assert_eq!(after.encoded - mid.encoded, 3, "{name}: every frame encoded on the GPU");
        if name.contains("interlaced") {
            for f in &resident.frames {
                assert_eq!(filmcraft_prores::layout::frame_layout(f).unwrap().header.interlace, filmcraft_prores::Interlace::TopFieldFirst, "{name}");
            }
        }
        let (rb, rs) = (decoded(&read_back), decoded(&resident));
        for i in 0..3 {
            let (a, b) = (psnr(&reference[i], &rb[i]), psnr(&reference[i], &rs[i]));
            eprintln!("{name} frame {i}: read-back {a:.3} dB, resident {b:.3} dB against the reference; resident vs read-back {:.2} dB", psnr(&rb[i], &rs[i]));
            assert!((a - b).abs() <= 0.1, "{name} frame {i}: read-back {a:.3} dB, resident {b:.3} dB");
        }
    }
}

/// A setting the GPU has no pass for (a timecode overlay) keeps the whole export on the read-back
/// path, decided before the first frame: no frame stays on the GPU or is tried there, and the file
/// is complete and still encoded on the GPU.
#[test]
fn an_unsupported_setting_keeps_the_read_back_path() {
    let _serial = serial();
    if filmcraft_gpu::ExportDevice::shared().is_none() {
        eprintln!("SKIPPED (resident export): no adapter");
        return;
    }
    register_pair();
    let _pair = PairMode::set(2);
    let media = alpha_clip();
    let mut s = ExportSettings { format: Format::ProRes, prores_profile: "hq".into(), ..Default::default() };
    s.effects.timecode_overlay.enabled = true;
    let before = filmcraft_export::gpu_render_stats();
    let r = resident_export(&media, "timecode-overlay", &s);
    let after = filmcraft_export::gpu_render_stats();
    assert_eq!(after.resident, before.resident, "no frame stays on the GPU ({after:?})");
    assert_eq!(after.resident_fallbacks, before.resident_fallbacks, "and none was tried there");
    assert!(after.encoded >= before.encoded + 3, "the frames are still encoded on the GPU");
    assert_eq!(decoded(&r).len(), 3);
    // the control: the same export without the overlay keeps every frame on the GPU
    s.effects.timecode_overlay.enabled = false;
    let before = filmcraft_export::gpu_render_stats();
    resident_export(&media, "timecode-overlay-control", &s);
    let after = filmcraft_export::gpu_render_stats();
    assert_eq!((after.resident - before.resident, after.resident_fallbacks - before.resident_fallbacks), (3, 0), "without the overlay: {after:?}");
}

/// Odd and tiny frames stay on the GPU and match the read-back export too: the export rounds a
/// video frame to even sides, so 17×13 is coded 16×12 (a picture of 16×9 letterboxed, resampled
/// by 20) and 255×143 is coded 254×142 (a 252×142 picture between one-column bars). A 1×1 frame (coded
/// 2×2) reduces the picture 160 times, more than the GPU's scaling pass takes
/// (`filmcraft_gpu::export_post::MAX_REDUCTION`): that export is read back, decided up front.
#[test]
fn resident_frames_of_odd_sizes_match_the_read_back() {
    let _serial = serial();
    if filmcraft_gpu::ExportDevice::shared().is_none() {
        eprintln!("SKIPPED (resident export): no adapter");
        return;
    }
    register_pair();
    let media = alpha_clip();
    let cases = [
        ("prores-hq", ExportSettings { format: Format::ProRes, prores_profile: "hq".into(), ..Default::default() }),
        ("prores-4444-alpha", ExportSettings { format: Format::ProRes, prores_profile: "4444".into(), alpha: true, ..Default::default() }),
        ("dnxhr-hq-alpha", ExportSettings { format: Format::DnxHr, dnx_profile: "hq".into(), alpha: true, ..Default::default() }),
    ];
    for size in [(17u32, 13u32), (1, 1), (255, 143)] {
        for (name, s) in &cases {
            let name = format!("{name}-{}x{}", size.0, size.1);
            let read_back = {
                let _pair = PairMode::set(1);
                resident_export_at(&media, &format!("{name}-read-back"), s, size)
            };
            // (the encoders' frame size: video exports round odd sizes to even ones)
            let coded = {
                let src = filmcraft_codecs::open_bytes(&read_back.path, std::fs::read(&read_back.path).unwrap().into()).unwrap();
                let v = src.info().video.clone().unwrap();
                (v.width, v.height)
            };
            let read_back = decoded(&read_back);
            let reference = {
                let _pair = PairMode::set(1);
                reference_frames_at(&media, &name, s.alpha, coded)
            };
            let before = filmcraft_export::gpu_render_stats();
            let resident = {
                let _pair = PairMode::set(2);
                decoded(&resident_export_at(&media, &format!("{name}-resident"), s, size))
            };
            let after = filmcraft_export::gpu_render_stats();
            let want = if size == (1, 1) { 0 } else { 3 };
            assert_eq!((after.resident - before.resident, after.resident_fallbacks - before.resident_fallbacks), (want, 0), "{name}: {after:?}");
            eprintln!("{name}: coded as {coded:?}");
            for i in 0..3 {
                let (a, b) = (psnr(&reference[i], &read_back[i]), psnr(&reference[i], &resident[i]));
                assert!((a - b).abs() <= 0.1 || (a.is_infinite() && b.is_infinite()), "{name} frame {i}: read-back {a:.3} dB, resident {b:.3} dB");
            }
        }
    }
}

/// The measurement of ADR 0003 Step 2 (run by hand: `cargo test --release -p filmcraft-gpu --test
/// export_pipeline -- --ignored --nocapture bench_uhd`): a 100-frame UHD export of a ProRes HQ clip
/// to ProRes HQ and to DNxHR HQX, on the CPU (rendering and encoding Off), with GPU rendering and
/// encoding reading every frame back, and with the frames staying on the GPU. Three runs each; the
/// median's wall time and its export stages (`perf.stats` `export.stages`).
#[test]
#[ignore]
fn bench_uhd_resident_export() {
    let _serial = serial();
    if filmcraft_gpu::ExportDevice::shared().is_none() {
        eprintln!("SKIPPED (bench): no adapter");
        return;
    }
    register_pair();
    let (w, h) = (3840u32, 2160u32);
    let (wu, hu) = (w as usize, h as usize);
    let distinct: Vec<Vec<u8>> = {
        let mut enc = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, w, h);
        (0..4)
            .map(|k| {
                let mut f = filmcraft_prores::Frame::new(w, h, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
                [f.y, f.cb, f.cr] = busy(wu, hu, wu / 2, 10, k + 1);
                enc.encode(&f).unwrap()
            })
            .collect()
    };
    let frames: Vec<Vec<u8>> = (0..100).map(|i| distinct[i % 4].clone()).collect();
    let media = mov(filmcraft_isobmff::SampleEntry::prores(filmcraft_isobmff::FourCc(*b"apch"), w as u16, h as u16), &frames);
    let (p, seq, m) = clip_project(&media, w, h, 100);
    for (format, profile) in [(Format::ProRes, "hq"), (Format::DnxHr, "hqx")] {
        for (mode, gpu) in [("cpu", 0u8), ("gpu read-back", 1), ("gpu resident", 2)] {
            // (the CPU runs take nothing from the pair; mode 1 reads every frame back)
            let _pair = PairMode::set(gpu.max(1));
            let mut runs = Vec::new();
            for run in 0..3 {
                let path = std::env::temp_dir().join(format!("fc-gpu-bench-{}-{run}.mov", std::process::id())).to_string_lossy().into_owned();
                let mut s = ExportSettings { format, path: path.clone(), include_audio: false, ..Default::default() };
                match format {
                    Format::ProRes => s.prores_profile = profile.into(),
                    _ => s.dnx_profile = profile.into(),
                }
                if gpu == 0 {
                    (s.gpu_rendering, s.hardware_encoding) = (GpuRendering::Off, filmcraft_export::HardwareEncoding::Off);
                } else {
                    (s.gpu_rendering, s.hardware_encoding) = (GpuRendering::Auto, filmcraft_export::HardwareEncoding::Auto);
                }
                let st0 = filmcraft_export::gpu_render_stats();
                let s0 = filmcraft_export::stage_times();
                let t0 = std::time::Instant::now();
                let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
                let secs = t0.elapsed().as_secs_f64();
                let stages: Vec<(&str, f64)> = filmcraft_export::stage_times()
                    .iter()
                    .zip(&s0)
                    .map(|((st, b), (_, a))| (st.name(), (b - a) as f64 / 1e9))
                    .filter(|(_, v)| *v > 0.0)
                    .collect();
                let st = filmcraft_export::gpu_render_stats();
                assert_eq!(r.frames, 100);
                let _ = std::fs::remove_file(&path);
                runs.push((secs, stages, st.resident - st0.resident, st.encoded - st0.encoded));
            }
            runs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (secs, stages, resident, encoded) = &runs[1];
            let stages = stages.iter().map(|(n, v)| format!("{n} {v:.2}")).collect::<Vec<_>>().join(", ");
            eprintln!(
                "BENCH {} {profile} UHD 100 frames, {mode}: median {secs:.2} s ({:.1} ms/frame; runs {:.2} / {:.2} / {:.2}), resident {resident}, gpu-encoded {encoded}; stages: {stages}",
                format.label(),
                secs * 10.0,
                runs[0].0,
                runs[1].0,
                runs[2].0
            );
        }
    }
}

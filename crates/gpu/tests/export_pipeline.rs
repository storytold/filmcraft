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

#[test]
fn gpu_export_matches_cpu_and_counts() {
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

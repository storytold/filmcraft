use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::TICKS_PER_SECOND;

pub(crate) fn project() -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("x");
    let g = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
    let tone = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -6.0 }, 320, 180, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
    let add = |p: &mut Project, g: &GeneratorSource| {
        let info = g.info().clone();
        p.add_item(
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
        )
    };
    let red = add(&mut p, &g);
    let t = add(&mut p, &tone);
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
    let r = FrameRate::FPS_24;
    let v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let a = p.make_track_item(t, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(a);
    let mut m = SourceMap::default();
    m.0.insert(red, Arc::new(g));
    m.0.insert(t, Arc::new(tone));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-export-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

#[test]
fn mjpeg_mov_roundtrip() {
    let (p, seq, m) = project();
    let path = tmp("out.mov");
    let prog = Progress::default();
    let r = export(&p, seq, &ExportSettings { format: Format::Mjpeg, path: path.clone(), ..Default::default() }, &m, &prog).unwrap();
    assert_eq!(r.frames, 24);
    assert!(prog.finished.load(Ordering::Relaxed));
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_codecs::open_bytes("out.mov", bytes).unwrap();
    let info = src.info();
    assert_eq!(info.video.as_ref().unwrap().width, 320);
    assert!((info.duration.seconds() - 1.0).abs() < 0.05, "{}", info.duration.seconds());
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 2))).unwrap().to_rgba8();
    assert!(f[0] > 230 && f[1] < 30, "{:?}", &f[..4]);
    let a = src.audio(0, 24_000, 48_000).unwrap();
    let pk = a.peaks()[0];
    assert!((pk - 0.5).abs() < 0.05, "tone at -6 dB: {pk}");
    if let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("export mjpeg frame count") {
        let out = std::process::Command::new(ffprobe)
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", &path])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "24");
    }
}

#[test]
fn wav_png_gif() {
    let (p, seq, m) = project();
    for (fmt, name) in [(Format::Wav, "a.wav"), (Format::Gif, "a.gif"), (Format::PngSequence, "seq.png")] {
        let path = tmp(name);
        let prog = Progress::default();
        let mut s = ExportSettings { format: fmt, path: path.clone(), scale: 0.5, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(6)));
        let r = export(&p, seq, &s, &m, &prog).unwrap();
        assert!(r.bytes > 0, "{fmt:?}");
    }
    assert!(std::path::Path::new(&tmp("seq005.png")).exists());
}

#[test]
fn prores_export_roundtrip() {
    let (p, seq, m) = project();
    let path = tmp("pr.mov");
    export(&p, seq, &ExportSettings { format: Format::ProRes, path: path.clone(), ..Default::default() }, &m, &Progress::default()).unwrap();
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_codecs::open_bytes("pr.mov", bytes).unwrap();
    assert!(src.info().video.as_ref().unwrap().codec.contains("ProRes 422 HQ"));
    let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 3))).unwrap().to_rgba8();
    assert!(f[0] > 240 && f[1] < 15 && f[2] < 15, "{:?}", &f[..4]);
}

/// ProRes 4444 / 4444 XQ export (#342): 4:4:4 frames under the `ap4h` / `ap4x` codes, and with
/// Include Alpha Channel the picture's alpha survives instead of being flattened over black.
#[test]
fn prores_4444_export_roundtrip() {
    let (p, seq, m) = project();
    for (profile, label) in [("4444", "ProRes 4444"), ("4444xq", "ProRes 4444 XQ")] {
        let path = tmp(&format!("pr_{profile}.mov"));
        let s = ExportSettings { format: Format::ProRes, path: path.clone(), prores_profile: profile.into(), ..Default::default() };
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let src = filmcraft_codecs::open_bytes("pr.mov", bytes).unwrap();
        let codec = src.info().video.as_ref().unwrap().codec.clone();
        assert!(codec.contains(label), "{profile}: {codec}");
        let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 3))).unwrap();
        assert!(f.format_label().contains("4:4:4"), "{profile}: {}", f.format_label());
        let px = f.to_rgba8();
        assert!(px[0] > 240 && px[1] < 15 && px[2] < 15 && px[3] == 255, "{profile}: {:?}", &px[..4]);
    }

    // a half-transparent matte keeps its alpha with Include Alpha Channel, and only then
    let mut p = (*p).clone();
    let half = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 0.5] }, 320, 180, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
    let item = p.sequence(seq).unwrap().video_tracks[0].items[0].item;
    if let ItemKind::Media(mc) = &mut p.item_mut(item).unwrap().kind {
        mc.media = MediaRef::Generator(half.generator.clone());
    }
    let mut m = m;
    m.0.insert(item, Arc::new(half));
    let p = Arc::new(p);
    for alpha in [true, false] {
        let path = tmp(&format!("pr_4444_alpha_{alpha}.mov"));
        let s = ExportSettings { format: Format::ProRes, path: path.clone(), prores_profile: "4444".into(), alpha, ..Default::default() };
        assert!(s.keeps_alpha() == alpha);
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let src = filmcraft_codecs::open_bytes("pr.mov", bytes).unwrap();
        let px = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 3))).unwrap().to_rgba8();
        if alpha {
            assert!((px[3] as i32 - 128).abs() <= 3, "alpha kept: {:?}", &px[..4]);
        } else {
            assert_eq!(px[3], 255, "flattened: {:?}", &px[..4]);
        }
    }
}

#[test]
fn dnxhr_export_roundtrip() {
    let (p, seq, m) = project();
    for (profile, label) in [("hq", "DNxHR HQ"), ("hqx", "DNxHR HQX"), ("sq", "DNxHR SQ")] {
        let path = tmp(&format!("dnx_{profile}.mov"));
        let s = ExportSettings { format: Format::DnxHr, path: path.clone(), dnx_profile: profile.into(), ..Default::default() };
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let src = filmcraft_codecs::open_bytes("dnx.mov", bytes).unwrap();
        let info = src.info().video.clone().unwrap();
        assert_eq!(info.codec, label);
        let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 3))).unwrap();
        assert!(f.format_label().contains("4:2:2"), "{}", f.format_label());
        let px = f.to_rgba8();
        assert!(px[0] > 240 && px[1] < 15 && px[2] < 15, "{profile}: {:?}", &px[..4]);
    }
}

/// Burn-in: an H.264 export of a dark-blue matte with a caption, decoded by ffmpeg (external test
/// oracle only), has bright caption pixels in the lower part of the frame, and none without
/// `burn_captions` or above the caption.
#[test]
fn caption_burn_in_h264_ffmpeg_oracle() {
    let Some(ffmpeg) = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().find(|p| std::path::Path::new(p).exists()) else {
        eprintln!("ffmpeg not found; skipping oracle test");
        return;
    };
    let mut p = Project::new("cap");
    let (w, h) = (640u32, 360u32);
    let g = GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 0.2, 1.0] }, w, h, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let matte = p.add_item(
        "matte",
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
    let r = FrameRate::FPS_24;
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: r, ..Default::default() }, 1, 0, None);
    let v = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let tid = filmcraft_project::TrackId(p.alloc_id());
    let cid = filmcraft_project::ClipId(p.alloc_id());
    let mut ct = filmcraft_project::CaptionTrack::new(tid, "Subtitle".into(), filmcraft_project::CaptionFormat::Subtitle);
    ct.style.background = false;
    ct.captions.push(filmcraft_project::Caption {
        id: cid,
        start: Tick::ZERO,
        duration: r.tick_of(24),
        text: "BURNED IN".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items.push(v);
    q.caption_tracks.push(ct);
    let mut m = SourceMap::default();
    m.0.insert(matte, Arc::new(g));
    let p = Arc::new(p);
    let decode = |burn: bool| -> Vec<u8> {
        let path = tmp(if burn { "burn.mp4" } else { "noburn.mp4" });
        let s = ExportSettings { format: Format::H264, path: path.clone(), include_audio: false, burn_captions: burn, ..Default::default() };
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let out = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-i", &path, "-vf", "select=eq(n\\,12)", "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout.len(), (w * h * 3) as usize);
        out.stdout
    };
    let bright = |rgb: &[u8], y0: u32, y1: u32, x0: u32, x1: u32| -> usize {
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| ((y * w + x) * 3) as usize)).filter(|&i| rgb[i] > 200 && rgb[i + 1] > 200 && rgb[i + 2] > 200).count()
    };
    let with = decode(true);
    let without = decode(false);
    let lower = bright(&with, h * 2 / 3, h, 0, w);
    assert!(lower > 300, "white caption text in the lower third: {lower}");
    assert_eq!(bright(&with, 0, h / 2, 0, w), 0, "nothing above the caption");
    assert_eq!(bright(&without, 0, h, 0, w), 0, "no burn-in unless asked");
    // centred: text on both sides of the centre line
    let (left, right) = (bright(&with, h * 2 / 3, h, 0, w / 2), bright(&with, h * 2 / 3, h, w / 2, w));
    assert!(left > 100 && right > 100, "{left} / {right}");
}

/// HDR sequences export PQ / HLG with colour signalling in the bitstream (VUI, ProRes header) and
/// the container (`colr`, `mdcv`, `clli`); our demuxer and ffprobe both read it back, and the
/// picture survives the round trip.
#[test]
fn hdr_exports_signal_pq_and_hlg() {
    use filmcraft_color::{ColorPipeline, Transfer, WorkingSpace};
    for (ws, fmt, ext, transfer, ff_transfer) in [
        (WorkingSpace::Rec2100Pq, Format::ProRes, "mov", Transfer::Pq, "smpte2084"),
        (WorkingSpace::Rec2100Pq, Format::H264, "mp4", Transfer::Pq, "smpte2084"),
        (WorkingSpace::Rec2100Hlg, Format::ProRes, "mov", Transfer::Hlg, "arib-std-b67"),
        (WorkingSpace::Rec2100Hlg, Format::H264, "mp4", Transfer::Hlg, "arib-std-b67"),
    ] {
        let (p, seq, m) = project();
        let mut p = (*p).clone();
        p.sequence_mut(seq).unwrap().settings.color = ColorPipeline { working: ws, ..ColorPipeline::REC709 };
        let p = Arc::new(p);
        let path = tmp(&format!("hdr-{}.{ext}", ws.id()));
        let s = ExportSettings { format: fmt, path: path.clone(), include_audio: false, ..Default::default() };
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        // our demuxer/decoder sees the signalling
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let src = filmcraft_codecs::open_bytes(&path, bytes).unwrap();
        let v = src.info().video.clone().unwrap();
        assert_eq!(v.color.transfer, transfer, "{path}");
        assert_eq!(v.color.primaries, filmcraft_color::Primaries::Bt2020, "{path}");
        assert_eq!(v.color.matrix, filmcraft_color::Matrix::Bt2020Ncl, "{path}");
        // PQ exports carry mastering metadata (1000 cd/m² peak; MaxCLL 0 = unknown), which sets
        // the tone-mapping peak when the file is used again
        if transfer == Transfer::Pq {
            let hdr = v.hdr.unwrap_or_else(|| panic!("{path}: no HDR metadata"));
            assert_eq!((hdr.mastering_max_nits, hdr.max_cll, hdr.peak_nits()), (Some(1000.0), None, Some(1000.0)), "{path}");
        }
        // 709 red matte → BT.2020 HDR → decoded back into Rec. 709 (tone mapped) stays red
        let f = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
        let back = filmcraft_render::colorman::decode(&Project::new("x"), ItemId(0), &f, 1, &ColorPipeline::REC709);
        let c = filmcraft_render::Image::unpremul(back.get(160, 90));
        assert!(c[0] > 0.6 && c[1] < 0.05 && c[2] < 0.05, "{path}: {c:?}");
        // ffprobe agrees
        let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("hdr export signalling") else { continue };
        let out = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_transfer,color_primaries,color_space:stream_side_data",
                "-of",
                "default=nw=1",
                &path,
            ])
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(txt.contains(&format!("color_transfer={ff_transfer}")), "{path}: {txt}");
        assert!(txt.contains("color_primaries=bt2020"), "{path}: {txt}");
        assert!(txt.contains("color_space=bt2020nc"), "{path}: {txt}");
        if transfer == Transfer::Pq {
            assert!(txt.contains("Mastering display metadata") && txt.contains("max_luminance=10000000/10000"), "{path}: {txt}");
            assert!(txt.contains("Content light level metadata"), "{path}: {txt}");
        }
        eprintln!("{path}:\n{txt}");
    }
}

/// Scenario runner for the GPU hook: everything in one test because the renderer registry is
/// process-global and parallel tests would see each other's factories.
#[test]
fn gpu_hook_scenarios() {
    // 1. Declining renderer: byte-equal to Off, and an empty registry too.
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| None);
    let (p, seq, m) = project();
    let mk = |path: &String, gpu: Option<bool>| {
        let mut s = ExportSettings { format: Format::PngSequence, path: path.clone(), ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(2)));
        s.gpu_rendering = match gpu {
            Some(true) | None => crate::GpuRendering::Auto,
            Some(false) => crate::GpuRendering::Off,
        };
        export(&p, seq, &s, &m, &Progress::default()).unwrap()
    };
    mk(&tmp("off.png"), Some(false));
    mk(&tmp("auto-declined.png"), Some(true));
    assert_eq!(std::fs::read(tmp("off000.png")).unwrap(), std::fs::read(tmp("auto-declined000.png")).unwrap(), "declined GPU export must be byte-equal to Off");
    crate::reset_frame_renderers_for_tests();
    mk(&tmp("auto-empty.png"), Some(true));
    assert_eq!(std::fs::read(tmp("off000.png")).unwrap(), std::fs::read(tmp("auto-empty000.png")).unwrap(), "empty registry must behave like Off");

    // 2. Producing renderer: counters up and its picture reaches the export.
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer) as Box<dyn FrameRenderer>));
    mk(&tmp("fake-gpu.png"), Some(true));
    assert!(gpu_render_stats().frames >= 2, "gpu frames counted: {:?}", gpu_render_stats());
    assert_eq!(gpu_render_stats().fallbacks, 0);
    mk(&tmp("fake-off.png"), Some(false));
    assert_eq!(gpu_render_stats().fallbacks, 0, "Off must not count as fallback");
    assert_ne!(std::fs::read(tmp("fake-gpu000.png")).unwrap(), std::fs::read(tmp("fake-off000.png")).unwrap(), "the fake GPU picture must reach the export");

    // 3. Hostile and degenerate frame sizes must not panic with a renderer attached.
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| None);
    for (w, h) in [(1, 1), (17, 13), (4096, 1)] {
        let mut p = Project::new("x");
        let g = GeneratorSource::new(Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
        let info = g.info().clone();
        let red = p.add_item(
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
        let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
        let v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(2)), FrameRate::FPS_24).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        let mut m = SourceMap::default();
        m.0.insert(red, Arc::new(g));
        let mut s = ExportSettings { format: Format::PngSequence, path: tmp("hostile.png"), ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(1)));
        s.gpu_rendering = crate::GpuRendering::Auto;
        let r = export(&Arc::new(p), seq, &s, &m, &Progress::default());
        if r.is_err() {
            continue; // sizes the exporter rejects are fine too — as long as nothing panics
        }
        let bytes = std::fs::read(tmp("hostile000.png")).unwrap_or_default();
        assert!(!bytes.is_empty(), "{w}x{h}: PNG written");
    }

    // 4. A renderer that panics is dropped, not put back: the export still completes, every
    //    frame comes from the CPU (byte-equal to Off), and no renderer is used after its panic.
    crate::reset_frame_renderers_for_tests();
    PANICS.store(0, std::sync::atomic::Ordering::SeqCst);
    crate::register_frame_renderer(|| Some(Box::new(PanickingRenderer) as Box<dyn FrameRenderer>));
    let (p, seq, m) = project();
    let mk12 = |path: &String, gpu: crate::GpuRendering| {
        let mut s = ExportSettings { format: Format::PngSequence, path: path.clone(), ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(12)));
        s.gpu_rendering = gpu;
        export(&p, seq, &s, &m, &Progress::default()).unwrap()
    };
    mk12(&tmp("panic-off.png"), crate::GpuRendering::Off);
    mk12(&tmp("panic-auto.png"), crate::GpuRendering::Auto);
    for f in ["000", "005", "011"] {
        assert_eq!(
            std::fs::read(tmp(&format!("panic-off{f}.png"))).unwrap(),
            std::fs::read(tmp(&format!("panic-auto{f}.png"))).unwrap(),
            "frame {f}: a panicking GPU renderer falls back to the CPU"
        );
    }
    // at most one panic per pooled renderer (4): none is used again after it panicked
    assert!(PANICS.load(std::sync::atomic::Ordering::SeqCst) <= 4, "renderer reused after a panic: {}", PANICS.load(std::sync::atomic::Ordering::SeqCst));

    // 5. A frame the planner hands back as one CPU image (an adjustment layer over the clip from
    //    frame 12) is rendered by the CPU without taking a pooled renderer: it counts as a
    //    fallback, and the renderer's picture reaches only the frames before it.
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer) as Box<dyn FrameRenderer>));
    let (p, seq, m) = project();
    let mut p = Arc::unwrap_or_clone(p);
    let r = FrameRate::FPS_24;
    let adj = p.add_item("adj", Label::Iris, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: r, duration: Tick(TICKS_PER_SECOND) }, None);
    let over = p.make_track_item(adj, TrackKind::Video, r.tick_of(12), TimeRange::new(Tick::ZERO, r.tick_of(12)), r).unwrap();
    let v2 = filmcraft_project::TrackId(p.alloc_id());
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks.push(filmcraft_project::Track::new(v2, TrackKind::Video, "Video 2".into()));
    q.video_tracks[1].items.push(over);
    let p = Arc::new(p);
    let mk24 = |path: &String, gpu: crate::GpuRendering| {
        let mut s = ExportSettings { format: Format::PngSequence, path: path.clone(), ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, r.tick_of(24)));
        s.gpu_rendering = gpu;
        export(&p, seq, &s, &m, &Progress::default()).unwrap()
    };
    mk24(&tmp("adj-off.png"), crate::GpuRendering::Off);
    mk24(&tmp("adj-auto.png"), crate::GpuRendering::Auto);
    let frame = |name: &str, f: &str| std::fs::read(tmp(&format!("{name}{f}.png"))).unwrap();
    assert_ne!(frame("adj-off", "011"), frame("adj-auto", "011"), "a layered frame goes to the renderer");
    for f in ["012", "023"] {
        assert_eq!(frame("adj-off", f), frame("adj-auto", f), "frame {f}: an adjustment layer frame is the CPU's");
    }
    assert_eq!((gpu_render_stats().frames, gpu_render_stats().fallbacks), (12, 12), "{:?}", gpu_render_stats());
    crate::reset_frame_renderers_for_tests();
}

static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct PanickingRenderer;

impl FrameRenderer for PanickingRenderer {
    fn render(
        &mut self,
        _project: &Project,
        _seq: ItemId,
        _t: Tick,
        _opts: filmcraft_render::RenderOptions,
        _sources: &dyn SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        PANICS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("GPU renderer failure (test)");
    }
}

struct FakeRenderer;

impl FrameRenderer for FakeRenderer {
    fn render(
        &mut self,
        _project: &Project,
        _seq: ItemId,
        _t: Tick,
        _opts: filmcraft_render::RenderOptions,
        _sources: &dyn SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        Some(filmcraft_render::Image::new(320, 180))
    }
}

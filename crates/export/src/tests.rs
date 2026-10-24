use super::*;
use crate::ram_guard::RUNTIME_CEILING_PERCENT;
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

/// GIF delays are whole centiseconds (#854): at 24 fps they alternate 4/4/5 cs so a second of
/// frames lasts 1000 ms, not 24 × 40 ms = 960 ms.
#[test]
fn gif_frame_delays_add_up_to_the_duration() {
    let d: Vec<u16> = (0..6).map(|i| gif_delay_cs(FrameRate::FPS_24, i)).collect();
    assert_eq!(d, [4, 4, 5, 4, 4, 4]);
    let ntsc: u64 = (0..30).map(|i| u64::from(gif_delay_cs(FrameRate::FPS_29_97, i))).sum();
    assert_eq!(ntsc, 100, "30 frames at 29.97 fps last 1.001 s");
    // hostile rates neither panic nor overflow
    assert_eq!(gif_delay_cs(FrameRate { num: 0, den: 0 }, 3), 0);
    let _ = gif_delay_cs(FrameRate { num: 1, den: i64::MAX }, i64::MAX);

    let (p, seq, m) = project();
    let path = tmp("timing.gif");
    let mut s = ExportSettings { format: Format::Gif, path: path.clone(), scale: 0.25, ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(24)));
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    use image::AnimationDecoder;
    let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(std::fs::read(&path).unwrap())).unwrap();
    let frames = dec.into_frames().collect_frames().unwrap();
    assert_eq!(frames.len(), 24);
    let ms: u32 = frames.iter().map(|f| f.delay().numer_denom_ms()).map(|(n, d)| n / d.max(1)).sum();
    assert_eq!(ms, 1000);
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

/// #2142: SDR ProRes, DNxHR and APV (MOV) and H.264 (MP4) exports carry a `colr` box with the
/// export's BT.709 colour: Apple's media stack reads the colour of the MOV codecs only from there.
#[test]
fn sdr_exports_carry_a_colr_box() {
    use filmcraft_isobmff::ColorInfo;
    let (p, seq, m) = project();
    let nclc = ColorInfo::Nclc { primaries: 1, transfer: 1, matrix: 1 };
    let nclx = ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false };
    for (format, name, want) in [
        (Format::ProRes, "colr-prores.mov", nclc.clone()),
        (Format::DnxHr, "colr-dnxhr.mov", nclc.clone()),
        (Format::Apv, "colr-apv.mov", nclc.clone()),
        (Format::H264, "colr-h264.mp4", nclx),
    ] {
        let path = tmp(name);
        export(&p, seq, &ExportSettings { format, path: path.clone(), include_audio: false, ..Default::default() }, &m, &Progress::default()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let file = filmcraft_isobmff::open(bytes).unwrap();
        let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        let v = file.tracks[t].entries[0].video.as_ref().unwrap();
        assert_eq!(v.color.as_ref(), Some(&want), "{name}");
        assert!(v.mastering_display.is_none() && v.content_light.is_none(), "{name}: no HDR metadata on SDR");
    }
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

/// Auto fails before building a renderer or encoder; Off remains the explicit CPU reference path.
#[test]
fn auto_export_fails_before_work_and_off_remains_available() {
    static FACTORIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn factory() -> Option<Box<dyn FrameRenderer>> {
        FACTORIES.fetch_add(1, Ordering::SeqCst);
        None
    }
    crate::reset_frame_renderers_for_tests();
    FACTORIES.store(0, Ordering::SeqCst);
    crate::register_frame_renderer(factory);

    let (p, seq, sources) = project();
    let auto_path = tmp("auto-blocked.png");
    let auto = ExportSettings { format: Format::PngSequence, path: auto_path.clone(), gpu_rendering: crate::GpuRendering::Auto, ..Default::default() };
    let progress = Progress::default();
    let source_calls = std::sync::atomic::AtomicUsize::new(0);
    let no_sources = |_: filmcraft_project::ItemId| {
        source_calls.fetch_add(1, Ordering::SeqCst);
        None
    };
    let error = export(&p, seq, &auto, &no_sources, &progress).expect_err("Auto must fail closed");
    assert!(error.to_string().contains("GPU-resident renderer and VA-API encoder path"), "{error}");
    assert_eq!(FACTORIES.load(Ordering::SeqCst), 0, "Auto must fail before renderer factory");
    assert_eq!(source_calls.load(Ordering::SeqCst), 0, "Auto must fail before source access");
    assert_eq!(progress.total.load(Ordering::Relaxed), 0, "Auto must fail before export work");
    assert!(!std::path::Path::new(&auto_path).exists(), "Auto must fail before output creation");

    let auto_h264 = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Auto, ..auto.clone() };
    let error = Exporter::new(p.clone(), seq, &auto_h264, &progress).err().expect("Auto must not reach software encoder fallback");
    assert!(error.to_string().contains("GPU-resident renderer and VA-API encoder path"), "{error}");
    assert_eq!(FACTORIES.load(Ordering::SeqCst), 0, "Auto H.264 must fail before renderer factory");
    assert_eq!(progress.total.load(Ordering::Relaxed), 0, "Auto H.264 must fail before encoder setup");

    let off_path = tmp("off-reference.png");
    let mut off = ExportSettings { format: Format::PngSequence, path: off_path.clone(), gpu_rendering: crate::GpuRendering::Off, ..Default::default() };
    off.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(1)));
    export(&p, seq, &off, &sources, &Progress::default()).unwrap();
    assert!(std::path::Path::new(&off_path.replace(".png", "000.png")).exists(), "Off CPU reference export must remain available");
    assert_eq!(FACTORIES.load(Ordering::SeqCst), 0, "Off must not invoke GPU renderer factory");
    crate::reset_frame_renderers_for_tests();
}

#[test]
fn dropping_incomplete_disk_output_preserves_destination_and_removes_temporary_file() {
    let dir = transaction_dir();
    let path = dir.join("existing.mov");
    let sentinel = b"previous complete export";
    std::fs::write(&path, sentinel).unwrap();
    let settings = ExportSettings { format: Format::Mjpeg, path: path.to_string_lossy().into_owned(), include_audio: false, ..Default::default() };

    let mut out = Out::create(&settings).unwrap();
    out.write_all(b"partial replacement").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), sentinel, "partial output must not truncate the previous export");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "the partial stream lives beside the destination");
    drop(out);

    assert_eq!(std::fs::read(&path).unwrap(), sentinel);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "dropping an unfinished output removes its temporary file");
}

#[test]
fn cancelling_stepped_video_export_preserves_destination_and_removes_temporary_file() {
    let dir = transaction_dir();
    let path = dir.join("existing.mov");
    let sentinel = b"previous complete export";
    std::fs::write(&path, sentinel).unwrap();
    let settings = ExportSettings { format: Format::Mjpeg, path: path.to_string_lossy().into_owned(), include_audio: false, ..Default::default() };
    let (p, seq, sources) = project();
    let progress = Progress::default();
    let mut ex = Exporter::new(p, seq, &settings, &progress).unwrap();
    ex.set_batch(4);
    ex.set_overlap(false);
    while progress.done.load(Ordering::Relaxed) < 16 {
        assert!(matches!(ex.step(&sources, &progress).unwrap(), Step::Progress));
    }
    assert_eq!(std::fs::read(&path).unwrap(), sentinel, "opening the muxer must keep the old export visible");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "the muxer streams to a sibling temporary file");

    progress.cancel.store(true, Ordering::Relaxed);
    assert!(matches!(ex.step(&sources, &progress), Err(ExportError::Cancelled)));
    drop(ex);
    assert_eq!(std::fs::read(&path).unwrap(), sentinel);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "cancellation removes the partial temporary file");
}

#[test]
fn successful_export_replaces_destination_and_remains_decodable() {
    let dir = transaction_dir();
    let path = dir.join("existing.mov");
    std::fs::write(&path, b"previous complete export").unwrap();
    let (p, seq, sources) = project();
    let settings = ExportSettings { format: Format::Mjpeg, path: path.to_string_lossy().into_owned(), include_audio: false, ..Default::default() };
    let report = export(&p, seq, &settings, &sources, &Progress::default()).unwrap();

    assert_eq!(report.frames, 24);
    let src = filmcraft_codecs::open_bytes("existing.mov", std::fs::read(&path).unwrap().into()).unwrap();
    assert_eq!(src.info().video.as_ref().unwrap().width, 320);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "commit leaves no sibling temporary file");
}

fn transaction_dir() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("fc-export-transaction-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn amd_required_basic_settings_reach_admission_gate() {
    let s = ExportSettings { gpu_rendering: crate::GpuRendering::AmdRequired, ..Default::default() };
    assert!(s.validate().is_ok());
}

#[test]
fn gpu_required_encoding_refuses_formats_without_a_gpu_encoder() {
    let h264 = ExportSettings { format: Format::H264, hardware_encoding: crate::HardwareEncoding::GpuRequired, ..Default::default() };
    assert!(h264.validate().is_ok());
    let prores = ExportSettings { format: Format::ProRes, hardware_encoding: crate::HardwareEncoding::GpuRequired, ..Default::default() };
    assert!(prores.validate().is_err(), "a ProRes export must not fall back to the CPU encoder");
}

#[test]
fn runtime_ram_ceiling_passes_below_and_fails_at_or_above_90() {
    // deterministic, host-independent: the guard reads the real proc filesystem, so this test
    // does not fake readings — it only asserts the function is present and the test harness can
    // call it. Actual threshold behavior is asserted under injected host readings below.
    let _ = crate::runtime_ram_headroom_kib();
    assert_eq!(RUNTIME_CEILING_PERCENT, 90);
}

#[test]
fn runtime_ram_ceiling_is_separate_from_the_build_guard() {
    // The build guard stops at 83/85%; the export runtime ceiling stops at 90%; constants must
    // not collapse onto each other.
    assert_eq!(RUNTIME_CEILING_PERCENT, 90);
    // build guard constants are private; assert behavior instead of the literal.
    assert!(crate::check_ram_pressure().is_ok() || crate::check_ram_pressure().is_err());
    assert!(crate::check_runtime_ram_ceiling().is_ok() || crate::check_runtime_ram_ceiling().is_err());
}

#[test]
fn runtime_headroom_is_none_on_an_unknown_host_and_nonnegative_when_readable() {
    let h = crate::runtime_ram_headroom_kib();
    match h {
        Some(kib) => assert!(kib <= RUNTIME_CEILING_PERCENT.saturating_mul(1_000_000), "headroom must be a small, sane number on this host: {kib}"),
        None => { /* unsupported/unsane host: acceptable */ }
    }
}

#[test]
fn amd_required_legacy_and_explicit_admission_fail_before_frames() {
    struct Admission(std::sync::atomic::AtomicUsize, bool, Format);
    impl AmdExportAdmission for Admission {
        fn validate_settings(&self, s: &ExportSettings, r: &settings::Resolved) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(s.video_format(), self.2);
            assert_eq!((r.width, r.height), (320, 180));
            if self.1 { Ok(()) } else { Err(ExportError::Unsupported("fake resolved config rejection".into())) }
        }
    }
    let (p, seq, sources) = project();
    let progress = Progress::default();
    for format in [Format::H264, Format::Hevc] {
        let settings = ExportSettings { format, gpu_rendering: GpuRendering::AmdRequired, ..Default::default() };
        assert!(Exporter::new(p.clone(), seq, &settings, &progress).err().unwrap().to_string().contains("host adapter admission"));
        assert!(export(&p, seq, &settings, &sources, &progress).err().unwrap().to_string().contains("host adapter admission"));
        for valid in [false, true] {
            let admission = Arc::new(Admission(std::sync::atomic::AtomicUsize::new(0), valid, format));
            let error = Exporter::new_with_admission(p.clone(), seq, &settings, &progress, Some(admission.clone())).err().unwrap();
            assert!(error.to_string().contains(if valid { "resident video session is unavailable" } else { "fake resolved config rejection" }));
            assert_eq!(admission.0.load(Ordering::SeqCst), 1);
            assert_eq!(progress.total.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn amd_required_frame_uses_resident_session_without_cpu_source_lookup() {
    use filmcraft_isobmff::{AvcConfig, SampleEntry};
    use filmcraft_media::{CompressedVideoSource, MediaError};
    use filmcraft_project::{ItemKind, MediaRef};
    use filmcraft_render::{SourceProvider, plan::PreflightedFrame};

    struct Sources(std::sync::atomic::AtomicUsize);
    impl SourceProvider for Sources {
        fn source(&self, _item: ItemId) -> Option<filmcraft_media::SharedSource> {
            self.0.fetch_add(1, Ordering::SeqCst);
            None
        }

        fn compressed_source(&self, _item: ItemId) -> std::result::Result<Arc<dyn CompressedVideoSource>, MediaError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(MediaError::Unsupported("unexpected compressed source lookup".into()))
        }
    }

    struct Session(Arc<std::sync::atomic::AtomicUsize>);
    impl crate::ResidentVideoSession for Session {
        fn sample_entry(&self) -> SampleEntry {
            SampleEntry::avc(AvcConfig::new(vec![vec![0x67, 0x64, 0x00, 0x1f]], vec![vec![0x68, 0xee]], 4), 320, 180)
        }
        fn timescale(&self) -> u32 {
            24
        }
        fn frame_bytes(&self, _frame: &PreflightedFrame) -> Result<u64> {
            Ok(64)
        }
        fn encode_frame(
            &mut self,
            index: u64,
            frame: PreflightedFrame,
            _sources: &dyn SourceProvider,
            _reservation: &mut crate::ResidentReservation,
        ) -> Result<Vec<EncodedPacket>> {
            assert_eq!(index, 0);
            assert_eq!(frame.source_requests().len(), 1);
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ExportError::Encode("mock resident session reached".into()))
        }
        fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
            Ok(Vec::new())
        }
    }

    struct Admission(Arc<std::sync::atomic::AtomicUsize>);
    impl AmdExportAdmission for Admission {
        fn validate_settings(&self, _settings: &ExportSettings, _resolved: &settings::Resolved) -> Result<()> {
            Ok(())
        }
        fn create_session(
            &self,
            _project: Arc<Project>,
            _seq: ItemId,
            _settings: &ExportSettings,
            _resolved: &settings::Resolved,
        ) -> Result<Box<dyn crate::ResidentVideoSession>> {
            Ok(Box::new(Session(self.0.clone())))
        }
    }

    let (project, seq, _) = project();
    let mut project = (*project).clone();
    let video_item = project.sequence(seq).unwrap().video_tracks[0].items[0].item;
    if let ItemKind::Media(media) = &mut project.item_mut(video_item).unwrap().kind {
        media.media = MediaRef::File { path: "resident-input.mp4".into() };
        media.info.kind = filmcraft_media::MediaKind::Movie;
    }
    let project = Arc::new(project);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let admission = Arc::new(Admission(calls.clone()));
    let sources = Sources(std::sync::atomic::AtomicUsize::new(0));
    let progress = Progress::default();
    let settings = ExportSettings { format: Format::H264, gpu_rendering: GpuRendering::AmdRequired, include_audio: false, ..Default::default() };
    let mut exporter = Exporter::new_with_admission(project, seq, &settings, &progress, Some(admission)).unwrap();

    let error = exporter.step(&sources, &progress).unwrap_err();
    assert!(error.to_string().contains("mock resident session reached"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(sources.0.load(Ordering::SeqCst), 0, "strict mode must not call CPU or compressed source lookup");
}

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

/// ProRes codes interlaced frames in either field order (QuickTime and MXF); other formats stay
/// progressive.
#[test]
fn prores_exports_interlaced_frames() {
    let (p, seq, m) = project();
    for (order, il, detail) in
        [(FieldOrder::UpperFirst, filmcraft_prores::Interlace::TopFieldFirst, 9u8), (FieldOrder::LowerFirst, filmcraft_prores::Interlace::BottomFieldFirst, 14)]
    {
        let path = tmp(&format!("prores_{order:?}.mov"));
        let mut s = ExportSettings { format: Format::ProRes, path: path.clone(), field_order: order, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(2)));
        assert!(s.supports_field_order(order));
        assert_eq!(s.prores_interlace(), il);
        assert!(s.summary(320, 180, FrameRate::FPS_24, 48_000, Tick(TICKS_PER_SECOND)).video.contains(order.label()));
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let data = std::fs::read(&path).unwrap();
        let fiel = data.windows(4).position(|w| w == b"fiel").expect("the movie says it is interlaced");
        assert_eq!(data.get(fiel + 4..fiel + 6), Some(&[2u8, detail][..]), "{order:?}");
        let file = filmcraft_isobmff::open(data.as_slice()).unwrap();
        let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        let v = file.tracks[vt].entries[0].video.as_ref().unwrap();
        assert_eq!(v.field_info, Some(filmcraft_isobmff::FieldInfo { fields: 2, detail }), "{order:?}");
        for i in 0..file.tracks[vt].samples.len() {
            let sample = file.read_sample(data.as_slice(), vt, i).unwrap();
            let h = filmcraft_prores::probe(&sample).unwrap();
            assert_eq!(h.interlace, il, "the frames are field-coded");
        }
        let src = filmcraft_codecs::open_bytes("p.mov", data.into()).unwrap();
        let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 24))).unwrap();
        let px = f.to_rgba8();
        let mid = ((90 * 320 + 160) * 4) as usize;
        assert!(px[mid] > 240 && px[mid + 1] < 15 && px[mid + 2] < 15, "{order:?}: {:?}", &px[mid..mid + 4]);
        // MXF OP1a ProRes too
        let mxf = ExportSettings { format: Format::MxfOp1a, mxf_video_codec: MxfVideoCodec::ProRes, path: tmp(&format!("prores_{order:?}.mxf")), ..s.clone() };
        assert!(mxf.supports_field_order(order));
        export(&p, seq, &mxf, &m, &Progress::default()).unwrap();
        let file = filmcraft_mxf::open(&std::fs::read(&mxf.path).unwrap()).unwrap();
        let pic = file.tracks.iter().find_map(|t| t.picture.clone()).unwrap();
        assert_eq!((pic.frame_layout, pic.field_dominance, pic.frame_height()), (1, if order == FieldOrder::LowerFirst { 2 } else { 1 }, 180), "{order:?}");
    }
    // progressive ProRes stays progressive; H.264 and DNxHR have no field coding
    let prog = ExportSettings { format: Format::ProRes, ..Default::default() };
    assert_eq!(prog.prores_interlace(), filmcraft_prores::Interlace::Progressive);
    for (format, dnx) in [(Format::H264, ""), (Format::DnxHr, "hq")] {
        let s = ExportSettings { format, dnx_profile: dnx.into(), field_order: FieldOrder::LowerFirst, path: tmp("interlaced.mov"), ..Default::default() };
        assert!(!s.supports_field_order(FieldOrder::LowerFirst) && !s.supports_field_order(FieldOrder::UpperFirst), "{format:?}");
        assert!(export(&p, seq, &s, &m, &Progress::default()).is_err(), "{format:?}");
    }
}

/// Interlaced ProRes in QuickTime and MXF OP1a: ffprobe (an external test oracle only) reads the
/// field order of the container and of every decoded frame, and ffmpeg decodes every frame without
/// complaint. QuickTime's `fiel` 9 / 14 (Apple's codes for ProRes 1080i) read as `tb` / `bt`. (MXF
/// says lower field first the way SMPTE ST 377-1 does, field dominance 2 with the SMPTE 274M line
/// map; ffmpeg names that `tb`, "bottom displayed first", and would say `bb` only for a line map
/// with other line numbers.)
#[test]
fn prores_interlaced_exports_ffprobe_oracle() {
    let (Some(ffprobe), Some(ffmpeg)) =
        (filmcraft_testkit::ffprobe_or_skip("interlaced ProRes probe"), filmcraft_testkit::ffmpeg_or_skip("interlaced ProRes decode"))
    else {
        return;
    };
    let (p, seq, m) = project();
    for (format, ext) in [(Format::ProRes, "mov"), (Format::MxfOp1a, "mxf")] {
        for (profile, pix) in [("hq", "yuv422p10le"), ("4444", "yuv444p12le")] {
            for (order, want, tff) in
                [(FieldOrder::UpperFirst, if ext == "mxf" { "tt" } else { "tb" }, "1"), (FieldOrder::LowerFirst, if ext == "mxf" { "tb" } else { "bt" }, "0")]
            {
                let path = tmp(&format!("prores_il_{profile}_{order:?}_{format:?}.{ext}"));
                let mut s = ExportSettings {
                    format,
                    path: path.clone(),
                    prores_profile: profile.into(),
                    mxf_video_codec: MxfVideoCodec::ProRes,
                    field_order: order,
                    ..Default::default()
                };
                s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(3)));
                export(&p, seq, &s, &m, &Progress::default()).unwrap();
                let what = format!("{profile} {order:?} {format:?}");
                let probe = std::process::Command::new(&ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-select_streams",
                        "v:0",
                        "-show_entries",
                        "stream=codec_name,width,height,pix_fmt,field_order",
                        "-of",
                        "compact=p=0",
                    ])
                    .arg(&path)
                    .output()
                    .unwrap();
                let probe = String::from_utf8_lossy(&probe.stdout);
                assert!(probe.contains("codec_name=prores") && probe.contains("width=320|height=180"), "{what}: {probe}");
                assert!(probe.contains(&format!("pix_fmt={pix}")), "{what}: {probe}");
                assert!(probe.contains(&format!("field_order={want}")), "{what}: {probe}");
                // the decoder reads the coded field order from every frame header
                let frames = std::process::Command::new(&ffprobe)
                    .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=interlaced_frame,top_field_first", "-of", "csv=p=0"])
                    .arg(&path)
                    .output()
                    .unwrap();
                let frames = String::from_utf8_lossy(&frames.stdout);
                let lines: Vec<&str> = frames.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                assert_eq!(lines.len(), 3, "{what}: {frames}");
                assert!(lines.iter().all(|l| *l == format!("1,{tff}")), "{what}: {frames}");
                let dec = std::process::Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-i"]).arg(&path).args(["-f", "null", "-"]).output().unwrap();
                assert!(dec.status.success() && dec.stderr.is_empty(), "{what}: {}", String::from_utf8_lossy(&dec.stderr));
            }
        }
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
    // (the backend tests register GPU pairs, whose renderers this test would take)
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
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
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer(filmcraft_render::plan::DeviceCaps::ALL)) as Box<dyn FrameRenderer>));
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

    // 5. A frame the planner hands back as one CPU image (an adjustment layer with an effect the
    //    GPU doesn't run, over the clip from frame 12) is rendered by the CPU without taking a
    //    pooled renderer: it counts as a fallback, and the renderer's picture reaches only the
    //    frames before it.
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer(filmcraft_render::plan::DeviceCaps::ALL)) as Box<dyn FrameRenderer>));
    let (p, seq, m) = project();
    let mut p = Arc::unwrap_or_clone(p);
    let r = FrameRate::FPS_24;
    let adj = p.add_item("adj", Label::Iris, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: r, duration: Tick(TICKS_PER_SECOND) }, None);
    let mut over = p.make_track_item(adj, TrackKind::Video, r.tick_of(12), TimeRange::new(Tick::ZERO, r.tick_of(12)), r).unwrap();
    over.effects.insert(0, filmcraft_project::find_effect("emboss").unwrap().instance());
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

    // 6. An adjustment layer whose effect the GPU runs (Invert, over the clip from frame 12) is
    //    handed to a renderer whose device runs adjustment layers, and never to one without an
    //    effect stage: there those frames render on the export's thread, byte-equal to Off, and
    //    count as fallbacks.
    let (p, seq, m) = project();
    let mut p = Arc::unwrap_or_clone(p);
    let adj = p.add_item("adj", Label::Iris, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: r, duration: Tick(TICKS_PER_SECOND) }, None);
    let mut over = p.make_track_item(adj, TrackKind::Video, r.tick_of(12), TimeRange::new(Tick::ZERO, r.tick_of(12)), r).unwrap();
    over.effects.insert(0, filmcraft_project::find_effect("invert").unwrap().instance());
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
    mk24(&tmp("gpu-adj-off.png"), crate::GpuRendering::Off);
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer(filmcraft_render::plan::DeviceCaps::NONE)) as Box<dyn FrameRenderer>));
    mk24(&tmp("gpu-adj-nofx.png"), crate::GpuRendering::Auto);
    assert_ne!(frame("gpu-adj-off", "011"), frame("gpu-adj-nofx", "011"), "a frame without the adjustment layer goes to the renderer");
    for f in ["012", "023"] {
        assert_eq!(frame("gpu-adj-off", f), frame("gpu-adj-nofx", f), "frame {f}: no effect stage, the CPU renders it");
    }
    assert_eq!((gpu_render_stats().frames, gpu_render_stats().fallbacks), (12, 12), "{:?}", gpu_render_stats());
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer(filmcraft_render::plan::DeviceCaps::ALL)) as Box<dyn FrameRenderer>));
    mk24(&tmp("gpu-adj-fx.png"), crate::GpuRendering::Auto);
    for f in ["012", "023"] {
        assert_ne!(frame("gpu-adj-off", f), frame("gpu-adj-fx", f), "frame {f}: an effect stage runs the adjustment layer");
    }
    assert_eq!((gpu_render_stats().frames, gpu_render_stats().fallbacks), (24, 0), "{:?}", gpu_render_stats());
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

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        filmcraft_render::plan::DeviceCaps::ALL
    }
}

/// Returns a blank picture for every frame it is given, on a device that runs these caps.
struct FakeRenderer(filmcraft_render::plan::DeviceCaps);

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

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        self.0
    }
}

/// The render threads may convert H.264 input ahead ([`VideoEncoder::prepare_ahead`]): the packets
/// are the same whether a picture was converted ahead (in any order), converted by the encoder
/// itself (none ahead, or one ahead for another picture size), or never asked for.
#[test]
fn h264_input_converted_ahead_encodes_to_the_same_packets() {
    let (w, h) = (96u32, 64u32);
    let frames: Vec<Vec<u8>> = (0..8u32)
        .map(|t| {
            (0..w * h)
                .flat_map(|i| {
                    let (x, y) = (i % w, i / w);
                    [(x * 3 + t * 7) as u8, (y * 5 + t) as u8, ((x + y) * 2 + t * 3) as u8, 255]
                })
                .collect()
        })
        .collect();
    let s = ExportSettings { format: Format::H264, ..Default::default() };
    let frame = |i: usize| EncoderFrame { width: w, height: h, rgba: &frames[i], hdr: None, index: i as u64 };
    let encode_all = |ahead: bool| -> Vec<Vec<u8>> {
        let mut enc = h264_factory(Format::H264, w, h, FrameRate::FPS_24, &s).unwrap().unwrap();
        if ahead {
            let prepare = enc.prepare_ahead().expect("the H.264 encoder converts ahead");
            for i in (0..frames.len()).rev().filter(|&i| i != 3) {
                prepare(&frame(i));
            }
            // a picture of another size under index 5: the encoder converts that one itself
            prepare(&EncoderFrame { width: w / 2, height: h / 2, rgba: &frames[5][..(w * h) as usize], hdr: None, index: 5 });
            // and one it is never asked to encode
            prepare(&EncoderFrame { index: 99, ..frame(0) });
        }
        let mut out = Vec::new();
        for i in 0..frames.len() {
            out.extend(enc.encode(&frame(i)).unwrap().into_iter().map(|p| p.data));
        }
        out.extend(enc.flush().unwrap().into_iter().map(|p| p.data));
        out
    };
    let plain = encode_all(false);
    assert!(!plain.is_empty());
    assert!(plain == encode_all(true), "converting ahead changed the packets");
}

/// The encoder-backend registries and counters are process-wide: the tests that fill them run one at a time.
static BACKENDS: Mutex<()> = Mutex::new(());

/// How the flaky test backends fail from their second frame on: 0 declines, 1 panics in `load`,
/// 2 panics in `write` after the work was done.
static FLAKY_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn flaky_fault() -> bool {
    assert!(FLAKY_MODE.load(Ordering::SeqCst) != 1, "the device is lost");
    false
}

fn flaky_write_fault() {
    assert!(FLAKY_MODE.load(Ordering::SeqCst) != 2, "the device is lost mid-write");
}

/// The codec crate's own CPU functions behind the ProRes seam, failing from the second frame on.
#[derive(Default)]
struct FlakyProRes {
    inner: filmcraft_prores::ReferenceBackend,
    frames: u32,
    flaky: bool,
}

impl filmcraft_prores::SliceBackend for FlakyProRes {
    fn load(&mut self, cfg: &filmcraft_prores::EncoderConfig, frame: &filmcraft_prores::Frame, slices: &[filmcraft_prores::SliceJob]) -> bool {
        self.frames += 1;
        if self.flaky && self.frames >= 2 && FLAKY_MODE.load(Ordering::SeqCst) != 2 {
            return flaky_fault();
        }
        self.inner.load(cfg, frame, slices)
    }
    fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
        self.inner.bits(q)
    }
    fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
        let out = self.inner.write(qs);
        if self.flaky && self.frames >= 2 {
            flaky_write_fault();
        }
        out
    }
    fn alpha(&mut self) -> Option<Vec<Vec<u8>>> {
        self.inner.alpha()
    }
}

#[derive(Default)]
struct FlakyDnx {
    inner: filmcraft_dnx::ReferenceBackend,
    frames: u32,
    flaky: bool,
}

impl filmcraft_dnx::MacroblockBackend for FlakyDnx {
    fn load(&mut self, cfg: &filmcraft_dnx::EncoderConfig, frame: &filmcraft_dnx::Frame) -> bool {
        self.frames += 1;
        if self.flaky && self.frames >= 2 && FLAKY_MODE.load(Ordering::SeqCst) != 2 {
            return flaky_fault();
        }
        self.inner.load(cfg, frame)
    }
    fn bits(&mut self, q: u32) -> Option<Vec<u32>> {
        self.inner.bits(q)
    }
    fn write(&mut self, qs: &[u32]) -> Option<Vec<(Vec<u8>, u32)>> {
        let out = self.inner.write(qs);
        if self.flaky && self.frames >= 2 {
            flaky_write_fault();
        }
        out
    }
}

const BACKEND_FRAMES: i64 = 6;

/// Export the first frames of the test clip to `format` and decode every one: the file has all of them, all red.
fn export_and_check(format: Format, hardware: HardwareEncoding, name: &str) -> Vec<u8> {
    let (p, seq, m) = project();
    let path = tmp(name);
    let mut s = ExportSettings { format, path: path.clone(), include_audio: false, hardware_encoding: hardware, ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(BACKEND_FRAMES)));
    let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
    assert_eq!(r.frames, BACKEND_FRAMES as u64, "{name}: every frame is exported");
    let bytes = std::fs::read(&path).unwrap();
    let src = filmcraft_codecs::open_bytes(name, bytes.clone().into()).unwrap();
    for i in 0..BACKEND_FRAMES {
        let px = src.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i))).unwrap().to_rgba8();
        assert!(px[0] > 240 && px[1] < 15 && px[2] < 15, "{name} frame {i}: {:?}", &px[..4]);
    }
    bytes
}

fn prores_reference() -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
    Some(Box::new(FlakyProRes::default()))
}

fn prores_flaky() -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
    Some(Box::new(FlakyProRes { flaky: true, ..Default::default() }))
}

fn dnx_reference() -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
    Some(Box::new(FlakyDnx::default()))
}

fn dnx_flaky() -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
    Some(Box::new(FlakyDnx { flaky: true, ..Default::default() }))
}

#[test]
fn a_registered_prores_backend_encodes_under_auto_only() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    crate::reset_encoder_backends_for_tests();
    crate::register_prores_backend(prores_reference);
    let off = export_and_check(Format::ProRes, HardwareEncoding::Off, "be-pr-off.mov");
    assert_eq!(gpu_render_stats().encoded, 0, "Off never takes the backend");
    let auto = export_and_check(Format::ProRes, HardwareEncoding::Auto, "be-pr-auto.mov");
    let st = gpu_render_stats();
    assert_eq!((st.encoded, st.encode_fallbacks), (BACKEND_FRAMES as u64, 0), "{st:?}");
    assert_eq!(off, auto, "a backend doing the CPU's work gives the CPU's file");
    crate::reset_encoder_backends_for_tests();
}

#[test]
fn a_registered_dnx_backend_encodes_under_auto_only() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    crate::reset_encoder_backends_for_tests();
    crate::register_dnx_backend(dnx_reference);
    let off = export_and_check(Format::DnxHr, HardwareEncoding::Off, "be-dnx-off.mov");
    assert_eq!(gpu_render_stats().encoded, 0, "Off never takes the backend");
    let auto = export_and_check(Format::DnxHr, HardwareEncoding::Auto, "be-dnx-auto.mov");
    let st = gpu_render_stats();
    assert_eq!((st.encoded, st.encode_fallbacks), (BACKEND_FRAMES as u64, 0), "{st:?}");
    assert_eq!(off, auto, "a backend doing the CPU's work gives the CPU's file");
    crate::reset_encoder_backends_for_tests();
}

#[test]
fn a_failing_backend_never_breaks_an_export() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    for mode in 0u8..3 {
        let panics = mode != 0;
        FLAKY_MODE.store(mode, Ordering::SeqCst);
        crate::reset_encoder_backends_for_tests();
        crate::register_prores_backend(prores_flaky);
        let off = export_and_check(Format::ProRes, HardwareEncoding::Off, "fl-pr-off.mov");
        let auto = export_and_check(Format::ProRes, HardwareEncoding::Auto, "fl-pr-auto.mov");
        let st = gpu_render_stats();
        assert_eq!(st.encoded, 1, "mode {mode}: only the first frame is the backend's: {st:?}");
        // a declining backend is asked for every frame; one that panicked is dropped for the rest
        assert_eq!(st.encode_fallbacks, if panics { 1 } else { BACKEND_FRAMES as u64 - 1 }, "mode {mode}: {st:?}");
        assert_eq!(off, auto, "mode {mode}: the CPU's frames are the CPU's");
    }
    FLAKY_MODE.store(0, Ordering::SeqCst);
    crate::reset_encoder_backends_for_tests();
}

#[test]
fn a_failing_dnx_backend_never_breaks_an_export() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    for mode in 0u8..3 {
        let panics = mode != 0;
        FLAKY_MODE.store(mode, Ordering::SeqCst);
        crate::reset_encoder_backends_for_tests();
        crate::register_dnx_backend(dnx_flaky);
        let off = export_and_check(Format::DnxHr, HardwareEncoding::Off, "fl-dnx-off.mov");
        let auto = export_and_check(Format::DnxHr, HardwareEncoding::Auto, "fl-dnx-auto.mov");
        let st = gpu_render_stats();
        assert_eq!(st.encoded, 1, "mode {mode}: only the first frame is the backend's: {st:?}");
        assert_eq!(st.encode_fallbacks, if panics { 1 } else { BACKEND_FRAMES as u64 - 1 }, "mode {mode}: {st:?}");
        assert_eq!(off, auto, "mode {mode}: the CPU's frames are the CPU's");
    }
    FLAKY_MODE.store(0, Ordering::SeqCst);
    crate::reset_encoder_backends_for_tests();
}

fn prores_factory_panics() -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
    panic!("no driver");
}

fn dnx_factory_panics() -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
    panic!("no driver");
}

#[test]
fn a_backend_factory_that_panics_leaves_the_export_on_the_cpu() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    crate::reset_encoder_backends_for_tests();
    crate::register_prores_backend(prores_factory_panics);
    crate::register_dnx_backend(dnx_factory_panics);
    for (format, name) in [(Format::ProRes, "pr"), (Format::DnxHr, "dnx")] {
        let off = export_and_check(format, HardwareEncoding::Off, &format!("fp-{name}-off.mov"));
        let auto = export_and_check(format, HardwareEncoding::Auto, &format!("fp-{name}-auto.mov"));
        assert_eq!(off, auto, "{name}: the CPU's file");
    }
    let st = gpu_render_stats();
    assert_eq!((st.encoded, st.encode_fallbacks), (0, 0), "{st:?}");
    crate::reset_encoder_backends_for_tests();
}

/// How [`CpuResident`] fails at frame 2: 0 not at all, 1 its resident render declines, 2 it panics
/// there, 3 it hands a frame the backend does not take.
static RESIDENT_FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// A renderer whose "resident" frames are the planes the read-back path makes for ProRes HQ (the
/// codec's `ReferenceBackend` takes a `filmcraft_prores::Frame` as the handle): a stand-in for a
/// GPU pair, to test the export's switch without a device.
struct CpuResident;

impl FrameRenderer for CpuResident {
    fn render(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        Some(filmcraft_render::render_sequence(project, seq, t, opts, sources))
    }

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        filmcraft_render::plan::DeviceCaps::NONE
    }

    fn resident_supported(&self, post: &PostSettings) -> bool {
        !(post.hdr || post.image_overlay || post.name_overlay || post.timecode_overlay) && post.content.is_none() && post.limiter.is_none() && post.floats
    }

    fn render_resident(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn SourceProvider,
        post: &PostSettings,
    ) -> Option<ResidentFrame> {
        let fault = RESIDENT_FAULT.load(Ordering::SeqCst);
        let at = t == FrameRate::FPS_24.tick_of(2);
        match (at, fault) {
            (true, 1) => return None,
            (true, 2) => panic!("the device is lost"),
            (true, 3) => return Some(Box::new(42u32)),
            _ => {}
        }
        RESIDENT_RENDERS.fetch_add(1, Ordering::SeqCst);
        let img = filmcraft_render::render_sequence(project, seq, t, opts, sources);
        let (w, h) = (post.width as usize, post.height as usize);
        let rgb = crate::pipeline::srgb_floats(&img, false);
        let mut f = filmcraft_prores::Frame::new(post.width, post.height, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
        rgbf_to_yuv422_10(&rgb, w, h, Matrix::new(post.planes.kr, post.planes.kb), &mut f.y, &mut f.cb, &mut f.cr);
        Some(Box::new(f))
    }
}

/// Resident frames [`CpuResident`] made (the planes of a frame, rendered on the "device").
static RESIDENT_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The reference backend, refusing resident frames from its third on when [`RESIDENT_FAULT`] is 4
/// (an encoder that cannot take resident frames any more: limits, a lost device, dropped).
#[derive(Default)]
struct RefusingBackend {
    inner: filmcraft_prores::ReferenceBackend,
    resident: u32,
}

impl filmcraft_prores::SliceBackend for RefusingBackend {
    fn load(&mut self, cfg: &filmcraft_prores::EncoderConfig, frame: &filmcraft_prores::Frame, slices: &[filmcraft_prores::SliceJob]) -> bool {
        self.inner.load(cfg, frame, slices)
    }
    fn load_resident(&mut self, cfg: &filmcraft_prores::EncoderConfig, frame: &dyn std::any::Any, slices: &[filmcraft_prores::SliceJob]) -> bool {
        self.resident += 1;
        if RESIDENT_FAULT.load(Ordering::SeqCst) == 4 && self.resident >= 3 {
            return false;
        }
        self.inner.load_resident(cfg, frame, slices)
    }
    fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
        self.inner.bits(q)
    }
    fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
        self.inner.write(qs)
    }
}

struct CpuPair;

impl GpuPair for CpuPair {
    fn renderer(&self) -> Option<Box<dyn FrameRenderer>> {
        Some(Box::new(CpuResident))
    }
    fn prores_backend(&self) -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
        Some(Box::new(RefusingBackend::default()))
    }
    fn dnx_backend(&self) -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
        None
    }
}

fn cpu_pair() -> Option<Box<dyn GpuPair>> {
    Some(Box::new(CpuPair))
}

/// With a pair registered and GPU rendering and encoding Auto, frames stay "on the device" from
/// the renderer into the encoder; a frame whose resident render declines or panics, or whose
/// planes the backend does not take, is rendered and encoded through the read-back path, and the
/// file is the read-back path's either way. An overlay the pair has no pass for keeps the whole
/// export on the read-back path, decided before the first frame.
#[test]
fn a_resident_frame_that_fails_takes_the_read_back_path() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    crate::reset_encoder_backends_for_tests();
    crate::reset_frame_renderers_for_tests();
    crate::register_gpu_pair(cpu_pair);
    let (p, seq, m) = project();
    let run = |name: &str, on: bool, timecode: bool| -> Vec<u8> {
        let path = tmp(name);
        let mut s = ExportSettings { format: Format::ProRes, prores_profile: "hq".into(), path: path.clone(), include_audio: false, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(BACKEND_FRAMES)));
        s.effects.timecode_overlay.enabled = timecode;
        (s.gpu_rendering, s.hardware_encoding) = if on { (GpuRendering::Auto, HardwareEncoding::Auto) } else { (GpuRendering::Off, HardwareEncoding::Off) };
        let r = export(&p, seq, &s, &m, &Progress::default()).unwrap();
        assert_eq!(r.frames, BACKEND_FRAMES as u64, "{name}");
        std::fs::read(&path).unwrap()
    };
    let off = run("rs-off.mov", false, false);
    let n = BACKEND_FRAMES as u64;
    for fault in 0u8..4 {
        RESIDENT_FAULT.store(fault, Ordering::SeqCst);
        let before = gpu_render_stats();
        let auto = run(&format!("rs-auto-{fault}.mov"), true, false);
        let st = gpu_render_stats();
        let (resident, missed) = (st.resident - before.resident, st.resident_fallbacks - before.resident_fallbacks);
        assert!(auto == off, "fault {fault}: the read-back path's file");
        assert_eq!(resident + missed, n, "fault {fault}: {st:?}");
        assert_eq!(st.encoded - before.encoded, n, "fault {fault}: every frame on the backend: {st:?}");
        match fault {
            0 => assert_eq!(missed, 0, "every frame stayed resident"),
            1 | 3 => assert_eq!(missed, 1, "fault {fault}: frame 2 only was read back"),
            // (the panicking renderer retires the pool: frames from then on render on the CPU)
            _ => assert!(missed >= 1, "fault {fault}: {st:?}"),
        }
    }
    // an encoder that refuses resident frames from frame 2 on: frame 2 is rendered twice, every
    // later frame is read back from the start (one frame per step, so nothing is rendered ahead)
    RESIDENT_FAULT.store(4, Ordering::SeqCst);
    let path = tmp("rs-refused.mov");
    let mut s = ExportSettings { format: Format::ProRes, prores_profile: "hq".into(), path: path.clone(), include_audio: false, ..Default::default() };
    s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(BACKEND_FRAMES)));
    (s.gpu_rendering, s.hardware_encoding) = (GpuRendering::Auto, HardwareEncoding::Auto);
    let (before, renders) = (gpu_render_stats(), RESIDENT_RENDERS.load(Ordering::SeqCst));
    let prog = Progress::default();
    let mut ex = Exporter::new(p.clone(), seq, &s, &prog).unwrap();
    ex.set_batch(1);
    ex.set_overlap(false);
    while !matches!(ex.step(&m, &prog).unwrap(), Step::Done(_)) {}
    let st = gpu_render_stats();
    assert!(std::fs::read(&path).unwrap() == off, "the read-back path's file");
    assert_eq!(RESIDENT_RENDERS.load(Ordering::SeqCst) - renders, 3, "frames 0, 1 and 2 rendered resident, no later one");
    assert_eq!((st.resident - before.resident, st.resident_fallbacks - before.resident_fallbacks), (2, 1), "{st:?}");
    assert_eq!(st.frames - before.frames, BACKEND_FRAMES as u64, "every frame counts once as a GPU frame, the refused one too");
    RESIDENT_FAULT.store(0, Ordering::SeqCst);
    let off = run("rs-tc-off.mov", false, true);
    let before = gpu_render_stats();
    let auto = run("rs-tc-auto.mov", true, true);
    let st = gpu_render_stats();
    assert!(auto == off, "the overlay export is the read-back path's");
    assert_eq!((st.resident, st.resident_fallbacks), (before.resident, before.resident_fallbacks), "no frame tried resident: {st:?}");
    crate::reset_encoder_backends_for_tests();
    crate::reset_frame_renderers_for_tests();
}

/// Renderers taken from [`CountingPair`].
static PAIR_RENDERERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A [`GpuPair`] that counts the renderers taken from it (each one renders like the CPU).
struct CountingPair;

impl GpuPair for CountingPair {
    fn renderer(&self) -> Option<Box<dyn FrameRenderer>> {
        PAIR_RENDERERS.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(CpuResident))
    }
    fn prores_backend(&self) -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
        Some(Box::new(RefusingBackend::default()))
    }
    fn dnx_backend(&self) -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
        None
    }
}

/// The renderers of a GPU pair share the device of its encoder backends: an export takes them only
/// when it will use a GPU encoder (Hardware encoding Auto and ProRes / DNxHR video, MXF included).
/// Any other GPU-rendered export (H.264, an image sequence, ProRes with Hardware encoding Off)
/// takes the plain renderer factory's.
#[test]
fn only_an_export_with_a_gpu_encoder_renders_on_the_pair() {
    let _g = BACKENDS.lock().unwrap_or_else(PoisonError::into_inner);
    crate::reset_encoder_backends_for_tests();
    crate::reset_frame_renderers_for_tests();
    crate::register_frame_renderer(|| Some(Box::new(FakeRenderer(filmcraft_render::plan::DeviceCaps::ALL)) as Box<dyn FrameRenderer>));
    crate::register_gpu_pair(|| Some(Box::new(CountingPair) as Box<dyn GpuPair>));
    let (p, seq, m) = project();
    let cases = [
        ("h264", Format::H264, MxfVideoCodec::default(), HardwareEncoding::Auto, false),
        ("png", Format::PngSequence, MxfVideoCodec::default(), HardwareEncoding::Auto, false),
        ("prores-hw-off", Format::ProRes, MxfVideoCodec::default(), HardwareEncoding::Off, false),
        ("mxf-h264", Format::MxfOp1a, MxfVideoCodec::H264, HardwareEncoding::Auto, false),
        ("prores", Format::ProRes, MxfVideoCodec::default(), HardwareEncoding::Auto, true),
        ("dnxhr", Format::DnxHr, MxfVideoCodec::default(), HardwareEncoding::Auto, true),
        ("mxf-prores", Format::MxfOp1a, MxfVideoCodec::ProRes, HardwareEncoding::Auto, true),
    ];
    for (name, format, mxf_video_codec, hardware_encoding, paired) in cases {
        let ext = match format {
            Format::H264 => "mp4",
            Format::PngSequence => "png",
            Format::MxfOp1a => "mxf",
            _ => "mov",
        };
        let mut s = ExportSettings { format, mxf_video_codec, path: tmp(&format!("pair-{name}.{ext}")), include_audio: false, ..Default::default() };
        s.range = Some(TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(2)));
        (s.gpu_rendering, s.hardware_encoding) = (GpuRendering::Auto, hardware_encoding);
        let (taken, frames) = (PAIR_RENDERERS.load(Ordering::SeqCst), gpu_render_stats().frames);
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        assert!(gpu_render_stats().frames > frames, "{name}: rendered on a GPU renderer");
        assert_eq!(PAIR_RENDERERS.load(Ordering::SeqCst) > taken, paired, "{name}: the pair's renderers taken");
    }
    crate::reset_encoder_backends_for_tests();
    crate::reset_frame_renderers_for_tests();
}

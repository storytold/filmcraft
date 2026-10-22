//! Real VideoToolbox profile coverage. Media is generated in process, never committed.
#![cfg(target_os = "macos")]

use filmcraft_export::{ColorSignal, EncoderFrame, ExportSettings, Format, HardwareEncoding};
use filmcraft_platform::hardware_encode::videotoolbox_encoder_factory;
use filmcraft_time::FrameRate;

struct TestLogger;
impl log::Log for TestLogger {
    /// Capture all native profile diagnostics emitted during these integration tests.
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    /// Print a native diagnostic so a failing profile case retains its underlying cause.
    fn log(&self, r: &log::Record) {
        eprintln!("{}", r.args());
    }
    /// No buffering is used by this stderr-backed test logger.
    fn flush(&self) {}
}
/// Exercise supported profiles, SDR/HDR signals and alpha combinations through native encode/decode and external oracles.
#[test]
fn profile_round_trips() {
    let _ = log::set_logger(&TestLogger);
    log::set_max_level(log::LevelFilter::Info);
    let (w, h) = (640u32, 360u32);
    let cases = [
        (Format::ProRes, "proxy", false),
        (Format::ProRes, "lt", false),
        (Format::ProRes, "standard", false),
        (Format::ProRes, "hq", false),
        (Format::ProRes, "4444", false),
        (Format::ProRes, "4444xq", false),
        (Format::ProRes, "4444", true),
        (Format::ProRes, "4444xq", true),
        (Format::Hevc, "main", false),
        (Format::Hevc, "main10", false),
        (Format::Hevc, "main42210", false),
        (Format::Hevc, "main", true),
        (Format::Hevc, "main10", true),
        (Format::Hevc, "main42210", true),
        (Format::Mjpeg, "", false),
    ];
    for (format, profile, alpha) in cases {
        for signal in [ColorSignal::default(), ColorSignal::PQ, ColorSignal::HLG] {
            if format == Format::Mjpeg && signal.is_hdr() || profile == "main" && signal.is_hdr() {
                continue;
            }
            let settings = ExportSettings {
                format,
                prores_profile: profile.into(),
                hevc_profile: profile.into(),
                alpha,
                signal,
                hardware_encoding: HardwareEncoding::Auto,
                bitrate_kbps: 20_000,
                ..Default::default()
            };
            let Some(result) = videotoolbox_encoder_factory(format, w, h, FrameRate::FPS_30, &settings) else {
                if format == Format::Hevc && alpha && (profile != "main" || signal.is_hdr()) {
                    continue;
                }
                assert!(std::env::var_os("FILMCRAFT_REQUIRE_VT").is_none(), "required hardware profile declined: {format:?} {profile}");
                eprintln!("SKIPPED {format:?} {profile} {signal:?}");
                continue;
            };
            if format == Format::Hevc && alpha && (profile != "main" || signal.is_hdr()) {
                assert!(result.is_err());
                continue;
            }
            let Ok(mut encoder) = result else {
                assert!(std::env::var_os("FILMCRAFT_REQUIRE_VT").is_none(), "required hardware profile declined: {format:?} {profile}");
                eprintln!("SKIPPED {format:?} {profile} {signal:?}");
                continue;
            };
            let mut packets = vec![];
            for index in 0..6 {
                let rgba: Vec<u8> = (0..w * h).flat_map(|i| [((i % w) * 255 / w) as u8, ((i / w) * 255 / h) as u8, 100, ((i % w) * 255 / w) as u8]).collect();
                let rgb =
                    signal.is_hdr().then(|| rgba.as_chunks::<4>().0.iter().flat_map(|p| p[..3].iter().map(|&v| f32::from(v) / 255.0)).collect::<Vec<_>>());
                packets.extend(encoder.encode(&EncoderFrame { width: w, height: h, rgba: &rgba, hdr: rgb.as_deref(), index }).unwrap());
            }
            packets.extend(encoder.flush().unwrap());
            assert_eq!(packets.len(), 6, "{format:?} {profile}");
            let entry = encoder.sample_entry();
            independent_oracle(format, profile, alpha, signal, &entry, &packets, encoder.timescale());
            if format == Format::Hevc {
                let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(&entry).unwrap().unwrap();
                assert_eq!(info.bit_depth_luma, if profile == "main" { 8 } else { 10 }, "{profile} alpha={alpha}");
                assert_eq!(info.chroma_format_idc, if profile == "main42210" { 2 } else { 1 }, "{profile} alpha={alpha}");
            }
            if format == Format::Hevc && alpha {
                use filmcraft_codecs::VideoDecoder;
                let filmcraft_isobmff::CodecConfig::Hevc(cfg) = &entry.codec else { panic!("HEVC config") };
                let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(&entry).unwrap().unwrap();
                let mut vt = filmcraft_platform::videotoolbox::VtDecoder::new_hevc(info.clone(), cfg).unwrap();
                assert!(vt.requires_hardware());
                vt.fail_after(0);
                let mut hybrid = filmcraft_platform::HybridDecoder::new(Box::new(vt), entry.clone(), info);
                hybrid.require_hardware();
                let error = hybrid.decode(&packets[0].data, 0).err().expect("hardware failure");
                assert!(error.to_string().contains("cannot preserve alpha"));
                // Without a hardware session the file still opens (ADR 0001), showing opaque.
                let mut sw = filmcraft_codecs::software_video_decoder(&entry).expect("HEVC alpha opens in software");
                let mut opaque = vec![];
                for (i, p) in packets.iter().enumerate() {
                    opaque.extend(sw.decode(&p.data, i as i64).unwrap());
                }
                opaque.extend(sw.flush());
                assert_eq!(opaque.len(), 6, "{profile}: software frames");
            }
            let Some(decoder) = filmcraft_platform::videotoolbox_factory(&entry) else {
                panic!("no decoder: {entry:?}");
            };
            let mut decoder = decoder.unwrap();
            let mut frames = vec![];
            for (i, p) in packets.iter().enumerate() {
                frames.extend(decoder.decode(&p.data, i as i64).unwrap());
            }
            frames.extend(decoder.flush());
            assert_eq!(frames.len(), 6, "{format:?} {profile}");
            assert!(decoder.name().starts_with("VideoToolbox"), "unexpected fallback: {}", decoder.name());
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(f.pts, i as i64);
                assert_eq!((f.frame.width, f.frame.height), (w, h));
                if i == 0 {
                    // A valid header alone would also accept a broken colour conversion (for
                    // example all-black planes). Check decoded signal codes independently.
                    let planar = f.frame.to_planar();
                    if let filmcraft_frame::PixelData::Yuv16 { planes, bits, chroma, .. } = &planar.data {
                        let (kr, kb) = signal.kr_kb();
                        let (r, g, b) = (127.0 / 255.0, 127.0 / 255.0, 100.0 / 255.0);
                        let yy = kr * r + (1.0 - kr - kb) * g + kb * b;
                        let expected = [64.0 + 876.0 * yy, 512.0 + 896.0 * (b - yy) / (2.0 * (1.0 - kb)), 512.0 + 896.0 * (r - yy) / (2.0 * (1.0 - kr))];
                        let (sx, sy) = chroma.shifts();
                        let y_at = (h / 2 * w + w / 2) as usize;
                        let c_at = (((h / 2) >> sy) * w.div_ceil(1 << sx) + ((w / 2) >> sx)) as usize;
                        let scale = (1u32 << (bits - 10)) as f32;
                        for (actual, expected) in [planes[0][y_at], planes[1][c_at], planes[2][c_at]].into_iter().zip(expected) {
                            assert!(
                                (f32::from(actual) / scale - expected).abs() < 16.0,
                                "{format:?} {profile} {signal:?}: signal code {actual}, expected {expected} at 10-bit"
                            );
                        }
                    } else {
                        let rgba = planar.to_rgba8();
                        let at = ((h / 2 * w + w / 2) * 4) as usize;
                        for (&actual, expected) in rgba[at..at + 3].iter().zip([127i16, 127, 100]) {
                            assert!((i16::from(actual) - expected).abs() < 16, "{format:?} {profile}: RGB centre {:?}", &rgba[at..at + 4]);
                        }
                    }
                }
                match &f.frame.data {
                    filmcraft_frame::PixelData::Yuv16 { bits, alpha: a, .. } => {
                        assert!(*bits >= 10);
                        if alpha {
                            assert!(a.is_some());
                        } else if let Some(a) = a {
                            assert!(a.iter().all(|&v| u32::from(v) == (1u32 << bits) - 1), "alpha-off must be fully opaque");
                        }
                        if alpha {
                            let a = a.as_ref().unwrap();
                            assert!(*a.iter().min().unwrap() < 50);
                            assert!(*a.iter().max().unwrap() > 900);
                        }
                    }
                    filmcraft_frame::PixelData::Biplanar16 { bits, .. } => {
                        assert!(*bits >= 10);
                        assert!(!alpha);
                    }
                    filmcraft_frame::PixelData::Rgba8(p) if alpha => {
                        assert!(p.as_chunks::<4>().0.iter().map(|p| p[3]).min().unwrap() < 5);
                        assert!(p.as_chunks::<4>().0.iter().map(|p| p[3]).max().unwrap() > 245);
                        let centre = &p[((h / 2 * w + w / 2) * 4) as usize..][..4];
                        for (&a, b) in centre.iter().zip([127i16, 127, 100, 127]) {
                            assert!((i16::from(a) - b).abs() < 8, "straight alpha centre: {centre:?}");
                        }
                    }
                    _ => assert!(!alpha),
                }
                if signal.is_hdr() {
                    assert!(matches!(f.frame.color.transfer, filmcraft_color::Transfer::Pq | filmcraft_color::Transfer::Hlg), "{profile}: {:?}", f.frame.color);
                }
            }
            eprintln!("PASS {format:?} {profile} {signal:?}: {} frames through {}", frames.len(), decoder.name());
        }
    }
}

/// Point at a local, uncommitted ProRes MOV; ordinary CI never needs private media.
#[test]
#[ignore = "requires FILMCRAFT_TEST_PRORES pointing at local media"]
fn local_prores_decode_and_seek() {
    use filmcraft_codecs::VideoDecoder;
    use filmcraft_frame::PixelData;
    let path = std::env::var("FILMCRAFT_TEST_PRORES").expect("local MOV path");
    let bytes = std::fs::read(path).unwrap();
    let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
    let track = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
    let t = &file.tracks[track];
    let entry = &t.entries[0];
    let mut hw = filmcraft_platform::videotoolbox::VtDecoder::new_intra(entry.clone()).unwrap();
    assert!(hw.uses_hardware(), "VideoToolbox must confirm an active hardware decoder");
    let mut sw = filmcraft_codecs::software_video_decoder(entry).unwrap();
    let start = std::time::Instant::now();
    let mut references = std::collections::BTreeMap::new();
    let mut frames = 0;
    let mut worst = 100f64;
    let mut max_difference = 0;
    let mut compare = |f: filmcraft_codecs::DecodedFrame, refs: &mut std::collections::BTreeMap<i64, filmcraft_codecs::DecodedFrame>| {
        let expected = refs.remove(&f.pts).expect("matching pts");
        assert_eq!((f.frame.width, f.frame.height, f.frame.par), (expected.frame.width, expected.frame.height, expected.frame.par));
        let (fa, fb) = (f.frame.to_planar(), expected.frame.to_planar());
        let (PixelData::Yuv16 { planes: p, bits: bits_a, chroma: ca, .. }, PixelData::Yuv16 { planes: q, bits: bits_b, chroma: cb, .. }) = (&fa.data, &fb.data)
        else {
            panic!("expected YUV16");
        };
        assert_eq!((bits_a, ca), (bits_b, cb));
        for (a, b) in p.iter().zip(q) {
            assert_eq!(a.len(), b.len());
            let mut squared = 0u64;
            for (&x, &y) in a.iter().zip(b.iter()) {
                let d = x.abs_diff(y);
                max_difference = max_difference.max(d);
                squared += u64::from(d) * u64::from(d);
            }
            let mse = squared as f64 / a.len() as f64;
            let peak = ((1u32 << *bits_a) - 1) as f64;
            if mse > 0.0 {
                worst = worst.min(10.0 * (peak * peak / mse).log10());
            }
        }
        frames += 1;
    };
    for (i, sample) in t.samples.iter().enumerate() {
        let data = file.read_sample(&bytes, track, i).unwrap();
        for f in sw.decode(&data, sample.pts).unwrap() {
            references.insert(f.pts, f);
        }
        for f in hw.decode(&data, sample.pts).unwrap() {
            compare(f, &mut references);
        }
    }
    for f in hw.flush() {
        compare(f, &mut references);
    }
    assert!(references.is_empty());
    assert_eq!(frames, t.samples.len());
    assert!(worst > 55.0, "HW/SW luma/chroma PSNR {worst:.2}");
    eprintln!(
        "LOCAL {}x{} ProRes: {frames} frames, comparison {:.2}s, worst PSNR {worst:.2} dB, max code error {max_difference}",
        entry.video.as_ref().unwrap().width,
        entry.video.as_ref().unwrap().height,
        start.elapsed().as_secs_f64()
    );
    // Every ProRes sample is independently decodable, including after a seek.
    for i in [0, t.samples.len() / 2, t.samples.len() - 1] {
        hw.reset();
        let s = &t.samples[i];
        let data = file.read_sample(&bytes, track, i).unwrap();
        let mut out = hw.decode(&data, s.pts).unwrap();
        out.extend(hw.flush());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pts, s.pts);
    }
    let start = std::time::Instant::now();
    let mut count = 0;
    hw.reset();
    for (i, s) in t.samples.iter().enumerate() {
        for f in hw.decode(&file.read_sample(&bytes, track, i).unwrap(), s.pts).unwrap() {
            std::hint::black_box(f.frame.to_planar());
            count += 1;
        }
    }
    for f in hw.flush() {
        std::hint::black_box(f.frame.to_planar());
        count += 1;
    }
    assert_eq!(count, t.samples.len());
    eprintln!("LOCAL hardware-only decoding (including sample copies and CPU readback): {:.1} fps", count as f64 / start.elapsed().as_secs_f64());
}

/// Mux native packets and verify independent decoding, frame counts, profile and color metadata with FFmpeg/ffprobe.
#[allow(clippy::too_many_arguments)]
fn independent_oracle(
    format: Format,
    profile: &str,
    alpha: bool,
    signal: ColorSignal,
    entry: &filmcraft_isobmff::SampleEntry,
    packets: &[filmcraft_export::EncodedPacket],
    timescale: u32,
) {
    use filmcraft_isobmff::{Brand, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
    let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("VT profiles") else { return };
    let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("VT profiles") else { return };
    let path = filmcraft_testkit::fixtures_dir("platform").join(format!("vt-{format:?}-{profile}-alpha{alpha}-tf{}.mov", signal.transfer));
    let mut mux = Mp4Writer::new(std::fs::File::create(&path).unwrap(), WriterOptions::new(Brand::Mov)).unwrap();
    let track = mux.add_track(TrackConfig::new(entry.clone(), timescale)).unwrap();
    for p in packets {
        mux.write_sample(track, WriteSample { data: &p.data, duration: p.duration, composition_offset: p.composition_offset, is_sync: p.key }).unwrap();
    }
    mux.finish().unwrap();
    let decoded = std::process::Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i"]).arg(&path).args(["-f", "null", "-"]).output().unwrap();
    assert!(decoded.status.success(), "oracle {format:?} {profile}: {}", String::from_utf8_lossy(&decoded.stderr));
    let probe = std::process::Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_frames",
            "-show_entries",
            "stream=codec_name,profile,codec_tag_string,pix_fmt,color_primaries,color_transfer,color_space,nb_read_frames:frame=side_data_list",
            "-read_intervals",
            "%+#6",
            "-of",
            "default=nw=1",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let info = String::from_utf8_lossy(&probe.stdout);
    assert!(info.contains("nb_read_frames=6"), "{info}");
    if format == Format::Hevc {
        assert!(
            info.contains(if profile == "main" {
                "pix_fmt=yuv420p"
            } else if profile == "main42210" {
                "pix_fmt=yuv422p10le"
            } else {
                "pix_fmt=yuv420p10le"
            }),
            "{info}"
        );
    }
    if format == Format::Hevc && signal == ColorSignal::PQ {
        assert!(info.contains("Mastering display metadata") && info.contains("Content light level metadata"), "{info}");
    }
    if signal.is_hdr() {
        assert!(info.contains("color_primaries=bt2020"), "{info}");
        assert!(info.contains(if signal == ColorSignal::PQ { "color_transfer=smpte2084" } else { "color_transfer=arib-std-b67" }), "{info}");
    }
    eprintln!(
        "ORACLE {format:?} {profile} alpha={alpha} tf={}: {}",
        signal.transfer,
        info.lines().filter(|l| l.starts_with("pix_fmt=") || l.starts_with("profile=")).collect::<Vec<_>>().join(", ")
    );
}

/// Check ProRes software recovery after native failure and reject truncated samples and malformed encoder inputs.
#[test]
fn prores_midstream_failure_and_hostile_frames() {
    use filmcraft_codecs::{VideoDecoder, hw::StreamInfo};
    use filmcraft_isobmff::{FourCc, SampleEntry};
    use filmcraft_platform::{HybridDecoder, videotoolbox::VtDecoder};
    let entry = SampleEntry::prores(FourCc(*b"apcs"), 640, 360);
    let Ok(mut vt) = VtDecoder::new_intra(entry.clone()) else { return };
    assert!(vt.uses_hardware());
    vt.fail_after(2);
    let mut decoder = HybridDecoder::new(Box::new(vt), entry.clone(), StreamInfo::Intra);
    let mut encoder = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Lt, 640, 360);
    let frame = filmcraft_prores::Frame::new(640, 360, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
    let data = encoder.encode(&frame).unwrap();
    let mut frames = vec![];
    for i in 0..5 {
        frames.extend(decoder.decode(&data, i).unwrap());
        if i < 2 {
            assert!(decoder.is_hardware(), "failure must occur at the injected point");
        }
    }
    frames.extend(decoder.flush());
    assert_eq!(frames.len(), 5);
    assert!(!decoder.is_hardware());
    assert!(decoder.intra_only());
    assert_eq!(frames.iter().map(|f| f.pts).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
    // The untrusted frame header is parsed before touching CoreMedia, and software receives
    // the failed sample. All truncations must return an error without unwinding.
    for n in [0, 1, 7, 8, 19, 27, data.len() / 2] {
        let mut decoder = HybridDecoder::new(Box::new(VtDecoder::new_intra(entry.clone()).unwrap()), entry.clone(), StreamInfo::Intra);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decoder.decode(&data[..n], 0))).is_ok());
    }
}

/// Reject invalid or incompatible explicit profiles instead of silently selecting a weaker format.
#[test]
fn explicit_profiles_are_validated_and_never_downgraded() {
    use filmcraft_platform::{
        hardware_encode::config_for,
        videotoolbox_encode::{VtEncoder, VtOptions, VtProfile},
    };
    let base = ExportSettings { format: Format::Hevc, ..Default::default() };
    assert!(ExportSettings { hevc_profile: "not-a-profile".into(), ..base.clone() }.validate().is_err());
    assert!(config_for(640, 360, FrameRate::FPS_30, &ExportSettings { hevc_profile: "main".into(), signal: ColorSignal::PQ, ..base.clone() }).is_err());
    for profile in ["main10", "main42210"] {
        let s = ExportSettings { hevc_profile: profile.into(), alpha: true, ..base.clone() };
        assert!(config_for(640, 360, FrameRate::FPS_30, &s).is_err());
    }
    let config = config_for(640, 360, FrameRate::FPS_30, &ExportSettings { hevc_profile: "main10".into(), ..base }).unwrap();
    let Ok(mut encoder) = VtEncoder::new(config.clone()) else { return };
    assert!(encoder.uses_hardware());
    assert!(encoder.encode(0, &[], &[], &[]).is_err());
    assert!(encoder.encode10(0, &[], &[], &[], None).is_err());
    assert!(encoder.encode10(0, &vec![1024; 640 * 360], &vec![512; 320 * 180], &vec![512; 320 * 180], None).is_err());
    assert!(VtEncoder::with_options(config, VtOptions { alpha: true, ..Default::default() }).is_err());
    let s = ExportSettings { format: Format::ProRes, hardware_encoding: HardwareEncoding::Auto, ..Default::default() };
    let mut config = config_for(128, 64, FrameRate::FPS_30, &s).unwrap();
    config.profile = VtProfile::ProRes(*b"fake");
    assert!(VtEncoder::new(config).is_err());
}

/// Verify that the product export pipeline preserves selected profiles, HDR metadata and supported transparency.
#[test]
fn full_export_preserves_profiles_hdr_and_transparency() {
    use filmcraft_color::{ColorPipeline, WorkingSpace};
    use filmcraft_media::{
        MediaSource,
        generators::{Generator, GeneratorSource},
    };
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
    use filmcraft_time::{Tick, TimeRange};
    use std::sync::Arc;
    filmcraft_platform::register();
    let (w, h, rate, frames) = (640, 360, FrameRate::FPS_30, 6);
    for (format, profile, working, alpha) in [
        (Format::ProRes, "lt", WorkingSpace::Rec709, false),
        (Format::ProRes, "4444xq", WorkingSpace::Rec2100Hlg, true),
        (Format::Hevc, "main10", WorkingSpace::Rec2100Pq, false),
        (Format::Hevc, "main42210", WorkingSpace::Rec2100Hlg, false),
        (Format::Hevc, "main", WorkingSpace::Rec709, true),
        (Format::Mjpeg, "", WorkingSpace::Rec709, false),
    ] {
        let mut project = Project::new("VT integration");
        let source = GeneratorSource::new(Generator::ColorMatte { color: [0.8, 0.3, 0.1, 0.5] }, w, h, rate, rate.tick_of(frames));
        let info = source.info().clone();
        let item = project.add_item(
            "generated matte",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(source.generator.clone()),
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
        let sequence = project.new_sequence(
            "test",
            SequenceSettings { width: w, height: h, frame_rate: rate, color: ColorPipeline { working, ..ColorPipeline::REC709 }, ..Default::default() },
            1,
            0,
            None,
        );
        let clip = project.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
        project.sequence_mut(sequence).unwrap().video_tracks[0].items.push(clip);
        let mut sources = filmcraft_render::SourceMap::default();
        sources.0.insert(item, Arc::new(source));
        let path = filmcraft_testkit::fixtures_dir("platform").join(format!("vt-export-{format:?}-{profile}.mov"));
        let settings = ExportSettings {
            format,
            hevc_profile: profile.into(),
            prores_profile: profile.into(),
            path: path.to_string_lossy().into(),
            alpha,
            hardware_encoding: HardwareEncoding::Auto,
            include_audio: false,
            ..Default::default()
        };
        // ProRes-only names are not HEVC profile choices.
        let settings = if format == Format::Hevc { settings } else { ExportSettings { hevc_profile: String::new(), ..settings } };
        let effective = ExportSettings {
            signal: if working == WorkingSpace::Rec2100Pq {
                ColorSignal::PQ
            } else if working == WorkingSpace::Rec2100Hlg {
                ColorSignal::HLG
            } else {
                ColorSignal::default()
            },
            ..settings.clone()
        };
        if !matches!(videotoolbox_encoder_factory(format, w, h, rate, &effective), Some(Ok(_))) {
            eprintln!("SKIPPED no {format:?} {profile} hardware");
            continue;
        }
        filmcraft_export::export(&Arc::new(project), sequence, &settings, &sources, &filmcraft_export::Progress::default()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
        let track = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        let t = &file.tracks[track];
        assert_eq!(t.samples.len(), frames as usize);
        let mut decoder =
            filmcraft_platform::videotoolbox_factory(&t.entries[0]).unwrap_or_else(|| panic!("{format:?} {profile} entry {:?}", t.entries[0])).unwrap();
        let mut output = vec![];
        for (i, s) in t.samples.iter().enumerate() {
            output.extend(decoder.decode(&file.read_sample(&bytes, track, i).unwrap(), s.pts).unwrap());
        }
        output.extend(decoder.flush());
        assert_eq!(output.len(), frames as usize);
        if alpha {
            for f in output {
                match f.frame.data {
                    filmcraft_frame::PixelData::Yuv16 { alpha: Some(a), bits, .. } => assert!(a.iter().all(|&v| (i32::from(v) - (1 << (bits - 1))).abs() < 16)),
                    filmcraft_frame::PixelData::Rgba8(p) => assert!(p.as_chunks::<4>().0.iter().all(|p| (i16::from(p[3]) - 128).abs() < 4)),
                    _ => panic!("lost transparency"),
                }
            }
        }
    }
}

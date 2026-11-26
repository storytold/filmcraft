//! VideoToolbox H.264 and HEVC encoding (macOS): what the hardware path takes and declines, a round trip
//! through our own software decoder (picture quality, frame count and order, keyframes, the
//! timestamps the muxer needs), a full export through `filmcraft_export` that decodes, the fallback
//! to the built-in encoder for everything hardware does not take, and hostile configurations that
//! must give errors and never crash. Tests that need a hardware encoder skip without one.
#![cfg(target_os = "macos")]

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use filmcraft_export::{
    BitrateMode, EncodedPacket, EncoderFrame, ExportSettings, Format, H264Pass, H264Profile, HardwareEncoding, Progress, VideoEncoder, export, hw_encode_stats,
    rgba_to_yuv420_8,
};
use filmcraft_frame::PixelData;
use filmcraft_isobmff::{CodecConfig, SampleEntry};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, MediaSource};
use filmcraft_platform::hardware_encode::{HardwareEncoder, config_for, hevc_available, videotoolbox_encoder_factory};
use filmcraft_platform::videotoolbox_encode::{VtConfig, VtEncoder, VtInput, VtProfile, VtRate};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

const W: u32 = 640;
const H: u32 = 360;
const FPS: FrameRate = FrameRate::FPS_30;

/// A moving test picture: gradients plus a bar travelling across.
fn picture(i: u64) -> Vec<u8> {
    let (w, h) = (W as usize, H as usize);
    let mut px = vec![255u8; w * h * 4];
    let bar = (i as usize * 7) % (w - 40);
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let on_bar = x >= bar && x < bar + 40 && y > h / 3 && y < 2 * h / 3;
            px[o] = if on_bar { 255 } else { ((x * 255 / w + i as usize * 3) % 256) as u8 };
            px[o + 1] = if on_bar { 255 } else { (y * 255 / h) as u8 };
            px[o + 2] = if on_bar { 255 } else { (((x + y) / 2 + i as usize * 5) % 256) as u8 };
        }
    }
    px
}

fn hardware_settings() -> ExportSettings {
    ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Auto, bitrate_kbps: 8_000, keyframe_distance: Some(30), ..Default::default() }
}

/// H.265 needs no opt-in: choosing the format is the opt-in.
fn hevc_settings() -> ExportSettings {
    ExportSettings { format: Format::Hevc, bitrate_kbps: 6_000, keyframe_distance: Some(30), ..Default::default() }
}

/// The hardware encoder for the settings, or `None` (skip) when this machine has none.
fn hardware(s: &ExportSettings, w: u32, h: u32) -> Option<Box<dyn VideoEncoder>> {
    match videotoolbox_encoder_factory(s.format, w, h, FPS, s) {
        Some(Ok(e)) => Some(e),
        Some(Err(e)) => panic!("factory error: {e}"),
        None => {
            eprintln!("SKIPPED: no VideoToolbox hardware {} encoder for these settings", s.format.label());
            None
        }
    }
}

fn encode_all(enc: &mut Box<dyn VideoEncoder>, frames: u64) -> Vec<EncodedPacket> {
    let mut packets = Vec::new();
    for i in 0..frames {
        let rgba = picture(i);
        packets.extend(enc.encode(&EncoderFrame { width: W, height: H, rgba: &rgba, hdr: None, index: i }).expect("encode"));
    }
    packets.extend(enc.flush().expect("flush"));
    packets
}

/// Decode `packets` (decoding order) with our software decoder: the pictures in presentation order.
fn decode(entry: &SampleEntry, packets: &[EncodedPacket]) -> Vec<(i64, Vec<u8>)> {
    let mut dec = filmcraft_codecs::software_video_decoder(entry).expect("software decoder");
    let mut out = Vec::new();
    let mut dts = 0i64;
    let take = |frames: Vec<filmcraft_codecs::DecodedFrame>, out: &mut Vec<(i64, Vec<u8>)>| {
        for f in frames {
            let PixelData::Yuv8 { planes, .. } = &f.frame.data else { panic!("expected 8-bit planes") };
            out.push((f.pts, planes[0].to_vec()));
        }
    };
    for p in packets {
        take(dec.decode(&p.data, dts + i64::from(p.composition_offset)).expect("decode"), &mut out);
        dts += i64::from(p.duration);
    }
    take(dec.flush(), &mut out);
    out.sort_by_key(|(pts, _)| *pts);
    out
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs()).sum::<f64>() / a.len() as f64
}

/// Decode `packets` (decoding order, no reordering) with our software decoder: every picture's
/// Y, Cb and Cr planes in presentation order.
fn decode_planes(entry: &SampleEntry, packets: &[EncodedPacket]) -> Vec<[Vec<u8>; 3]> {
    let mut dec = filmcraft_codecs::software_video_decoder(entry).expect("software decoder");
    let mut out = Vec::new();
    let take = |frames: Vec<filmcraft_codecs::DecodedFrame>, out: &mut Vec<(i64, [Vec<u8>; 3])>| {
        for f in frames {
            let PixelData::Yuv8 { planes, .. } = &f.frame.data else { panic!("expected 8-bit planes") };
            out.push((f.pts, [planes[0].to_vec(), planes[1].to_vec(), planes[2].to_vec()]));
        }
    };
    for (i, p) in packets.iter().enumerate() {
        take(dec.decode(&p.data, i as i64).expect("decode"), &mut out);
    }
    take(dec.flush(), &mut out);
    out.sort_by_key(|(pts, _)| *pts);
    out.into_iter().map(|(_, planes)| planes).collect()
}

/// The sample entry of a session's stream (known once its first frame came out).
fn entry_of(vt: &VtEncoder) -> SampleEntry {
    let p = vt.parameter_sets().expect("parameter sets after the first frame");
    SampleEntry::avc(filmcraft_isobmff::AvcConfig::new(p.sps, p.pps, 4), W as u16, H as u16)
}

/// RGBA pictures handed to VideoToolbox as they are (`32BGRA`, VideoToolbox converting) encode the
/// same pictures as the planar path (our own BT.709 limited-range conversion into NV12): every
/// plane of both decodes is within a few codes of our conversion of the source. A swapped channel,
/// a full-range conversion, another matrix or a transfer-function conversion would be tens of codes
/// off in some plane. The hardware output differs between the two paths by rate control and chroma
/// filtering only, so the quality is compared, not the bytes.
#[test]
fn rgba_pictures_encode_like_the_cpu_conversion() {
    let cfg = config_for(W, H, FPS, &hardware_settings()).expect("a hardware configuration");
    let Ok(mut rgb) = VtEncoder::with_rgba_input(cfg.clone()) else {
        eprintln!("SKIPPED: no VideoToolbox hardware H.264 encoder");
        return;
    };
    let mut planar = VtEncoder::new(cfg).expect("the planar session opens where the RGB one does");
    assert_eq!((rgb.input(), planar.input()), (VtInput::Bgra, VtInput::Nv12));
    let n = 24u64;
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut copy = std::time::Duration::ZERO;
    for i in 0..n {
        let px = picture(i);
        match rgb.encode_rgba(i, &px) {
            Ok((packets, copied)) => {
                a.extend(packets);
                copy += copied;
            }
            Err(why) => {
                eprintln!("SKIPPED: this VideoToolbox does not take RGBA pictures ({why})");
                return;
            }
        }
        rgba_to_yuv420_8(&px, W as usize, H as usize, &mut y, &mut u, &mut v);
        b.extend(planar.encode(i, &y, &u, &v).expect("planar encode"));
    }
    a.extend(rgb.flush().expect("flush"));
    b.extend(planar.flush().expect("flush"));
    let a: Vec<EncodedPacket> = a.into_iter().map(|p| EncodedPacket { data: p.data, key: p.key, duration: 1, composition_offset: 0 }).collect();
    let b: Vec<EncodedPacket> = b.into_iter().map(|p| EncodedPacket { data: p.data, key: p.key, duration: 1, composition_offset: 0 }).collect();
    assert_eq!((a.len() as u64, b.len() as u64), (n, n));
    let (da, db) = (decode_planes(&entry_of(&rgb), &a), decode_planes(&entry_of(&planar), &b));
    assert_eq!((da.len() as u64, db.len() as u64), (n, n));
    let mut worst = [99.0f64; 3];
    let mut gap = [0.0f64; 3];
    let mut apart = [0.0f64; 3];
    for i in 0..n as usize {
        rgba_to_yuv420_8(&picture(i as u64), W as usize, H as usize, &mut y, &mut u, &mut v);
        for (c, reference) in [&y, &u, &v].into_iter().enumerate() {
            let (pa, pb) = (psnr(&da[i][c], reference), psnr(&db[i][c], reference));
            worst[c] = worst[c].min(pa);
            gap[c] = gap[c].max(pb - pa);
            apart[c] = apart[c].max(mean_abs_diff(&da[i][c], &db[i][c]));
        }
    }
    // The planar path's chroma is our conversion itself (2×2 average), so against that reference it
    // scores near-perfectly where VideoToolbox's filtered chroma siting scores ~40 dB: the gap is
    // reported, the bounds are the absolute ones. (M2 Max: Y/Cb/Cr 52 / 41 / 37 dB, paths 0.2 / 0.6 /
    // 0.8 codes apart; a swapped channel or a range or matrix mistake is tens of codes.)
    eprintln!(
        "RGBA input: worst PSNR Y/Cb/Cr {worst:.1?} dB, at most {gap:.1?} dB under the planar path, mean |Δ| between the paths {apart:.2?} codes; copy {:.2} ms a frame",
        copy.as_secs_f64() * 1000.0 / n as f64
    );
    for c in 0..3 {
        assert!(worst[c] > 30.0, "plane {c}: worst PSNR {:.1} dB against our conversion", worst[c]);
        assert!(apart[c] < 2.0, "plane {c}: the paths are {:.2} codes apart on average", apart[c]);
    }
    assert!(gap[0] < 3.0, "luma {:.1} dB below the planar path", gap[0]);
}

/// The adapter the factory registers prefers RGB input; the pictures it writes decode at the same quality.
#[test]
fn the_encoder_takes_rgba_pictures_where_videotoolbox_converts() {
    let cfg = config_for(W, H, FPS, &hardware_settings()).expect("a hardware configuration");
    let Ok(enc) = HardwareEncoder::open(cfg) else {
        eprintln!("SKIPPED: no VideoToolbox hardware H.264 encoder");
        return;
    };
    assert!(enc.takes_rgba(), "the RGB session is tried first");
    let mut enc: Box<dyn VideoEncoder> = Box::new(enc);
    let n = 20u64;
    let packets = encode_all(&mut enc, n);
    assert_eq!(packets.len() as u64, n);
    let decoded = decode(&enc.sample_entry(), &packets);
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut worst = 99.0f64;
    for (i, (_, luma)) in decoded.iter().enumerate() {
        rgba_to_yuv420_8(&picture(i as u64), W as usize, H as usize, &mut y, &mut u, &mut v);
        worst = worst.min(psnr(&y, luma));
    }
    assert!(worst > 30.0, "worst luma PSNR {worst:.1} dB");
}

/// Hostile RGBA pictures and the wrong kind of picture for a session give errors, never crashes.
#[test]
fn rgba_input_refuses_what_does_not_fit() {
    let cfg = config_for(W, H, FPS, &hardware_settings()).expect("a hardware configuration");
    let Ok(mut rgb) = VtEncoder::with_rgba_input(cfg.clone()) else {
        eprintln!("SKIPPED: no VideoToolbox hardware H.264 encoder");
        return;
    };
    let (w, h) = (W as usize, H as usize);
    assert!(calm("empty", || rgb.encode_rgba(0, &[])).is_err());
    assert!(calm("short", || rgb.encode_rgba(0, &vec![0; w * h * 4 - 1])).is_err());
    assert!(calm("planes to an RGB session", || rgb.encode(0, &vec![16; w * h], &vec![128; w * h / 4], &vec![128; w * h / 4])).is_err());
    let mut planar = VtEncoder::new(cfg).expect("planar session");
    assert!(calm("RGBA to a planar session", || planar.encode_rgba(0, &vec![0; w * h * 4])).is_err());
    // a picture larger than its size is fine (the extra bytes are ignored)
    let _ = calm("long", || rgb.encode_rgba(0, &vec![0; w * h * 4 + 64]));
    let _ = calm("flush", || rgb.flush());
}

/// An export at another frame rate than the sequence's (frames are repeated or dropped on the way) and
/// a cancelled export followed by a complete one in the same process: the hardware path must give the
/// asked-for frame count, release its sessions and start clean again.
#[test]
fn rate_conversion_and_cancel_then_export_again() {
    filmcraft_platform::register();
    let probe = ExportSettings { include_audio: false, ..hardware_settings() };
    if hardware(&probe, W, H).is_none() {
        return;
    }
    let (frames, (p, seq, m)) = (60i64, project(60));
    // 60 frames at 30 fps become 120 at 60 fps and 50 at 25 fps
    for (rate, want) in [(FrameRate::FPS_60, 120usize), (FrameRate::FPS_25, 50)] {
        let path = tmp(&format!("rate-{}.mp4", rate.num));
        let s = ExportSettings { path: path.clone(), frame_rate: Some(rate), ..probe.clone() };
        let report = export(&p, seq, &s, &m, &Progress::default()).expect("export");
        assert_eq!(report.frames as usize, want, "{rate:?}");
        let (samples, pictures, _) = inspect(&path);
        assert_eq!((samples, pictures), (want, want), "{rate:?}");
    }
    // cancel from another thread once a few frames are done
    let before = hw_encode_stats();
    let path = tmp("cancelled.mp4");
    let s = ExportSettings { path: path.clone(), ..probe.clone() };
    let progress = Progress::default();
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while progress.done.load(std::sync::atomic::Ordering::Relaxed) < 8 && !progress.finished.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        export(&p, seq, &s, &m, &progress)
    });
    match result {
        Err(filmcraft_export::ExportError::Cancelled) => {}
        Ok(r) => eprintln!("the export finished before the cancel landed ({} frames)", r.frames),
        Err(e) => panic!("a cancelled export must report Cancelled or finish, not {e}"),
    }
    // and the next export in the same process is complete
    let path = tmp("after-cancel.mp4");
    let s = ExportSettings { path: path.clone(), ..probe };
    let report = export(&p, seq, &s, &m, &Progress::default()).expect("export after a cancel");
    assert_eq!(report.frames, frames as u64);
    let (samples, pictures, _) = inspect(&path);
    assert_eq!((samples, pictures), (frames as usize, frames as usize));
    let after = hw_encode_stats();
    assert!(after.sessions >= before.sessions + 2, "both exports opened a hardware session");
    assert_eq!(after.declined, before.declined, "nothing was declined to the software encoder");
}

#[test]
fn declines_what_the_hardware_path_does_not_take() {
    let ok = hardware_settings();
    assert!(config_for(W, H, FPS, &ok).is_ok());
    let declined = |s: &ExportSettings, what: &str| assert!(config_for(W, H, FPS, s).is_err(), "{what} must not use the hardware encoder");
    declined(&ExportSettings { hardware_encoding: HardwareEncoding::Off, ..ok.clone() }, "hardware encoding off");
    declined(&ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..ok.clone() }, "two-pass VBR");
    declined(&ExportSettings { bitrate_mode: BitrateMode::Crf, ..ok.clone() }, "CRF");
    declined(&ExportSettings { h264_pass: H264Pass::First, ..ok.clone() }, "an analysis pass");
    declined(&ExportSettings { signal: filmcraft_export::ColorSignal::PQ, ..ok.clone() }, "HDR");
    declined(&ExportSettings { pixel_aspect: Some((4, 3)), ..ok.clone() }, "non-square pixels");
    declined(&ExportSettings { format: Format::MxfOp1a, ..ok.clone() }, "MXF");
    assert!(config_for(W, H, FrameRate { num: 0, den: 1 }, &ok).is_err());
    assert!(config_for(W, H, FrameRate { num: 30, den: 0 }, &ok).is_err());
    assert!(config_for(W, H, FrameRate { num: -30, den: 1 }, &ok).is_err());
    // square pixels, explicit or implied, are fine
    assert!(config_for(W, H, FPS, &ExportSettings { pixel_aspect: Some((1, 1)), ..ok.clone() }).is_ok());
    // the configuration follows the settings
    let c =
        config_for(W, H, FPS, &ExportSettings { h264_profile: H264Profile::Main, bitrate_mode: BitrateMode::Cbr, bitrate_kbps: 5000, ..ok.clone() }).unwrap();
    assert_eq!((c.profile, c.rate, c.keyframe_interval), (VtProfile::Main, VtRate::Cbr { kbps: 5000 }, 30));
    let c = config_for(W, H, FPS, &ExportSettings { keyframe_distance: None, max_bitrate_kbps: Some(20_000), ..ok }).unwrap();
    assert_eq!((c.rate, c.keyframe_interval), (VtRate::Vbr { target_kbps: 8000, max_kbps: 20_000 }, 60));
}

#[test]
fn factory_only_acts_when_asked() {
    let s = hardware_settings();
    for format in [Format::ProRes, Format::DnxHr, Format::Mjpeg, Format::PngSequence] {
        assert!(videotoolbox_encoder_factory(format, W, H, FPS, &s).is_none(), "{format:?}");
    }
    assert!(videotoolbox_encoder_factory(Format::H264, W, H, FPS, &ExportSettings { hardware_encoding: HardwareEncoding::Off, ..s }).is_none());
}

/// `perf.stats` reads these counters: a declined request, a session and every encoded picture must show.
/// The counters are process-wide and other tests add to them, so only increases are checked.
#[test]
fn hardware_encoder_counters_follow_the_export() {
    let s = hardware_settings();
    let before = hw_encode_stats();
    // an odd picture size is declined before any session is created
    assert!(videotoolbox_encoder_factory(Format::H264, W + 1, H, FPS, &s).is_none());
    assert!(hw_encode_stats().declined > before.declined, "a declined request is counted");
    let Some(mut enc) = hardware(&s, W, H) else { return };
    let created = hw_encode_stats();
    assert!(created.sessions > before.sessions, "a created session is counted");
    encode_all(&mut enc, 3);
    assert!(hw_encode_stats().frames >= created.frames + 3, "every encoded picture is counted");
}

#[test]
fn round_trip_through_our_software_decoder() {
    let s = hardware_settings();
    let Some(mut enc) = hardware(&s, W, H) else { return };
    let n = 75u64;
    let packets = encode_all(&mut enc, n);
    let entry = enc.sample_entry();
    let CodecConfig::Avc(avc) = &entry.codec else { panic!("expected an AVC sample entry, got {}", entry.codec.name()) };
    assert!(!avc.sps.is_empty() && !avc.pps.is_empty());
    assert_eq!(avc.length_size, 4);
    assert_eq!(enc.timescale(), 30);

    // one compressed frame per picture, the first a keyframe, keyframes no further apart than asked
    assert_eq!(packets.len() as u64, n);
    assert!(packets[0].key);
    assert!(packets.iter().all(|p| p.duration == 1 && p.composition_offset >= 0 && !p.data.is_empty()));
    let keys: Vec<usize> = packets.iter().enumerate().filter(|(_, p)| p.key).map(|(i, _)| i).collect();
    assert!(keys.windows(2).all(|w| w[1] - w[0] <= 30), "keyframes at {keys:?}");

    // no frame reordering: every frame is presented at its decode time (the muxer numbers decode
    // times n × duration), so there are no composition offsets and no edit list
    assert!(packets.iter().all(|p| p.composition_offset == 0), "composition offsets {:?}", packets.iter().map(|p| p.composition_offset).collect::<Vec<_>>());
    assert_eq!(enc.media_start(), None);

    // our decoder reproduces every picture, in order, at a good quality
    let decoded = decode(&entry, &packets);
    assert_eq!(decoded.len() as u64, n);
    assert_eq!(decoded.iter().map(|(p, _)| *p).collect::<Vec<_>>(), (0..n as i64).collect::<Vec<_>>());
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut worst = 99.0f64;
    for (i, (_, luma)) in decoded.iter().enumerate() {
        rgba_to_yuv420_8(&picture(i as u64), W as usize, H as usize, &mut y, &mut u, &mut v);
        worst = worst.min(psnr(&y, luma));
    }
    assert!(worst > 30.0, "worst luma PSNR {worst:.1} dB");
    eprintln!("hardware H.264: {} bytes for {n} frames, worst luma PSNR {worst:.1} dB", packets.iter().map(|p| p.data.len()).sum::<usize>());
}

/// A project with an animated demo clip as the whole sequence.
fn project(frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let mut p = Project::new("hw");
    let g = GeneratorSource::demo(DemoScene::Plasma);
    let info = g.info().clone();
    let item = p.add_item(
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
    let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: FPS, ..Default::default() }, 1, 1, None);
    let mut clip = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(frames)), FPS).unwrap();
    clip.scale_to_frame = true;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
    let mut m = SourceMap::default();
    m.0.insert(item, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-hw-encode-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// (sample count, decoded picture count, sync sample count) of an MP4's video track, decoded by our decoder.
fn inspect(path: &str) -> (usize, usize, usize) {
    let bytes = std::fs::read(path).unwrap();
    let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
    let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
    let track = &file.tracks[vt];
    let entry = track.entries.first().expect("a sample entry");
    assert!(matches!(&entry.codec, CodecConfig::Avc(_) | CodecConfig::Hevc(_)), "neither AVC nor HEVC: {}", entry.codec.name());
    let mut dec = filmcraft_codecs::software_video_decoder(entry).expect("software decoder");
    let mut pictures = 0;
    for (i, s) in track.samples.iter().enumerate() {
        let data = file.read_sample(bytes.as_slice(), vt, i).unwrap();
        pictures += dec.decode(&data, s.pts).expect("decode").len();
    }
    pictures += dec.flush().len();
    (track.samples.len(), pictures, track.samples.iter().filter(|s| s.is_sync).count())
}

#[test]
fn export_through_the_pipeline_with_hardware_encoding() {
    filmcraft_platform::register();
    let path = tmp("hw.mp4");
    let s = ExportSettings { path: path.clone(), include_audio: false, ..hardware_settings() };
    // is the hardware encoder available at all on this machine?
    if hardware(&s, W, H).is_none() {
        return;
    }
    let frames = 70;
    let (p, seq, m) = project(frames);
    let report = export(&p, seq, &s, &m, &Progress::default()).expect("export");
    assert_eq!(report.frames, frames as u64);
    let (samples, pictures, syncs) = inspect(&path);
    assert_eq!((samples, pictures), (frames as usize, frames as usize));
    assert!(syncs >= 3, "keyframes every 30 frames: {syncs} sync samples");
}

#[test]
fn what_hardware_does_not_take_still_exports_with_the_built_in_encoder() {
    filmcraft_platform::register();
    let (frames, (p, seq, m)) = (30, project(30));
    for (name, settings) in [
        ("two-pass", ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..hardware_settings() }),
        ("off", ExportSettings { hardware_encoding: HardwareEncoding::Off, ..hardware_settings() }),
        ("odd-aspect", ExportSettings { pixel_aspect: Some((4, 3)), ..hardware_settings() }),
    ] {
        let path = tmp(&format!("{name}.mp4"));
        let s = ExportSettings { path: path.clone(), include_audio: false, ..settings };
        export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (samples, pictures, _) = inspect(&path);
        assert_eq!((samples, pictures), (frames, frames), "{name}");
    }
}

/// Run `f` and require that it neither panics nor hangs the test.
fn calm<R>(what: &str, f: impl FnOnce() -> R) -> R {
    std::panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| panic!("{what} panicked"))
}

#[test]
fn hostile_configurations_give_errors_not_crashes() {
    let base = VtConfig {
        width: W,
        height: H,
        timescale: 30,
        frame_duration: 1,
        keyframe_interval: 30,
        profile: VtProfile::High,
        rate: VtRate::Vbr { target_kbps: 8000, max_kbps: 12_000 },
    };
    let variants: Vec<(&str, VtConfig)> = vec![
        ("zero width", VtConfig { width: 0, ..base.clone() }),
        ("zero height", VtConfig { height: 0, ..base.clone() }),
        ("huge width", VtConfig { width: u32::MAX, ..base.clone() }),
        ("huge height", VtConfig { height: 100_000, ..base.clone() }),
        ("1x1", VtConfig { width: 1, height: 1, ..base.clone() }),
        ("odd size", VtConfig { width: 321, height: 181, ..base.clone() }),
        ("odd width", VtConfig { width: 321, ..base.clone() }),
        ("zero timescale", VtConfig { timescale: 0, ..base.clone() }),
        ("zero frame duration", VtConfig { frame_duration: 0, ..base.clone() }),
        ("huge timescale", VtConfig { timescale: u32::MAX, ..base.clone() }),
        ("zero keyframe interval", VtConfig { keyframe_interval: 0, ..base.clone() }),
        ("huge keyframe interval", VtConfig { keyframe_interval: u32::MAX, ..base.clone() }),
        ("zero bitrate", VtConfig { rate: VtRate::Vbr { target_kbps: 0, max_kbps: 0 }, ..base.clone() }),
        ("huge bitrate", VtConfig { rate: VtRate::Vbr { target_kbps: u32::MAX, max_kbps: u32::MAX }, ..base.clone() }),
        ("max below target", VtConfig { rate: VtRate::Vbr { target_kbps: 8000, max_kbps: 1 }, ..base.clone() }),
        ("zero cbr", VtConfig { rate: VtRate::Cbr { kbps: 0 }, ..base.clone() }),
        ("baseline", VtConfig { profile: VtProfile::Baseline, ..base.clone() }),
        ("main", VtConfig { profile: VtProfile::Main, ..base.clone() }),
        ("cbr", VtConfig { rate: VtRate::Cbr { kbps: 6000 }, ..base.clone() }),
    ];
    for (name, config) in variants {
        match calm(name, || VtEncoder::new(config)) {
            // when a hardware session exists, pictures of the wrong size or planes must be refused
            Ok(mut enc) => {
                let (w, h) = (enc.config().width as usize, enc.config().height as usize);
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                assert!(calm(name, || enc.encode(0, &[], &[], &[])).is_err(), "{name}: empty planes");
                assert!(calm(name, || enc.encode(0, &vec![0; w * h - 1], &vec![128; cw * ch], &vec![128; cw * ch])).is_err(), "{name}: short luma");
                let _ = calm(name, || enc.encode(0, &vec![16; w * h], &vec![128; cw * ch], &vec![128; cw * ch]));
                let _ = calm(name, || enc.flush());
            }
            Err(why) => eprintln!("{name}: declined ({why})"),
        }
    }
}

#[test]
fn encoder_refuses_pictures_of_another_size() {
    let s = hardware_settings();
    let Some(mut enc) = hardware(&s, W, H) else { return };
    let rgba = vec![0u8; 100 * 100 * 4];
    assert!(enc.encode(&EncoderFrame { width: 100, height: 100, rgba: &rgba, hdr: None, index: 0 }).is_err());
    let short = vec![0u8; 16];
    assert!(enc.encode(&EncoderFrame { width: W, height: H, rgba: &short, hdr: None, index: 0 }).is_err());
    let hdr = vec![0.0f32; (W * H * 3) as usize];
    assert!(enc.encode(&EncoderFrame { width: W, height: H, rgba: &vec![0u8; (W * H * 4) as usize], hdr: Some(&hdr), index: 0 }).is_err());
}

#[test]
fn encoders_can_be_dropped_at_any_point() {
    let s = hardware_settings();
    for frames in [0u64, 1, 2, 5] {
        let Some(mut enc) = hardware(&s, W, H) else { return };
        for i in 0..frames {
            let rgba = picture(i);
            let _ = calm("encode", || enc.encode(&EncoderFrame { width: W, height: H, rgba: &rgba, hdr: None, index: i }));
        }
        drop(enc); // pending frames, no flush: must not crash or hang
    }
}

#[test]
fn ffmpeg_decodes_what_the_hardware_encoder_writes() {
    filmcraft_platform::register();
    let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("hardware H.264 export") else { return };
    let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("hardware H.264 export") else { return };
    let path = tmp("oracle.mp4");
    let s = ExportSettings { path: path.clone(), include_audio: false, ..hardware_settings() };
    if hardware(&s, W, H).is_none() {
        return;
    }
    let frames = 65;
    let (p, seq, m) = project(frames);
    export(&p, seq, &s, &m, &Progress::default()).expect("export");
    // an independent decoder reads the whole file without a complaint
    let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-i", &path, "-f", "null", "-"]).output().expect("run ffmpeg");
    assert!(out.status.success() && out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    // and finds the stream we asked for
    let out = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=profile,width,height,nb_read_frames,has_b_frames,color_space",
            "-of",
            "default=nw=1",
            &path,
        ])
        .output()
        .expect("run ffprobe");
    let info = String::from_utf8_lossy(&out.stdout).to_string();
    for want in ["profile=High", &format!("width={W}"), &format!("height={H}"), &format!("nb_read_frames={frames}"), "has_b_frames=0", "color_space=bt709"] {
        assert!(info.contains(want), "ffprobe lacks `{want}`:\n{info}");
    }
}

#[test]
fn pictures_keep_their_exact_size() {
    let s = hardware_settings();
    // odd sizes cannot be cropped exactly in 4:2:0: the built-in encoder takes them
    assert!(config_for(321, 181, FPS, &s).is_err() && config_for(640, 361, FPS, &s).is_err());
    assert!(videotoolbox_encoder_factory(Format::H264, 321, 181, FPS, &s).is_none());
    // even sizes that are not multiples of 16 come out at exactly their size
    for (w, h) in [(640u32, 362u32), (642, 360), (1280, 720), (16, 16)] {
        let Some(mut enc) = hardware(&s, w, h) else { continue };
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let mut packets = Vec::new();
        for i in 0..6u64 {
            packets.extend(enc.encode(&EncoderFrame { width: w, height: h, rgba: &rgba, hdr: None, index: i }).expect("encode"));
        }
        packets.extend(enc.flush().expect("flush"));
        assert_eq!(packets.len(), 6, "{w}x{h}");
        let entry = enc.sample_entry();
        let mut dec = filmcraft_codecs::software_video_decoder(&entry).expect("software decoder");
        let mut pictures = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            pictures.extend(dec.decode(&p.data, i as i64).expect("decode"));
        }
        pictures.extend(dec.flush());
        assert_eq!(pictures.len(), 6, "{w}x{h}");
        assert!(
            pictures.iter().all(|f| (f.frame.width, f.frame.height) == (w, h)),
            "{w}x{h}: decoded {}x{}",
            pictures[0].frame.width,
            pictures[0].frame.height
        );
    }
}

#[test]
fn an_empty_range_never_leaves_a_broken_file() {
    filmcraft_platform::register();
    let probe = ExportSettings { include_audio: false, ..hardware_settings() };
    if hardware(&probe, W, H).is_none() {
        return;
    }
    for (name, hardware_encoding) in [("empty-hw", HardwareEncoding::Auto), ("empty-sw", HardwareEncoding::Off)] {
        let path = tmp(&format!("{name}.mp4"));
        let _ = std::fs::remove_file(&path);
        let s = ExportSettings { path: path.clone(), range: Some(TimeRange::new(Tick::ZERO, Tick::ZERO)), hardware_encoding, ..probe.clone() };
        let (p, seq, m) = project(30);
        let result = export(&p, seq, &s, &m, &Progress::default());
        let file = std::fs::read(&path).ok();
        eprintln!("{name}: {:?}, file {:?} bytes", result.as_ref().map(|r| r.frames).map_err(|e| e.to_string()), file.as_ref().map(Vec::len));
        match (result, file) {
            // a clean error is fine; so is a file with no pictures that parses
            (Err(_), _) => {}
            (Ok(r), Some(bytes)) => {
                assert_eq!(r.frames, 0, "{name}");
                let f = filmcraft_isobmff::open(bytes.as_slice()).unwrap_or_else(|e| panic!("{name}: the empty file does not parse: {e}"));
                let vt = f.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("a video track");
                assert!(f.tracks[vt].samples.is_empty(), "{name}");
            }
            (Ok(_), None) => panic!("{name}: reported success without a file"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// HEVC (H.265)
// ---------------------------------------------------------------------------------------------

#[test]
fn hevc_declines_what_the_hardware_path_does_not_take() {
    let ok = hevc_settings();
    let config = config_for(W, H, FPS, &ok).expect("HEVC needs no hardware flag");
    assert_eq!((config.profile, config.keyframe_interval, config.rate), (VtProfile::HevcMain, 30, VtRate::Vbr { target_kbps: 6000, max_kbps: 9000 }));
    let declined = |s: &ExportSettings, what: &str| assert!(config_for(W, H, FPS, s).is_err(), "{what} must not use the hardware encoder");
    declined(&ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..ok.clone() }, "two-pass VBR");
    declined(&ExportSettings { signal: filmcraft_export::ColorSignal::PQ, ..ok.clone() }, "HDR");
    declined(&ExportSettings { pixel_aspect: Some((4, 3)), ..ok.clone() }, "non-square pixels");
    assert!(config_for(321, 181, FPS, &ok).is_err() && config_for(640, 361, FPS, &ok).is_err(), "odd sizes");
    assert!(config_for(W, H, FrameRate { num: 0, den: 1 }, &ok).is_err());
    // constant bitrate, and the H.264 profile setting does not matter
    let c = config_for(W, H, FPS, &ExportSettings { bitrate_mode: BitrateMode::Cbr, bitrate_kbps: 4000, h264_profile: H264Profile::Baseline, ..ok }).unwrap();
    assert_eq!((c.profile, c.rate), (VtProfile::HevcMain, VtRate::Cbr { kbps: 4000 }));
}

#[test]
fn hevc_format_follows_the_machine() {
    filmcraft_platform::register();
    // the format list asks `available`: it must agree with the probe
    assert_eq!(filmcraft_export::available(Format::Hevc), hevc_available());
    let s = hevc_settings();
    // no hardware flag needed, but nothing else is taken by this factory
    assert_eq!(videotoolbox_encoder_factory(Format::Hevc, W, H, FPS, &s).is_some(), hevc_available());
    assert!(videotoolbox_encoder_factory(Format::Hevc, 321, 181, FPS, &s).is_none());
    assert!(videotoolbox_encoder_factory(Format::H264, W, H, FPS, &s).is_none(), "H.264 still needs the hardware flag");
}

#[test]
fn hevc_round_trip_through_our_software_decoder() {
    let s = hevc_settings();
    let Some(mut enc) = hardware(&s, W, H) else { return };
    let n = 75u64;
    let packets = encode_all(&mut enc, n);
    let entry = enc.sample_entry();
    assert_eq!(&entry.format.0, b"hvc1");
    let CodecConfig::Hevc(hevc) = &entry.codec else { panic!("expected an HEVC sample entry, got {}", entry.codec.name()) };
    assert!(!hevc.vps().is_empty() && !hevc.sps().is_empty() && !hevc.pps().is_empty());
    assert_eq!(hevc.length_size, 4);
    // Main profile, 8-bit 4:2:0
    assert_eq!((hevc.general_profile_idc, hevc.chroma_format_idc, hevc.bit_depth_luma, hevc.bit_depth_chroma), (1, 1, 8, 8));
    assert!(hevc.general_level_idc >= 90, "level {} cannot hold 640x360", hevc.general_level_idc);
    assert_eq!(enc.timescale(), 30);

    // one compressed frame per picture, the first a keyframe, keyframes no further apart than asked
    assert_eq!(packets.len() as u64, n);
    assert!(packets[0].key);
    assert!(packets.iter().all(|p| p.duration == 1 && p.composition_offset == 0 && !p.data.is_empty()));
    let keys: Vec<usize> = packets.iter().enumerate().filter(|(_, p)| p.key).map(|(i, _)| i).collect();
    assert!(keys.windows(2).all(|w| w[1] - w[0] <= 30), "keyframes at {keys:?}");
    assert_eq!(enc.media_start(), None);

    // our decoder reproduces every picture, in order, at a good quality
    let decoded = decode(&entry, &packets);
    assert_eq!(decoded.len() as u64, n);
    assert_eq!(decoded.iter().map(|(p, _)| *p).collect::<Vec<_>>(), (0..n as i64).collect::<Vec<_>>());
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut worst = 99.0f64;
    for (i, (_, luma)) in decoded.iter().enumerate() {
        rgba_to_yuv420_8(&picture(i as u64), W as usize, H as usize, &mut y, &mut u, &mut v);
        worst = worst.min(psnr(&y, luma));
    }
    assert!(worst > 30.0, "worst luma PSNR {worst:.1} dB");
    eprintln!("hardware HEVC: {} bytes for {n} frames, worst luma PSNR {worst:.1} dB", packets.iter().map(|p| p.data.len()).sum::<usize>());
}

#[test]
fn hevc_export_through_the_pipeline() {
    filmcraft_platform::register();
    if !hevc_available() {
        eprintln!("SKIPPED: no VideoToolbox hardware HEVC encoder");
        return;
    }
    let frames = 70;
    let (p, seq, m) = project(frames);
    // MP4 (the default container) and QuickTime
    for (name, multiplexer) in [("hevc.mp4", filmcraft_export::Multiplexer::Mp4), ("hevc.mov", filmcraft_export::Multiplexer::Mov)] {
        let path = tmp(name);
        let s = ExportSettings { path: path.clone(), include_audio: false, multiplexer, ..hevc_settings() };
        let report = export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(report.frames, frames as u64, "{name}");
        let (samples, pictures, syncs) = inspect(&path);
        assert_eq!((samples, pictures), (frames as usize, frames as usize), "{name}");
        assert!(syncs >= 3, "{name}: keyframes every 30 frames: {syncs} sync samples");
    }
}

#[test]
fn hevc_export_with_audio_writes_aac_in_mp4() {
    filmcraft_platform::register();
    if !hevc_available() {
        eprintln!("SKIPPED: no VideoToolbox hardware HEVC encoder");
        return;
    }
    let (p, seq, m) = project(60);
    let path = tmp("hevc-audio.mp4");
    let s = ExportSettings { path: path.clone(), include_audio: true, ..hevc_settings() };
    export(&p, seq, &s, &m, &Progress::default()).expect("export");
    let bytes = std::fs::read(&path).unwrap();
    let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
    assert!(file.track_of_kind(filmcraft_isobmff::TrackKind::Video).is_some());
    let at = file.track_of_kind(filmcraft_isobmff::TrackKind::Audio).expect("an audio track");
    assert!(matches!(&file.tracks[at].entries[0].codec, CodecConfig::Aac(_)), "audio is {}", file.tracks[at].entries[0].codec.name());
}

#[test]
fn hevc_has_no_two_pass_mode() {
    filmcraft_platform::register();
    let (p, seq, m) = project(10);
    let s = ExportSettings { path: tmp("hevc-2pass.mp4"), include_audio: false, bitrate_mode: BitrateMode::Vbr2Pass, ..hevc_settings() };
    let err = export(&p, seq, &s, &m, &Progress::default()).expect_err("two-pass HEVC must be refused").to_string();
    assert!(err.contains("two-pass"), "{err}");
}

#[test]
fn hevc_hostile_configurations_give_errors_not_crashes() {
    let base = VtConfig {
        width: W,
        height: H,
        timescale: 30,
        frame_duration: 1,
        keyframe_interval: 30,
        profile: VtProfile::HevcMain,
        rate: VtRate::Vbr { target_kbps: 6000, max_kbps: 9000 },
    };
    let variants: Vec<(&str, VtConfig)> = vec![
        ("zero width", VtConfig { width: 0, ..base.clone() }),
        ("zero height", VtConfig { height: 0, ..base.clone() }),
        ("huge width", VtConfig { width: u32::MAX, ..base.clone() }),
        ("1x1", VtConfig { width: 1, height: 1, ..base.clone() }),
        ("2x2", VtConfig { width: 2, height: 2, ..base.clone() }),
        ("odd size", VtConfig { width: 321, height: 181, ..base.clone() }),
        ("zero timescale", VtConfig { timescale: 0, ..base.clone() }),
        ("zero frame duration", VtConfig { frame_duration: 0, ..base.clone() }),
        ("huge timescale", VtConfig { timescale: u32::MAX, ..base.clone() }),
        ("zero keyframe interval", VtConfig { keyframe_interval: 0, ..base.clone() }),
        ("huge keyframe interval", VtConfig { keyframe_interval: u32::MAX, ..base.clone() }),
        ("zero bitrate", VtConfig { rate: VtRate::Vbr { target_kbps: 0, max_kbps: 0 }, ..base.clone() }),
        ("huge bitrate", VtConfig { rate: VtRate::Vbr { target_kbps: u32::MAX, max_kbps: u32::MAX }, ..base.clone() }),
        ("max below target", VtConfig { rate: VtRate::Vbr { target_kbps: 6000, max_kbps: 1 }, ..base.clone() }),
        ("zero cbr", VtConfig { rate: VtRate::Cbr { kbps: 0 }, ..base.clone() }),
        ("cbr", VtConfig { rate: VtRate::Cbr { kbps: 4000 }, ..base.clone() }),
    ];
    for (name, config) in variants {
        match calm(name, || VtEncoder::new(config)) {
            Ok(mut enc) => {
                let (w, h) = (enc.config().width as usize, enc.config().height as usize);
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                assert!(calm(name, || enc.encode(0, &[], &[], &[])).is_err(), "{name}: empty planes");
                assert!(calm(name, || enc.encode(0, &vec![0; w * h - 1], &vec![128; cw * ch], &vec![128; cw * ch])).is_err(), "{name}: short luma");
                let _ = calm(name, || enc.encode(0, &vec![16; w * h], &vec![128; cw * ch], &vec![128; cw * ch]));
                let _ = calm(name, || enc.flush());
            }
            Err(why) => eprintln!("{name}: declined ({why})"),
        }
    }
}

#[test]
fn hevc_encoders_can_be_dropped_at_any_point() {
    let s = hevc_settings();
    for frames in [0u64, 1, 2, 5] {
        let Some(mut enc) = hardware(&s, W, H) else { return };
        for i in 0..frames {
            let rgba = picture(i);
            let _ = calm("encode", || enc.encode(&EncoderFrame { width: W, height: H, rgba: &rgba, hdr: None, index: i }));
        }
        drop(enc); // pending frames, no flush: must not crash or hang
    }
}

#[test]
fn hevc_pictures_keep_their_exact_size() {
    let s = hevc_settings();
    // even sizes that are not multiples of the coding block size come out at exactly their size
    for (w, h) in [(640u32, 362u32), (642, 360), (1280, 720), (1920, 1080), (64, 64)] {
        let Some(mut enc) = hardware(&s, w, h) else { continue };
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let mut packets = Vec::new();
        for i in 0..6u64 {
            packets.extend(enc.encode(&EncoderFrame { width: w, height: h, rgba: &rgba, hdr: None, index: i }).expect("encode"));
        }
        packets.extend(enc.flush().expect("flush"));
        assert_eq!(packets.len(), 6, "{w}x{h}");
        let entry = enc.sample_entry();
        let mut dec = filmcraft_codecs::software_video_decoder(&entry).expect("software decoder");
        let mut pictures = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            pictures.extend(dec.decode(&p.data, i as i64).expect("decode"));
        }
        pictures.extend(dec.flush());
        assert_eq!(pictures.len(), 6, "{w}x{h}");
        assert!(
            pictures.iter().all(|f| (f.frame.width, f.frame.height) == (w, h)),
            "{w}x{h}: decoded {}x{}",
            pictures[0].frame.width,
            pictures[0].frame.height
        );
    }
}

#[test]
fn ffmpeg_decodes_what_the_hevc_encoder_writes() {
    filmcraft_platform::register();
    let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("hardware HEVC export") else { return };
    let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("hardware HEVC export") else { return };
    if !hevc_available() {
        eprintln!("SKIPPED: no VideoToolbox hardware HEVC encoder");
        return;
    }
    let frames = 65;
    let (p, seq, m) = project(frames);
    for (name, multiplexer) in [("oracle-hevc.mp4", filmcraft_export::Multiplexer::Mp4), ("oracle-hevc.mov", filmcraft_export::Multiplexer::Mov)] {
        let path = tmp(name);
        let s = ExportSettings { path: path.clone(), include_audio: false, multiplexer, ..hevc_settings() };
        export(&p, seq, &s, &m, &Progress::default()).expect("export");
        // an independent decoder reads the whole file without a complaint
        let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-i", &path, "-f", "null", "-"]).output().expect("run ffmpeg");
        assert!(out.status.success() && out.stderr.is_empty(), "{name}: ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
        // and finds the stream we asked for: HEVC Main, tagged hvc1 (what QuickTime takes), 4:2:0 BT.709
        let out = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-count_frames",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,profile,codec_tag_string,pix_fmt,width,height,nb_read_frames,has_b_frames,color_space",
                "-of",
                "default=nw=1",
                &path,
            ])
            .output()
            .expect("run ffprobe");
        let info = String::from_utf8_lossy(&out.stdout).to_string();
        for want in [
            "codec_name=hevc",
            "profile=Main",
            "codec_tag_string=hvc1",
            "pix_fmt=yuv420p",
            &format!("width={W}"),
            &format!("height={H}"),
            &format!("nb_read_frames={frames}"),
            "has_b_frames=0",
            "color_space=bt709",
        ] {
            assert!(info.contains(want), "{name}: ffprobe lacks `{want}`:\n{info}");
        }
    }
}

/// The sessions are created with a hardware encoder required; ask VideoToolbox whether it agrees, so
/// that a requirement dropped by accident could never become a silent software encode.
#[test]
fn the_sessions_run_on_the_hardware_encoder() {
    let config = |profile| VtConfig {
        width: W,
        height: H,
        timescale: 30,
        frame_duration: 1,
        keyframe_interval: 30,
        profile,
        rate: VtRate::Vbr { target_kbps: 6000, max_kbps: 9000 },
    };
    for (name, profile) in [("H.264", VtProfile::High), ("HEVC", VtProfile::HevcMain)] {
        match VtEncoder::new(config(profile)) {
            Ok(enc) => assert!(enc.uses_hardware(), "{name}: VideoToolbox does not report a hardware encoder for the session"),
            Err(why) => eprintln!("SKIPPED {name}: no hardware encoder ({why})"),
        }
    }
}

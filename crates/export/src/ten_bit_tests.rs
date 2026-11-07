//! SDR exports to encoders that code more than 8 bits keep what the compositor holds: a 10-bit ramp
//! keeps its codes in the file. Encoders that code 8 bits still get RGBA8.

use super::*;
use crate::pipeline::{Pipeline, srgb_floats};
use filmcraft_color::ColorInfo;
use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaError, MediaInfo, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::{Image, SourceMap};
use filmcraft_time::TICKS_PER_SECOND;

const W: u32 = 1024;
const H: u32 = 128;

/// A 10-bit 4:2:2 picture: luma 64..=940 from left to right, every row alike, neutral chroma.
struct Ramp(MediaInfo);

impl MediaSource for Ramp {
    fn info(&self) -> &MediaInfo {
        &self.0
    }
    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let row = (0..W).map(|x| (64 + 876 * x / (W - 1)) as u16);
        let y: Vec<u16> = (0..H).flat_map(|_| row.clone()).collect();
        let c = Arc::new(vec![512u16; (W / 2 * H) as usize]);
        let data = PixelData::Yuv16 { planes: [Arc::new(y), c.clone(), c], chroma: Chroma::C422, bits: 10, alpha: None };
        Ok(Arc::new(VideoFrame { width: W, height: H, data, color: ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO }))
    }
    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}

/// One clip over a whole sequence: a source made from the info of a generator of the same size.
fn project_of(color: [f32; 4], source: impl Fn(&GeneratorSource) -> filmcraft_media::SharedSource) -> (Arc<Project>, ItemId, SourceMap) {
    let r = FrameRate::FPS_24;
    let g = GeneratorSource::new(Generator::ColorMatte { color }, W, H, r, Tick(TICKS_PER_SECOND));
    let info = g.info().clone();
    let mut p = Project::new("ten-bit");
    let clip = MediaClip {
        media: MediaRef::Generator(g.generator.clone()),
        info: info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item(&info.name, Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: r, ..Default::default() }, 1, 0, None);
    let ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(4)), r).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut m = SourceMap::default();
    m.0.insert(item, source(&g));
    (Arc::new(p), seq, m)
}

fn ramp() -> (Arc<Project>, ItemId, SourceMap) {
    project_of([0.0; 4], |g| Arc::new(Ramp(g.info().clone())))
}

fn settings(format: Format, name: &str) -> ExportSettings {
    let d = std::env::temp_dir().join(format!("fc-export-10bit-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    ExportSettings { format, path: d.join(name).to_string_lossy().to_string(), include_audio: false, ..Default::default() }
}

/// Distinct luma codes, in 10 bits, on the middle row of the exported file's first frame.
fn luma_levels(s: &ExportSettings) -> usize {
    let bytes: Arc<[u8]> = std::fs::read(&s.path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(&s.path, bytes).unwrap();
    let f = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
    let (w, row) = (f.width as usize, f.height as usize / 2);
    let mut codes: Vec<u32> = match &f.data {
        PixelData::Yuv16 { planes, bits, .. } => planes[0][row * w..(row + 1) * w].iter().map(|&v| u32::from(v) << 10 >> bits).collect(),
        PixelData::Yuv8 { planes, .. } => planes[0][row * w..(row + 1) * w].iter().map(|&v| u32::from(v) << 2).collect(),
        other => panic!("{}: unexpected frame {other:?}", s.path),
    };
    codes.sort_unstable();
    codes.dedup();
    codes.len()
}

/// An 8-bit stage in the path leaves about 256 of the ramp's 877 codes, whatever the wrapper says.
#[test]
fn a_10_bit_ramp_keeps_its_codes_in_10_bit_files() {
    let (p, seq, m) = ramp();
    let prores = |profile: &str, name: &str| ExportSettings { prores_profile: profile.into(), ..settings(Format::ProRes, name) };
    let dnx_hqx = |format, name: &str| ExportSettings { dnx_profile: "hqx".into(), ..settings(format, name) };
    let cases = [
        prores("hq", "hq.mov"),
        prores("lt", "lt.mov"),
        prores("4444", "4444.mov"),
        dnx_hqx(Format::DnxHr, "hqx.mov"),
        settings(Format::Apv, "apv.mov"),
        dnx_hqx(Format::MxfOp1a, "hqx.mxf"),
        ExportSettings { mxf_video_codec: MxfVideoCodec::ProRes, ..settings(Format::MxfOp1a, "prores.mxf") },
    ];
    for s in cases {
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let levels = luma_levels(&s);
        assert!(levels > 850, "{}: {levels} of 877 luma codes", s.path);
    }
}

/// DNxHR HQ and the default MXF code 8 bits, so nothing about their frames changes.
#[test]
fn only_encoders_above_8_bit_get_float_frames() {
    let (p, seq, m) = ramp();
    let dnx = |format, profile: &str, name: &str| ExportSettings { dnx_profile: profile.into(), ..settings(format, name) };
    let cases = [
        (settings(Format::ProRes, "a.mov"), true),
        (settings(Format::Apv, "a.mov"), true),
        (dnx(Format::DnxHr, "hqx", "a.mov"), true),
        (dnx(Format::MxfOp1a, "hqx", "a.mxf"), true),
        (ExportSettings { mxf_video_codec: MxfVideoCodec::ProRes, ..settings(Format::MxfOpAtom, "a.mxf") }, true),
        (dnx(Format::DnxHr, "lb", "a.mov"), false),
        (dnx(Format::DnxHr, "sq", "a.mov"), false),
        (dnx(Format::DnxHr, "hq", "a.mov"), false),
        (dnx(Format::MxfOp1a, "hq", "a.mxf"), false),
        (settings(Format::MxfOp1a, "a.mxf"), false),
        (ExportSettings { mxf_video_codec: MxfVideoCodec::H264, ..settings(Format::MxfOp1a, "a.mxf") }, false),
        (settings(Format::H264, "a.mp4"), false),
        (settings(Format::Mjpeg, "a.mov"), false),
    ];
    for (s, float) in cases {
        let name = format!("{} {:?} {:?}", s.format.label(), s.dnx_profile, s.mxf_video_codec);
        assert_eq!(s.encodes_above_8_bit(), float, "{name}");
        let (rgba, floats) = Pipeline::new(p.clone(), seq, &s, false).unwrap().frame(0, &m);
        let px = (W * H) as usize;
        assert_eq!((rgba.len(), floats.len()), if float { (0, px * 3) } else { (px * 4, 0) }, "{name}");
    }
}

/// Float frames are the RGBA8 picture with more precision: never more than the rounding of one
/// code apart, opaque or (ProRes 4444 with alpha) as straight colour next to the same alpha.
#[test]
fn float_frames_are_the_rgba8_picture_with_more_precision() {
    let close = |floats: &[f32], rgba: &[u8], name: &str| {
        assert_eq!(floats.len() / 3, rgba.len() / 4, "{name}");
        for (f, c) in floats.as_chunks::<3>().0.iter().zip(rgba.as_chunks::<4>().0) {
            for k in 0..3 {
                assert!((f[k] * 255.0 - f32::from(c[k])).abs() <= 1.0, "{name}: {f:?} and {c:?}");
            }
        }
    };
    let (p, seq, m) = ramp();
    let eight = Pipeline::new(p.clone(), seq, &settings(Format::H264, "a.mp4"), false).unwrap().frame(0, &m).0;
    let (_, floats) = Pipeline::new(p, seq, &settings(Format::ProRes, "a.mov"), false).unwrap().frame(0, &m);
    close(&floats, &eight, "opaque");

    let (p, seq, m) =
        project_of([1.0, 0.25, 0.0, 0.5], |g| Arc::new(GeneratorSource::new(g.generator.clone(), W, H, FrameRate::FPS_24, Tick(TICKS_PER_SECOND))));
    let s = ExportSettings { prores_profile: "4444".into(), alpha: true, ..settings(Format::ProRes, "a.mov") };
    let (rgba, floats) = Pipeline::new(p, seq, &s, false).unwrap().frame(0, &m);
    close(&floats, &rgba, "straight");
    assert!(rgba[3].abs_diff(128) <= 1, "{:?}", &rgba[..4]);
}

/// The in-flight budget's estimate of a frame ([`crate::job::frame_bytes`]) is the size of what
/// [`Pipeline::frame`] returns, whichever buffers a frame keeps.
#[test]
fn frame_bytes_is_what_a_frame_holds() {
    use filmcraft_color::{ColorPipeline, WorkingSpace};
    let (sdr, seq, m) = ramp();
    let mut hdr = (*sdr).clone();
    hdr.sequence_mut(seq).unwrap().settings.color = ColorPipeline { working: WorkingSpace::Rec2100Pq, ..ColorPipeline::REC709 };
    let hdr = Arc::new(hdr);
    let with_alpha = |profile: &str| ExportSettings { prores_profile: profile.into(), alpha: true, ..settings(Format::ProRes, "a.mov") };
    // (what, project, settings, HDR, bytes per pixel)
    let cases = [
        ("H.264", &sdr, settings(Format::H264, "a.mp4"), false, 4),
        ("ProRes HQ", &sdr, settings(Format::ProRes, "a.mov"), false, 12),
        ("ProRes 4444", &sdr, ExportSettings { prores_profile: "4444".into(), ..settings(Format::ProRes, "a.mov") }, false, 12),
        ("ProRes 4444 with alpha", &sdr, with_alpha("4444"), false, 16),
        ("ProRes HQ ignores alpha", &sdr, with_alpha("hq"), false, 12),
        ("HDR ProRes 4444 with alpha", &hdr, with_alpha("4444"), true, 12),
    ];
    for (what, p, s, hdr_out, per_pixel) in cases {
        let pipe = Pipeline::new(p.clone(), seq, &s, hdr_out).unwrap();
        let (rgba, floats) = pipe.frame(0, &m);
        let held = (rgba.len() + floats.len() * 4) as u64;
        assert_eq!(held, u64::from(W * H) * per_pixel, "{what}: bytes held");
        assert_eq!(crate::job::frame_bytes(pipe.w, pipe.h, pipe.float_frames, pipe.keeps_rgba_with_floats()), held, "{what}: estimate");
    }
}

#[test]
fn the_video_limiter_holds_on_float_frames() {
    let (p, seq, m) = ramp();
    let mut s = settings(Format::ProRes, "a.mov");
    s.effects.video_limiter.enabled = true;
    s.effects.video_limiter.max_percent = 50.0;
    let (_, floats) = Pipeline::new(p, seq, &s, false).unwrap().frame(0, &m);
    let brightest = floats.iter().copied().fold(0.0f32, f32::max);
    assert!((brightest - 0.5).abs() < 1e-3, "{brightest}");
}

#[test]
fn float_frames_stay_in_range_on_values_out_of_range() {
    let mut img = Image::new(5, 1);
    img.px.copy_from_slice(&[
        2.0,
        -1.0,
        f32::NAN,
        1.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0,
        1.0,
        0.5,
        0.5,
        0.5,
        0.0,
        0.5,
        0.25,
        0.0,
        0.5,
        0.1,
        0.1,
        0.1,
        f32::NAN,
    ]);
    let opaque = srgb_floats(&img, false);
    let white = filmcraft_color::linear_to_srgb(1.0);
    assert_eq!(&opaque[..6], &[white, 0.0, 0.0, white, 0.0, 0.0]);
    assert!(opaque.iter().all(|v| (0.0..=1.0).contains(v)), "{opaque:?}");
    let straight = srgb_floats(&img, true);
    assert_eq!(&straight[6..9], &[0.0; 3], "alpha 0 has no colour");
    assert_eq!(&straight[9..12], &[filmcraft_color::linear_to_srgb(1.0), filmcraft_color::linear_to_srgb(0.5), 0.0], "half alpha divides");
    assert!(straight.iter().all(|v| (0.0..=1.0).contains(v)), "{straight:?}");
    assert!(srgb_floats(&Image::new(0, 0), false).is_empty());
}

/// An RGBA8 picture of a red disc with a soft edge: opaque inside, transparent outside, and a band
/// of partial alpha (every code from 0 to 255) between.
struct SoftDisc(MediaInfo);

impl MediaSource for SoftDisc {
    fn info(&self) -> &MediaInfo {
        &self.0
    }
    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let mut px = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                let (dx, dy) = (x as f32 - W as f32 / 2.0, y as f32 - H as f32 / 2.0);
                let a = (255.0 * ((50.0 - (dx * dx + dy * dy).sqrt()) / 24.0 + 0.5).clamp(0.0, 1.0)) as u8;
                px.extend_from_slice(&[200, 40, 20, a]);
            }
        }
        Ok(Arc::new(VideoFrame { width: W, height: H, data: PixelData::Rgba8(Arc::new(px)), color: ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO }))
    }
    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}

/// DNxHR with Include Alpha Channel codes the picture's alpha losslessly: the file decodes with our
/// decoder, its alpha is the source's at the coded depth, and the sample entry and the frame header
/// say there is alpha. Without the setting there is none.
#[test]
fn a_dnxhr_export_with_alpha_keeps_its_alpha() {
    let (p, seq, m) = project_of([0.0; 4], |g| Arc::new(SoftDisc(g.info().clone())));
    for (profile, depth) in [("lb", 8u8), ("sq", 8), ("hq", 8), ("hqx", 10)] {
        let s = ExportSettings { dnx_profile: profile.into(), alpha: true, ..settings(Format::DnxHr, &format!("alpha-{profile}.mov")) };
        assert!(s.supports_alpha() && s.keeps_alpha(), "{profile}");
        // the alpha the pipeline hands the encoder (RGBA8, kept next to the floats at 10 bits)
        let (rgba, _) = Pipeline::new(p.clone(), seq, &s, false).unwrap().frame(0, &m);
        export(&p, seq, &s, &m, &Progress::default()).unwrap_or_else(|e| panic!("{profile}: {e}"));

        let data = std::fs::read(&s.path).unwrap();
        let file = filmcraft_isobmff::open(data.as_slice()).unwrap();
        let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        assert_eq!(file.tracks[vt].entries[0].video.as_ref().map(|v| v.depth), Some(32), "{profile}: sample entry depth");
        let sample = file.read_sample(data.as_slice(), vt, 0).unwrap();
        let header = filmcraft_dnx::probe(&sample).unwrap();
        assert!(header.alpha && header.lossless_alpha, "{profile}: header flags");
        let frame = filmcraft_dnx::decode_frame(&sample).unwrap();
        assert_eq!(frame.bit_depth, depth, "{profile}");
        let got = frame.alpha.as_ref().expect("the decoded frame has alpha");
        let want: Vec<u16> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let a10 = (u32::from(c[3]) * 1023 + 127) / 255;
                (if depth == 8 { ((a10 + 2) >> 2).min(255) } else { a10 }) as u16
            })
            .collect();
        assert_eq!(got.len(), want.len(), "{profile}");
        assert!(got.iter().zip(&want).all(|(a, b)| a == b), "{profile}: alpha differs");
        assert!(got.iter().any(|&a| a > 0 && a < 255 >> (8 - depth.min(8))), "{profile}: the edge is soft");

        let off = ExportSettings { alpha: false, ..s.clone() };
        assert!(!off.keeps_alpha());
        export(&p, seq, &off, &m, &Progress::default()).unwrap();
        let data = std::fs::read(&off.path).unwrap();
        let file = filmcraft_isobmff::open(data.as_slice()).unwrap();
        let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        assert_eq!(file.tracks[vt].entries[0].video.as_ref().map(|v| v.depth), Some(24), "{profile}: opaque entry");
        let sample = file.read_sample(data.as_slice(), vt, 0).unwrap();
        assert!(!filmcraft_dnx::probe(&sample).unwrap().alpha, "{profile}: no alpha without the setting");
    }
}

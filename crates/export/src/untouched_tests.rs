//! Untouched frames: a clip nothing is drawn on reaches a 4:2:0 encoder with the samples it was
//! decoded with; anything that changes the picture, or a frame the export would convert
//! differently, still goes through the renderer.
use super::*;
use crate::pipeline::Pipeline;
use filmcraft_color::{ColorInfo, Matrix, Range, Transfer};
use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource};
use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, ItemKind, Label, MediaClip, MediaRef, ParamValue, SequenceSettings, TrackId, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

const W: u32 = 64;
const H: u32 = 36;
const RATE: FrameRate = FrameRate::FPS_24;

/// The decoded picture of frame `n` of a camera clip (it changes from frame to frame).
fn picture(n: usize, color: ColorInfo, chroma: Chroma, alpha: bool) -> VideoFrame {
    let (w, h) = (W as usize, H as usize);
    let (sx, sy) = chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let y: Vec<u8> = (0..w * h).map(|i| (16 + (i * 7 + n * 13 + (i / w) * 3) % 220) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (60 + (i * 5 + n) % 130) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (70 + (i * 3 + 2 * n) % 120) as u8).collect();
    VideoFrame {
        width: W,
        height: H,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, alpha: alpha.then(|| Arc::new(vec![200u8; w * h])) },
        color,
        par: (1, 1),
        pts: Tick::ZERO,
    }
}

/// A camera clip decoding to [`picture`]s with the given colour, chroma sampling and alpha.
struct Camera {
    info: MediaInfo,
    color: ColorInfo,
    chroma: Chroma,
    alpha: bool,
}

impl MediaSource for Camera {
    /// The generator's `W` x `H` stream at `RATE`.
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    /// The [`picture`] of the frame at the requested time.
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let n = usize::try_from(RATE.frame_at(req.time)).unwrap_or(0);
        Ok(Arc::new(picture(n, self.color, self.chroma, self.alpha)))
    }
    /// Stereo silence.
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// A media item of `W` x `H` at `RATE`, read through `source`.
fn add_media(p: &mut Project, m: &mut SourceMap, name: &str, source: impl FnOnce(MediaInfo) -> Arc<dyn MediaSource>) -> ItemId {
    let g = GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.4, 0.6, 1.0] }, W, H, RATE, Tick(2 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let id = p.add_item(
        name,
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
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
    m.0.insert(id, source(info));
    id
}

/// One second of a camera clip on V1 of a `W` x `H` sequence (two video tracks), then `edit`. The
/// camera declares the colour and alpha its frames have, as a file's stream description would.
fn camera_project(color: ColorInfo, chroma: Chroma, alpha: bool, edit: impl FnOnce(&mut Project, ItemId, &mut SourceMap)) -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("untouched");
    let mut m = SourceMap::default();
    let cam = add_media(&mut p, &mut m, "camera", |mut info| {
        if let Some(v) = info.video.as_mut() {
            v.color = color;
            v.has_alpha = alpha;
        }
        Arc::new(Camera { info, color, chroma, alpha })
    });
    let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: RATE, ..Default::default() }, 2, 1, None);
    let clip = p.make_track_item(cam, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, RATE.tick_of(24)), RATE).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
    edit(&mut p, seq, &mut m);
    (Arc::new(p), seq, m)
}

/// H.264 export settings without audio, writing `name` in the test directory.
fn h264(name: &str) -> ExportSettings {
    ExportSettings { format: Format::H264, path: tmp(name), include_audio: false, ..Default::default() }
}

/// `name` in this process's scratch directory, created if needed.
fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-export-untouched-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// The three 8-bit planes of a decoded frame.
fn planes(f: &VideoFrame) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let PixelData::Yuv8 { planes, .. } = &f.data else { panic!("not 8-bit Y'CbCr") };
    (planes[0].to_vec(), planes[1].to_vec(), planes[2].to_vec())
}

/// A plain clip's frames, BT.709 or sRGB transfer, reach the encoder as the planes they were decoded
/// with.
#[test]
fn an_untouched_clip_reaches_the_encoder_as_it_was_decoded() {
    for color in [ColorInfo::REC709, ColorInfo { transfer: Transfer::Srgb, ..ColorInfo::REC709 }] {
        let (p, seq, m) = camera_project(color, Chroma::C420, false, |_, _, _| {});
        let pipe = Pipeline::new(p, seq, &h264("a.mp4"), false).unwrap();
        for f in 0..6 {
            let got = pipe.untouched_yuv420(f, &m).unwrap_or_else(|| panic!("{color:?}: frame {f} was rendered"));
            assert_eq!((got.y, got.u, got.v), planes(&picture(f as usize, color, Chroma::C420, false)), "{color:?}: frame {f}");
        }
    }
}

/// The clip on V1.
fn clip(p: &mut Project, seq: ItemId) -> &mut filmcraft_project::TrackItem {
    &mut p.sequence_mut(seq).unwrap().video_tracks[0].items[0]
}

/// Sets a float parameter of an effect of the clip on V1.
fn set(p: &mut Project, seq: ItemId, effect: &str, param: &str, value: f64) {
    clip(p, seq).effect_mut(effect).unwrap().param_mut(param).unwrap().value = ParamValue::Float(value);
}

/// A red matte on V2 over the second half second (frames 12..24).
fn matte_above(p: &mut Project, seq: ItemId, m: &mut SourceMap) {
    let top = add_media(p, m, "matte", |info| Arc::new(GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, W, H, RATE, info.duration)));
    let clip = p.make_track_item(top, TrackKind::Video, RATE.tick_of(12), TimeRange::new(Tick::ZERO, RATE.tick_of(12)), RATE).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[1].items.push(clip);
}

/// A subtitle caption over the whole clip.
fn caption(p: &mut Project, seq: ItemId, _: &mut SourceMap) {
    let mut track = CaptionTrack::new(TrackId(p.alloc_id()), "Subtitle".into(), CaptionFormat::Subtitle);
    let id = ClipId(p.alloc_id());
    track.captions.push(Caption {
        id,
        start: Tick::ZERO,
        duration: RATE.tick_of(24),
        text: "Hello".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    p.sequence_mut(seq).unwrap().caption_tracks.push(track);
}

/// Every edit that changes the picture (opacity, scale, an effect, a clip or caption above), every
/// export setting that changes it (limiter, overlays, another size, HDR) and every decoded format
/// the encoder would convert differently sends the frame through the renderer.
#[test]
fn anything_that_changes_the_picture_goes_through_the_renderer() {
    type Edit = fn(&mut Project, ItemId, &mut SourceMap);
    let none: Edit = |_, _, _| {};
    let rec709 = ColorInfo::REC709;
    let settings = |name: &str, tweak: fn(&mut ExportSettings)| {
        let mut s = h264(name);
        tweak(&mut s);
        s
    };
    let plain = |name: &str| h264(name);
    let cases: Vec<(&str, ColorInfo, Chroma, bool, Edit, ExportSettings, bool)> = vec![
        ("half opacity", rec709, Chroma::C420, false, |p, s, _| set(p, s, "opacity", "opacity", 50.0), plain("o.mp4"), false),
        ("scaled down", rec709, Chroma::C420, false, |p, s, _| set(p, s, "motion", "scale", 50.0), plain("s.mp4"), false),
        (
            "an effect",
            rec709,
            Chroma::C420,
            false,
            |p, s, _| clip(p, s).effects.push(filmcraft_project::find_effect("gaussian_blur").unwrap().instance()),
            plain("e.mp4"),
            false,
        ),
        ("a clip above", rec709, Chroma::C420, false, matte_above, plain("v2.mp4"), false),
        ("a caption burned in", rec709, Chroma::C420, false, caption, settings("c.mp4", |s| s.burn_captions = true), false),
        ("the video limiter", rec709, Chroma::C420, false, none, settings("l.mp4", |s| s.effects.video_limiter.enabled = true), false),
        ("a timecode overlay", rec709, Chroma::C420, false, none, settings("t.mp4", |s| s.effects.timecode_overlay.enabled = true), false),
        ("another frame size", rec709, Chroma::C420, false, none, settings("z.mp4", |s| s.frame_size = Some((32, 18))), false),
        ("an HDR export", rec709, Chroma::C420, false, none, plain("h.mp4"), true),
        ("BT.601 samples", ColorInfo { matrix: Matrix::Bt601, ..rec709 }, Chroma::C420, false, none, plain("601.mp4"), false),
        ("full-range samples", ColorInfo { range: Range::Full, ..rec709 }, Chroma::C420, false, none, plain("full.mp4"), false),
        ("4:2:2 samples", rec709, Chroma::C422, false, none, plain("422.mp4"), false),
        ("an alpha plane", rec709, Chroma::C420, true, none, plain("alpha.mp4"), false),
    ];
    for (name, color, chroma, alpha, edit, settings, hdr) in cases {
        let (p, seq, m) = camera_project(color, chroma, alpha, edit);
        let pipe = Pipeline::new(p, seq, &settings, hdr).unwrap();
        // frame 18: under the matte on V2 in that case
        assert!(pipe.untouched_yuv420(18, &m).is_none(), "{name}: the frame was not rendered");
    }
    // the matte covers only the second half: the first half is untouched
    let (p, seq, m) = camera_project(rec709, Chroma::C420, false, matte_above);
    let pipe = Pipeline::new(p, seq, &plain("half.mp4"), false).unwrap();
    assert!(pipe.untouched_yuv420(6, &m).is_some() && pipe.untouched_yuv420(18, &m).is_none());
    // the same size given explicitly is still the sequence's own
    let (p, seq, m) = camera_project(rec709, Chroma::C420, false, none);
    let pipe = Pipeline::new(p, seq, &settings("w.mp4", |s| s.frame_size = Some((W, H))), false).unwrap();
    assert!(pipe.untouched_yuv420(6, &m).is_some());
}

/// Mean absolute difference of two equally long sample planes.
fn mean_error(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len().max(1) as f64
}

/// Export the camera project (constant quality, nearly lossless) with untouched frames handed over
/// or not, and decode its luma and chroma with FilmCraft's own decoder.
fn export_and_decode(
    name: &str,
    untouched: bool,
    threads: usize,
    batch: i64,
    overlap: bool,
    edit: impl FnOnce(&mut Project, ItemId, &mut SourceMap),
) -> (Vec<u8>, Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>) {
    let (p, seq, m) = camera_project(ColorInfo::REC709, Chroma::C420, false, edit);
    let s = ExportSettings { bitrate_mode: BitrateMode::Crf, crf: 0.0, ..h264(name) };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    pool.install(|| {
        let prog = Progress::default();
        let mut ex = Exporter::new(p, seq, &s, &prog).unwrap();
        ex.set_batch(batch);
        ex.set_overlap(overlap);
        ex.set_untouched(untouched);
        while !matches!(ex.step(&m, &prog).unwrap(), Step::Done(_)) {}
    });
    let bytes = std::fs::read(&s.path).unwrap();
    let src = filmcraft_codecs::open_bytes(name, bytes.clone().into()).unwrap();
    let frames = (0..24).map(|f| planes(&src.video_frame(FrameRequest::full(RATE.tick_of(f) + Tick(1))).unwrap())).collect();
    (bytes, frames)
}

/// Untouched frames encoded from their decoded samples come back closer to the source than the same
/// frames rendered and converted again.
#[test]
fn untouched_footage_is_encoded_from_its_own_samples() {
    let (_, direct) = export_and_decode("direct.mp4", true, 3, 5, true, |_, _, _| {});
    let (_, rendered) = export_and_decode("rendered.mp4", false, 3, 5, true, |_, _, _| {});
    let (mut direct_err, mut rendered_err) = (0.0, 0.0);
    for (f, (d, r)) in direct.iter().zip(&rendered).enumerate() {
        let (y, u, v) = planes(&picture(f, ColorInfo::REC709, Chroma::C420, false));
        direct_err += mean_error(&d.0, &y) + mean_error(&d.1, &u) + mean_error(&d.2, &v);
        rendered_err += mean_error(&r.0, &y) + mean_error(&r.1, &u) + mean_error(&r.2, &v);
    }
    // the rendered path converts to linear light, 8-bit sRGB and back, a code off here and there;
    // the decoded samples skip that round trip and come back as encoded (CRF 0: next to lossless)
    assert!(direct_err < rendered_err, "untouched {direct_err:.3}, rendered {rendered_err:.3} (sum of mean errors over 24 frames)");
    assert!(direct_err / 24.0 < 0.5, "untouched frames came back {direct_err:.3} codes off");
}

/// An export mixing untouched and rendered frames is the same file on any core count and batch,
/// with the matte drawn only where it is.
#[test]
fn untouched_and_rendered_frames_mix_into_the_same_file_on_any_core_count() {
    // frames 12..24 have a matte above (rendered); 0..12 are untouched
    let (reference, frames) = export_and_decode("mix-ref.mp4", true, 1, 1, false, matte_above);
    for (threads, batch, overlap) in [(2, 3, true), (5, 7, true), (16, 24, false)] {
        let (other, _) = export_and_decode(&format!("mix-{threads}-{batch}.mp4"), true, threads, batch, overlap, matte_above);
        assert!(reference == other, "{threads} threads, {batch} per step: a different file");
    }
    // the second half shows the red matte on top, the first half the camera
    let red = |f: &(Vec<u8>, Vec<u8>, Vec<u8>)| f.2.iter().all(|&v| v > 200);
    assert!(frames[..12].iter().all(|f| !red(f)) && frames[12..].iter().all(red));
}

/// A source that counts the frames asked of it.
struct Counting {
    inner: Arc<dyn MediaSource>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl MediaSource for Counting {
    /// The wrapped source's.
    fn info(&self) -> &MediaInfo {
        self.inner.info()
    }
    /// Counts the request and passes it on.
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.video_frame(req)
    }
    /// The wrapped source's audio.
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
}

/// A source that declares frames the encoder can't take as they are (BT.601, full range, alpha)
/// is never decoded to look at them: the renderer asks it for each frame once, on any core count.
#[test]
fn a_source_that_declares_other_frames_is_decoded_once_per_frame() {
    let cases = [
        ("601", ColorInfo { matrix: Matrix::Bt601, ..ColorInfo::REC709 }, false),
        ("full range", ColorInfo { range: Range::Full, ..ColorInfo::REC709 }, false),
        ("alpha", ColorInfo::REC709, true),
    ];
    for (name, color, alpha) in cases {
        for threads in [1, 4] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = calls.clone();
            let (p, seq, m) = camera_project(color, Chroma::C420, alpha, move |_, _, m| {
                for source in m.0.values_mut() {
                    *source = Arc::new(Counting { inner: source.clone(), calls: counter.clone() });
                }
            });
            let s = h264(&format!("count-{name}-{threads}.mp4"));
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            pool.install(|| {
                let prog = Progress::default();
                let mut ex = Exporter::new(p, seq, &s, &prog).unwrap();
                ex.set_untouched(true);
                while !matches!(ex.step(&m, &prog).unwrap(), Step::Done(_)) {}
            });
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 24, "{name}, {threads} threads: frames asked of the source");
        }
    }
}

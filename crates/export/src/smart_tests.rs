//! Smart rendering: frames that show a ProRes / DNxHR source untouched are written as the source's
//! compressed samples, unchanged; everything else is rendered and encoded as before, in the same
//! track. The sources are made here with FilmCraft's own encoders and MOV writer, and opened
//! through the normal media path (`filmcraft_codecs::open_bytes`).
use super::*;
use filmcraft_color::{ColorPipeline, WorkingSpace};
use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource, SharedSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, ParamValue, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{Tick, TimeRange};

const W: u32 = 256;
const H: u32 = 144;
const RATE: FrameRate = FrameRate::FPS_24;
const N: usize = 24;

/// `name` in this process's scratch directory, created if needed.
fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-export-smart-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// 10-bit planes of picture `n` (it changes from frame to frame).
fn fill(n: usize, y: &mut [u16], cb: &mut [u16], cr: &mut [u16], w: usize) {
    for (i, v) in y.iter_mut().enumerate() {
        *v = (64 + (i % w * 3 + i / w * 2 + n * 17) % 876) as u16;
    }
    for (i, v) in cb.iter_mut().enumerate() {
        *v = (300 + (i * 5 + n * 11) % 400) as u16;
    }
    for (i, v) in cr.iter_mut().enumerate() {
        *v = (350 + (i * 3 + n * 7) % 300) as u16;
    }
}

/// `n` ProRes frames of `profile` (alpha coded when `alpha`), the frame headers saying `color`.
fn prores_samples(profile: filmcraft_prores::Profile, n: usize, alpha: bool, color: filmcraft_prores::ColorInfo) -> Vec<Vec<u8>> {
    prores_samples_sized(profile, W, H, n, alpha, color)
}

/// [`prores_samples`] of `w` x `h`.
fn prores_samples_sized(profile: filmcraft_prores::Profile, w: u32, h: u32, n: usize, alpha: bool, color: filmcraft_prores::ColorInfo) -> Vec<Vec<u8>> {
    let mut cfg = filmcraft_prores::EncoderConfig::new(profile, w, h);
    cfg.color = color;
    cfg.encode_alpha = alpha;
    let mut enc = filmcraft_prores::Encoder::with_config(cfg);
    (0..n)
        .map(|k| {
            let mut f = filmcraft_prores::Frame::new(w, h, profile.chroma(), 10, alpha);
            fill(k, &mut f.y, &mut f.cb, &mut f.cr, w as usize);
            if let Some(a) = f.alpha.as_mut() {
                a.iter_mut().enumerate().for_each(|(i, v)| *v = (i % 1024) as u16);
            }
            enc.encode(&f).unwrap()
        })
        .collect()
}

/// `n` DNxHR frames of `profile`.
fn dnx_samples(profile: filmcraft_dnx::Profile, n: usize) -> Vec<Vec<u8>> {
    let mut enc = filmcraft_dnx::Encoder::with_config(filmcraft_dnx::EncoderConfig::new(profile, W, H)).unwrap();
    (0..n)
        .map(|k| {
            let mut f = filmcraft_dnx::Frame::new(W, H, filmcraft_dnx::ChromaFormat::Yuv422, 10, false);
            fill(k, &mut f.y, &mut f.cb, &mut f.cr, W as usize);
            enc.encode(&f).unwrap()
        })
        .collect()
}

/// A MOV of `samples` at `rate` described by `entry` (labelled BT.709 unless `entry` says otherwise).
fn mov(name: &str, mut entry: SampleEntry, rate: FrameRate, samples: &[Vec<u8>]) -> (String, SharedSource) {
    if entry.video.as_ref().is_some_and(|v| v.color.is_none()) {
        ColorSignal::default().apply_to(&mut entry, true);
    }
    unlabelled_mov(name, entry, rate, samples)
}

/// A MOV of `samples` at `rate` described by `entry` as it is (no `colr` box unless it has one).
fn unlabelled_mov(name: &str, entry: SampleEntry, rate: FrameRate, samples: &[Vec<u8>]) -> (String, SharedSource) {
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let track = mux.add_track(TrackConfig::new(entry, rate.num as u32)).unwrap();
    for s in samples {
        mux.write_sample(track, WriteSample { data: s, duration: rate.den as u32, composition_offset: 0, is_sync: true }).unwrap();
    }
    let bytes = mux.finish().unwrap().into_inner();
    let path = tmp(name);
    std::fs::write(&path, &bytes).unwrap();
    let src = filmcraft_codecs::open_bytes(name, bytes.into()).unwrap();
    (path, src)
}

/// A `W` x `H` MOV sample description of `profile`, without colour.
fn prores_entry(profile: filmcraft_prores::Profile) -> SampleEntry {
    SampleEntry::prores(FourCc(profile.fourcc()), W as u16, H as u16)
}

/// A MOV of `N` ProRes 422 HQ frames at `RATE`, labelled BT.709: its path, source and samples.
fn hq_source(name: &str) -> (String, SharedSource, Vec<Vec<u8>>) {
    let samples = prores_samples(filmcraft_prores::Profile::Hq, N, false, Default::default());
    let (path, src) = mov(name, prores_entry(filmcraft_prores::Profile::Hq), RATE, &samples);
    (path, src, samples)
}

/// A media item reading `src`, registered in the source map.
fn add_item(p: &mut Project, m: &mut SourceMap, name: &str, media: MediaRef, src: SharedSource) -> ItemId {
    let info: MediaInfo = src.info().clone();
    let kind = ItemKind::Media(MediaClip {
        media,
        info,
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    });
    let id = p.add_item(name, Label::Iris, kind, None);
    m.0.insert(id, src);
    id
}

/// A `W` x `H` sequence at `rate` (two video tracks) showing the whole source on V1, then `edit`.
fn project(
    path: &str,
    src: SharedSource,
    settings: SequenceSettings,
    edit: impl FnOnce(&mut Project, ItemId, ItemId, &mut SourceMap),
) -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("smart");
    let mut m = SourceMap::default();
    let item = add_item(&mut p, &mut m, "source", MediaRef::File { path: path.into() }, src);
    let rate = settings.frame_rate;
    let seq = p.new_sequence("s", settings, 2, 1, None);
    let clip = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(N as i64)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
    edit(&mut p, seq, item, &mut m);
    (Arc::new(p), seq, m)
}

/// A `W` x `H` sequence at `RATE`.
fn seq_settings() -> SequenceSettings {
    SequenceSettings { width: W, height: H, frame_rate: RATE, ..Default::default() }
}

/// ProRes export settings of `profile`, without audio, writing `name`.
fn prores(name: &str, profile: &str) -> ExportSettings {
    ExportSettings { format: Format::ProRes, path: tmp(name), include_audio: false, prores_profile: profile.into(), ..Default::default() }
}

/// DNxHR export settings of `profile`, without audio, writing `name`.
fn dnxhr(name: &str, profile: &str) -> ExportSettings {
    ExportSettings { format: Format::DnxHr, path: tmp(name), include_audio: false, dnx_profile: profile.into(), ..Default::default() }
}

/// Export on a pool of `threads`, `batch` frames per step; the file and the frames copied.
fn export_with(p: Arc<Project>, seq: ItemId, m: &SourceMap, s: &ExportSettings, threads: usize, batch: i64, overlap: bool, copies: bool) -> (Vec<u8>, u64) {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let copied = pool.install(|| {
        let prog = Progress::default();
        let mut ex = Exporter::new(p, seq, s, &prog).unwrap();
        ex.set_batch(batch);
        ex.set_overlap(overlap);
        ex.set_copies(copies);
        while !matches!(ex.step(m, &prog).unwrap(), Step::Done(_)) {}
        ex.copied_frames()
    });
    (std::fs::read(&s.path).unwrap(), copied)
}

/// [`export_with`] on four threads, six frames per step, rendering ahead, taking copies.
fn export(p: Arc<Project>, seq: ItemId, m: &SourceMap, s: &ExportSettings) -> (Vec<u8>, u64) {
    export_with(p, seq, m, s, 4, 6, true, true)
}

/// The video samples of a MOV with their (dts, pts, duration, sync) and the track's timescale
/// and sample description fourcc.
fn video_samples(bytes: &[u8]) -> (Vec<Vec<u8>>, Vec<(i64, i64, u32, bool)>, u32, [u8; 4]) {
    let f = filmcraft_isobmff::open(bytes).unwrap();
    let t = f.tracks.iter().position(|t| t.kind == filmcraft_isobmff::TrackKind::Video).unwrap();
    let track = &f.tracks[t];
    let data = (0..track.samples.len()).map(|i| f.read_sample(bytes, t, i).unwrap()).collect();
    let timing = track.samples.iter().map(|s| (s.dts, s.pts, s.duration, s.is_sync)).collect();
    (data, timing, track.timescale, track.entries[0].format.0)
}

/// Decoded planes of frame `k` of `src` (16-bit Y'CbCr).
fn decoded(src: &dyn MediaSource, k: usize) -> Vec<Vec<u16>> {
    planes(&src.video_frame(FrameRequest::full(RATE.tick_of(k as i64))).unwrap())
}

/// The planes of a decoded Y'CbCr frame as 16-bit values.
fn planes(f: &filmcraft_frame::VideoFrame) -> Vec<Vec<u16>> {
    match &f.data {
        filmcraft_frame::PixelData::Yuv16 { planes, .. } => planes.iter().map(|p| p.to_vec()).collect(),
        filmcraft_frame::PixelData::Yuv8 { planes, .. } => planes.iter().map(|p| p.iter().map(|&v| u16::from(v)).collect()).collect(),
        other => panic!("not Y'CbCr: {:?}", std::mem::discriminant(other)),
    }
}

/// An untouched ProRes HQ clip exported to ProRes HQ is the source's samples, byte for byte, with
/// the source's timing and fourcc, and the copies are counted for `perf.stats`.
#[test]
fn an_untouched_prores_hq_clip_is_copied_sample_for_sample() {
    let (path, src, samples) = hq_source("hq.mov");
    let (p, seq, m) = project(&path, src, seq_settings(), |_, _, _, _| {});
    let before = smart_render_stats();
    let (bytes, copied) = export(p, seq, &m, &prores("hq-out.mov", "hq"));
    assert_eq!(copied, N as u64);
    assert!(smart_render_stats().copied >= before.copied + N as u64, "counted for perf.stats");
    let (out, timing, timescale, fourcc) = video_samples(&bytes);
    assert_eq!(&fourcc, b"apch");
    assert_eq!(timescale, 24);
    assert_eq!(out.len(), N, "frame count");
    for (k, (o, s)) in out.iter().zip(&samples).enumerate() {
        assert!(o == s, "sample {k} differs from the source's");
        assert_eq!(timing[k], (k as i64, k as i64, 1, true), "timing of sample {k}");
    }
}

/// Sets a float parameter of an effect of clip `clip` on V1.
fn set(p: &mut Project, seq: ItemId, clip: usize, effect: &str, param: &str, value: f64) {
    let c = &mut p.sequence_mut(seq).unwrap().video_tracks[0].items[clip];
    c.effect_mut(effect).unwrap().param_mut(param).unwrap().value = ParamValue::Float(value);
}

/// V1: the source in three clips, 0..8 as it is, 8..16 at half opacity, 16..24 blurred; V2: a red
/// matte over frames 4..6. Frames 0..4 and 6..8 are untouched.
fn mixed_edit(p: &mut Project, seq: ItemId, item: ItemId, m: &mut SourceMap) {
    let clips: Vec<_> = (0..3)
        .map(|k| p.make_track_item(item, TrackKind::Video, RATE.tick_of(8 * k), TimeRange::new(RATE.tick_of(8 * k), RATE.tick_of(8)), RATE).unwrap())
        .collect();
    p.sequence_mut(seq).unwrap().video_tracks[0].items = clips;
    set(p, seq, 1, "opacity", "opacity", 50.0);
    let blur = filmcraft_project::find_effect("gaussian_blur").unwrap().instance();
    p.sequence_mut(seq).unwrap().video_tracks[0].items[2].effects.push(blur);
    let matte = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, W, H, RATE, RATE.tick_of(48));
    let g = matte.generator.clone();
    let top = add_item(p, m, "matte", MediaRef::Generator(g), Arc::new(matte));
    let clip = p.make_track_item(top, TrackKind::Video, RATE.tick_of(4), TimeRange::new(Tick::ZERO, RATE.tick_of(2)), RATE).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[1].items.push(clip);
}

const UNTOUCHED: [usize; 6] = [0, 1, 2, 3, 6, 7];

/// Only the frames nothing is drawn on are copied; the others are the same packets as an export
/// without copies, and the file decodes to the source, the matte and the faded frames.
#[test]
fn only_the_frames_nothing_is_drawn_on_are_copied() {
    let (path, src, samples) = hq_source("mixed.mov");
    let (p, seq, m) = project(&path, src.clone(), seq_settings(), mixed_edit);
    let (bytes, copied) = export(p.clone(), seq, &m, &prores("mixed-out.mov", "hq"));
    let (reference, none) = export_with(p, seq, &m, &prores("mixed-ref.mov", "hq"), 4, 6, true, false);
    assert_eq!((copied, none), (UNTOUCHED.len() as u64, 0));
    let (out, timing, _, _) = video_samples(&bytes);
    let (rendered, _, _, _) = video_samples(&reference);
    assert_eq!(out.len(), N);
    for k in 0..N {
        assert_eq!(timing[k], (k as i64, k as i64, 1, true), "timing of sample {k}");
        if UNTOUCHED.contains(&k) {
            assert!(out[k] == samples[k], "frame {k}: not the source's sample");
        } else {
            // ProRes encodes every frame on its own: the rendered frames are the same as without copies
            assert!(out[k] == rendered[k], "frame {k}: not rendered and encoded as before");
        }
    }
    // our decoder: copied frames are the source's pictures; the matte frames are red
    let file = filmcraft_codecs::open_bytes("mixed-out.mov", bytes.into()).unwrap();
    for k in UNTOUCHED {
        assert!(decoded(&*file, k) == decoded(&*src, k), "frame {k} decodes differently from the source");
    }
    for k in [4, 5] {
        let cr = &decoded(&*file, k)[2];
        assert!(cr.iter().all(|&v| v > 800), "frame {k}: not the red matte");
    }
    let first_opacity_frame = &decoded(&*file, 8)[0];
    assert!(first_opacity_frame != &decoded(&*src, 8)[0], "frame 8 is at half opacity");
}

/// The file with copies is the same on any thread count, batch size and with or without
/// rendering ahead.
#[test]
fn the_file_is_the_same_on_any_core_count_and_batch() {
    let (path, src, _) = hq_source("det.mov");
    let (p, seq, m) = project(&path, src, seq_settings(), mixed_edit);
    let (reference, copied) = export_with(p.clone(), seq, &m, &prores("det-ref.mov", "hq"), 1, 1, false, true);
    assert_eq!(copied, UNTOUCHED.len() as u64);
    for (threads, batch, overlap) in [(2, 3, true), (5, 7, true), (16, 24, false), (3, 1, true)] {
        let (other, n) = export_with(p.clone(), seq, &m, &prores(&format!("det-{threads}-{batch}.mov"), "hq"), threads, batch, overlap, true);
        assert_eq!(n, copied);
        assert!(reference == other, "{threads} threads, {batch} per step: a different file");
    }
}

/// Export `settings` of a project and say how many frames were copied.
fn copies(
    path: &str,
    src: SharedSource,
    seq: SequenceSettings,
    edit: impl FnOnce(&mut Project, ItemId, ItemId, &mut SourceMap),
    settings: &ExportSettings,
) -> u64 {
    let (p, seq, m) = project(path, src, seq, edit);
    export(p, seq, &m, settings).1
}

/// Nothing is copied into another ProRes profile, frame size or frame rate, from a speed change or
/// a reversed clip, or into an export that draws on the frame or writes HDR.
#[test]
fn a_different_flavour_size_rate_or_timing_is_rendered() {
    let none = |_: &mut Project, _: ItemId, _: ItemId, _: &mut SourceMap| {};
    let (path, src, _) = hq_source("cases.mov");
    // the same source copies into HQ...
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &prores("c-hq.mov", "hq")), N as u64);
    // ...but not into another profile
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &prores("c-lt.mov", "lt")), 0, "HQ source, LT export");
    let lt = prores_samples(filmcraft_prores::Profile::Lt, N, false, Default::default());
    let (lt_path, lt_src) = mov("lt.mov", prores_entry(filmcraft_prores::Profile::Lt), RATE, &lt);
    assert_eq!(copies(&lt_path, lt_src.clone(), seq_settings(), none, &prores("c-lt-hq.mov", "hq")), 0, "LT source, HQ export");
    assert_eq!(copies(&lt_path, lt_src, seq_settings(), none, &prores("c-lt-lt.mov", "lt")), N as u64, "LT source, LT export");
    // another output size
    let half = ExportSettings { frame_size: Some((W / 2, H / 2)), ..prores("c-size.mov", "hq") };
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &half), 0, "another frame size");
    // a sequence at another rate than the source (frames sampled, not copied)
    let at25 = SequenceSettings { frame_rate: FrameRate::FPS_25, ..seq_settings() };
    assert_eq!(copies(&path, src.clone(), at25, none, &prores("c-25.mov", "hq")), 0, "24 fps source, 25 fps export");
    let out25 = ExportSettings { frame_rate: Some(FrameRate::FPS_25), ..prores("c-out25.mov", "hq") };
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &out25), 0, "24 fps source, 25 fps output rate");
    // a speed change (Frame Sampling: the renderer shows source frames as they are)
    let speed = |p: &mut Project, seq: ItemId, _: ItemId, _: &mut SourceMap| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].speed = 2.0;
    assert_eq!(copies(&path, src.clone(), seq_settings(), speed, &prores("c-speed.mov", "hq")), 0, "speed 200 %");
    let reverse = |p: &mut Project, seq: ItemId, _: ItemId, _: &mut SourceMap| p.sequence_mut(seq).unwrap().video_tracks[0].items[0].reverse = true;
    assert_eq!(copies(&path, src.clone(), seq_settings(), reverse, &prores("c-rev.mov", "hq")), 0, "reversed");
    // the export draws on the frame
    let mut tc = prores("c-tc.mov", "hq");
    tc.effects.timecode_overlay.enabled = true;
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &tc), 0, "timecode overlay");
    let mut lim = prores("c-lim.mov", "hq");
    lim.effects.video_limiter.enabled = true;
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &lim), 0, "video limiter");
    // HDR output
    let hdr = SequenceSettings { color: ColorPipeline { working: WorkingSpace::Rec2100Hlg, ..Default::default() }, ..seq_settings() };
    assert_eq!(copies(&path, src, hdr, none, &prores("c-hdr.mov", "hq")), 0, "HDR export");
}

/// ProRes 4444 with alpha is not copied into an export without (or with) alpha, 4444 without alpha
/// is; a BT.2020 container colour or BT.601 frame headers are not copied, unspecified frame
/// headers under a BT.709 container are.
#[test]
fn alpha_and_colour_must_match_the_export() {
    let none = |_: &mut Project, _: ItemId, _: ItemId, _: &mut SourceMap| {};
    use filmcraft_prores::{ColorInfo as Codes, Profile};
    // 4444 with alpha into 4444 without (the alpha would be dropped, not copied)
    let alpha = prores_samples(Profile::P4444, N, true, Default::default());
    let (path, src) = mov("alpha.mov", prores_entry(Profile::P4444), RATE, &alpha);
    assert!(src.info().video.as_ref().unwrap().has_alpha);
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &prores("a-flat.mov", "4444")), 0, "alpha dropped");
    let keep = ExportSettings { alpha: true, ..prores("a-keep.mov", "4444") };
    assert_eq!(copies(&path, src, seq_settings(), none, &keep), 0, "alpha kept: the export composites over nothing, still not copied");
    // 4444 without alpha copies into 4444
    let opaque = prores_samples(Profile::P4444, N, false, Default::default());
    let (path, src) = mov("opaque.mov", prores_entry(Profile::P4444), RATE, &opaque);
    assert_eq!(copies(&path, src, seq_settings(), none, &prores("a-opaque.mov", "4444")), N as u64);
    // another colour in the container...
    let mut e = prores_entry(Profile::Hq);
    e.video.as_mut().unwrap().color = Some(filmcraft_isobmff::ColorInfo::Nclc { primaries: 9, transfer: 1, matrix: 9 });
    let samples = prores_samples(Profile::Hq, N, false, Default::default());
    let (path, src) = mov("c2020.mov", e, RATE, &samples);
    assert_eq!(copies(&path, src, seq_settings(), none, &prores("c-2020.mov", "hq")), 0, "BT.2020 container colour");
    // ...or in the frame headers
    let samples = prores_samples(Profile::Hq, N, false, Codes { primaries: 1, transfer: 1, matrix: 6 });
    let (path, src) = mov("c601.mov", prores_entry(Profile::Hq), RATE, &samples);
    assert_eq!(copies(&path, src, seq_settings(), none, &prores("c-601.mov", "hq")), 0, "BT.601 frame headers");
    // unspecified colour in the frame headers: the container's BT.709 applies
    let samples = prores_samples(Profile::Hq, N, false, Codes { primaries: 2, transfer: 2, matrix: 2 });
    let (path, src) = mov("c-unspec.mov", prores_entry(Profile::Hq), RATE, &samples);
    assert_eq!(copies(&path, src, seq_settings(), none, &prores("c-unspec-out.mov", "hq")), N as u64, "unspecified frame colour");
}

/// DNxHR copies into an export of its own profile only, byte for byte with the source's timing.
#[test]
fn dnxhr_copies_into_the_same_profile_only() {
    use filmcraft_dnx::Profile;
    let none = |_: &mut Project, _: ItemId, _: ItemId, _: &mut SourceMap| {};
    let entry = || SampleEntry::dnx(FourCc(*b"AVdh"), W as u16, H as u16);
    let hq = dnx_samples(Profile::Hq, N);
    let (path, src) = mov("dnx-hq.mov", entry(), RATE, &hq);
    let (p, seq, m) = project(&path, src.clone(), seq_settings(), none);
    let (bytes, copied) = export(p, seq, &m, &dnxhr("dnx-hq-out.mov", "hq"));
    assert_eq!(copied, N as u64);
    let (out, timing, _, fourcc) = video_samples(&bytes);
    assert_eq!(&fourcc, b"AVdh");
    for k in 0..N {
        assert!(out[k] == hq[k], "sample {k}");
        assert_eq!(timing[k], (k as i64, k as i64, 1, true));
    }
    let lb = dnx_samples(Profile::Lb, N);
    let (path, src) = mov("dnx-lb.mov", entry(), RATE, &lb);
    assert_eq!(copies(&path, src.clone(), seq_settings(), none, &dnxhr("dnx-lb-hq.mov", "hq")), 0, "LB source, HQ export");
    assert_eq!(copies(&path, src, seq_settings(), none, &dnxhr("dnx-lb-lb.mov", "lb")), N as u64, "LB source, LB export");
}

/// Truncated, resized, oversized or padded ProRes samples and DNxHR samples of another compression
/// ID or cut short are rendered instead, and the rest of the export still copies.
#[test]
fn damaged_or_lying_samples_are_rendered_and_the_export_succeeds() {
    let none = |_: &mut Project, _: ItemId, _: ItemId, _: &mut SourceMap| {};
    let mut samples = prores_samples(filmcraft_prores::Profile::Hq, N, false, Default::default());
    // 3: truncated; 5: a frame header that says another width; 7: a frame size past the sample;
    // 9: one byte appended (the frame header's size no longer matches the sample's)
    let cut = samples[3].len() - 100;
    samples[3].truncate(cut);
    samples[5][16..18].copy_from_slice(&((W - 16) as u16).to_be_bytes());
    let len = samples[7].len() as u32 + 64;
    samples[7][0..4].copy_from_slice(&len.to_be_bytes());
    samples[9].push(0);
    let (path, src) = mov("lies.mov", prores_entry(filmcraft_prores::Profile::Hq), RATE, &samples);
    let (p, seq, m) = project(&path, src, seq_settings(), none);
    let (bytes, copied) = export(p, seq, &m, &prores("lies-out.mov", "hq"));
    assert_eq!(copied, N as u64 - 4);
    let (out, _, _, _) = video_samples(&bytes);
    assert_eq!(out.len(), N);
    for k in [3, 5, 7, 9] {
        assert!(out[k] != samples[k], "damaged sample {k} was copied");
        filmcraft_prores::check_frame(&out[k]).expect("re-encoded frame");
    }
    // DNxHR: a compression ID that is not the export's, and a sample cut short
    use filmcraft_dnx::Profile;
    let mut dnx = dnx_samples(Profile::Hq, N);
    dnx[2][0x28..0x2C].copy_from_slice(&Profile::Lb.cid().to_be_bytes());
    let cut = dnx[4].len() / 2;
    dnx[4].truncate(cut);
    let (path, src) = mov("dnx-lies.mov", SampleEntry::dnx(FourCc(*b"AVdh"), W as u16, H as u16), RATE, &dnx);
    let (p, seq, m) = project(&path, src, seq_settings(), none);
    let (bytes, copied) = export(p, seq, &m, &dnxhr("dnx-lies-out.mov", "hq"));
    assert_eq!(copied, N as u64 - 2);
    let (out, _, _, _) = video_samples(&bytes);
    assert!(out[2] != dnx[2] && out[4] != dnx[4]);
}

/// The sample check accepts the encoders' own samples and returns an error, never a panic, for
/// truncated and bit-flipped ones.
#[test]
fn the_sample_check_survives_hostile_bytes() {
    let copy = SampleCopy {
        flavour: CopyFlavour::ProRes { profile: filmcraft_prores::Profile::Hq, alpha: false },
        width: W,
        height: H,
        rate: RATE,
        signal: ColorSignal::default(),
    };
    let dnx_copy = SampleCopy { flavour: CopyFlavour::DnxHr { profile: filmcraft_dnx::Profile::Hq, bit_depth: 8 }, ..copy };
    let sample = |codec, fourcc: &[u8; 4], data: Vec<u8>| filmcraft_media::CompressedSample {
        codec,
        fourcc: *fourcc,
        width: W,
        height: H,
        color: Some(filmcraft_color::ColorInfo::REC709),
        par: (1, 1),
        index: 0,
        data,
    };
    let prores = prores_samples(filmcraft_prores::Profile::Hq, 1, false, Default::default()).remove(0);
    let dnx = dnx_samples(filmcraft_dnx::Profile::Hq, 1).remove(0);
    assert!(copy.check(&sample(filmcraft_media::IntraCodec::ProRes, b"apch", prores.clone())).is_ok());
    assert!(dnx_copy.check(&sample(filmcraft_media::IntraCodec::Dnx, b"AVdh", dnx.clone())).is_ok());
    let mut rng = 0x1234_5678_u32;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng as usize
    };
    for (c, codec, fourcc, good) in [(copy, filmcraft_media::IntraCodec::ProRes, b"apch", prores), (dnx_copy, filmcraft_media::IntraCodec::Dnx, b"AVdh", dnx)] {
        for len in [0, 1, 7, 8, 27, 28, 100, 0x27F, 0x280, 0x2000, good.len() - 1] {
            let data = good.get(..len.min(good.len())).unwrap().to_vec();
            let r = std::panic::catch_unwind(|| c.check(&sample(codec, fourcc, data)));
            assert!(r.is_ok_and(|r| r.is_err()), "truncated to {len}");
        }
        for _ in 0..2000 {
            let mut data = good.clone();
            for _ in 0..1 + next() % 4 {
                let i = next() % 0x300.min(data.len());
                data[i] ^= 1 << (next() % 8);
            }
            let r = std::panic::catch_unwind(|| c.check(&sample(codec, fourcc, data)));
            assert!(r.is_ok(), "the check panicked on flipped bits");
        }
    }
}

/// [`filmcraft_render::plan::untouched_clip`] finds a frame exactly where `untouched_frame` does,
/// and names the source frame it returns, through cuts, effects, scaling and speed changes.
#[test]
fn untouched_clip_names_the_frame_untouched_frame_returns() {
    use filmcraft_render::RenderOptions;
    type Edit = fn(&mut Project, ItemId, ItemId, &mut SourceMap);
    let cases: [(&str, Edit); 5] = [
        ("plain", |_, _, _, _| {}),
        ("mixed", mixed_edit),
        ("scaled", |p, s, _, _| set(p, s, 0, "motion", "scale", 50.0)),
        ("speed 2 sampled", |p, s, _, _| p.sequence_mut(s).unwrap().video_tracks[0].items[0].speed = 2.0),
        ("speed 1.5 blended", |p, s, _, _| {
            let c = &mut p.sequence_mut(s).unwrap().video_tracks[0].items[0];
            c.speed = 1.5;
            c.time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
        }),
    ];
    let (path, src, _) = hq_source("agree.mov");
    for (name, edit) in cases {
        let (p, seq, m) = project(&path, src.clone(), seq_settings(), edit);
        let opts = RenderOptions::default();
        for k in 0..N as i64 {
            let t = RATE.tick_of(k);
            let frame = filmcraft_render::plan::untouched_frame(&p, seq, t, opts, &m);
            let clip = filmcraft_render::plan::untouched_clip(&p, seq, t, opts, &m);
            assert_eq!(frame.is_some(), clip.is_some(), "{name}: frame {k}");
            if let (Some(frame), Some(clip)) = (frame, clip) {
                let named = src.video_frame(FrameRequest::full(clip.time)).unwrap();
                assert!(Arc::ptr_eq(&frame, &named) || planes(&frame) == planes(&named), "{name}: frame {k} is not the one named");
            }
        }
    }
}

/// Samples of an MXF source are copied into a MOV export as they are stored in the MXF.
#[test]
fn an_mxf_source_copies_too() {
    // the HQ source exported to MXF without copies (re-encoded), then that MXF exported to MOV
    let (path, src, _) = hq_source("for-mxf.mov");
    let (p, seq, m) = project(&path, src, seq_settings(), |_, _, _, _| {});
    let mxf = ExportSettings { format: Format::MxfOp1a, mxf_video_codec: MxfVideoCodec::ProRes, ..prores("src.mxf", "hq") };
    let (mxf_bytes, _) = export_with(p, seq, &m, &mxf, 2, 4, true, false);
    let mxf_src = filmcraft_codecs::open_bytes("src.mxf", mxf_bytes.into()).unwrap();
    let wanted: Vec<Vec<u8>> = (0..N).map(|k| mxf_src.compressed_video_sample(RATE.tick_of(k as i64), u64::MAX).unwrap().unwrap().data).collect();
    let mxf_path = mxf.path.clone();
    let (p, seq, m) = project(&mxf_path, mxf_src, seq_settings(), |_, _, _, _| {});
    let (bytes, copied) = export(p, seq, &m, &prores("from-mxf.mov", "hq"));
    assert_eq!(copied, N as u64);
    let (out, _, _, _) = video_samples(&bytes);
    assert!(out == wanted, "the MXF's samples");
}

/// A sample is copied when the colour the source decodes it with is BT.709, whatever the frame
/// size: without a container colour the ProRes and DNx decoders give BT.709 for unspecified codes
/// at any size (no SD default), so an SD source without colour is copied, as an HD one is. A
/// BT.601 frame header, or a `colr` box that leaves the matrix unspecified (resolved by size:
/// BT.601 for SD), decodes as BT.601 and is rendered. The check agrees with the decoded frame in
/// every case.
#[test]
fn the_colour_the_source_decodes_with_decides_a_copy() {
    use filmcraft_color::{ColorInfo, Matrix};
    use filmcraft_prores::{ColorInfo as Codes, Profile};
    let none = |_: &mut Project, _: ItemId, _: ItemId, _: &mut SourceMap| {};
    let unspecified = Codes { primaries: 2, transfer: 2, matrix: 2 };
    let colr_unspecified = Some(filmcraft_isobmff::ColorInfo::Nclc { primaries: 2, transfer: 2, matrix: 2 });
    let entry = |w: u32, h: u32, colr: Option<filmcraft_isobmff::ColorInfo>| {
        let mut e = SampleEntry::prores(FourCc(Profile::Hq.fourcc()), w as u16, h as u16);
        e.video.as_mut().unwrap().color = colr;
        e
    };
    let bt601 = ColorInfo { matrix: Matrix::Bt601, ..ColorInfo::REC709 };
    // (case, size, container colour, frame header codes, the colour the source decodes with)
    let cases = [
        ("SD without colour", (W, H), None, unspecified, ColorInfo::REC709),
        ("SD, header codes 0", (W, H), None, Codes { primaries: 0, transfer: 0, matrix: 0 }, ColorInfo::REC709),
        ("SD, BT.709 header", (W, H), None, Codes { primaries: 1, transfer: 1, matrix: 1 }, ColorInfo::REC709),
        ("HD without colour", (1280, 720), None, unspecified, ColorInfo::REC709),
        ("SD, BT.601 header", (W, H), None, Codes { primaries: 1, transfer: 1, matrix: 6 }, bt601),
        ("SD, colr without a matrix", (W, H), colr_unspecified.clone(), unspecified, bt601),
    ];
    for (k, (name, (w, h), colr, codes, want)) in cases.into_iter().enumerate() {
        let samples = prores_samples_sized(Profile::Hq, w, h, 1, false, codes);
        let (_, src) = unlabelled_mov(&format!("colour-{k}.mov"), entry(w, h, colr), RATE, &samples);
        let sample = src.compressed_video_sample(Tick::ZERO, u64::MAX).unwrap().unwrap();
        let decoded = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().color;
        let copy =
            SampleCopy { flavour: CopyFlavour::ProRes { profile: Profile::Hq, alpha: false }, width: w, height: h, rate: RATE, signal: ColorSignal::default() };
        let checked = copy.check(&sample);
        assert_eq!(checked.is_ok(), decoded == ColorInfo::REC709, "{name}: the check ({checked:?}) disagrees with the decoded {decoded:?}");
        assert_eq!((sample.color, decoded), (Some(want), want), "{name}");
    }
    // DNxHR without colour decodes as BT.709 too
    let dnx = dnx_samples(filmcraft_dnx::Profile::Hq, 1);
    let (_, src) = unlabelled_mov("colour-dnx.mov", SampleEntry::dnx(FourCc(*b"AVdh"), W as u16, H as u16), RATE, &dnx);
    let sample = src.compressed_video_sample(Tick::ZERO, u64::MAX).unwrap().unwrap();
    assert_eq!((sample.color, src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().color), (Some(ColorInfo::REC709), ColorInfo::REC709));
    let dnx_copy = SampleCopy {
        flavour: CopyFlavour::DnxHr { profile: filmcraft_dnx::Profile::Hq, bit_depth: 8 },
        width: W,
        height: H,
        rate: RATE,
        signal: ColorSignal::default(),
    };
    assert_eq!(dnx_copy.check(&sample), Ok(()));
    // end to end: the SD source without colour is copied, the one whose `colr` leaves the matrix to
    // the frame size is rendered
    let samples = prores_samples(Profile::Hq, N, false, unspecified);
    let (path, src) = unlabelled_mov("colour-sd.mov", entry(W, H, None), RATE, &samples);
    assert_eq!(copies(&path, src, seq_settings(), none, &prores("colour-sd-out.mov", "hq")), N as u64, "SD without colour");
    let (path, src) = unlabelled_mov("colour-sd-colr.mov", entry(W, H, colr_unspecified), RATE, &samples);
    assert_eq!(copies(&path, src, seq_settings(), none, &prores("colour-sd-colr-out.mov", "hq")), 0, "SD, colr without a matrix");
}

/// `frame` with `n` zero bytes after the slices of its (first) picture, the picture and frame sizes
/// grown to cover them: a decoder skips them, so it decodes as before.
fn pad_picture(frame: &[u8], n: usize) -> Vec<u8> {
    let off = 8 + usize::from(u16::from_be_bytes([frame[8], frame[9]]));
    let pic = u32::from_be_bytes(frame[off + 1..off + 5].try_into().unwrap()) as usize;
    let mut v = frame.to_vec();
    v.splice(off + pic..off + pic, std::iter::repeat_n(0u8, n));
    v[off + 1..off + 5].copy_from_slice(&((pic + n) as u32).to_be_bytes());
    let size = v.len() as u32;
    v[0..4].copy_from_slice(&size.to_be_bytes());
    v
}

/// A ProRes sample whose sizes are consistent but which carries bytes no slice covers, inside its
/// picture or after it, decodes like the frame without them but is rendered and encoded instead
/// of copied (its size would not follow from what it codes), and the export succeeds. The padded
/// samples stay under the per-frame cap, so the frame check is what refuses them.
#[test]
fn padded_prores_samples_are_rendered() {
    let none = |_: &mut Project, _: ItemId, _: ItemId, _: &mut SourceMap| {};
    let mut samples = prores_samples(filmcraft_prores::Profile::Hq, N, false, Default::default());
    let cap = crate::job::frame_bytes(W, H, false) as usize;
    // 3: padding inside the picture, after its slices; 5: padding after the last picture
    samples[3] = pad_picture(&samples[3], 64 << 10);
    samples[5].extend(std::iter::repeat_n(0u8, 64 << 10));
    let size = samples[5].len() as u32;
    samples[5][0..4].copy_from_slice(&size.to_be_bytes());
    for k in [3, 5] {
        assert!(samples[k].len() < cap, "sample {k} is over the size cap");
        let padded = filmcraft_prores::decode_frame(&samples[k]).expect("a padded frame decodes");
        assert!(padded == filmcraft_prores::decode_frame(&prores_samples(filmcraft_prores::Profile::Hq, N, false, Default::default())[k]).unwrap());
    }
    let (path, src) = mov("padded.mov", prores_entry(filmcraft_prores::Profile::Hq), RATE, &samples);
    let (p, seq, m) = project(&path, src, seq_settings(), none);
    let (bytes, copied) = export(p, seq, &m, &prores("padded-out.mov", "hq"));
    assert_eq!(copied, N as u64 - 2);
    let (out, _, _, _) = video_samples(&bytes);
    for k in [3, 5] {
        assert!(out[k] != samples[k], "padded sample {k} was copied");
        filmcraft_prores::check_frame(&out[k]).expect("re-encoded frame");
    }
}

/// An in-memory reader that counts the bytes it is asked for and remembers the largest read.
struct CountingReader {
    inner: filmcraft_media::reader::MemReader,
    total: std::sync::atomic::AtomicU64,
    largest: std::sync::atomic::AtomicU64,
}

impl filmcraft_media::reader::ByteReader for CountingReader {
    /// The bytes' length.
    fn len(&self) -> u64 {
        self.inner.len()
    }
    /// Counts the read and passes it on.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        self.total.fetch_add(buf.len() as u64, SeqCst);
        self.largest.fetch_max(buf.len() as u64, SeqCst);
        self.inner.read_at(offset, buf)
    }
}

/// A source that records the caps it is asked for compressed samples with.
struct CapSeen {
    inner: SharedSource,
    caps: std::sync::Mutex<Vec<u64>>,
}

impl MediaSource for CapSeen {
    /// The wrapped source's.
    fn info(&self) -> &MediaInfo {
        self.inner.info()
    }
    /// The wrapped source's frame.
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        self.inner.video_frame(req)
    }
    /// The wrapped source's audio.
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
    /// Records `max_bytes` and passes the request on.
    fn compressed_video_sample(&self, time: Tick, max_bytes: u64) -> filmcraft_media::Result<Option<filmcraft_media::CompressedSample>> {
        self.caps.lock().unwrap().push(max_bytes);
        self.inner.compressed_video_sample(time, max_bytes)
    }
}

/// A sample over the caller's cap is refused from the sample table, before any of its bytes are
/// read, also when the table declares one far larger than the file; within the cap it is read.
/// The export's cap is the bytes its batches budget for one rendered frame.
#[test]
fn a_sample_over_the_cap_is_never_read() {
    use std::sync::atomic::Ordering::SeqCst;
    let (path, src, samples) = hq_source("cap.mov");
    let bytes = std::fs::read(&path).unwrap();
    let open = |bytes: Vec<u8>| {
        let reader = Arc::new(CountingReader { inner: filmcraft_media::reader::MemReader(bytes.into()), total: 0.into(), largest: 0.into() });
        let src = filmcraft_codecs::Mp4Source::open_reader("cap.mov", reader.clone()).unwrap();
        (reader, src)
    };
    let (reader, mov) = open(bytes.clone());
    let size = samples[0].len() as u64;
    let before = reader.total.load(SeqCst);
    let e = mov.compressed_video_sample(Tick::ZERO, size - 1).unwrap_err();
    assert!(e.to_string().contains("over the"), "{e}");
    assert_eq!(reader.total.load(SeqCst), before, "bytes were read for a sample over the cap");
    assert!(mov.compressed_video_sample(Tick::ZERO, size).unwrap().unwrap().data == samples[0], "a sample at the cap is read");
    // the sample table says the first sample is 300 MiB
    let cap = crate::job::frame_bytes(W, H, false);
    let mut lying = bytes;
    let p = lying.windows(4).position(|w| w == b"stsz").unwrap();
    assert_eq!(&lying[p + 8..p + 12], &[0u8; 4], "one size per sample");
    lying[p + 16..p + 20].copy_from_slice(&(300u32 << 20).to_be_bytes());
    let (reader, mov) = open(lying);
    reader.largest.store(0, SeqCst);
    assert!(mov.compressed_video_sample(Tick::ZERO, cap).is_err());
    assert!(reader.largest.load(SeqCst) <= cap, "a read of {} bytes", reader.largest.load(SeqCst));
    // the export asks with the bytes of one RGBA frame, and copies every frame within it
    let seen = Arc::new(CapSeen { inner: src, caps: Default::default() });
    let (p, seq, m) = project(&path, seen.clone(), seq_settings(), |_, _, _, _| {});
    let (_, copied) = export(p, seq, &m, &prores("cap-out.mov", "hq"));
    assert_eq!(copied, N as u64);
    let caps = seen.caps.lock().unwrap();
    assert!(caps.len() >= N && caps.iter().all(|&c| c == u64::from(W * H * 4)), "{caps:?}");
}

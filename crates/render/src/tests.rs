use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

fn setup() -> (Project, ItemId, ItemId, ItemId, SourceMap) {
    let mut p = Project::new("r");
    let mut map = SourceMap::default();
    let mut add = |p: &mut Project, g: GeneratorSource| {
        let info = g.info().clone();
        let generator = g.generator.clone();
        let id = p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(generator),
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
        map.0.insert(id, Arc::new(g) as SharedSource);
        id
    };
    let red =
        add(&mut p, GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)));
    let ocean = add(&mut p, GeneratorSource::demo(DemoScene::OceanSunset));
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: FrameRate::FPS_24, ..Default::default() }, 2, 2, None);
    (p, red, ocean, seq, map)
}

fn place(p: &mut Project, seq: ItemId, track: usize, item: ItemId, start_f: i64, dur_f: i64) -> filmcraft_project::ClipId {
    let r = FrameRate::FPS_24;
    let ti = p.make_track_item(item, TrackKind::Video, r.tick_of(start_f), TimeRange::new(Tick::ZERO, r.tick_of(dur_f)), r).unwrap();
    let id = ti.id;
    let mut ti = ti;
    ti.scale_to_frame = true;
    p.sequence_mut(seq).unwrap().video_tracks[track].items.push(ti);
    p.sequence_mut(seq).unwrap().video_tracks[track].sort();
    id
}

#[test]
fn composites_tracks_and_opacity() {
    let (mut p, red, ocean, seq, map) = setup();
    place(&mut p, seq, 0, ocean, 0, 48);
    let top = place(&mut p, seq, 1, red, 0, 48);
    let img = render_sequence(&p, seq, Tick(1000), RenderOptions::default(), &map);
    assert_eq!((img.w, img.h), (320, 180));
    let c = img.get(160, 90);
    assert!((c[0] - 1.0).abs() < 1e-4 && c[1] < 1e-4, "red on top: {c:?}");
    // 50% opacity shows the bottom layer through
    let s = p.sequence_mut(seq).unwrap();
    let (_, it) = s.find_item_mut(top).unwrap();
    it.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    let img = render_sequence(&p, seq, Tick(1000), RenderOptions::default(), &map);
    let c = img.get(160, 20);
    assert!(c[0] > 0.5 && c[0] < 1.0 && c[2] > 0.0, "{c:?}");
}

#[test]
fn half_resolution_matches_full_downsampled() {
    let (mut p, _red, ocean, seq, map) = setup();
    place(&mut p, seq, 0, ocean, 0, 48);
    let full = render_sequence(&p, seq, Tick(1000), RenderOptions::default(), &map);
    let half = render_sequence(&p, seq, Tick(1000), RenderOptions { scale: 0.5, ..Default::default() }, &map);
    assert_eq!((half.w, half.h), (160, 90));
    let a = full.get(200, 60);
    let b = half.get(100, 30);
    for k in 0..3 {
        assert!((a[k] - b[k]).abs() < 0.06, "{a:?} vs {b:?}");
    }
}

#[test]
fn motion_scale_and_position() {
    let (mut p, red, _o, seq, map) = setup();
    let id = place(&mut p, seq, 0, red, 0, 48);
    let s = p.sequence_mut(seq).unwrap();
    let (_, it) = s.find_item_mut(id).unwrap();
    let m = it.effect_mut("motion").unwrap();
    m.params.get_mut("scale").unwrap().value = ParamValue::Float(50.0);
    m.params.get_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(80.0, 45.0));
    let img = render_sequence(&p, seq, Tick(0), RenderOptions::default(), &map);
    assert!(img.get(80, 45)[3] > 0.99);
    assert!(img.get(10, 10)[3] > 0.99, "top-left quadrant covered");
    assert_eq!(img.get(200, 120)[3], 0.0, "rest transparent");
}

#[test]
fn cross_dissolve_midpoint() {
    let (mut p, red, _o, seq, map) = setup();
    let a = place(&mut p, seq, 0, red, 0, 24);
    let blue_src = GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 1.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND));
    let info = blue_src.info().clone();
    let blue = p.add_item(
        "blue",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(blue_src.generator.clone()),
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
    let mut map = map;
    map.0.insert(blue, Arc::new(blue_src));
    let b = place(&mut p, seq, 0, blue, 24, 24);
    let r = FrameRate::FPS_24;
    let tr = filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(999),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: r.tick_of(18),
        duration: r.tick_of(12),
        from: Some(a),
        to: Some(b),
        align: Default::default(),
        reverse: false,
    };
    p.sequence_mut(seq).unwrap().video_tracks[0].transitions.push(tr);
    let img = render_sequence(&p, seq, r.tick_of(24), RenderOptions::default(), &map);
    let c = img.get(10, 10);
    assert!((c[0] - 0.5).abs() < 0.05 && (c[2] - 0.5).abs() < 0.05, "{c:?}");
    // Iris Round at 50 %: B opens in the centre; reversed, A closes into the centre instead.
    for reverse in [false, true] {
        let trs = &mut p.sequence_mut(seq).unwrap().video_tracks[0].transitions;
        trs[0].effect = filmcraft_project::find_effect("iris_round").unwrap().instance();
        trs[0].reverse = reverse;
        let opts = RenderOptions { scale: 0.25, ..RenderOptions::default() };
        let img = render_sequence(&p, seq, r.tick_of(24), opts, &map);
        let (mid, corner) = (img.get(img.w / 2, img.h / 2), img.get(2, 2));
        let (centre_is_b, corner_is_b) = (mid[2] > 0.9 && mid[0] < 0.1, corner[2] > 0.9 && corner[0] < 0.1);
        assert_eq!((centre_is_b, corner_is_b), (!reverse, reverse), "reverse={reverse}: {mid:?} {corner:?}");
    }
}

#[test]
fn audio_mix_bars_tone() {
    let mut p = Project::new("a");
    let g = GeneratorSource::new(Generator::BarsAndTone, 64, 36, FrameRate::FPS_24, Tick(5 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let id = p.add_item(
        "bars",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(Generator::BarsAndTone),
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
    let mut map = SourceMap::default();
    map.0.insert(id, Arc::new(g));
    let seq = p.new_sequence("s", SequenceSettings::default(), 1, 1, None);
    let r = FrameRate::FPS_23_976;
    let mut ti = p.make_track_item(id, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), r).unwrap();
    ti.effect_mut("volume").unwrap().params.get_mut("level").unwrap().value = ParamValue::Float(-6.0);
    p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(ti);
    let s = p.sequence(seq).unwrap();
    let buf = audio::mix_sequence(&p, s, 0, 4800, &map);
    let peak = buf.peaks()[0];
    let expect = 10f32.powf(-26.0 / 20.0);
    assert!((peak - expect).abs() < 0.01, "{peak} vs {expect}");
}

#[test]
fn plan_matches_reference_renderer() {
    let (mut p, red, ocean, seq, map) = setup();
    place(&mut p, seq, 0, ocean, 0, 48);
    let top = place(&mut p, seq, 1, red, 10, 20);
    {
        let s = p.sequence_mut(seq).unwrap();
        let (_, it) = s.find_item_mut(top).unwrap();
        let m = it.effect_mut("motion").unwrap();
        m.params.get_mut("scale").unwrap().value = ParamValue::Float(40.0);
        m.params.get_mut("rotation").unwrap().value = ParamValue::Float(15.0);
        it.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(70.0);
    }
    let r = FrameRate::FPS_24;
    for f in [0, 12, 20, 40] {
        let opts = RenderOptions { scale: 0.5, ..Default::default() };
        let reference = render_sequence(&p, seq, r.tick_of(f), opts, &map);
        let plan = plan::plan_frame(&p, seq, r.tick_of(f), opts, &map);
        assert!(matches!(plan, plan::FramePlan::Layers { .. }), "simple frame should be GPU-drawable");
        let got = plan::execute_cpu(&plan);
        assert_eq!((got.w, got.h), (reference.w, reference.h));
        let max = got.px.iter().zip(&reference.px).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(max < 0.02, "frame {f}: max diff {max}");
    }
}

#[test]
fn push_plan_preserves_reference_track_order_and_reverse() {
    let (mut p, red, ocean, seq, map) = setup();
    place(&mut p, seq, 0, ocean, 0, 48);
    let a = place(&mut p, seq, 1, red, 0, 24);
    let b = place(&mut p, seq, 1, ocean, 24, 24);
    let rate = FrameRate::FPS_24;
    let s = p.sequence_mut(seq).unwrap();
    for item in &mut s.video_tracks[1].items {
        item.effect_mut("opacity").unwrap().param_mut("opacity").unwrap().value = ParamValue::Float(55.0);
        item.effect_mut("motion").unwrap().param_mut("scale").unwrap().value = ParamValue::Float(75.0);
    }
    s.video_tracks[1].transitions.push(filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(999),
        effect: filmcraft_project::find_effect("push").unwrap().instance(),
        start: rate.tick_of(18),
        duration: rate.tick_of(12),
        from: Some(a),
        to: Some(b),
        align: Default::default(),
        reverse: false,
    });
    for reverse in [false, true] {
        for scale in [1.0, 0.5] {
            for missing in [false, true] {
                let tr = &mut p.sequence_mut(seq).unwrap().video_tracks[1].transitions[0];
                tr.reverse = reverse;
                tr.to = if missing { None } else { Some(b) };
                for f in [18, 21, 24, 29] {
                    let opts = RenderOptions { scale, ..Default::default() };
                    let plan = plan::plan_frame(&p, seq, rate.tick_of(f), opts, &map);
                    assert!(matches!(plan, plan::FramePlan::Composite { .. }));
                    let got = plan::execute_cpu(&plan);
                    let reference = render_sequence(&p, seq, rate.tick_of(f), opts, &map);
                    let max = got.px.iter().zip(&reference.px).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
                    assert!(max < 1e-5, "reverse {reverse}, scale {scale}, missing {missing}, frame {f}: {max}");
                }
            }
        }
    }
}

#[test]
fn morph_cut_frames_render_on_the_cpu_with_the_morph() {
    // the GPU compositor can only cross-fade layers: a Morph Cut frame is planned whole on the
    // CPU, so preview and export show the optical-flow morph, not a dissolve
    let (mut p, red, ocean, seq, map) = setup();
    let a = place(&mut p, seq, 0, red, 0, 24);
    let b = place(&mut p, seq, 0, ocean, 24, 24);
    let rate = FrameRate::FPS_24;
    p.sequence_mut(seq).unwrap().video_tracks[0].transitions.push(filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(998),
        effect: filmcraft_project::find_effect("morph_cut").unwrap().instance(),
        start: rate.tick_of(18),
        duration: rate.tick_of(12),
        from: Some(a),
        to: Some(b),
        align: Default::default(),
        reverse: false,
    });
    let opts = RenderOptions { scale: 0.5, ..Default::default() };
    assert!(plan::is_cpu_frame(&p, seq, rate.tick_of(24)));
    assert!(!plan::is_cpu_frame(&p, seq, rate.tick_of(10)), "outside the transition the frame stays on the GPU");
    let plan = plan::plan_frame(&p, seq, rate.tick_of(24), opts, &map);
    assert!(matches!(plan, plan::FramePlan::Image(_)));
    assert_eq!(plan::execute_cpu(&plan).px, render_sequence(&p, seq, rate.tick_of(24), opts, &map).px);
}

/// A project with a 4 s bars-and-tone clip (1 kHz at −20 dBFS) on A1, with extra audio effects.
fn tone_with(effects: &[(&str, &[(&str, f64)])]) -> (Project, ItemId, SourceMap) {
    let mut p = Project::new("fx");
    let g = GeneratorSource::new(Generator::BarsAndTone, 64, 36, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let id = p.add_item(
        "bars",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(Generator::BarsAndTone),
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
    let mut map = SourceMap::default();
    map.0.insert(id, Arc::new(g));
    let seq = p.new_sequence("s", SequenceSettings::default(), 1, 1, None);
    let r = FrameRate::FPS_23_976;
    let mut ti = p.make_track_item(id, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(4 * TICKS_PER_SECOND)), r).unwrap();
    for (fx, params) in effects {
        let mut e = filmcraft_project::find_effect(fx).expect("effect").instance();
        for (k, v) in *params {
            e.param_mut(k).expect("param").value = ParamValue::Float(*v);
        }
        ti.effects.push(e);
    }
    p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(ti);
    (p, seq, map)
}

fn mix(p: &Project, seq: ItemId, map: &SourceMap, start: i64, n: usize) -> Vec<f32> {
    audio::mix_sequence(p, p.sequence(seq).unwrap(), start, n, map).channels[0].clone()
}

#[test]
fn audio_fx_limiter_holds_ceiling() {
    // +30 dB boost via Amplify (tone at +10 dBFS), then a −6 dB hard limiter.
    let (p, seq, map) = tone_with(&[("amplify", &[("gain", 30.0)]), ("hard_limiter", &[("max", -6.0)])]);
    let out = mix(&p, seq, &map, 24_000, 48_000);
    let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
    let ceiling = 10f32.powf(-6.0 / 20.0);
    assert!(peak <= ceiling * 1.01 && peak > ceiling * 0.7, "peak {peak} vs ceiling {ceiling}");
}

#[test]
fn audio_fx_chain_is_continuous_and_random_access_matches() {
    let (p, seq, map) = tone_with(&[("delay", &[("delay", 0.05), ("feedback", 50.0), ("mix", 50.0)])]);
    let start = 48_000;
    // one long call (fresh chain with pre-roll)
    let whole = mix(&p, seq, &map, start, 9_600);
    // the same range in consecutive blocks from a different position (continues a cached chain)
    let (p2, seq2, map2) = tone_with(&[("delay", &[("delay", 0.05), ("feedback", 50.0), ("mix", 50.0)])]);
    let mut parts = Vec::new();
    let mut s = start - 4_800;
    let _ = mix(&p2, seq2, &map2, s, 4_800);
    s += 4_800;
    while s < start + 9_600 {
        parts.extend(mix(&p2, seq2, &map2, s, 1_200));
        s += 1_200;
    }
    let max = whole.iter().zip(&parts).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(max < 1e-4, "sequential blocks differ from one call by {max}");
    // the delay changes the signal (wet echoes mixed in)
    let dry = mix(&tone_with(&[]).0, tone_with(&[]).1, &tone_with(&[]).2, start, 9_600);
    let diff = whole.iter().zip(&dry).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(diff > 1e-3, "delay had no effect");
}

#[test]
fn offline_slate_is_deterministic_and_marked() {
    use crate::offline::{FIELD, OfflineReason, slate_rgba8};
    let a = slate_rgba8(640, 360, "A001_C002.mov", OfflineReason::Missing);
    assert_eq!(a, slate_rgba8(640, 360, "A001_C002.mov", OfflineReason::Missing));
    assert_ne!(a, slate_rgba8(640, 360, "A001_C002.mov", OfflineReason::MadeOffline), "the headline names the reason");
    // corners are the red field / stripes, opaque
    let px = |x: usize, y: usize| &a[(y * 640 + x) * 4..(y * 640 + x) * 4 + 4];
    assert_eq!(px(0, 359)[3], 255);
    assert!(px(0, 359)[0] >= FIELD[0] && px(0, 359)[1] <= 0x20, "{:?}", px(0, 359));
    // tiny sizes work (thumbnails)
    assert_eq!(slate_rgba8(8, 4, "x", OfflineReason::Unreadable).len(), 8 * 4 * 4);
    if let Some(p) = std::env::var_os("FILMCRAFT_SLATE_PNG") {
        ::image::save_buffer(p, &a, 640, 360, ::image::ExtendedColorType::Rgba8).unwrap();
    }
}

/// A 320×180 4:2:0 source with smooth gradients (what decoders hand the renderer).
struct YuvSource(GeneratorSource);

impl MediaSource for YuvSource {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.0.info()
    }
    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        let (w, h) = (320usize, 180usize);
        let y = (0..w * h).map(|i| (16 + (i % w) * 200 / w + (i / w) * 20 / h) as u8).collect();
        let u = (0..w * h / 4).map(|i| (90 + (i % (w / 2)) * 60 / (w / 2)) as u8).collect();
        let v = (0..w * h / 4).map(|i| (100 + (i / (w / 2)) * 50 / (h / 2)) as u8).collect();
        Ok(Arc::new(filmcraft_frame::VideoFrame {
            width: w as u32,
            height: h as u32,
            data: filmcraft_frame::PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: filmcraft_frame::Chroma::C420, alpha: None },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        }))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.0.audio(start, frames, sample_rate)
    }
}

#[test]
fn draft_reduced_resolution_plans_carry_decimated_yuv() {
    let (mut p, red, _ocean, seq, mut map) = setup();
    let matte = GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 0.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND));
    map.0.insert(red, Arc::new(YuvSource(matte)) as SharedSource);
    place(&mut p, seq, 0, red, 0, 48);
    let t = FrameRate::FPS_24.tick_of(3);
    for draft in [false, true] {
        for (scale, small_w) in [(1.0f32, 320u32), (0.5, 160), (0.25, 80)] {
            let opts = RenderOptions { scale, ..Default::default() };
            let plan = filmcraft_media::cancel::with_draft(draft, || plan::plan_frame(&p, seq, t, opts, &map));
            let plan::FramePlan::Layers { layers, .. } = &plan else { panic!("GPU-drawable") };
            // exact playback hands over the decoded picture; draft playback planes at the drawn size
            assert_eq!(layers[0].frame.width, if draft { small_w } else { 320 }, "scale {scale}, draft {draft}");
            assert!(matches!(layers[0].frame.data, filmcraft_frame::PixelData::Yuv8 { .. }));
            let got = plan::execute_cpu(&plan);
            let reference = render_sequence(&p, seq, t, opts, &map);
            assert_eq!((got.w, got.h), (reference.w, reference.h));
            let max = got.px.iter().zip(&reference.px).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            // the Y'CbCr-code mean differs from a linear-light mean by a little on gradients
            assert!(max < if draft { 0.03 } else { 1e-5 }, "scale {scale}, draft {draft}: max diff {max}");
        }
    }
}

/// A damaged project can hold a multi-camera clip whose angle is the clip itself. Planning and
/// rendering its frame must end: the plan's multi-camera shortcut used to recurse until the stack
/// overflowed.
#[test]
fn a_multicam_clip_showing_itself_plans_and_renders_to_an_end() {
    let mut p = Project::new("cycle");
    let a = p.new_sequence("a", SequenceSettings::default(), 1, 0, None);
    let rate = FrameRate::default();
    let mut it = p.make_track_item(a, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), rate).unwrap();
    it.multicam = Some(filmcraft_project::multicam::MulticamSel { enabled: true, angle: 0 });
    p.sequence_mut(a).unwrap().video_tracks[0].items.push(it);
    let opts = RenderOptions { scale: 0.1, ..Default::default() };
    let sources = SourceMap::default();
    let img = plan::execute_cpu(&plan::plan_frame(&p, a, Tick::ZERO, opts, &sources));
    assert!(img.w > 0 && img.h > 0);
    let img = render_sequence(&p, a, Tick::ZERO, opts, &sources);
    assert!(img.w > 0 && img.h > 0);
}

/// A 24 fps source that picks frames like the MP4 reader: the requested time is converted to the
/// track timescale rounding to the nearest unit, then the frame at or before it is shown. It
/// records the frames it was asked for.
struct NearestUnitSource {
    inner: GeneratorSource,
    asked: std::sync::Mutex<Vec<i64>>,
}

impl MediaSource for NearestUnitSource {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.inner.info()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        // 12288 units a second: 512 a frame at 24 fps
        let units = req.time.to_rational_round(1, 12_288);
        self.asked.lock().unwrap().push(units.div_euclid(512));
        self.inner.video_frame(req)
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
}

/// #318: a trimmed clip (source frames 10–29) played in reverse shows 29 down to 10. Its source
/// time is the instant just before the mirrored position; a reader that rounds to the nearest
/// timescale unit used to land on the next frame, showing frame 30 (outside the clip) and never 10.
#[test]
fn a_reversed_clip_shows_exactly_its_source_range() {
    let r = FrameRate::FPS_24;
    let mut p = Project::new("r");
    let g = GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.4, 0.6, 1.0] }, 64, 64, r, Tick(4 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let item = p.add_item(
        "ids",
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
    let src = Arc::new(NearestUnitSource { inner: g, asked: Default::default() });
    let mut map = SourceMap::default();
    map.0.insert(item, src.clone() as SharedSource);
    let seq = p.new_sequence("s", SequenceSettings { width: 64, height: 64, frame_rate: r, ..Default::default() }, 1, 0, None);
    let ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(r.tick_of(10), r.tick_of(20)), r).unwrap();
    let id = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let opts = RenderOptions { scale: 1.0, ..Default::default() };
    let shown = |p: &Project, f: i64, planned: bool| -> Vec<i64> {
        src.asked.lock().unwrap().clear();
        if planned {
            plan::execute_cpu(&plan::plan_frame(p, seq, r.tick_of(f), opts, &map));
        } else {
            render_sequence(p, seq, r.tick_of(f), opts, &map);
        }
        let mut asked = src.asked.lock().unwrap().clone();
        asked.dedup();
        asked
    };
    // forward control: 10, 17, 29
    for (f, want) in [(0, 10), (7, 17), (19, 29)] {
        for planned in [false, true] {
            assert_eq!(shown(&p, f, planned), [want], "forward, timeline frame {f}, planned {planned}");
        }
    }
    p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap().1.reverse = true;
    for (f, want) in [(0, 29), (7, 22), (19, 10)] {
        for planned in [false, true] {
            assert_eq!(shown(&p, f, planned), [want], "reversed, timeline frame {f}, planned {planned}");
        }
    }
    // every timeline frame stays inside the clip's source range, each source frame shown once
    let all: Vec<i64> = (0..20).flat_map(|f| shown(&p, f, false)).collect();
    assert_eq!(all, (10..=29).rev().collect::<Vec<_>>());
}

fn shadow_scene(direction: f64, only: bool, softness: f64) -> (Project, ItemId, filmcraft_project::ClipId, SourceMap) {
    let (mut p, red, _, seq, map) = setup();
    let id = place(&mut p, seq, 0, red, 0, 48);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    it.effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(50.0);
    let mut shadow = filmcraft_project::find_effect("drop_shadow").unwrap().instance();
    for (k, v) in [("direction", direction), ("distance", 40.0), ("opacity", 100.0), ("softness", softness)] {
        shadow.params.get_mut(k).unwrap().value = ParamValue::Float(v);
    }
    shadow.params.get_mut("only").unwrap().value = ParamValue::Bool(only);
    it.effects.push(shadow);
    (p, seq, id, map)
}

#[test]
fn drop_shadow_extends_past_all_four_clip_edges() {
    for (direction, outside) in [(90.0, (250, 90)), (270.0, (70, 90)), (0.0, (160, 35)), (180.0, (160, 145))] {
        for only in [false, true] {
            let (p, seq, _, map) = shadow_scene(direction, only, 0.0);
            for scale in [1.0, 0.5, 0.25] {
                let opts = RenderOptions { scale, ..Default::default() };
                let img = render_sequence(&p, seq, Tick::ZERO, opts, &map);
                let at = |x: usize, y: usize| img.get((x as f32 * scale) as usize, (y as f32 * scale) as usize);
                let c = at(outside.0, outside.1);
                assert!(c[3] > 0.99 && c[0] < 0.001, "shadow outside clip: direction={direction}, only={only}, scale={scale}: {c:?}");
                assert_eq!(at(10, 10)[3], 0.0);
                assert_eq!(at(160, 90)[0], if only { 0.0 } else { 1.0 }, "source position must not change");
                let planned = plan::execute_cpu(&plan::plan_frame(&p, seq, Tick::ZERO, opts, &map));
                assert_eq!(img, planned, "GPU plan's CPU fallback must preserve the expanded shadow");
            }
        }
    }
}

#[test]
fn drop_shadow_softness_has_room_outside_the_shifted_clip() {
    let (p, seq, _, map) = shadow_scene(90.0, true, 20.0);
    let img = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    let a = img.get(262, 90)[3];
    assert!(a > 0.01 && a < 0.5, "blur must extend past the shifted right edge: {a}");
    assert!(img.get(250, 90)[3] > a);
}

#[test]
fn drop_shadow_keeps_following_points_masks_and_effect_order() {
    let (mut p, seq, id, map) = shadow_scene(270.0, false, 0.0);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    // An explicit identity Transform must agree with its automatic anchor/position.
    let mut transform = filmcraft_project::find_effect("transform").unwrap().instance();
    for key in ["anchor", "position"] {
        transform.params.get_mut(key).unwrap().value = ParamValue::Vec2(Vec2::new(160.0, 90.0));
    }
    it.effects.push(transform);
    // Inverting after the shadow must also turn the shadow white.
    let mut invert = filmcraft_project::find_effect("invert").unwrap().instance();
    let mask = filmcraft_project::Mask::new("left", filmcraft_project::MaskPath::rect(-80.0, -80.0, 160.0, 260.0));
    invert.masks.push(mask);
    it.effects.push(invert);
    let img = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    assert_eq!(img.get(70, 90), [1.0; 4], "masked Invert reaches the expanded shadow");
    assert_eq!(img.get(120, 90), [0.0, 1.0, 1.0, 1.0], "mask stays in source coordinates");
    assert_eq!(img.get(180, 90), [1.0, 0.0, 0.0, 1.0], "unmasked source is unchanged");
    // Opacity masks also use the original source coordinates after expansion.
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    it.effect_mut("opacity").unwrap().masks.push(filmcraft_project::Mask::new("right", filmcraft_project::MaskPath::rect(160.0, 0.0, 320.0, 180.0)));
    let img = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    assert_eq!(img.get(120, 90)[3], 0.0);
    assert_eq!(img.get(180, 90), [1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn drop_shadow_effect_mask_can_cover_its_expanded_bounds() {
    let (mut p, seq, id, map) = shadow_scene(270.0, false, 0.0);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    it.effect_mut("drop_shadow").unwrap().masks.push(filmcraft_project::Mask::new("upper half", filmcraft_project::MaskPath::rect(-80.0, -80.0, 400.0, 90.0)));
    let img = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    assert_eq!(img.get(70, 65)[3], 1.0);
    assert_eq!(img.get(70, 115)[3], 0.0);
    assert_eq!(img.get(160, 115), [1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn drop_shadow_multiple_instances_and_animated_distance() {
    let (mut p, seq, id, map) = shadow_scene(90.0, true, 0.0);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    let shadow = it.effect_mut("drop_shadow").unwrap();
    shadow.params.get_mut("distance").unwrap().keyframes = vec![
        filmcraft_project::Keyframe::new(Tick::ZERO, ParamValue::Float(0.0)),
        filmcraft_project::Keyframe::new(Tick::from_seconds_f64(1.0), ParamValue::Float(80.0)),
    ];
    // A second shadow moves the first one down, in the original clip's pixel units.
    let mut second = filmcraft_project::find_effect("drop_shadow").unwrap().instance();
    second.params.get_mut("direction").unwrap().value = ParamValue::Float(180.0);
    second.params.get_mut("distance").unwrap().value = ParamValue::Float(40.0);
    second.params.get_mut("opacity").unwrap().value = ParamValue::Float(100.0);
    second.params.get_mut("only").unwrap().value = ParamValue::Bool(true);
    it.effects.push(second);
    let early = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    let late = render_sequence(&p, seq, Tick::from_seconds_f64(1.0), RenderOptions::default(), &map);
    assert_eq!(early.get(270, 145)[3], 0.0);
    assert_eq!(late.get(270, 145)[3], 1.0);
    assert_eq!(late.get(100, 90)[3], 0.0);
}

#[test]
fn drop_shadow_survives_temporal_resampling_and_motion_rotation() {
    let (mut p, seq, id, map) = shadow_scene(90.0, false, 0.0);
    let time = Tick::from_seconds_f64(0.04);
    let expected = render_sequence(&p, seq, time, RenderOptions::default(), &map);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    it.effects.push(filmcraft_project::find_effect("posterize_time").unwrap().instance());
    assert_eq!(render_sequence(&p, seq, time, RenderOptions::default(), &map), expected);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    it.effect_mut("motion").unwrap().params.get_mut("rotation").unwrap().value = ParamValue::Float(90.0);
    let img = render_sequence(&p, seq, time, RenderOptions::default(), &map);
    assert_eq!(img.get(160, 90), [1.0, 0.0, 0.0, 1.0]);
    assert!(img.get(160, 175)[3] > 0.99, "the shadow rotates with the clip");
}

#[test]
fn drop_shadow_disabled_and_hostile_parameters() {
    let (mut p, seq, id, map) = shadow_scene(90.0, false, 0.0);
    let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
    it.effect_mut("drop_shadow").unwrap().enabled = false;
    let img = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    assert_eq!(img.get(250, 90)[3], 0.0);
    assert_eq!(img.get(160, 90), [1.0, 0.0, 0.0, 1.0]);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let (_, it) = p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap();
        let e = it.effect_mut("drop_shadow").unwrap();
        e.enabled = true;
        for key in ["direction", "distance", "softness", "opacity"] {
            e.params.get_mut(key).unwrap().value = ParamValue::Float(value);
        }
        let img = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
        assert!(img.px.iter().all(|v| v.is_finite()));
        assert_eq!(img.get(160, 90), [1.0, 0.0, 0.0, 1.0]);
    }
}

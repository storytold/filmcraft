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

/// A 4:4:4 10-bit grey picture with an alpha plane: transparent on the left half, opaque on the right
/// (what a ProRes 4444 overlay decodes to).
struct AlphaOverlay {
    info: filmcraft_media::MediaInfo,
}

impl MediaSource for AlphaOverlay {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        let (w, h) = (320usize, 180usize);
        let alpha = (0..w * h).map(|i| if i % w < w / 2 { 0u16 } else { 1023 }).collect();
        Ok(Arc::new(filmcraft_frame::VideoFrame {
            width: w as u32,
            height: h as u32,
            data: filmcraft_frame::PixelData::Yuv16 {
                planes: [Arc::new(vec![700u16; w * h]), Arc::new(vec![512u16; w * h]), Arc::new(vec![512u16; w * h])],
                chroma: filmcraft_frame::Chroma::C444,
                bits: 10,
                alpha: Some(Arc::new(alpha)),
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        }))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// A layer that carries an alpha plane (ProRes 4444) must not reach the GPU as Y'CbCr: the GPU
/// uploads only Y/Cb/Cr and used to draw such a layer opaque, so a transparent overlay (banner,
/// lower third) covered the footage under it in the Program monitor. It is drawn on the CPU instead.
#[test]
fn layer_with_alpha_plane_is_drawn_on_the_cpu_and_stays_transparent() {
    let (mut p, overlay, ocean, seq, mut map) = setup();
    let info = map.0.get(&overlay).map(|s| s.info().clone()).expect("overlay source");
    map.0.insert(overlay, Arc::new(AlphaOverlay { info }) as SharedSource);
    place(&mut p, seq, 0, ocean, 0, 48);
    let r = FrameRate::FPS_24;
    let t = r.tick_of(10);
    let background = render_sequence(&p, seq, t, RenderOptions::default(), &map);
    place(&mut p, seq, 1, overlay, 0, 48);

    let plan = plan::plan_frame(&p, seq, t, RenderOptions::default(), &map);
    let plan::FramePlan::Layers { layers, .. } = &plan else { panic!("a simple stack plans as layers") };
    assert!(
        layers.iter().all(|l| !matches!(
            l.frame.data,
            filmcraft_frame::PixelData::Yuv8 { alpha: Some(_), .. } | filmcraft_frame::PixelData::Yuv16 { alpha: Some(_), .. }
        )),
        "a layer with an alpha plane reached the GPU path, which drops the alpha"
    );

    let img = plan::execute_cpu(&plan);
    let (left, right) = (img.get(40, 90), img.get(280, 90));
    let bg_left = background.get(40, 90);
    assert!((0..3).all(|k| (left[k] - bg_left[k]).abs() < 0.02), "transparent half shows the footage: {left:?} vs {bg_left:?}");
    let bg_right = background.get(280, 90);
    assert!((0..3).any(|k| (right[k] - bg_right[k]).abs() > 0.05), "opaque half covers the footage: {right:?} vs {bg_right:?}");
}

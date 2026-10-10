//! The compositor's shortcuts must give exactly what the general path gives, bit for bit: an opaque
//! bottom layer taken as the canvas, a layer that is only its non-transparent rectangle, and the
//! recycled float buffers. The reference is the plain loop over `item_layer` + `blend::composite`
//! (the path every clip takes when no shortcut applies).

use super::*;
use filmcraft_frame::{AudioBuffer, Chroma};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaInfo, MediaSource};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const FPS: FrameRate = FrameRate::FPS_24;

struct Still {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
}

impl MediaSource for Still {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Ok(self.frame.clone())
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn frame(w: usize, h: usize, data: PixelData) -> Arc<VideoFrame> {
    Arc::new(VideoFrame { width: w as u32, height: h as u32, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO })
}

/// A camera-like picture: 8-bit 4:2:0, no alpha.
fn camera(w: usize, h: usize) -> Arc<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y: Vec<u8> = (0..w * h).map(|i| (16 + (i * 7 + i / w * 3) % 219) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (60 + i * 5 % 140) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (50 + i * 11 % 150) as u8).collect();
    frame(w, h, PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None })
}

/// Alpha 10-bit: 0 outside `rect` (x0, y0, x1, y1), 1023 inside, with a half-transparent one-pixel rim.
fn alpha_in(w: usize, h: usize, rect: (usize, usize, usize, usize)) -> Vec<u16> {
    let (x0, y0, x1, y1) = rect;
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            if x < x0 || x >= x1 || y < y0 || y >= y1 {
                0
            } else if x == x0 || y == y0 {
                512
            } else {
                1023
            }
        })
        .collect()
}

/// A ProRes-4444-like picture: 10-bit 4:4:4 with alpha only inside `rect`.
fn banner(w: usize, h: usize, rect: (usize, usize, usize, usize)) -> Arc<VideoFrame> {
    let n = w * h;
    frame(
        w,
        h,
        PixelData::Yuv16 {
            planes: [
                Arc::new((0..n).map(|i| (200 + i * 3 % 600) as u16).collect()),
                Arc::new((0..n).map(|i| (400 + i % 200) as u16).collect()),
                Arc::new((0..n).map(|i| (300 + i * 7 % 300) as u16).collect()),
            ],
            chroma: Chroma::C444,
            bits: 10,
            alpha: Some(Arc::new(alpha_in(w, h, rect))),
        },
    )
}

/// An 8-bit RGBA still with alpha only inside `rect`.
fn sticker(w: usize, h: usize, rect: (usize, usize, usize, usize)) -> Arc<VideoFrame> {
    let a = alpha_in(w, h, rect);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i * 13 % 251) as u8, (i * 7 % 241) as u8, (i * 29 % 239) as u8, (a[i] / 4) as u8]).collect();
    frame(w, h, PixelData::Rgba8(Arc::new(px)))
}

struct Spec {
    frame: Arc<VideoFrame>,
    opacity: f64,
    blend: u32,
    motion_scale: Option<f64>,
    effect: Option<&'static str>,
}

impl Spec {
    fn new(frame: Arc<VideoFrame>) -> Self {
        Self { frame, opacity: 100.0, blend: 0, motion_scale: None, effect: None }
    }
}

const W: usize = 96;
const H: usize = 54;

fn scene(specs: &[Spec]) -> (Project, ItemId, SourceMap) {
    let mut p = Project::new("regions");
    let mut map = SourceMap::default();
    let seq = p.new_sequence("s", SequenceSettings { width: W as u32, height: H as u32, frame_rate: FPS, ..Default::default() }, specs.len(), 1, None);
    for (track, spec) in specs.iter().enumerate() {
        let g =
            GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 0.0, 1.0] }, spec.frame.width, spec.frame.height, FPS, Tick(10 * TICKS_PER_SECOND));
        let info = g.info().clone();
        let item = p.add_item(
            &format!("layer{track}"),
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
        let mut clip = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(48)), FPS).expect("clip");
        clip.scale_to_frame = true;
        {
            let o = clip.effect_mut("opacity").expect("opacity");
            o.params.get_mut("opacity").expect("opacity").value = ParamValue::Float(spec.opacity);
            o.params.get_mut("blend").expect("blend").value = ParamValue::Choice(spec.blend);
        }
        if let Some(s) = spec.motion_scale {
            clip.effect_mut("motion").expect("motion").params.get_mut("scale").expect("scale").value = ParamValue::Float(s);
        }
        if let Some(id) = spec.effect {
            clip.effects.push(find_effect(id).expect("effect").instance());
        }
        if let Some(t) = p.sequence_mut(seq).and_then(|s| s.video_tracks.get_mut(track)) {
            t.items.push(clip);
        }
        map.0.insert(item, Arc::new(Still { info, frame: spec.frame.clone() }));
    }
    (p, seq, map)
}

/// What every clip gets when no shortcut applies: each layer from the general path, mixed into a
/// zeroed canvas.
fn reference(p: &Project, seq: ItemId, t: Tick, opts: RenderOptions, map: &SourceMap) -> Image {
    let s = p.sequence(seq).expect("sequence");
    let (w, h) = output_size(s, opts.scale);
    let mut canvas = Image::new(w, h);
    let tc = format_time(t, s.settings.frame_rate, s.settings.drop_frame, TimeDisplay::Timecode, s.settings.sample_rate as i64);
    for track in &s.video_tracks {
        let Some(item) = track.item_at(t) else { continue };
        if let Some((layer, op, bl)) = item_layer(p, s, item, t, opts, map, &tc).unwrap() {
            blend::composite(&mut canvas, &layer, op, bl);
        }
    }
    canvas
}

fn same_bits(a: &Image, b: &Image, what: &str) {
    assert_eq!((a.w, a.h, a.px.len()), (b.w, b.h, b.px.len()), "{what}: size");
    if let Some(i) = a.px.iter().zip(&b.px).position(|(x, y)| x.to_bits() != y.to_bits()) {
        panic!("{what}: first difference at pixel {} channel {}: {} vs {}", i / 4, i % 4, a.px[i], b.px[i]);
    }
}

fn check(what: &str, specs: &[Spec], scale: f32) {
    let (p, seq, map) = scene(specs);
    let opts = RenderOptions { scale, ..Default::default() };
    for f in [0, 7, 30] {
        let t = FPS.tick_of(f);
        // twice: the second render starts from recycled buffers
        for round in 0..2 {
            let got = render_sequence(&p, seq, t, opts, &map).unwrap();
            same_bits(&got, &reference(&p, seq, t, opts, &map), &format!("{what} (frame {f}, render {round})"));
            pool::recycle_f32(got.px);
        }
    }
}

#[test]
fn shortcuts_give_the_general_paths_bits() {
    let bar = (0, H - 9, W, H);
    let corner = (50, 3, 91, 20);
    let full = (0, 0, W, H);
    let cam = || Spec::new(camera(W, H));
    let ban = |rect| Spec::new(banner(W, H, rect));

    check("camera only", &[cam()], 1.0);
    check("camera + bar banner", &[cam(), ban(bar)], 1.0);
    check("camera + corner banner", &[cam(), ban(corner)], 1.0);
    check("camera + banner covering everything", &[cam(), ban(full)], 1.0);
    check("camera + two banners", &[cam(), ban(bar), ban(corner)], 1.0);
    check("banner alone (first layer)", &[ban(bar)], 1.0);
    check("banner under the camera", &[ban(bar), cam()], 1.0);
    check("camera + sticker", &[cam(), Spec::new(sticker(W, H, corner))], 1.0);
    check("camera + transparent banner", &[cam(), ban((0, 0, 0, 0))], 1.0);
    // opacity and blend modes (every mode, over the camera and as the first layer)
    for mode in 0..Blend::ALL.len() as u32 {
        check(&format!("banner blend {mode}"), &[cam(), Spec { opacity: 70.0, blend: mode, ..ban(corner) }], 1.0);
        check(&format!("camera blend {mode} first"), &[Spec { opacity: 100.0, blend: mode, ..cam() }, ban(bar)], 1.0);
    }
    check("camera at half opacity", &[Spec { opacity: 50.0, ..cam() }, ban(bar)], 1.0);
    check("camera at zero opacity", &[Spec { opacity: 0.0, ..cam() }, ban(bar)], 1.0);
    check("banner at zero opacity", &[cam(), Spec { opacity: 0.0, ..ban(bar) }], 1.0);
    // not placed as it is, or with effects: the general path
    check("moved camera", &[Spec { motion_scale: Some(50.0), ..cam() }, ban(bar)], 1.0);
    check("scaled banner", &[cam(), Spec { motion_scale: Some(60.0), ..ban(corner) }], 1.0);
    check("camera with an effect", &[Spec { effect: Some("brightness_contrast"), ..cam() }, ban(bar)], 1.0);
    check("banner with an effect", &[cam(), Spec { effect: Some("gaussian_blur"), ..ban(bar) }], 1.0);
    // a source of another size than the sequence's
    check("bigger banner", &[cam(), Spec::new(banner(W * 2, H * 2, (0, H * 2 - 18, W * 2, H * 2)))], 1.0);
    // reduced-resolution rendering decimates every frame
    check("half scale", &[cam(), ban(bar)], 0.5);
    check("quarter scale", &[cam(), ban(corner)], 0.25);
}

#[test]
fn which_layers_take_a_shortcut() {
    let (p, seq, map) = scene(&[Spec::new(camera(W, H)), Spec::new(banner(W, H, (0, H - 9, W, H)))]);
    let s = p.sequence(seq).expect("sequence");
    let (opts, t) = (RenderOptions::default(), FPS.tick_of(3));
    let layer = |track: usize, allow: bool| item_layer_ex(&p, s, s.video_tracks[track].item_at(t).expect("clip"), t, opts, &map, "", allow).unwrap();
    // a camera frame has no alpha: opaque, the whole picture
    assert!(matches!(layer(0, true), Some(Layer::Full { opaque: true, .. })));
    // a banner is only its rectangle (rows H-9.. of a 96-wide picture)
    match layer(1, true) {
        Some(Layer::Region { image, x, y, .. }) => assert_eq!((x, y, image.w, image.h), (0, H - 9, W, 9)),
        _ => panic!("the banner should be a region"),
    }
    // without permission every layer is a whole image, and none is known to be opaque
    assert!(matches!(layer(0, false), Some(Layer::Full { opaque: false, .. })));
    assert!(matches!(layer(1, false), Some(Layer::Full { opaque: false, .. })));
    // a transparent banner is an empty region
    let (p, seq, map) = scene(&[Spec::new(banner(W, H, (0, 0, 0, 0)))]);
    let s = p.sequence(seq).expect("sequence");
    match item_layer_ex(&p, s, s.video_tracks[0].item_at(t).expect("clip"), t, opts, &map, "", true).unwrap() {
        Some(Layer::Region { image, .. }) => assert_eq!((image.w, image.h, image.px.len()), (0, 0, 0)),
        _ => panic!("a transparent banner is an empty region"),
    }
    // an effect or a move keeps the clip on the general path
    for spec in [Spec { effect: Some("brightness_contrast"), ..Spec::new(camera(W, H)) }, Spec { motion_scale: Some(50.0), ..Spec::new(camera(W, H)) }] {
        let (p, seq, map) = scene(&[spec]);
        let s = p.sequence(seq).expect("sequence");
        assert!(matches!(
            item_layer_ex(&p, s, s.video_tracks[0].item_at(t).expect("clip"), t, opts, &map, "", true).unwrap(),
            Some(Layer::Full { opaque: false, .. })
        ));
    }
}

#[test]
fn a_rectangle_mixes_like_the_padded_layer_in_every_blend_mode() {
    let (w, h) = (40usize, 30usize);
    let rect = Image { w: 11, h: 7, px: (0..11 * 7 * 4).map(|i| [0.0, 0.2, 0.9, 0.5, 0.01][i % 5] * ((i / 4 % 3) as f32 + 1.0) * 0.3).collect() };
    let base = Image { w, h, px: (0..w * h * 4).map(|i| if i % 4 == 3 { 0.8 } else { ((i * 37 % 100) as f32) / 130.0 }).collect() };
    for mode in Blend::ALL.iter().copied() {
        for (x0, y0) in [(0usize, 0usize), (13, 9), (29, 23), (35, 28)] {
            // the rectangle placed in a transparent layer of the full size
            let mut padded = Image::new(w, h);
            for ry in 0..rect.h {
                for rx in 0..rect.w {
                    if x0 + rx < w && y0 + ry < h {
                        let (s, d) = ((ry * rect.w + rx) * 4, ((y0 + ry) * w + x0 + rx) * 4);
                        padded.px[d..d + 4].copy_from_slice(&rect.px[s..s + 4]);
                    }
                }
            }
            let (mut a, mut b) = (base.clone(), base.clone());
            blend::composite(&mut a, &padded, 0.8, mode);
            blend::composite_at(&mut b, &rect, x0, y0, 0.8, mode);
            same_bits(&a, &b, &format!("{mode:?} at ({x0}, {y0})"));
        }
    }
    // a rectangle that misses the picture, or an empty one, changes nothing
    let mut c = base.clone();
    blend::composite_at(&mut c, &rect, 40, 0, 1.0, Blend::Normal);
    blend::composite_at(&mut c, &rect, 0, 30, 1.0, Blend::Normal);
    blend::composite_at(&mut c, &Image { w: 0, h: 0, px: vec![] }, 3, 3, 1.0, Blend::Normal);
    same_bits(&c, &base, "outside");
}

#[test]
fn a_region_layer_expands_to_the_whole_layer() {
    let image = Image { w: 3, h: 2, px: (0..24).map(|i| i as f32 + 1.0).collect() };
    let (full, op, bl) = Layer::Region { image, x: 2, y: 1, opacity: 0.5, blend: Blend::Multiply }.into_full(6, 4);
    assert_eq!((full.w, full.h, op, bl), (6, 4, 0.5, Blend::Multiply));
    assert_eq!(&full.px[((6 + 2) * 4)..((6 + 5) * 4)], &(1..=12).map(|i| i as f32).collect::<Vec<_>>()[..]);
    assert_eq!(&full.px[((2 * 6 + 2) * 4)..((2 * 6 + 5) * 4)], &(13..=24).map(|i| i as f32).collect::<Vec<_>>()[..]);
    assert_eq!(full.px.iter().filter(|v| **v != 0.0).count(), 24);
    // a region that sticks out is cut, not a panic
    let sticks = Image { w: 4, h: 3, px: vec![1.0; 48] };
    let (full, ..) = Layer::Region { image: sticks, x: 4, y: 2, opacity: 1.0, blend: Blend::Normal }.into_full(6, 4);
    assert_eq!(full.px.len(), 6 * 4 * 4);
    // only the two columns and two rows inside the output are copied, none wrapped into the next row
    assert_eq!(full.px.iter().filter(|v| **v == 1.0).count(), 2 * 2 * 4);
    assert!(full.px[..(2 * 6 + 4) * 4].iter().all(|v| *v == 0.0));
}

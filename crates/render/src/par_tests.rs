//! Pixel aspect ratio: a clip with non-square pixels is placed at its display aspect (a 1440 x 1080
//! picture with 4:3 pixels covers a 1920 x 1080 square-pixel frame), a sequence with non-square
//! pixels holds clips at its own aspect, Interpret Footage overrides the file's ratio, and hostile
//! ratios are square. Small in-memory frames stand in for 1440 x 1080 / 1920 x 1080 at a tenth of
//! the size: 144 x 108 with 4:3 pixels shows 192 x 108.

use super::*;
use filmcraft_frame::AudioBuffer;
use filmcraft_media::{MediaInfo, MediaKind, MediaSource, VideoStreamInfo};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind, resolve_auto_points};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const FPS: FrameRate = FrameRate::FPS_24;

/// One still picture: the left half red, the right half blue (opaque).
struct Halves {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
}

impl MediaSource for Halves {
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

fn halves(w: u32, h: u32, par: (u32, u32)) -> Halves {
    let px: Vec<u8> = (0..w * h).flat_map(|i| if i % w < w / 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
    let frame = Arc::new(VideoFrame { par, ..VideoFrame::rgba8(w, h, px) });
    let info = MediaInfo {
        name: "halves".into(),
        kind: MediaKind::Movie,
        duration: Tick(10 * TICKS_PER_SECOND),
        video: Some(VideoStreamInfo {
            width: w,
            height: h,
            frame_rate: FPS,
            par,
            codec: "test".into(),
            pixel_format: "RGBA 8-bit".into(),
            color: filmcraft_color::ColorInfo::SRGB_FULL,
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: Vec::new(),
        container: "test".into(),
        start_timecode: None,
        file_size: None,
    };
    Halves { info, frame }
}

/// A project with a `src`-sized clip of `src_par` pixels on V1 of a `seq`-sized sequence with
/// `seq_par` pixels, placed as the timeline places media (Motion's points resolved, 100 %).
fn project(src: (u32, u32), src_par: (u32, u32), seq: (u32, u32), seq_par: (u32, u32), scale_to_frame: bool) -> (Project, ItemId, ItemId, SourceMap) {
    let mut p = Project::new("par");
    let source = halves(src.0, src.1, src_par);
    let clip = MediaClip {
        media: MediaRef::File { path: "halves.mov".into() },
        info: source.info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("halves", Label::Iris, ItemKind::Media(clip), None);
    let settings = SequenceSettings { width: seq.0, height: seq.1, par: seq_par, frame_rate: FPS, ..Default::default() };
    let sid = p.new_sequence("s", settings, 1, 0, None);
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(24)), FPS).unwrap();
    for e in &mut ti.effects {
        resolve_auto_points(e, seq, src);
    }
    ti.scale_to_frame = scale_to_frame;
    p.sequence_mut(sid).unwrap().video_tracks[0].items.push(ti);
    let mut map = SourceMap::default();
    map.0.insert(item, Arc::new(source) as SharedSource);
    (p, sid, item, map)
}

fn render(p: &Project, seq: ItemId, map: &SourceMap) -> Image {
    render_sequence(p, seq, Tick::ZERO, RenderOptions::default(), map)
}

/// Middle-row colour of column `x`: 'r', 'b' or '.' (transparent).
fn col(img: &Image, x: usize) -> char {
    let c = img.get(x, img.h / 2);
    match c {
        _ if c[3] < 0.01 => '.',
        _ if c[3] > 0.99 && c[0] > 0.99 && c[2] < 0.01 => 'r',
        _ if c[3] > 0.99 && c[2] > 0.99 && c[0] < 0.01 => 'b',
        _ => '?',
    }
}

/// The middle row as a string of `col`s, one per column.
fn row(img: &Image) -> String {
    (0..img.w).map(|x| col(img, x)).collect()
}

/// `got` is `want` except where bilinear filtering mixes neighbours: next to a change in `want`
/// and at the frame's edges.
#[track_caller]
fn assert_row(got: &str, want: &str) {
    let (g, w): (Vec<char>, Vec<char>) = (got.chars().collect(), want.chars().collect());
    assert_eq!(g.len(), w.len(), "\n got {got}\nwant {want}");
    for i in 1..w.len().saturating_sub(1) {
        if w[i - 1] == w[i] && w[i + 1] == w[i] {
            assert_eq!(g[i], w[i], "column {i}\n got {got}\nwant {want}");
        }
    }
}

#[test]
fn an_anamorphic_clip_fills_a_square_pixel_frame_at_100_percent() {
    // 1440 x 1080 with 4:3 pixels in 1920 x 1080: the whole frame, split in the middle
    let (p, seq, _, map) = project((144, 108), (4, 3), (192, 108), (1, 1), false);
    let img = render(&p, seq, &map);
    assert_row(&row(&img), &format!("{}{}", "r".repeat(96), "b".repeat(96)));
    // every row is covered, top to bottom
    for y in [1, 53, 106] {
        assert!(img.get(1, y)[3] > 0.99 && img.get(190, y)[3] > 0.99, "row {y}");
    }
    // the same picture with square pixels is pillarboxed at 1440 (144) wide: what the bug showed
    let (p, seq, _, map) = project((144, 108), (1, 1), (192, 108), (1, 1), false);
    assert_row(&row(&render(&p, seq, &map)), &format!("{}{}{}{}", ".".repeat(24), "r".repeat(72), "b".repeat(72), ".".repeat(24)));
}

#[test]
fn the_gpu_plan_places_an_anamorphic_clip_like_the_cpu() {
    let (p, seq, _, map) = project((144, 108), (4, 3), (192, 108), (1, 1), false);
    for scale in [1.0, 0.5] {
        let opts = RenderOptions { scale, ..Default::default() };
        let want = render_sequence(&p, seq, Tick::ZERO, opts, &map);
        let got = plan::execute_cpu(&plan::plan_frame(&p, seq, Tick::ZERO, opts, &map));
        assert_eq!((got.w, got.h), (want.w, want.h));
        assert_row(&row(&got), &row(&want));
    }
}

#[test]
fn scale_to_frame_size_fits_the_display_size() {
    // 1920-wide display picture in a 3840 x 1080 frame: fitted by height at 1:1, 1920 wide
    let (p, seq, _, map) = project((144, 108), (4, 3), (384, 108), (1, 1), true);
    assert_row(&row(&render(&p, seq, &map)), &format!("{}{}{}{}", ".".repeat(96), "r".repeat(96), "b".repeat(96), ".".repeat(96)));
    // fitted by width: 1920 x 1080 display in 960 x 1080 → 960 x 540, letterboxed
    let (p, seq, _, map) = project((144, 108), (4, 3), (96, 108), (1, 1), true);
    let img = render(&p, seq, &map);
    assert_row(&row(&img), &format!("{}{}", "r".repeat(48), "b".repeat(48)));
    assert!(img.get(48, 10)[3] < 0.01 && img.get(48, 54)[3] > 0.99, "letterboxed to 54 rows");
}

#[test]
fn a_sequence_with_the_clips_pixel_aspect_shows_it_pixel_for_pixel() {
    // New Sequence From Clip: 1440 x 1080, 4:3 pixels; the clip maps 1:1 onto the frame
    let (p, seq, _, map) = project((144, 108), (4, 3), (144, 108), (4, 3), false);
    let m = motion_matrix(p.sequence(seq).unwrap(), &p.sequence(seq).unwrap().video_tracks[0].items[0], (144, 108), Some((4, 3)), Tick::ZERO);
    assert!(identity_placement(&m), "{m:?}");
    let img = render(&p, seq, &map);
    assert_eq!(row(&img), format!("{}{}", "r".repeat(72), "b".repeat(72)));
    // the same ratio written differently is the same ratio
    let (p, seq, _, map) = project((144, 108), (8, 6), (144, 108), (4, 3), false);
    assert_eq!(render(&p, seq, &map).px, img.px);
}

#[test]
fn square_pixel_clips_are_narrowed_in_a_non_square_sequence() {
    // 1920 x 1080 square in 1440 x 1080 with 4:3 pixels: exactly the frame
    let (p, seq, _, map) = project((192, 108), (1, 1), (144, 108), (4, 3), false);
    assert_row(&row(&render(&p, seq, &map)), &format!("{}{}", "r".repeat(72), "b".repeat(72)));
    // 1440 x 1080 square (4:3 picture) in it: 1080 sequence pixels wide, pillarboxed
    let (p, seq, _, map) = project((144, 108), (1, 1), (144, 108), (4, 3), false);
    assert_row(&row(&render(&p, seq, &map)), &format!("{}{}{}{}", ".".repeat(18), "r".repeat(54), "b".repeat(54), ".".repeat(18)));
}

#[test]
fn rotation_happens_at_the_display_aspect() {
    // a 100 x 100 square-pixel source in a 200 x 100 sequence with 2:1 pixels shows 50 x 100
    // sequence pixels (a square on screen); turned 90° it is still that square
    let settings = SequenceSettings { width: 200, height: 100, par: (2, 1), ..Default::default() };
    let mut p = Project::new("rot");
    let sid = p.new_sequence("s", settings, 1, 0, None);
    let adj = p.add_item("a", Label::Iris, ItemKind::AdjustmentLayer { width: 100, height: 100, rate: FPS, duration: Tick(TICKS_PER_SECOND) }, None);
    let mut ti = p.make_track_item(adj, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(1)), FPS).unwrap();
    for e in &mut ti.effects {
        resolve_auto_points(e, (200, 100), (100, 100));
    }
    let seq = p.sequence(sid).unwrap();
    let r = filmcraft_geom::Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
    for rot in [0.0, 90.0, -270.0] {
        ti.effect_mut("motion").unwrap().params.get_mut("rotation").unwrap().value = ParamValue::Float(rot);
        let b = motion_matrix(seq, &ti, (100, 100), Some((1, 1)), Tick::ZERO).bounds(&r);
        assert!((b.w - 50.0).abs() < 1e-9 && (b.h - 100.0).abs() < 1e-9 && (b.x - 75.0).abs() < 1e-9, "{rot}°: {b:?}");
    }
    // a 45° turn: the square's diagonal, √2 · 100 display units, both ways
    ti.effect_mut("motion").unwrap().params.get_mut("rotation").unwrap().value = ParamValue::Float(45.0);
    let b = motion_matrix(seq, &ti, (100, 100), Some((1, 1)), Tick::ZERO).bounds(&r);
    let d = 100.0 * std::f64::consts::SQRT_2;
    assert!((b.w - d / 2.0).abs() < 1e-9 && (b.h - d).abs() < 1e-9, "{b:?}");
    // graphics and adjustment layers (no ratio of their own) are in sequence pixels
    ti.effect_mut("motion").unwrap().params.get_mut("rotation").unwrap().value = ParamValue::Float(0.0);
    let b = motion_matrix(seq, &ti, (100, 100), None, Tick::ZERO).bounds(&r);
    assert!((b.w - 100.0).abs() < 1e-9, "{b:?}");
}

#[test]
fn interpret_footage_overrides_the_files_pixel_aspect() {
    // the file says square; Interpret Footage says 4:3
    let (mut p, seq, item, map) = project((144, 108), (1, 1), (192, 108), (1, 1), false);
    match &mut p.item_mut(item).unwrap().kind {
        ItemKind::Media(m) => m.interpret.par = Some((4, 3)),
        _ => unreachable!(),
    }
    assert_row(&row(&render(&p, seq, &map)), &format!("{}{}", "r".repeat(96), "b".repeat(96)));
    // and the other way round: a 4:3 file interpreted as square is pillarboxed
    let (mut p, seq, item, map) = project((144, 108), (4, 3), (192, 108), (1, 1), false);
    match &mut p.item_mut(item).unwrap().kind {
        ItemKind::Media(m) => m.interpret.par = Some((1, 1)),
        _ => unreachable!(),
    }
    assert!(row(&render(&p, seq, &map)).starts_with(&".".repeat(24)));
}

#[test]
fn hostile_pixel_aspect_ratios_render_square_without_panicking() {
    let pillarboxed = format!("{}{}{}{}", ".".repeat(24), "r".repeat(72), "b".repeat(72), ".".repeat(24));
    let filled = format!("{}{}", "r".repeat(96), "b".repeat(96));
    let hostile = [(0, 0), (0, 3), (4, 0), (u32::MAX, 1), (1, u32::MAX), (9, 1), (1, 9)];
    for bad in hostile {
        // a clip's ratio: square
        let r = std::panic::catch_unwind(|| {
            let (p, seq, _, map) = project((144, 108), bad, (192, 108), (1, 1), false);
            row(&render(&p, seq, &map))
        });
        assert_row(&r.unwrap_or_default(), &pillarboxed);
        // a sequence's ratio (a damaged project): square, so a 4:3-pixel clip fills it
        let r = std::panic::catch_unwind(|| {
            let (p, seq, _, map) = project((144, 108), (4, 3), (192, 108), bad, true);
            let plan = plan::execute_cpu(&plan::plan_frame(&p, seq, Tick::ZERO, RenderOptions::default(), &map));
            (row(&render(&p, seq, &map)), row(&plan))
        });
        let (cpu, gpu) = r.unwrap_or_default();
        assert_row(&cpu, &filled);
        assert_row(&gpu, &filled);
        // an Interpret Footage override that makes no sense: the file's ratio
        let r = std::panic::catch_unwind(|| {
            let (mut p, seq, item, map) = project((144, 108), (4, 3), (192, 108), (1, 1), false);
            if let ItemKind::Media(m) = &mut p.item_mut(item).unwrap().kind {
                m.interpret.par = Some(bad);
            }
            row(&render(&p, seq, &map))
        });
        assert_row(&r.unwrap_or_default(), &filled);
    }
    // extreme but valid ratios on degenerate sizes stay finite
    for (src, par) in [((1, 1), (8, 1)), ((1, 32_768), (1, 8)), ((32_768, 1), (8, 1))] {
        let (p, seq, _, map) = project(src, par, (16, 16), (1, 8), true);
        let img = render(&p, seq, &map);
        assert!(img.px.iter().all(|v| v.is_finite()), "{src:?} {par:?}");
    }
}

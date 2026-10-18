//! Per-frame cost of the CPU compositor at 1920×1080, stage by stage, with synthetic sources:
//! a camera-like 8-bit 4:2:0 picture and a ProRes-4444-like 10-bit 4:4:4 banner whose alpha is
//! transparent except for its bottom 100 rows.
//!
//!     cargo run --release -p filmcraft-render --example frame_bench
//!
//! Prints the median of 40 frames (after 6 warm-up frames) in milliseconds, wall time on the
//! global rayon pool, and a checksum of the final 8-bit picture of each scenario so two builds can
//! be compared for identical output.

use std::sync::Arc;
use std::time::Instant;

use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::blend::{self, Blend};
use filmcraft_render::{Image, RenderOptions, SourceMap, render_sequence};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

const W: usize = 1920;
const H: usize = 1080;
const FPS: FrameRate = FrameRate::FPS_25;

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

/// A camera picture: 8-bit 4:2:0, limited range BT.709, no alpha.
fn camera() -> Arc<VideoFrame> {
    let y: Vec<u8> = (0..W * H).map(|i| (16 + ((i % W) * 200 / W + (i / W) * 30 / H + (i * 7919) % 5) % 219) as u8).collect();
    let (cw, ch) = (W / 2, H / 2);
    let u: Vec<u8> = (0..cw * ch).map(|i| (100 + (i % cw) * 40 / cw) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (150 - (i / cw) * 40 / ch) as u8).collect();
    Arc::new(VideoFrame {
        width: W as u32,
        height: H as u32,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    })
}

/// A banner like the channel's: 10-bit 4:4:4 with an alpha plane, opaque only in the bottom `bar` rows
/// (with a soft one-row edge), transparent elsewhere.
fn banner(bar: usize) -> Arc<VideoFrame> {
    let n = W * H;
    let top = H - bar;
    let alpha: Vec<u16> = (0..n)
        .map(|i| match i / W {
            r if r + 1 < top => 0,
            r if r < top => 512,
            _ => 1023,
        })
        .collect();
    Arc::new(VideoFrame {
        width: W as u32,
        height: H as u32,
        data: PixelData::Yuv16 {
            planes: [Arc::new(vec![300u16; n]), Arc::new((0..n).map(|i| 512 + (i % 64) as u16).collect()), Arc::new(vec![600u16; n])],
            chroma: Chroma::C444,
            bits: 10,
            alpha: Some(Arc::new(alpha)),
        },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    })
}

fn project(layers: &[Arc<VideoFrame>]) -> (Project, filmcraft_project::ItemId, SourceMap) {
    let mut p = Project::new("bench");
    let mut map = SourceMap::default();
    let seq = p.new_sequence("s", SequenceSettings { width: W as u32, height: H as u32, frame_rate: FPS, ..Default::default() }, layers.len().max(1), 1, None);
    for (track, frame) in layers.iter().enumerate() {
        let g = GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 0.0, 1.0] }, W as u32, H as u32, FPS, Tick(60 * TICKS_PER_SECOND));
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
        let mut clip = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(2000)), FPS).expect("clip");
        clip.scale_to_frame = true;
        if let Some(t) = p.sequence_mut(seq).and_then(|s| s.video_tracks.get_mut(track)) {
            t.items.push(clip);
        }
        map.0.insert(item, Arc::new(Still { info, frame: frame.clone() }));
    }
    (p, seq, map)
}

fn median_ms(mut f: impl FnMut()) -> f64 {
    for _ in 0..6 {
        f();
    }
    let mut v: Vec<f64> = (0..40)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3))
}

fn frame_failed<T>(error: String) -> T {
    eprintln!("Frame benchmark failed: {error}");
    std::process::exit(1)
}

fn main() {
    println!("rayon threads: {}", rayon::current_num_threads());
    let (cam, ban) = (camera(), banner(100));
    println!("\n-- stages (ms per frame, 1920x1080) --");
    println!(
        "camera frame -> linear f32 image : {:6.2}",
        median_ms(|| drop(std::hint::black_box(cam.to_linear_f32_decimated(1).unwrap_or_else(frame_failed))))
    );
    println!(
        "banner frame -> linear f32 image : {:6.2}",
        median_ms(|| drop(std::hint::black_box(ban.to_linear_f32_decimated(1).unwrap_or_else(frame_failed))))
    );
    println!("Image::new (zeroed 33 MB canvas) : {:6.2}", median_ms(|| drop(std::hint::black_box(Image::new(W, H)))));
    let (_, _, cpx) = cam.to_linear_f32_decimated(1).unwrap_or_else(frame_failed);
    let (_, _, bpx) = ban.to_linear_f32_decimated(1).unwrap_or_else(frame_failed);
    let (cam_img, ban_img) = (Image { w: W, h: H, px: cpx }, Image { w: W, h: H, px: bpx });
    let mut canvas = Image::new(W, H);
    println!("composite camera over empty      : {:6.2}", median_ms(|| blend::composite(&mut canvas, &cam_img, 1.0, Blend::Normal)));
    println!("composite banner over camera     : {:6.2}", median_ms(|| blend::composite(&mut canvas, &ban_img, 1.0, Blend::Normal)));
    println!("linear f32 -> sRGB 8-bit RGBA    : {:6.2}", median_ms(|| drop(std::hint::black_box(canvas.over_black_rgba8()))));

    // FC_RECYCLE=1 hands each frame's float image back to the pool, as the exporter does
    let recycle = std::env::var_os("FC_RECYCLE").is_some();
    println!("\n-- whole frames: render_sequence + 8-bit conversion (ms per frame){} --", if recycle { ", buffers recycled" } else { "" });
    for (name, layers) in [("camera only", vec![cam.clone()]), ("camera + banner", vec![cam.clone(), ban.clone()])] {
        let (p, seq, map) = project(&layers);
        let opts = RenderOptions::default();
        let mut last = Vec::new();
        let ms = median_ms(|| {
            let img = render_sequence(&p, seq, FPS.tick_of(10), opts, &map).unwrap_or_else(frame_failed);
            last = img.over_black_rgba8();
            if recycle {
                filmcraft_frame::pool::recycle_f32(img.px);
            }
        });
        let render_only = median_ms(|| {
            let img = std::hint::black_box(render_sequence(&p, seq, FPS.tick_of(10), opts, &map).unwrap_or_else(frame_failed));
            if recycle {
                filmcraft_frame::pool::recycle_f32(img.px);
            }
        });
        println!("{name:16}: render {render_only:6.2}   render + 8-bit {ms:6.2}   checksum {:016x}", checksum(&last));
    }
}

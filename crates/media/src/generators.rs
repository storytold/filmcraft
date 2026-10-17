//! Synthetic media: Bars and Tone, Color Matte, Black/Transparent Video, Universal Counting Leader,
//! and procedural demo scenes (so the editor has beautiful moving footage with no files at all).
//!
//! Every generator is a pure function of (settings, media time, output scale): deterministic and
//! resolution-independent, so low-resolution playback renders directly at the reduced size.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use filmcraft_color::ColorInfo;
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::digits::{draw_text, text_width};
use crate::{AudioStreamInfo, FrameCache, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DemoScene {
    Aurora,
    OceanSunset,
    CityNight,
    Dunes,
    Plasma,
    Forest,
}

impl DemoScene {
    pub const ALL: [DemoScene; 6] = [DemoScene::OceanSunset, DemoScene::Aurora, DemoScene::CityNight, DemoScene::Dunes, DemoScene::Forest, DemoScene::Plasma];
    pub fn file_name(self) -> &'static str {
        match self {
            DemoScene::Aurora => "Aurora_Timelapse.mp4",
            DemoScene::OceanSunset => "Ocean_Sunset.mp4",
            DemoScene::CityNight => "City_Night_Drive.mp4",
            DemoScene::Dunes => "Desert_Dunes.mp4",
            DemoScene::Plasma => "Neon_Loop.mov",
            DemoScene::Forest => "Misty_Forest.mp4",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Generator {
    BarsAndTone,
    ColorMatte {
        color: [f32; 4],
    },
    BlackVideo,
    TransparentVideo,
    CountingLeader,
    Demo(DemoScene),
    /// Pure audio tone (Hz, dBFS).
    Tone {
        hz: f32,
        db: f32,
    },
}

impl Generator {
    pub fn label(&self) -> String {
        match self {
            Generator::BarsAndTone => "Bars and Tone".into(),
            Generator::ColorMatte { .. } => "Color Matte".into(),
            Generator::BlackVideo => "Black Video".into(),
            Generator::TransparentVideo => "Transparent Video".into(),
            Generator::CountingLeader => "Universal Counting Leader".into(),
            Generator::Demo(d) => d.file_name().into(),
            Generator::Tone { hz, .. } => format!("Tone {hz} Hz"),
        }
    }
    fn has_audio(&self) -> bool {
        matches!(self, Generator::BarsAndTone | Generator::CountingLeader | Generator::Demo(_) | Generator::Tone { .. })
    }
    fn has_video(&self) -> bool {
        !matches!(self, Generator::Tone { .. })
    }
}

/// A generator bound to a frame size/rate/duration.
pub struct GeneratorSource {
    pub generator: Generator,
    info: MediaInfo,
    cache: FrameCache<(i64, u16)>,
}

impl GeneratorSource {
    pub fn new(generator: Generator, width: u32, height: u32, rate: FrameRate, duration: Tick) -> Self {
        let video = generator.has_video().then(|| VideoStreamInfo {
            width,
            height,
            frame_rate: rate,
            par: (1, 1),
            codec: match &generator {
                Generator::Demo(_) => "H.264 (synthetic)".into(),
                _ => "Synthetic".into(),
            },
            pixel_format: "RGBA 8-bit".into(),
            color: ColorInfo::SRGB_FULL,
            has_alpha: matches!(generator, Generator::TransparentVideo),
            bitrate: None,
            hdr: None,
        });
        let audio =
            generator.has_audio().then(|| AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: "PCM (synthetic)".into(), bits_per_sample: Some(32) });
        let info = MediaInfo {
            name: generator.label(),
            kind: if matches!(generator, Generator::Demo(_)) { MediaKind::Movie } else { MediaKind::Synthetic },
            duration,
            video,
            audio_streams: audio.into_iter().collect(),
            container: if matches!(generator, Generator::Demo(_)) { "Synthetic MPEG-4".into() } else { "Synthetic".into() },
            start_timecode: None,
            file_size: None,
        };
        Self { generator, info, cache: FrameCache::new(256 << 20) }
    }

    pub fn with_name(mut self, name: &str) -> Self {
        self.info.name = name.to_string();
        self
    }

    /// The default demo clip for a scene (1080p, 23.976, 12–20 s).
    pub fn demo(scene: DemoScene) -> Self {
        let secs = match scene {
            DemoScene::OceanSunset => 18,
            DemoScene::Aurora => 16,
            DemoScene::CityNight => 14,
            DemoScene::Dunes => 15,
            DemoScene::Plasma => 10,
            DemoScene::Forest => 12,
        };
        Self::new(Generator::Demo(scene), 1920, 1080, FrameRate::FPS_23_976, Tick(secs * TICKS_PER_SECOND))
    }
}

impl MediaSource for GeneratorSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        let v = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let rate = v.frame_rate;
        let frame = rate.frame_at(req.time.max(Tick::ZERO));
        let scale = req.scale.clamp(1.0 / 64.0, 1.0);
        let bucket = (scale * 64.0).round() as u16;
        let s = bucket as f32 / 64.0;
        let (w, h) = (((v.width as f32 * s).round() as u32).max(2), ((v.height as f32 * s).round() as u32).max(2));
        let t = rate.tick_of(frame).seconds() as f32;
        self.cache.get_or_insert_with((frame, bucket), || {
            let px = render(&self.generator, w, h, t, frame, rate);
            Ok(Arc::new(VideoFrame::rgba8(w, h, px).with_pts(rate.tick_of(frame))))
        })
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        if !self.generator.has_audio() {
            return Err(MediaError::NoStream("audio"));
        }
        let mut out = AudioBuffer::silence(sample_rate, 2, frames);
        let sr = sample_rate as f32;
        let total = self.info.duration.to_units_floor(sample_rate as i64);
        for i in 0..frames {
            let n = start + i as i64;
            if n < 0 || n >= total {
                continue;
            }
            let t = n as f32 / sr;
            let (l, r) = audio_sample(&self.generator, t, n);
            out.channels[0][i] = l;
            out.channels[1][i] = r;
        }
        Ok(out)
    }
}

fn db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn hash(n: i64) -> f32 {
    let mut x = n as u64 ^ 0x9E37_79B9_7F4A_7C15;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
}

fn audio_sample(g: &Generator, t: f32, n: i64) -> (f32, f32) {
    match g {
        Generator::BarsAndTone => {
            let s = (TAU * 1000.0 * t).sin() * db(-20.0);
            (s, s)
        }
        Generator::Tone { hz, db: d } => {
            let s = (TAU * hz * t).sin() * db(*d);
            (s, s)
        }
        Generator::CountingLeader => {
            // 1 kHz beep for one frame at each second mark (the "2-pop" style beep).
            let frac = t.fract();
            let s = if frac < 1.0 / 24.0 { (TAU * 1000.0 * t).sin() * db(-20.0) } else { 0.0 };
            (s, s)
        }
        Generator::Demo(scene) => demo_audio(*scene, t, n),
        _ => (0.0, 0.0),
    }
}

fn demo_audio(scene: DemoScene, t: f32, n: i64) -> (f32, f32) {
    // A slow pad: chord tones with gentle detune + scene texture.
    let chord: &[f32] = match scene {
        DemoScene::OceanSunset => &[220.0, 277.18, 329.63, 440.0],
        DemoScene::Aurora => &[196.0, 246.94, 293.66, 369.99],
        DemoScene::CityNight => &[110.0, 164.81, 207.65, 246.94],
        DemoScene::Dunes => &[146.83, 220.0, 261.63, 329.63],
        DemoScene::Plasma => &[130.81, 196.0, 261.63, 311.13],
        DemoScene::Forest => &[174.61, 220.0, 261.63, 349.23],
    };
    let env = 0.6 + 0.4 * (TAU * t / 8.0).sin();
    let mut l = 0.0;
    let mut r = 0.0;
    for (i, f) in chord.iter().enumerate() {
        let ph = TAU * f * t;
        let trem = 0.8 + 0.2 * (TAU * (0.2 + i as f32 * 0.07) * t).sin();
        l += (ph * 1.002).sin() * trem;
        r += (ph * 0.998).sin() * trem;
    }
    let noise = hash(n)
        * match scene {
            DemoScene::OceanSunset => 0.25 * (0.5 + 0.5 * (TAU * t / 6.0).sin()).powi(2),
            DemoScene::Forest => 0.05,
            DemoScene::CityNight => 0.08,
            _ => 0.02,
        };
    // A soft kick in the city/plasma scenes to make waveforms interesting.
    let beat = if matches!(scene, DemoScene::CityNight | DemoScene::Plasma) {
        let bt = (t * 2.0).fract() / 2.0;
        (TAU * 55.0 * bt).sin() * (-bt * 18.0).exp() * 0.9
    } else {
        0.0
    };
    let g = 0.09 * env;
    (l * g + noise + beat, r * g + noise * 0.9 + beat)
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn vnoise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (xf, yf) = (x - xi, y - yi);
    let h = |a: f32, b: f32| hash((a as i64).wrapping_mul(73_856_093) ^ (b as i64).wrapping_mul(19_349_663));
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let a = h(xi, yi);
    let b = h(xi + 1.0, yi);
    let c = h(xi, yi + 1.0);
    let d = h(xi + 1.0, yi + 1.0);
    (a + (b - a) * u) + ((c + (d - c) * u) - (a + (b - a) * u)) * v
}

fn fbm(x: f32, y: f32, oct: u32) -> f32 {
    let mut s = 0.0;
    let mut a = 0.5;
    let (mut fx, mut fy) = (x, y);
    for _ in 0..oct {
        s += a * vnoise(fx, fy);
        fx = fx * 2.03 + 1.7;
        fy = fy * 2.01 + 9.2;
        a *= 0.5;
    }
    s
}

fn to8(c: [f32; 3]) -> [u8; 4] {
    [(c[0].clamp(0.0, 1.0) * 255.0) as u8, (c[1].clamp(0.0, 1.0) * 255.0) as u8, (c[2].clamp(0.0, 1.0) * 255.0) as u8, 255]
}

/// Render a generator frame as straight-alpha sRGB RGBA8.
pub fn render(g: &Generator, w: u32, h: u32, t: f32, frame: i64, rate: FrameRate) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![0u8; wu * hu * 4];
    match g {
        Generator::BarsAndTone => bars(&mut px, wu, hu),
        Generator::ColorMatte { color } => {
            let c = [(color[0] * 255.0) as u8, (color[1] * 255.0) as u8, (color[2] * 255.0) as u8, (color[3] * 255.0) as u8];
            px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.copy_from_slice(&c));
        }
        Generator::BlackVideo => px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.copy_from_slice(&[0, 0, 0, 255])),
        Generator::TransparentVideo | Generator::Tone { .. } => {}
        Generator::CountingLeader => leader(&mut px, wu, hu, t, frame, rate),
        Generator::Demo(scene) => {
            let scene = *scene;
            px.par_chunks_mut(wu * 4).enumerate().for_each(|(y, row)| {
                let v = y as f32 / hu as f32;
                for x in 0..wu {
                    let u = x as f32 / wu as f32;
                    let c = demo_pixel(scene, u, v, wu as f32 / hu as f32, t);
                    row[x * 4..x * 4 + 4].copy_from_slice(&to8(c));
                }
            });
        }
    }
    px
}

fn bars(px: &mut [u8], w: usize, h: usize) {
    // SMPTE RP 219-style HD bars (75% bars, 40% grey side panels, ramp row, PLUGE row).
    let g40 = [104, 104, 104, 255];
    let bars75: [[u8; 4]; 7] =
        [[191, 191, 191, 255], [191, 191, 0, 255], [0, 191, 191, 255], [0, 191, 0, 255], [191, 0, 191, 255], [191, 0, 0, 255], [0, 0, 191, 255]];
    let side = w / 8;
    let bw = (w - 2 * side) as f32 / 7.0;
    let r1 = h * 7 / 12;
    let r2 = r1 + h / 12;
    let r3 = r2 + h / 12;
    for y in 0..h {
        for x in 0..w {
            let c: [u8; 4] = if y < r1 {
                if x < side || x >= w - side { g40 } else { bars75[(((x - side) as f32 / bw) as usize).min(6)] }
            } else if y < r2 {
                if x < side {
                    [0, 255, 255, 255]
                } else if x >= w - side {
                    [0, 0, 255, 255]
                } else if x < side + bw as usize {
                    [235, 235, 235, 255]
                } else {
                    [191, 191, 191, 255]
                }
            } else if y < r3 {
                if x < side {
                    [255, 255, 0, 255]
                } else if x >= w - side {
                    [255, 0, 0, 255]
                } else {
                    let v = ((x - side) as f32 / (w - 2 * side) as f32 * 255.0) as u8;
                    [v, v, v, 255]
                }
            } else {
                let fx = x as f32 / w as f32;
                let v: u8 = if fx < 0.125 {
                    38
                } else if fx < 0.3 {
                    0
                } else if fx < 0.5 {
                    255
                } else if fx < 0.65 {
                    0
                } else if fx < 0.68 {
                    5
                } else if fx < 0.71 {
                    0
                } else if fx < 0.74 {
                    10
                } else if fx < 0.875 {
                    0
                } else {
                    38
                };
                [v, v, v, 255]
            };
            px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
        }
    }
}

fn leader(px: &mut [u8], w: usize, h: usize, t: f32, _frame: i64, _rate: FrameRate) {
    // 8-second countdown: number = 8 - floor(t), sweep angle = fract(t).
    let n = (8 - t.floor() as i64).clamp(2, 8);
    let sweep = t.fract() * TAU;
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let r_out = h as f32 * 0.42;
    px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let ang = (dx.atan2(-dy) + TAU) % TAU;
            let mut c = if ang < sweep { [0.55, 0.55, 0.55] } else { [0.78, 0.78, 0.78] };
            let ring = |r: f32, wdt: f32| (d - r).abs() < wdt;
            if ring(r_out, 3.0) || ring(r_out * 0.86, 2.0) || dx.abs() < 1.5 || dy.abs() < 1.5 {
                c = [0.1, 0.1, 0.1];
            }
            if d > r_out + 3.0 {
                c = [0.2, 0.2, 0.2];
            }
            row[x * 4..x * 4 + 4].copy_from_slice(&to8(c));
        }
    });
    let dh = (h as i32) / 3;
    let s = n.to_string();
    let tw = text_width(dh, &s);
    draw_text(px, w, h, (w as i32 - tw) / 2, (h as i32 - dh) / 2, dh, &s, [20, 20, 20, 255]);
}

fn demo_pixel(scene: DemoScene, u: f32, v: f32, aspect: f32, t: f32) -> [f32; 3] {
    match scene {
        DemoScene::OceanSunset => {
            let horizon = 0.58;
            let sun = [0.5 + 0.02 * (t * 0.05).sin(), 0.42 + t * 0.006];
            if v < horizon {
                let k = v / horizon;
                let mut c = lerp3([0.16, 0.10, 0.32], [0.98, 0.52, 0.28], k.powf(1.6));
                c = lerp3(c, [1.0, 0.82, 0.55], smoothstep(0.75, 1.0, k) * 0.6);
                let dx = (u - sun[0]) * aspect;
                let dy = v - sun[1];
                let d = (dx * dx + dy * dy).sqrt();
                let glow = (-d * 9.0).exp();
                c = lerp3(c, [1.0, 0.93, 0.75], glow * 0.8);
                if d < 0.055 {
                    c = [1.0, 0.95, 0.82];
                }
                // wispy clouds
                let cl = fbm(u * 4.0 + t * 0.02, v * 14.0, 4);
                let cm = smoothstep(0.15, 0.45, cl) * smoothstep(0.1, 0.45, v) * (1.0 - smoothstep(0.45, 0.55, v));
                lerp3(c, [0.55, 0.28, 0.35], cm * 0.55)
            } else {
                let k = (v - horizon) / (1.0 - horizon);
                let mut c = lerp3([0.35, 0.18, 0.28], [0.03, 0.05, 0.12], k.powf(0.7));
                let wave = fbm(u * 18.0 * (1.0 - k * 0.6) + t * 0.3, (v - horizon) * 90.0 / (0.3 + k) - t * 1.4, 3);
                let dx = (u - sun[0]) * aspect;
                let path = (-dx * dx * 160.0 / (0.2 + k * 2.0)).exp();
                let sparkle = smoothstep(0.05, 0.4, wave) * path;
                c = lerp3(c, [1.0, 0.78, 0.5], sparkle * (1.0 - k * 0.5));
                lerp3(c, [0.9, 0.5, 0.35], path * 0.25 * (1.0 - k))
            }
        }
        DemoScene::Aurora => {
            let mut c = lerp3([0.01, 0.02, 0.08], [0.02, 0.1, 0.18], v);
            // stars
            let sx = (u * 900.0).floor();
            let sy = (v * 500.0).floor();
            let st = hash((sx as i64) * 7919 + sy as i64 * 104_729);
            if st > 0.994 && v < 0.7 {
                let tw = 0.6 + 0.4 * (t * 3.0 + st * 100.0).sin();
                c = lerp3(c, [0.9, 0.95, 1.0], tw);
            }
            // aurora curtains
            for (i, col) in [[0.2, 1.0, 0.55], [0.55, 0.3, 1.0], [0.1, 0.9, 0.8]].iter().enumerate() {
                let fi = i as f32;
                let base = 0.28 + 0.08 * fi + 0.06 * (u * 3.0 + t * 0.15 + fi).sin() + 0.05 * fbm(u * 3.0 + t * 0.05, fi, 3);
                let dy = v - base;
                let ray = 0.5 + 0.5 * fbm(u * 40.0 + t * 0.4 + fi * 10.0, fi, 2);
                let a = (-dy * dy * 90.0).exp() * if dy > 0.0 { 1.0 } else { (dy * 12.0).exp() } * ray;
                c = lerp3(c, *col, (a * 0.8).min(1.0));
            }
            // mountains
            let m = 0.72 + 0.08 * fbm(u * 3.0, 3.3, 5) + 0.05 * (u * 5.0).sin();
            if v > m {
                let snow = smoothstep(0.02, 0.0, v - m) * 0.3;
                c = lerp3([0.02, 0.03, 0.05], [0.4, 0.5, 0.6], snow);
            }
            // reflection lake
            if v > 0.9 {
                c = lerp3(c, [0.05, 0.25, 0.2], 0.3 * (1.0 + (v * 400.0 + t * 4.0).sin()) * 0.5);
            }
            c
        }
        DemoScene::CityNight => {
            let mut c = lerp3([0.05, 0.02, 0.15], [0.55, 0.2, 0.35], v.powf(2.2));
            let layers = [(0.15, 0.55, [0.12, 0.08, 0.2], 0.02), (0.28, 0.62, [0.07, 0.05, 0.12], 0.05), (0.45, 0.7, [0.03, 0.02, 0.06], 0.12)];
            for (li, (bw, base, col, speed)) in layers.iter().enumerate() {
                let x = u * aspect / bw + t * speed;
                let bi = x.floor();
                let hgt = base - 0.32 * (0.5 + 0.5 * hash(bi as i64 * 31 + li as i64));
                if v > hgt {
                    c = *col;
                    let wx = (x.fract() * 6.0).floor();
                    let wy = ((v - hgt) * 70.0).floor();
                    let lit = hash((bi as i64) * 1000 + wx as i64 * 37 + wy as i64 * 101 + li as i64);
                    if (x.fract() * 6.0).fract() < 0.55 && ((v - hgt) * 70.0).fract() < 0.5 && lit > 0.35 && x.fract() > 0.08 && x.fract() < 0.92 {
                        let warm = [1.0, 0.8 + 0.1 * lit, 0.45];
                        c = lerp3(c, warm, 0.35 + 0.35 * (li as f32 / 2.0));
                    }
                }
            }
            // light trails on the road
            if v > 0.86 {
                c = [0.04, 0.03, 0.06];
                for k in 0..6 {
                    let lane = 0.88 + k as f32 * 0.02;
                    let d = (v - lane).abs();
                    let streak = (u * 6.0 - t * (1.5 + k as f32 * 0.4) + k as f32).fract();
                    let col = if k % 2 == 0 { [1.0, 0.25, 0.2] } else { [1.0, 0.9, 0.7] };
                    c = lerp3(c, col, (-d * 900.0).exp() * smoothstep(0.0, 0.8, streak));
                }
            }
            c
        }
        DemoScene::Dunes => {
            let mut c = lerp3([0.35, 0.6, 0.9], [0.98, 0.85, 0.65], smoothstep(0.0, 0.5, v));
            let sun_d = ((u - 0.75) * aspect).hypot(v - 0.22);
            c = lerp3(c, [1.0, 0.97, 0.85], (-sun_d * 14.0).exp());
            for k in 0..4 {
                let fk = k as f32;
                let x = u * (1.0 + fk * 0.6) + t * 0.01 * (fk + 1.0);
                let ridge = 0.45 + fk * 0.12 + 0.06 * (x * 6.0 + fk).sin() + 0.03 * fbm(x * 4.0, fk, 3);
                if v > ridge {
                    let shade = 0.5 + 0.5 * ((x * 6.0 + fk).cos());
                    let base = lerp3([0.78, 0.45, 0.22], [0.98, 0.72, 0.45], shade);
                    c = lerp3(base, [0.55, 0.28, 0.15], (v - ridge).min(0.2) * 2.0 * (fk / 3.0));
                }
            }
            c
        }
        DemoScene::Plasma => {
            let x = u * aspect * 3.0;
            let y = v * 3.0;
            let p = (x + t).sin() + (y * 1.3 - t * 0.7).sin() + ((x + y + t * 0.5).sin()) + ((x * x + y * y).sqrt() * 2.0 - t * 1.2).sin();
            let h = p * 0.25 + t * 0.05;
            [0.5 + 0.5 * (PI * h).sin(), 0.5 + 0.5 * (PI * h + 2.1).sin() * 0.6, 0.5 + 0.5 * (PI * h + 4.2).sin()]
        }
        DemoScene::Forest => {
            let mut c = lerp3([0.75, 0.82, 0.8], [0.35, 0.45, 0.4], v);
            // god rays
            let ray = 0.5 + 0.5 * fbm(u * 8.0 - v * 3.0 + t * 0.05, 1.0, 2);
            c = lerp3(c, [1.0, 0.97, 0.85], ray * (1.0 - v) * 0.35);
            for k in 0..5 {
                let fk = k as f32;
                let depth = 1.0 - fk / 5.0;
                let spacing = 0.12 + fk * 0.03;
                let x = u / spacing + t * 0.03 * (fk + 1.0) + fk * 3.3;
                let trunk_x = x.fract();
                let tw = 0.08 + 0.05 * (1.0 - depth);
                let id = x.floor();
                if (trunk_x - 0.5).abs() < tw && hash(id as i64 * 13 + k) > -0.2 {
                    let dark = lerp3([0.45, 0.5, 0.48], [0.05, 0.07, 0.05], 1.0 - depth * 0.8);
                    c = lerp3(c, dark, 0.85);
                }
            }
            let mist = smoothstep(0.55, 1.0, v) * (0.6 + 0.4 * fbm(u * 3.0 + t * 0.1, v * 5.0, 3));
            lerp3(c, [0.85, 0.88, 0.86], mist * 0.6)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_scaled() {
        let g = GeneratorSource::demo(DemoScene::OceanSunset);
        let a = g.video_frame(FrameRequest { time: Tick(TICKS_PER_SECOND), scale: 0.25 }).unwrap();
        assert_eq!((a.width, a.height), (480, 270));
        let b = render(&Generator::Demo(DemoScene::OceanSunset), 480, 270, FrameRate::FPS_23_976.tick_of(23).seconds() as f32, 23, FrameRate::FPS_23_976);
        assert_eq!(a.to_rgba8().unwrap(), b);
    }

    #[test]
    fn bars_values() {
        let px = render(&Generator::BarsAndTone, 800, 600, 0.0, 0, FrameRate::FPS_25);
        // first bar after the side panel is 75% white
        let i = (10 * 800 + 150) * 4;
        assert_eq!(&px[i..i + 4], &[191, 191, 191, 255]);
        let g = GeneratorSource::new(Generator::BarsAndTone, 800, 600, FrameRate::FPS_25, Tick(TICKS_PER_SECOND));
        let a = g.audio(0, 480, 48000).unwrap();
        let peak = a.peaks()[0];
        assert!((peak - db(-20.0)).abs() < 1e-3);
    }

    #[test]
    fn all_scenes_render() {
        for s in DemoScene::ALL {
            let px = render(&Generator::Demo(s), 64, 36, 1.5, 36, FrameRate::FPS_24);
            assert!(px.as_chunks::<4>().0.iter().any(|p| p[0] > 20 || p[1] > 20 || p[2] > 20));
        }
        let px = render(&Generator::CountingLeader, 64, 36, 1.5, 36, FrameRate::FPS_24);
        assert_eq!(px.len(), 64 * 36 * 4);
    }
}

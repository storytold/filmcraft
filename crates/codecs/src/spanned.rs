//! A source that plays several same-format sources back to back as one clip.
//!
//! Insta360 cameras split a long recording into numbered parts (`VID_…_00_054.insv`,
//! `…_055`, `…_056`, …) that each play standalone; the engine joins the parts of one
//! recording into a single [`SpannedSource`] so the timeline sees one clip. Media time
//! `t` maps to the part whose `[start, end)` covers it, requested at `t - start`; an audio
//! window that straddles a boundary is read from both parts and stitched. The parts are
//! opened by the caller (each through the normal openers), so this combinator is format-
//! agnostic.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, SharedSource};
use filmcraft_time::Tick;

/// Same-format sources concatenated in `parts` order into one clip.
pub struct SpannedSource {
    parts: Vec<SharedSource>,
    /// Global start of each part (`starts[0]` is zero); `ends[i]` its end.
    starts: Vec<Tick>,
    ends: Vec<Tick>,
    info: MediaInfo,
}

impl SpannedSource {
    /// Join `parts` (non-empty, in play order). The combined info takes the first part's
    /// streams and the summed duration and size.
    pub fn new(name: &str, parts: Vec<SharedSource>) -> Result<Self, MediaError> {
        if parts.is_empty() {
            return Err(MediaError::Unsupported("spanned clip with no parts".into()));
        }
        let mut starts = Vec::with_capacity(parts.len());
        let mut ends = Vec::with_capacity(parts.len());
        let mut acc = Tick::ZERO;
        for p in &parts {
            starts.push(acc);
            acc += p.info().duration.max(Tick::ZERO);
            ends.push(acc);
        }
        let mut info = parts[0].info().clone();
        // The parts must present the same picture and sound: a mismatch is a damaged or
        // mixed-up set, and joining them would play nonsense on the timeline.
        let key = |i: &MediaInfo| {
            let v = i.video.as_ref().map(|v| (v.width, v.height, v.frame_rate.num, v.frame_rate.den));
            let a: Vec<(u32, u32)> = i.audio_streams.iter().map(|a| (a.sample_rate, a.channels)).collect();
            (v, a)
        };
        let k0 = key(&info);
        for p in &parts[1..] {
            if key(p.info()) != k0 {
                return Err(MediaError::Unsupported(format!("{}: spanned parts differ in size, frame rate or audio", p.info().name)));
            }
        }
        info.name = name.to_string();
        info.container = format!("{} (spanned)", info.container);
        info.duration = acc;
        info.file_size = parts.iter().map(|p| p.info().file_size).try_fold(0u64, |a, s| Some(a.checked_add(s?))?);
        Ok(Self { parts, starts, ends, info })
    }

    /// The part index presenting media time `t` (clamped to the last part past the end).
    fn part_at(&self, t: Tick) -> usize {
        // the first part whose end is past `t`
        self.ends.iter().position(|&e| e > t).unwrap_or(self.parts.len() - 1)
    }
}

impl MediaSource for SpannedSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let t = req.time.max(Tick::ZERO);
        let i = self.part_at(t);
        let local = (t - self.starts[i]).min(self.parts[i].info().duration);
        self.parts[i].video_frame(FrameRequest { time: local, scale: req.scale })
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.audio_stream(0, start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let rate = i64::from(sample_rate.max(1));
        let ch = self.info.audio_streams.get(stream).map_or(2, |a| a.channels.max(1) as usize);
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
        if frames == 0 {
            return Ok(out);
        }
        // the requested window in global ticks, then each part's slice of it
        let g0 = Tick::from_rational(start, 1, rate);
        let g1 = Tick::from_rational(start.saturating_add(frames as i64), 1, rate);
        for i in 0..self.parts.len() {
            let lo = g0.max(self.starts[i]);
            let hi = g1.min(self.ends[i]);
            if lo >= hi {
                continue;
            }
            let ls = (lo - self.starts[i]).to_rational_floor(1, rate);
            let ln = (hi - lo).to_rational_floor(1, rate).max(1);
            let os = (lo - g0).to_rational_floor(1, rate).max(0) as usize;
            let got = self.parts[i].audio_stream(stream, ls, ln as usize, sample_rate)?;
            let n = got.frames().min(frames.saturating_sub(os));
            for c in 0..ch.min(got.channel_count()) {
                let (dst, src) = (&mut out.channels[c], &got.channels[c]);
                for k in 0..n {
                    if let (Some(d), Some(s)) = (dst.get_mut(os + k), src.get(k)) {
                        *d = *s;
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_frame::PixelData;
    use filmcraft_media::{AudioStreamInfo, MediaKind, VideoStreamInfo};
    use filmcraft_time::FrameRate;

    /// A synthetic source: `dur` seconds at 24 fps; every pixel's red is `v` (the part's
    /// id) and the first pixel's alpha is the local frame index (to check the time map).
    struct Dummy {
        info: MediaInfo,
        v: u8,
    }

    impl Dummy {
        fn source(dur_secs: i64, v: u8) -> SharedSource {
            let rate = FrameRate::FPS_24;
            Arc::new(Dummy {
                info: MediaInfo {
                    name: format!("part{v}"),
                    kind: MediaKind::Movie,
                    duration: rate.tick_of(dur_secs * 24),
                    video: Some(VideoStreamInfo {
                        width: 4,
                        height: 2,
                        frame_rate: rate,
                        par: (1, 1),
                        codec: "test".into(),
                        pixel_format: "RGBA 8-bit".into(),
                        color: filmcraft_color::ColorInfo::SRGB_FULL,
                        has_alpha: false,
                        bitrate: None,
                        hdr: None,
                        color_assumed: None,
                    }),
                    audio_streams: vec![AudioStreamInfo { sample_rate: 48_000, channels: 1, codec: "pcm".into(), bits_per_sample: None }],
                    container: "Test".into(),
                    start_timecode: None,
                    file_size: Some(100),
                },
                v,
            })
        }
    }

    impl MediaSource for Dummy {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
            let f = self.info.frame_rate().frame_at(req.time) as u8;
            let mut px = vec![self.v; 4 * 2 * 4];
            px[3] = f;
            Ok(Arc::new(VideoFrame {
                width: 4,
                height: 2,
                data: PixelData::Rgba8(Arc::new(px)),
                color: self.info.video.as_ref().unwrap().color,
                par: (1, 1),
                pts: req.time,
            }))
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
            Ok(AudioBuffer { sample_rate, channels: vec![vec![self.v as f32; frames]] })
        }
    }

    fn first_px(d: &PixelData) -> (u8, u8) {
        match d {
            PixelData::Rgba8(v) => (v[0], v[3]),
            _ => (0, 0),
        }
    }

    #[test]
    fn joins_durations_and_maps_time_to_parts() {
        let s = SpannedSource::new("spanned", vec![Dummy::source(10, 1), Dummy::source(20, 2)]).unwrap();
        assert_eq!(s.info().duration, FrameRate::FPS_24.tick_of(30 * 24), "10s + 20s");
        assert_eq!(s.info().file_size, Some(200), "sizes sum");
        // t=0 is part a's frame 0; t=10s is part b's frame 0; t=10s+5f is b's frame 5
        assert_eq!(first_px(&s.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().data), (1, 0), "first frame from part a");
        assert_eq!(first_px(&s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(10 * 24))).unwrap().data), (2, 0), "boundary frame is part b's first");
        assert_eq!(first_px(&s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(10 * 24 + 5))).unwrap().data), (2, 5), "mid part b, local time");
        // past the end clamps to the last part
        assert_eq!(first_px(&s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(1000))).unwrap().data).0, 2, "clamped to the last part");
    }

    #[test]
    fn audio_window_straddling_a_boundary_reads_both_parts() {
        let s = SpannedSource::new("spanned", vec![Dummy::source(10, 1), Dummy::source(20, 2)]).unwrap();
        // a window centred on the 10s boundary: half from a (value 1), half from b (value 2)
        let half: usize = 4800; // 0.1s at 48k
        let buf = s.audio_stream(0, 10 * 48_000 - half as i64, half * 2, 48_000).unwrap();
        assert_eq!(buf.frames(), half * 2);
        assert!(buf.channels[0][..half].iter().all(|&v| v == 1.0), "first half from part a");
        assert!(buf.channels[0][half..].iter().all(|&v| v == 2.0), "second half from part b");
    }

    #[test]
    fn empty_join_is_an_error() {
        assert!(SpannedSource::new("x", Vec::new()).is_err());
    }
}

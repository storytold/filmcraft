//! Synthetic mixer fixtures shared by correctness and isolated throughput tests.
#![allow(dead_code)] // Each test executable uses a different subset of the shared rig.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource};
use filmcraft_project::mixer::LANE_VOLUME;
use filmcraft_project::{ItemId, ItemKind, Keyframe, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, Track, TrackId, TrackKind, TrackSend};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use super::{SourceMap, mixer};
use mixer::{LiveMix, mix_graph};

pub(crate) const SR: i64 = 48_000;

/// A synthetic stereo source: `f(sample, channel)`.
struct FnSource {
    info: MediaInfo,
    f: fn(i64, usize) -> f32,
    ch: usize,
}

impl MediaSource for FnSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Err(MediaError::Unsupported("audio only".into()))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        let f = self.f;
        Ok(AudioBuffer { sample_rate, channels: (0..self.ch).map(|c| (0..frames).map(|i| f(start + i as i64, c)).collect()).collect() })
    }
}

pub(crate) struct Rig {
    pub(crate) p: Project,
    pub(crate) seq: ItemId,
    pub(crate) map: SourceMap,
}

impl Rig {
    /// A sequence with `n` audio tracks, each holding one 10 s clip of its signal.
    pub(crate) fn new(signals: &[fn(i64, usize) -> f32]) -> Rig {
        Self::with_channels(signals, 2)
    }
    /// Like [`Rig::new`] with `ch`-channel sources.
    pub(crate) fn with_channels(signals: &[fn(i64, usize) -> f32], ch: usize) -> Rig {
        let mut p = Project::new("mix");
        let seq = p.new_sequence("s", SequenceSettings::default(), 0, signals.len(), None);
        let mut map = SourceMap::default();
        for (k, f) in signals.iter().enumerate() {
            let info = MediaInfo {
                name: format!("sig{k}"),
                kind: MediaKind::AudioOnly,
                duration: Tick(10 * TICKS_PER_SECOND),
                video: None,
                audio_streams: vec![AudioStreamInfo { sample_rate: 48_000, channels: ch as u32, codec: "test".into(), bits_per_sample: Some(32) }],
                container: "test".into(),
                start_timecode: None,
                file_size: None,
            };
            let id = p.add_item(
                &info.name.clone(),
                Label::Iris,
                ItemKind::Media(MediaClip {
                    media: MediaRef::File { path: format!("/sig{k}.wav") },
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
            map.0.insert(id, Arc::new(FnSource { info, f: *f, ch }));
            let ti = p.make_track_item(id, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(10 * TICKS_PER_SECOND)), FrameRate::FPS_24).unwrap();
            p.sequence_mut(seq).unwrap().audio_tracks[k].items.push(ti);
        }
        Rig { p, seq, map }
    }
    pub(crate) fn track(&mut self, k: usize) -> &mut Track {
        &mut self.p.sequence_mut(self.seq).unwrap().audio_tracks[k]
    }
    pub(crate) fn add_submix(&mut self, name: &str) -> TrackId {
        let id = TrackId(self.p.alloc_id());
        self.p.sequence_mut(self.seq).unwrap().submix_tracks.push(Track::new(id, TrackKind::Audio, name.into()));
        id
    }
    pub(crate) fn submix(&mut self, id: TrackId) -> &mut Track {
        self.p.sequence_mut(self.seq).unwrap().mix_track_mut(id).unwrap()
    }
    pub(crate) fn mix(&self, start: i64, n: usize) -> [Vec<f32>; 2] {
        let b = mix_graph(&self.p, self.p.sequence(self.seq).unwrap(), start, n, &self.map, None);
        [b.channels[0].clone(), b.channels[1].clone()]
    }
    /// Mix `[start, start + n)` in consecutive requests of the given sizes (cycled).
    pub(crate) fn mix_cut(&self, start: i64, n: usize, sizes: &[usize], live: Option<&LiveMix>) -> [Vec<f32>; 2] {
        let mut out = [Vec::new(), Vec::new()];
        let (mut pos, mut k) = (start, 0);
        while pos < start + n as i64 {
            let m = sizes[k % sizes.len()].min((start + n as i64 - pos) as usize);
            let b = mix_graph(&self.p, self.p.sequence(self.seq).unwrap(), pos, m, &self.map, live);
            out[0].extend_from_slice(&b.channels[0]);
            out[1].extend_from_slice(&b.channels[1]);
            pos += m as i64;
            k += 1;
        }
        out
    }
}

pub(crate) fn kf(t_samples: i64, v: f64) -> Keyframe {
    Keyframe::new(Tick::from_units(t_samples, SR), ParamValue::Float(v))
}

pub(crate) fn noise(i: i64, c: usize) -> f32 {
    let x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (c as u64 * 0x1234_5678);
    let x = x ^ (x >> 29);
    ((x.wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.4
}

pub(crate) fn busy_rig(tracks: usize) -> Rig {
    let sig: Vec<fn(i64, usize) -> f32> = vec![noise; tracks];
    let mut r = Rig::new(&sig);
    let bus = r.add_submix("Bus");
    let mut comp = filmcraft_project::find_effect("dynamics").unwrap().instance();
    comp.param_mut("threshold").unwrap().value = ParamValue::Float(-20.0);
    r.submix(bus).effects.push(comp);
    for k in 0..tracks {
        let t = r.track(k);
        for (id, post) in [("parametric_eq", false), ("dynamics", false), ("studio_reverb", true)] {
            let mut e = filmcraft_project::find_effect(id).unwrap().instance();
            e.post_fader = post;
            t.effects.push(e);
        }
        t.pan = (k as f64 * 37.0) % 200.0 - 100.0;
        t.lane_mut(LANE_VOLUME).unwrap().keyframes = vec![kf(0, -6.0), kf(96_000, 0.0)];
        if k % 3 == 0 {
            t.mixer.sends.push(TrackSend::new(bus));
        }
    }
    r
}

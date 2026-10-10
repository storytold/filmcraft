//! Mixer graph tests: gain and pan laws, automation, solo/mute, sends, latency, identity, speed.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource};
use filmcraft_project::mixer::{LANE_MUTE, LANE_PAN, LANE_VOLUME, send_lane};
use filmcraft_project::{
    AudioChannels, AutomationMode, ItemId, ItemKind, Keyframe, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, Track, TrackId, TrackKind,
    TrackSend,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use crate::SourceMap;
use crate::mixer::{LiveMix, balance, graph_latency, mix_graph, pan_law_mono};

const SR: i64 = 48_000;

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

struct Rig {
    p: Project,
    seq: ItemId,
    map: SourceMap,
}

impl Rig {
    /// A sequence with `n` audio tracks, each holding one 10 s clip of its signal.
    fn new(signals: &[fn(i64, usize) -> f32]) -> Rig {
        Self::with_channels(signals, 2)
    }
    /// Like [`Rig::new`] with `ch`-channel sources.
    fn with_channels(signals: &[fn(i64, usize) -> f32], ch: usize) -> Rig {
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
    fn track(&mut self, k: usize) -> &mut Track {
        &mut self.p.sequence_mut(self.seq).unwrap().audio_tracks[k]
    }
    fn add_submix(&mut self, name: &str) -> TrackId {
        let id = TrackId(self.p.alloc_id());
        self.p.sequence_mut(self.seq).unwrap().submix_tracks.push(Track::new(id, TrackKind::Audio, name.into()));
        id
    }
    fn submix(&mut self, id: TrackId) -> &mut Track {
        self.p.sequence_mut(self.seq).unwrap().mix_track_mut(id).unwrap()
    }
    fn mix(&self, start: i64, n: usize) -> [Vec<f32>; 2] {
        let b = mix_graph(&self.p, self.p.sequence(self.seq).unwrap(), start, n, &self.map, None);
        [b.channels[0].clone(), b.channels[1].clone()]
    }
    /// Mix `[start, start + n)` in consecutive requests of the given sizes (cycled).
    fn mix_cut(&self, start: i64, n: usize, sizes: &[usize], live: Option<&LiveMix>) -> [Vec<f32>; 2] {
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

fn dc(_: i64, _: usize) -> f32 {
    0.5
}

fn kf(t_samples: i64, v: f64) -> Keyframe {
    Keyframe::new(Tick::from_units(t_samples, SR), ParamValue::Float(v))
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

/// The renderer must request the stream selected by each clip, rather than reading stream zero
/// twice when a container item has clips on two audio tracks.
#[test]
fn audio_clips_from_one_container_mix_distinct_streams() {
    struct TwoStreams(MediaInfo);
    impl MediaSource for TwoStreams {
        fn info(&self) -> &MediaInfo {
            &self.0
        }
        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Err(MediaError::NoStream("video"))
        }
        fn audio(&self, start: i64, frames: usize, sr: u32) -> filmcraft_media::Result<AudioBuffer> {
            self.audio_stream(0, start, frames, sr)
        }
        fn audio_stream(&self, stream: usize, _start: i64, frames: usize, sr: u32) -> filmcraft_media::Result<AudioBuffer> {
            let v = match stream {
                0 => 0.25,
                1 => -0.125,
                _ => return Err(MediaError::NoStream("audio")),
            };
            Ok(AudioBuffer { sample_rate: sr, channels: vec![vec![v; frames], vec![v; frames]] })
        }
    }
    let mut p = Project::new("streams");
    let seq = p.new_sequence("mix", SequenceSettings::default(), 0, 2, None);
    let mut info = MediaInfo {
        name: "two.mov".into(),
        kind: MediaKind::AudioOnly,
        duration: Tick(TICKS_PER_SECOND),
        video: None,
        audio_streams: vec![AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: "PCM".into(), bits_per_sample: Some(16) }],
        container: "MOV".into(),
        start_timecode: None,
        file_size: None,
    };
    info.audio_streams.push(info.audio_streams[0].clone());
    let id = p.add_item(
        "two.mov",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: "two.mov".into() },
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
    let mut map = SourceMap::default();
    map.0.insert(id, Arc::new(TwoStreams(info)));
    for stream in 0..2 {
        let mut clip = p.make_track_item(id, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), FrameRate::FPS_24).unwrap();
        clip.audio_stream = stream;
        p.sequence_mut(seq).unwrap().audio_tracks[stream].items.push(clip);
    }
    let buffer = crate::audio::mix_sequence(&p, p.sequence(seq).unwrap(), 0, 32, &map);
    assert!(buffer.channels.iter().all(|c| c.iter().all(|&v| close(v, 0.125, 1e-6))));
}

#[test]
fn fader_gain_is_sample_exact() {
    let mut r = Rig::new(&[dc]);
    r.track(0).volume_db = -6.0;
    let [l, rr] = r.mix(1000, 256);
    let g = 0.5 * 10f32.powf(-6.0 / 20.0);
    assert!(l.iter().chain(&rr).all(|&s| close(s, g, 1e-6)), "{} vs {g}", l[0]);
    // −∞ silences; +15 dB is the top of the fader
    r.track(0).volume_db = -96.0;
    assert!(r.mix(0, 64)[0].iter().all(|&s| s == 0.0));
    r.track(0).volume_db = 15.0;
    assert!(close(r.mix(0, 64)[0][10], 0.5 * 10f32.powf(0.75), 1e-5));
    // Mix fader multiplies on top
    r.track(0).volume_db = -6.0;
    r.p.sequence_mut(r.seq).unwrap().master_volume_db = -6.0;
    assert!(close(r.mix(0, 64)[0][3], 0.5 * 10f32.powf(-12.0 / 20.0), 1e-6));
}

#[test]
fn pan_laws() {
    // mono track: −3 dB constant power (centre 0.7071 per side, power constant across the arc)
    for p in [-100.0, -50.0, 0.0, 25.0, 100.0] {
        let (l, r) = pan_law_mono((p / 100.0) as f32);
        assert!(close(l * l + r * r, 1.0, 1e-6), "power at {p}");
    }
    let (l, r) = pan_law_mono(0.0);
    assert!(close(20.0 * l.log10(), -3.0103, 1e-3) && close(l, r, 1e-7));
    let mut rig = Rig::new(&[dc]);
    rig.track(0).channels = AudioChannels::Mono;
    rig.track(0).pan = 50.0;
    let [l, r] = rig.mix(0, 128);
    let a = 1.5 * std::f32::consts::FRAC_PI_4;
    assert!(close(l[64], 0.5 * a.cos(), 1e-6) && close(r[64], 0.5 * a.sin(), 1e-6), "{} {}", l[64], r[64]);
    rig.track(0).pan = -100.0;
    let [l, r] = rig.mix(0, 128);
    assert!(close(l[1], 0.5, 1e-6) && r[1].abs() < 1e-6);
    // stereo track: balance (centre unity, far side attenuated)
    rig.track(0).channels = AudioChannels::Stereo;
    rig.track(0).pan = 0.0;
    let [l, r] = rig.mix(0, 64);
    assert!(close(l[5], 0.5, 1e-7) && close(r[5], 0.5, 1e-7));
    rig.track(0).pan = 50.0;
    let (bl, br) = balance(0.5);
    let [l, r] = rig.mix(0, 64);
    assert!(close(l[5], 0.5 * bl, 1e-6) && close(r[5], 0.5 * br, 1e-6) && br == 1.0 && bl < 0.6);
}

#[test]
fn volume_automation_is_sample_accurate_and_block_invariant() {
    let mut r = Rig::new(&[dc]);
    {
        let t = r.track(0);
        let lane = t.lane_mut(LANE_VOLUME).unwrap();
        lane.keyframes = vec![kf(0, 0.0), kf(1000, -20.0), kf(1500, -20.0), kf(1600, 0.0)];
    }
    let whole = r.mix(0, 2000);
    for k in [0usize, 1, 63, 64, 65, 127, 128, 500, 999, 1000, 1500, 1550, 1599, 1600] {
        let db = if k <= 1000 {
            -20.0 * k as f64 / 1000.0
        } else if k <= 1500 {
            -20.0
        } else if k <= 1600 {
            -20.0 + 20.0 * (k - 1500) as f64 / 100.0
        } else {
            0.0
        };
        let want = 0.5 * 10f64.powf(db / 20.0);
        assert!((whole[0][k] as f64 - want).abs() < 1e-6, "sample {k}: {} vs {want}", whole[0][k]);
    }
    // the same range cut into odd-sized requests is bit-identical
    for sizes in [&[1usize, 63, 64, 65][..], &[333], &[7, 1024, 5]] {
        let cut = r.mix_cut(0, 2000, sizes, None);
        assert_eq!(cut, whole, "cut {sizes:?}");
    }
    // Off mode ignores the automation
    r.track(0).mixer.mode = AutomationMode::Off;
    assert!(close(r.mix(1200, 16)[0][0], 0.5, 1e-7));
}

#[test]
fn effect_and_pan_automation_on_the_block_grid() {
    let mut r = Rig::new(&[dc]);
    {
        let t = r.track(0);
        let mut amp = filmcraft_project::find_effect("amplify").unwrap().instance();
        let g = amp.param_mut("gain").unwrap();
        g.keyframes = vec![kf(0, 0.0), kf(4800, 6.0)];
        t.effects.push(amp);
        t.lane_mut(LANE_PAN).unwrap().keyframes = vec![kf(0, -100.0), kf(4800, 100.0)];
    }
    let whole = r.mix(0, 4800);
    for sizes in [&[100usize, 17][..], &[64], &[4800]] {
        assert_eq!(r.mix_cut(0, 4800, sizes, None), whole, "{sizes:?}");
    }
    // hard left at the start, hard right at the end
    assert!(whole[1][0].abs() < 1e-6 && whole[0][4799].abs() < 1e-3);
}

#[test]
fn clip_volume_and_track_automation_both_apply() {
    let mut r = Rig::new(&[dc]);
    {
        let t = r.track(0);
        let it = &mut t.items[0];
        let lvl = it.effect_mut("volume").unwrap().param_mut("level").unwrap();
        lvl.keyframes = vec![Keyframe::new(Tick::ZERO, ParamValue::Float(-6.0)), Keyframe::new(Tick(TICKS_PER_SECOND), ParamValue::Float(-6.0))];
        t.lane_mut(LANE_VOLUME).unwrap().keyframes = vec![kf(0, -6.0), kf(48_000, -6.0)];
    }
    let [l, _] = r.mix(100, 64);
    assert!(close(l[0], 0.5 * 10f32.powf(-12.0 / 20.0), 1e-6), "{}", l[0]);
}

fn level(r: &Rig) -> f32 {
    r.mix(2000, 64)[0][10]
}

fn dc1(_: i64, _: usize) -> f32 {
    0.1
}
fn dc2(_: i64, _: usize) -> f32 {
    0.2
}
fn dc4(_: i64, _: usize) -> f32 {
    0.4
}

#[test]
fn solo_and_mute_logic() {
    let mut r = Rig::new(&[dc1, dc2, dc4]);
    let s1 = r.add_submix("Submix 1");
    r.track(1).mixer.output = Some(s1);
    assert!(close(level(&r), 0.7, 1e-6));
    r.track(0).solo = true;
    assert!(close(level(&r), 0.1, 1e-6), "solo A1");
    r.track(0).muted = true;
    assert!(close(level(&r), 0.0, 1e-6), "soloed but muted");
    r.track(0).muted = false;
    r.track(0).solo = false;
    // soloing a submix keeps its sources
    r.submix(s1).solo = true;
    assert!(close(level(&r), 0.2, 1e-6), "solo S1");
    r.submix(s1).solo = false;
    // soloing a track keeps its destination submix
    r.track(1).solo = true;
    assert!(close(level(&r), 0.2, 1e-6), "solo A2 through S1");
    // solo safe: a solo-safe submix and its sources stay audible while others solo
    let s2 = r.add_submix("Dialogue");
    r.track(2).mixer.output = Some(s2);
    r.submix(s2).mixer.solo_safe = true;
    assert!(close(level(&r), 0.6, 1e-6), "solo A2 + solo-safe S2");
    r.submix(s2).mixer.solo_safe = false;
    assert!(close(level(&r), 0.2, 1e-6));
    r.track(1).solo = false;
    // mute automation (hold)
    r.track(2).lane_mut(LANE_MUTE).unwrap().keyframes = vec![kf(0, 0.0), kf(2005, 1.0)];
    let x = r.mix(2000, 10)[0].clone();
    assert!(close(x[4], 0.7, 1e-6) && close(x[5], 0.3, 1e-6), "{x:?}");
}

#[test]
fn sends_route_to_submixes() {
    let mut r = Rig::new(&[dc]);
    let fx = r.add_submix("Reverb");
    {
        let t = r.track(0);
        let mut s = TrackSend::new(fx);
        s.level_db = -6.0;
        t.mixer.sends.push(s);
    }
    let g6 = 10f32.powf(-6.0 / 20.0);
    assert!(close(level(&r), 0.5 + 0.5 * g6, 1e-6), "post-fader send adds to the Mix");
    // post-fader send follows the fader
    r.track(0).volume_db = -6.0;
    assert!(close(level(&r), 0.5 * g6 + 0.5 * g6 * g6, 1e-6));
    // pre-fader send ignores fader and mute
    r.track(0).mixer.sends[0].pre_fader = true;
    r.track(0).muted = true;
    assert!(close(level(&r), 0.5 * g6, 1e-6), "pre-fader send of a muted track");
    r.track(0).muted = false;
    r.track(0).volume_db = -96.0;
    assert!(close(level(&r), 0.5 * g6, 1e-6));
    // send level automation and submix fader
    r.track(0).lane_mut(&send_lane(0)).unwrap().keyframes = vec![kf(0, 0.0), kf(4000, 0.0)];
    r.submix(fx).volume_db = -6.0;
    assert!(close(level(&r), 0.5 * g6, 1e-6));
    // the track output can go to a submix; submixes chain forward only
    r.track(0).volume_db = 0.0;
    r.track(0).mixer.sends.clear();
    r.track(0).mixer.output = Some(fx);
    let fx2 = r.add_submix("Bus 2");
    r.submix(fx).mixer.output = Some(fx2);
    r.submix(fx2).volume_db = -6.0;
    assert!(close(level(&r), 0.5 * g6 * g6, 1e-6));
    // a backwards route (feedback) falls back to the Mix
    r.submix(fx2).mixer.output = Some(fx);
    assert!(close(level(&r), 0.5 * g6 * g6, 1e-6));
}

fn impulse(i: i64, _: usize) -> f32 {
    if i == 10_000 { 0.25 } else { 0.0 }
}

#[test]
fn latency_compensation_aligns_paths() {
    // A1 goes through a look-ahead limiter (latency), A2 is dry; both carry the same impulse.
    let mut r = Rig::new(&[impulse, impulse]);
    let mut lim = filmcraft_project::find_effect("hard_limiter").unwrap().instance();
    lim.param_mut("max").unwrap().value = ParamValue::Float(0.0);
    r.track(0).effects.push(lim.clone());
    let lat = graph_latency(r.p.sequence(r.seq).unwrap());
    assert!(lat > 0, "limiter reports look-ahead");
    let [l, _] = r.mix(9_000, 2_000);
    let peak = l.iter().enumerate().fold((0usize, 0f32), |m, (i, &s)| if s.abs() > m.1 { (i, s.abs()) } else { m });
    assert_eq!(peak.0 + 9_000, 10_000, "aligned impulse lands on its timeline sample");
    assert!(close(peak.1, 0.5, 1e-3), "both paths sum coherently: {}", peak.1);
    // through a submix with its own latency, a send and a dry path stay aligned
    let bus = r.add_submix("Bus");
    r.submix(bus).effects.push(lim);
    r.track(1).mixer.sends.push(TrackSend::new(bus));
    let [l, _] = r.mix(9_000, 2_000);
    let peak = l.iter().enumerate().fold((0usize, 0f32), |m, (i, &s)| if s.abs() > m.1 { (i, s.abs()) } else { m });
    assert_eq!(peak.0 + 9_000, 10_000);
    assert!(close(peak.1, 0.75, 2e-3), "{}", peak.1);
    // automation is evaluated at content time: a volume step on A1 at the impulse applies to it
    r.track(0).lane_mut(LANE_VOLUME).unwrap().keyframes = vec![kf(0, 0.0), kf(9_999, 0.0), kf(10_000, -96.0)];
    r.track(0).lane_mut(LANE_VOLUME).unwrap().keyframes[1].interp = filmcraft_project::Interpolation::Hold;
    let [l, _] = r.mix(9_000, 2_000);
    assert!(close(l[1000], 0.5, 2e-3), "A1 silenced exactly at its content sample: {}", l[1000]);
}

fn noise(i: i64, c: usize) -> f32 {
    let x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (c as u64 * 0x1234_5678);
    let x = x ^ (x >> 29);
    ((x.wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.4
}

fn busy_rig(tracks: usize) -> Rig {
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

#[test]
fn render_and_playback_paths_are_identical() {
    // export renders in large batches; playback pulls small device blocks with live meters
    let r = busy_rig(4);
    let export = r.mix_cut(0, 24_000, &[8_000], None);
    let live = LiveMix::new();
    let playback = r.mix_cut(0, 24_000, &[512, 480, 1024], Some(&live));
    assert_eq!(export, playback);
    assert!(export[0].iter().any(|s| s.abs() > 1e-3));
    let m = live.take_meters();
    assert!(m.get(&crate::mixer::MASTER).is_some_and(|p| p[0] > 0.0), "meters published");
    // deterministic: a second render from scratch is identical
    let again = Rig::new(&[]);
    drop(again);
    assert_eq!(busy_rig(4).mix_cut(0, 24_000, &[8_000], None), export);
}

#[test]
fn live_override_holds_and_ramps_back() {
    let mut r = Rig::new(&[dc]);
    r.track(0).lane_mut(LANE_VOLUME).unwrap().keyframes = vec![kf(0, 0.0), kf(96_000, 0.0)];
    let id = r.track(0).id;
    let live = LiveMix::new();
    live.hold(id, LANE_VOLUME, -6.0);
    let b = mix_graph(&r.p, r.p.sequence(r.seq).unwrap(), 0, 64, &r.map, Some(&live));
    assert!(close(b.channels[0][0], 0.5 * 10f32.powf(-0.3), 1e-6));
    // released at sample 4800 with a 4800-sample ramp: halfway back at 7200
    live.release(id, LANE_VOLUME, Tick::from_units(4800, SR), Tick::from_units(4800, SR));
    let b = mix_graph(&r.p, r.p.sequence(r.seq).unwrap(), 7200, 1, &r.map, Some(&live));
    assert!(close(b.channels[0][0], 0.5 * 10f32.powf(-3.0 / 20.0), 1e-6), "{}", b.channels[0][0]);
    let b = mix_graph(&r.p, r.p.sequence(r.seq).unwrap(), 9600, 1, &r.map, Some(&live));
    assert!(close(b.channels[0][0], 0.5, 1e-6));
}

/// 24 tracks × 3 inserts (EQ, Dynamics, Studio Reverb) + a compressed submix at 48 kHz, one core.
#[test]
// Off Unix there is no per-thread CPU clock (`thread_cpu_secs`), so the timing falls back to wall
// time, which a loaded CI host (Windows runners run the suite in parallel) can push under the
// threshold: genuinely timing-flaky there. On Unix it measures this thread's CPU time and stays on.
#[cfg_attr(not(unix), ignore = "wall-clock timing without a thread CPU clock is load-dependent; run with --ignored")]
fn perf_24_tracks_3_effects_realtime_factor() {
    let r = busy_rig(24);
    let secs = if cfg!(debug_assertions) { 2.0 } else { 10.0 };
    let n = (secs * SR as f64) as usize;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    // warm up (fresh graph + pre-roll), then time steady-state playback-sized blocks
    pool.install(|| r.mix_cut(0, 4_800, &[1024], None));
    // Thread CPU time, not wall clock: the test runs alongside the whole suite (and other builds),
    // and only the work done on this one thread matters.
    let el = pool.install(|| {
        let t0 = thread_cpu_secs();
        let w0 = std::time::Instant::now();
        r.mix_cut(4_800, n, &[1024], None);
        t0.map(|t0| thread_cpu_secs().unwrap_or(t0) - t0).unwrap_or_else(|| w0.elapsed().as_secs_f64())
    });
    let rt = secs / el;
    eprintln!("mixer perf: 24 tracks x 3 inserts + submix, {secs} s of 48 kHz stereo in {el:.3} s of CPU on one core = {rt:.1}x realtime");
    let min = if cfg!(debug_assertions) { 1.0 } else { 4.0 };
    assert!(rt > min, "realtime factor {rt:.2}");
}

/// CPU seconds used by the calling thread (None where there is no thread clock; callers fall back
/// to wall time).
fn thread_cpu_secs() -> Option<f64> {
    #[cfg(unix)]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
        Some(t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[test]
fn audio_transition_curves() {
    use crate::transitions::audio_gains;
    for i in 0..=20 {
        let p = i as f32 / 20.0;
        let (a, b) = audio_gains("constant_power", p);
        assert!(close(a * a + b * b, 1.0, 1e-5), "constant power keeps power at {p}");
        let (a, b) = audio_gains("constant_gain", p);
        assert!(close(a + b, 1.0, 1e-6), "constant gain keeps amplitude at {p}");
        let (a, b) = audio_gains("exponential_fade", p);
        assert!((0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b));
    }
    for k in ["constant_power", "constant_gain", "exponential_fade"] {
        assert_eq!(audio_gains(k, 0.0), (1.0, 0.0), "{k} starts on A");
        let (a, b) = audio_gains(k, 1.0);
        assert!(a.abs() < 1e-6 && close(b, 1.0, 1e-6), "{k} ends on B");
    }
}

// ------------------------------------------------------------------------------------- 5.1

/// Distinct DC level per source channel: 0.1, 0.2, … (L, R, C, LFE, Ls, Rs for 6-channel sources).
fn chan_dc(_: i64, c: usize) -> f32 {
    (c + 1) as f32 * 0.1
}

fn set_master_51(r: &mut Rig) {
    r.p.sequence_mut(r.seq).unwrap().settings.audio_master = AudioChannels::Surround51;
}

fn mix_all(r: &Rig, start: i64, n: usize) -> Vec<Vec<f32>> {
    mix_graph(&r.p, r.p.sequence(r.seq).unwrap(), start, n, &r.map, None).channels
}

#[test]
fn stereo_and_mono_tracks_into_a_51_mix_use_the_51_panner() {
    use filmcraft_project::mixer::{LANE_PAN51_CENTER, LANE_PAN51_LFE, LANE_PAN51_X, LANE_PAN51_Y};
    let mut r = Rig::new(&[chan_dc]);
    set_master_51(&mut r);
    // stereo track at the default puck (front centre): L → L, R → R, nothing else
    let m = mix_all(&r, 0, 256);
    assert_eq!(m.len(), 6);
    let at = |m: &Vec<Vec<f32>>, c: usize| m[c][100];
    assert!(close(at(&m, 0), 0.1, 1e-6) && close(at(&m, 1), 0.2, 1e-6), "{:?}", m.iter().map(|c| c[100]).collect::<Vec<_>>());
    for c in 2..6 {
        assert!(at(&m, c).abs() < 1e-6, "channel {c} = {}", at(&m, c));
    }
    // LFE knob: mean of the source channels
    r.track(0).set_lane_static(LANE_PAN51_LFE, 0.0);
    let m = mix_all(&r, 0, 256);
    assert!(close(at(&m, 3), 0.15, 1e-6));
    r.track(0).set_lane_static(LANE_PAN51_LFE, -96.0);
    // mono track: folded, then a point source; front centre at 100 % centre = C only
    r.track(0).channels = AudioChannels::Mono;
    let m = mix_all(&r, 0, 256);
    assert!(close(at(&m, 2), 0.15, 1e-6) && at(&m, 0).abs() < 1e-6 && at(&m, 4).abs() < 1e-6);
    // hard rear right
    r.track(0).set_lane_static(LANE_PAN51_X, 100.0);
    r.track(0).set_lane_static(LANE_PAN51_Y, -100.0);
    let m = mix_all(&r, 0, 256);
    assert!(close(at(&m, 5), 0.15, 1e-6));
    // room centre, 0 % centre: equal power over L, R, Ls, Rs (−6 dB each)
    r.track(0).set_lane_static(LANE_PAN51_X, 0.0);
    r.track(0).set_lane_static(LANE_PAN51_Y, 0.0);
    r.track(0).set_lane_static(LANE_PAN51_CENTER, 0.0);
    let m = mix_all(&r, 0, 256);
    for c in [0, 1, 4, 5] {
        assert!(close(at(&m, c), 0.075, 1e-6), "channel {c}");
    }
    let power: f32 = (0..6).map(|c| at(&m, c) * at(&m, c)).sum();
    assert!(close(power, 0.15 * 0.15, 1e-6), "equal power: {power}");
}

#[test]
fn a_51_track_passes_through_a_51_mix_and_folds_into_stereo() {
    let mut r = Rig::with_channels(&[chan_dc], 6);
    r.track(0).channels = AudioChannels::Surround51;
    set_master_51(&mut r);
    let m = mix_all(&r, 0, 256);
    for (c, ch) in m.iter().enumerate() {
        assert!(close(ch[17], (c + 1) as f32 * 0.1, 1e-6), "channel {c} = {}", ch[17]);
    }
    // the same track into a stereo Mix: ITU-R BS.775 downmix (LFE omitted)
    r.p.sequence_mut(r.seq).unwrap().settings.audio_master = AudioChannels::Stereo;
    let k = std::f32::consts::FRAC_1_SQRT_2;
    let st = mix_all(&r, 0, 256);
    assert_eq!(st.len(), 2);
    assert!(close(st[0][9], 0.1 + k * 0.3 + k * 0.5, 1e-6) && close(st[1][9], 0.2 + k * 0.3 + k * 0.6, 1e-6));
    // mix_sequence of a 5.1 Mix is the BS.775 stereo fold of the 6-channel mix
    set_master_51(&mut r);
    let folded = crate::audio::mix_sequence(&r.p, r.p.sequence(r.seq).unwrap(), 0, 256, &r.map);
    assert_eq!(folded.channels.len(), 2);
    assert!(close(folded.channels[0][9], st[0][9], 1e-6) && close(folded.channels[1][9], st[1][9], 1e-6));
    // Front Only mixdown for playback devices drops the surrounds
    let front = crate::audio::mix_sequence_layout(
        &r.p,
        r.p.sequence(r.seq).unwrap(),
        0,
        256,
        &r.map,
        filmcraft_audio_dsp::channels::Layout::Stereo,
        filmcraft_audio_dsp::channels::Mixdown::Front,
    );
    assert!(close(front.channels[0][9], 0.1 + k * 0.3, 1e-6));
    // meters: one per channel of the strip
    let live = LiveMix::new();
    mix_graph(&r.p, r.p.sequence(r.seq).unwrap(), 0, 256, &r.map, Some(&live));
    let meters = live.take_meters();
    assert_eq!(meters.get(&crate::mixer::MASTER).map(Vec::len), Some(6));
    assert!(close(meters[&crate::mixer::MASTER][5], 0.6, 1e-6));
}

#[test]
fn stereo_track_through_a_51_submix_into_a_stereo_mix() {
    let mut r = Rig::new(&[chan_dc]);
    let sub = r.add_submix("5.1 bus");
    r.submix(sub).channels = AudioChannels::Surround51;
    r.track(0).mixer.output = Some(sub);
    // puck hard left at the front: source L on L, source R at front centre (C); folded into stereo
    // L = L + k·C, R = k·C
    r.track(0).set_lane_static(filmcraft_project::mixer::LANE_PAN51_X, -100.0);
    let st = mix_all(&r, 0, 128);
    let k = std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(st[0][50], 0.1 + k * 0.2, 1e-6) && close(st[1][50], k * 0.2, 1e-6), "{} {}", st[0][50], st[1][50]);
}

#[test]
fn pan51_automation_is_block_invariant() {
    use filmcraft_project::mixer::LANE_PAN51_X;
    let mut r = Rig::new(&[noise]);
    set_master_51(&mut r);
    let p = r.track(0).lane_mut(LANE_PAN51_X).unwrap();
    p.keyframes.push(kf(0, -100.0));
    p.keyframes.push(kf(12_000, 100.0));
    let whole = mix_all(&r, 0, 12_000);
    let mut cut: Vec<Vec<f32>> = vec![Vec::new(); 6];
    let mut pos = 0i64;
    for m in [97usize, 1000, 4000, 6903] {
        let b = mix_all(&r, pos, m);
        for (c, ch) in b.into_iter().enumerate() {
            cut[c].extend(ch);
        }
        pos += m as i64;
    }
    assert_eq!(whole, cut);
    // the puck moved from left to right: L dominates early, R late
    let energy = |c: usize, a: usize, b: usize| whole[c][a..b].iter().map(|x| x * x).sum::<f32>();
    assert!(energy(0, 0, 2000) > 5.0 * energy(1, 0, 2000));
    assert!(energy(1, 10_000, 12_000) > 5.0 * energy(0, 10_000, 12_000));
}

/// A damaged project can hold a sequence that contains itself (the editor refuses to make one).
/// Mixing it must end: this used to recurse until the stack overflowed.
#[test]
fn a_sequence_nested_in_itself_mixes_to_an_end() {
    let mut p = Project::new("cycle");
    let a = p.new_sequence("a", SequenceSettings::default(), 1, 1, None);
    let b = p.new_sequence("b", SequenceSettings::default(), 1, 1, None);
    let rate = FrameRate::default();
    let second = TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND));
    // a holds itself and b; b holds a
    for (outer, inner) in [(a, a), (a, b), (b, a)] {
        let it = p.make_track_item(inner, TrackKind::Audio, Tick::ZERO, second, rate).unwrap();
        p.sequence_mut(outer).unwrap().audio_tracks[0].items.push(it);
    }
    let seq = p.sequence(a).unwrap();
    let out = mix_graph(&p, seq, 0, 4800, &SourceMap::default(), None);
    assert_eq!(out.channels.len(), 2);
    assert!(out.channels.iter().all(|c| c.len() == 4800 && c.iter().all(|s| *s == 0.0)), "no source: silence");
}

// ---- the sound of nested sequences

/// A mono-in-stereo sine that knows its sample rate: the same pitch at whatever rate it is read.
struct Tone {
    info: MediaInfo,
    hz: f64,
}

impl Tone {
    fn at(&self, seconds: f64) -> f32 {
        (std::f64::consts::TAU * self.hz * seconds).sin() as f32 * 0.5
    }
}

impl MediaSource for Tone {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Err(MediaError::Unsupported("audio only".into()))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        let ch: Vec<f32> = (0..frames).map(|i| self.at((start + i as i64) as f64 / sample_rate as f64)).collect();
        Ok(AudioBuffer { sample_rate, channels: vec![ch.clone(), ch] })
    }
}

/// An outer 48 kHz sequence whose A1 holds one clip of an inner sequence (at `inner_rate`) that
/// holds 10 s of a 500 Hz tone. Returns the project, the outer sequence, the sources and the tone.
fn nested_tone(inner_rate: u32) -> (Project, ItemId, SourceMap, Arc<Tone>) {
    let mut p = Project::new("nest");
    let ten = TimeRange::new(Tick::ZERO, Tick(10 * TICKS_PER_SECOND));
    let info = MediaInfo {
        name: "tone".into(),
        kind: MediaKind::AudioOnly,
        duration: ten.duration,
        video: None,
        audio_streams: vec![AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: "test".into(), bits_per_sample: Some(32) }],
        container: "test".into(),
        start_timecode: None,
        file_size: None,
    };
    let media = p.add_item(
        "tone",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: "/tone.wav".into() },
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
    let tone = Arc::new(Tone { info, hz: 500.0 });
    let mut map = SourceMap::default();
    map.0.insert(media, tone.clone());
    let inner = p.new_sequence("inner", SequenceSettings { sample_rate: inner_rate, ..SequenceSettings::default() }, 1, 1, None);
    let clip = p.make_track_item(media, TrackKind::Audio, Tick::ZERO, ten, FrameRate::FPS_24).unwrap();
    p.sequence_mut(inner).unwrap().audio_tracks[0].items.push(clip);
    let outer = p.new_sequence("outer", SequenceSettings { sample_rate: 48_000, ..SequenceSettings::default() }, 1, 1, None);
    let nest = p.make_track_item(inner, TrackKind::Audio, Tick::ZERO, ten, FrameRate::FPS_24).unwrap();
    p.sequence_mut(outer).unwrap().audio_tracks[0].items.push(nest);
    (p, outer, map, tone)
}

/// The largest difference between the mix's left channel and `want(seconds)`, over samples
/// `skip..` (resampling and fades settle within the first few samples).
fn worst(mix: &AudioBuffer, start: i64, skip: usize, want: impl Fn(f64) -> f32) -> f32 {
    mix.channels[0].iter().enumerate().skip(skip).map(|(i, s)| (s - want((start + i as i64) as f64 / 48_000.0)).abs()).fold(0.0, f32::max)
}

#[test]
fn a_nest_plays_its_sequence_as_it_sounds_there() {
    let (p, outer, map, tone) = nested_tone(48_000);
    let start = 48_000 * 2;
    let mix = mix_graph(&p, p.sequence(outer).unwrap(), start, 4800, &map, None);
    assert!(worst(&mix, start, 0, |t| tone.at(t)) < 1e-4);
}

#[test]
fn a_nest_at_another_sample_rate_keeps_its_pitch_and_timing() {
    // the nested sequence used to be read sample for sample: a 44.1 kHz nest in a 48 kHz
    // sequence played 8.8% fast and sharp
    for rate in [44_100, 96_000, 32_000] {
        let (p, outer, map, tone) = nested_tone(rate);
        let start = 48_000 * 3 + 17;
        let mix = mix_graph(&p, p.sequence(outer).unwrap(), start, 4800, &map, None);
        assert_eq!(mix.channels[0].len(), 4800);
        assert!(worst(&mix, start, 8, |t| tone.at(t)) < 0.01, "{rate} Hz nest: off by {}", worst(&mix, start, 8, |t| tone.at(t)));
    }
}

#[test]
fn speed_and_reverse_on_a_nest_change_its_sound_like_a_clips() {
    // speed and reverse used to be ignored for the sound of a nest
    let (mut p, outer, map, tone) = nested_tone(48_000);
    let nest = &mut p.sequence_mut(outer).unwrap().audio_tracks[0].items[0];
    nest.speed = 2.0;
    nest.duration = Tick(5 * TICKS_PER_SECOND);
    let start = 48_000 + 5;
    let mix = mix_graph(&p, p.sequence(outer).unwrap(), start, 4800, &map, None);
    assert!(worst(&mix, start, 8, |t| tone.at(2.0 * t)) < 0.01, "double speed: twice as far into the sequence, an octave up");
    // reversed at normal speed: the 10 s nest plays from its end
    let nest = &mut p.sequence_mut(outer).unwrap().audio_tracks[0].items[0];
    nest.speed = 1.0;
    nest.duration = Tick(10 * TICKS_PER_SECOND);
    nest.reverse = true;
    let mix = mix_graph(&p, p.sequence(outer).unwrap(), start, 4800, &map, None);
    // (a reversed clip reads its source a sample or two off, media and nests alike)
    let w = (0..4).map(|k| worst(&mix, start, 8, |t| tone.at(10.0 - t - k as f64 / 48_000.0))).fold(f32::MAX, f32::min);
    assert!(w < 0.01, "reversed: off by {w}");
}

#[test]
fn a_nest_trimmed_in_starts_later_in_its_sequence() {
    let (mut p, outer, map, tone) = nested_tone(44_100);
    let nest = &mut p.sequence_mut(outer).unwrap().audio_tracks[0].items[0];
    nest.source_in = Tick(3 * TICKS_PER_SECOND);
    nest.duration = Tick(7 * TICKS_PER_SECOND);
    let start = 48_000;
    let mix = mix_graph(&p, p.sequence(outer).unwrap(), start, 4800, &map, None);
    assert!(worst(&mix, start, 8, |t| tone.at(t + 3.0)) < 0.01);
}

//! 5.1 export: WAV (WAVE_FORMAT_EXTENSIBLE, 6 channels), ProRes MOV with 6 PCM channels (`chan`
//! atom) and H.264 MP4 with AAC 5.1 (channel configuration 6). ffprobe checks the channel layout;
//! ffmpeg decodes the PCM files sample-exactly and the AAC file per channel (each channel carries
//! its own tone). ffmpeg / ffprobe are external test oracles only.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource};
use filmcraft_project::{AudioChannels, ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::TICKS_PER_SECOND;

use super::*;

const SR: u32 = 48_000;
/// Tone frequency of each 5.1 channel (L, R, C, LFE, Ls, Rs).
const HZ: [f64; 6] = [500.0, 700.0, 900.0, 60.0, 1300.0, 1700.0];

/// A 6-channel source: channel c is a 0.25-amplitude sine at `HZ[c]`.
struct Tones6 {
    info: MediaInfo,
}

fn tone(c: usize, i: i64) -> f32 {
    (0.25 * (2.0 * std::f64::consts::PI * HZ[c] * i as f64 / SR as f64).sin()) as f32
}

impl MediaSource for Tones6 {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Err(MediaError::Unsupported("audio only".into()))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer { sample_rate, channels: (0..6).map(|c| (0..frames).map(|i| tone(c, start + i as i64)).collect()).collect() })
    }
}

/// A 320×180 sequence with a 5.1 Mix and one 5.1 audio track holding 1 s of the six tones.
fn surround() -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("51");
    let info = MediaInfo {
        name: "tones51".into(),
        kind: MediaKind::AudioOnly,
        duration: Tick(2 * TICKS_PER_SECOND),
        video: None,
        audio_streams: vec![AudioStreamInfo { sample_rate: SR, channels: 6, codec: "test".into(), bits_per_sample: Some(32) }],
        container: "test".into(),
        start_timecode: None,
        file_size: None,
    };
    let id = p.add_item(
        "tones51",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: "/tones51.wav".into() },
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
    let r = FrameRate::FPS_24;
    let mut st = SequenceSettings { width: 320, height: 180, frame_rate: r, ..Default::default() };
    st.audio_master = AudioChannels::Surround51;
    let seq = p.new_sequence("s", st, 1, 1, None);
    let ai = p.make_track_item(id, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let q = p.sequence_mut(seq).unwrap();
    q.audio_tracks[0].channels = AudioChannels::Surround51;
    q.audio_tracks[0].items.push(ai);
    let mut m = SourceMap::default();
    m.0.insert(id, Arc::new(Tones6 { info }) as Arc<dyn MediaSource>);
    (Arc::new(p), seq, m)
}

fn out_path(name: &str) -> String {
    let d = filmcraft_testkit::fixtures::workspace_root().join("target").join("export-tests").join(format!("surround-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

fn settings(format: Format, path: &str) -> ExportSettings {
    let mut s = ExportSettings { format, path: path.to_string(), ..Default::default() };
    s.audio.channels = 6;
    s.audio.bits = 24;
    s.audio.sample_rate = Some(SR);
    s
}

/// ffprobe's (channels, channel_layout) of the first audio stream.
fn probe(path: &str) -> Option<(u64, String)> {
    let ffprobe = filmcraft_testkit::ffprobe_or_skip("5.1 export")?;
    let out = std::process::Command::new(ffprobe).args(["-v", "error", "-of", "json", "-select_streams", "a:0", "-show_streams", path]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let s = &j["streams"][0];
    Some((s["channels"].as_u64().unwrap_or(0), s["channel_layout"].as_str().unwrap_or("").to_string()))
}

/// ffmpeg's decode of the first audio stream as planar f64 (from s32le: exact for 24-bit PCM).
fn decode(path: &str) -> Option<Vec<Vec<f64>>> {
    let ffmpeg = filmcraft_testkit::ffmpeg_or_skip("5.1 export")?;
    let out =
        std::process::Command::new(ffmpeg).args(["-v", "error", "-i", path, "-map", "0:a:0", "-f", "s32le", "-acodec", "pcm_s32le", "-"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ints: Vec<i32> = out.stdout.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let n = ints.len() / 6;
    Some((0..6).map(|c| (0..n).map(|i| ints[i * 6 + c] as f64).collect()).collect())
}

/// Every decoded sample equals our 24-bit quantisation of the tone (sample-exact).
fn assert_sample_exact(ch: &[Vec<f64>], frames: usize) {
    assert_eq!(ch.len(), 6);
    assert_eq!(ch[0].len(), frames, "frame count");
    for (c, v) in ch.iter().enumerate() {
        for (i, x) in v.iter().enumerate() {
            let want = ((tone(c, i as i64).clamp(-1.0, 1.0) * 8_388_607.0).round() as i64) << 8;
            assert_eq!(*x as i64, want, "channel {c} sample {i}");
        }
    }
}

fn is_51(layout: &str) -> bool {
    layout.starts_with("5.1")
}

#[test]
fn wav_51_is_extensible_and_sample_exact() {
    let (p, seq, m) = surround();
    let path = out_path("s51.wav");
    export(&p, seq, &settings(Format::Wav, &path), &m, &Progress::default()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 0xFFFE, "WAVE_FORMAT_EXTENSIBLE");
    assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 6);
    assert_eq!(u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]), 0x3F, "L R C LFE Ls Rs");
    if let Some((n, layout)) = probe(&path) {
        assert_eq!(n, 6);
        assert!(is_51(&layout), "{layout}");
    }
    if let Some(ch) = decode(&path) {
        assert_sample_exact(&ch, SR as usize);
    }
}

#[test]
fn prores_mov_carries_six_pcm_channels() {
    let (p, seq, m) = surround();
    let path = out_path("s51.mov");
    export(&p, seq, &settings(Format::ProRes, &path), &m, &Progress::default()).unwrap();
    if let Some((n, layout)) = probe(&path) {
        assert_eq!(n, 6);
        assert!(is_51(&layout), "{layout}");
    }
    if let Some(ch) = decode(&path) {
        assert_sample_exact(&ch, SR as usize);
    }
}

/// Energy of `x` at `hz` (Goertzel), normalised by length.
fn tone_power(x: &[f64], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / SR as f64;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for v in x {
        let s0 = v + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2) / (x.len() as f64).powi(2)
}

#[test]
fn aac_51_in_mp4_keeps_every_channel_in_place() {
    let (p, seq, m) = surround();
    let path = out_path("s51.mp4");
    let mut st = settings(Format::H264, &path);
    st.audio.bitrate_kbps = 384;
    export(&p, seq, &st, &m, &Progress::default()).unwrap();
    // our own demuxer sees an AAC track with channel configuration 6
    let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let src = filmcraft_codecs::open_bytes("s51.mp4", bytes).unwrap();
    assert_eq!(src.info().audio().unwrap().channels, 6);
    if let Some((n, layout)) = probe(&path) {
        assert_eq!(n, 6);
        assert!(is_51(&layout), "{layout}");
    }
    // ffmpeg decodes to L R C LFE Ls Rs: each channel's own tone dominates (≥ 30 dB above the
    // others' tones), so no channel was swapped by the AAC reordering
    if let Some(ch) = decode(&path) {
        for (c, v) in ch.iter().enumerate() {
            let mid = &v[v.len() / 4..v.len() * 3 / 4];
            let own = tone_power(mid, HZ[c]);
            for (k, hz) in HZ.iter().enumerate().filter(|(k, _)| *k != c) {
                let other = tone_power(mid, *hz);
                assert!(own > other * 1000.0, "channel {c}: own {own:e} vs tone {k} {other:e}");
            }
        }
    }
}

#[test]
fn stereo_export_of_a_51_mix_is_the_bs775_downmix() {
    let (p, seq, m) = surround();
    let path = out_path("fold.wav");
    let mut st = settings(Format::Wav, &path);
    st.audio.channels = 2;
    export(&p, seq, &st, &m, &Progress::default()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 1, "plain PCM for stereo");
    let k = std::f32::consts::FRAC_1_SQRT_2;
    let at = |frame: usize, c: usize| {
        let o = 44 + (frame * 2 + c) * 3;
        (i32::from_le_bytes([0, bytes[o], bytes[o + 1], bytes[o + 2]]) >> 8) as f32 / 8_388_607.0
    };
    for i in [10usize, 1234, 30_000] {
        let l = tone(0, i as i64) + k * tone(2, i as i64) + k * tone(4, i as i64);
        let r = tone(1, i as i64) + k * tone(2, i as i64) + k * tone(5, i as i64);
        assert!((at(i, 0) - l).abs() < 2e-6 && (at(i, 1) - r).abs() < 2e-6, "frame {i}");
    }
}

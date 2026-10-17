//! Opt-in generated AV1 metadata-mismatch acceptance through the production Windows factory.
//! Requires SDR/PQ/HLG fixtures and real DXVA output; an unavailable decoder fails, never skips.
#![cfg(target_os = "windows")]

mod common;

use common::{Stream, assert_same, decode_all, read_stream};
use filmcraft_codecs::hw::{Av1ParameterDifference, FrameStreamInfo, hw_stats};
use filmcraft_codecs::{DecodedFrame, VideoDecoder};
use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
use filmcraft_isobmff::{Brand, CodecConfig, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
use filmcraft_media::{FrameRequest, MediaSource};
use std::io::Cursor;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Baseline stand-in proves the old strict preflight rejects before native decode. It is
/// explicitly not physical hardware evidence; positive checks use the production factory.
struct StrictStandIn(Arc<AtomicUsize>);
impl VideoDecoder for StrictStandIn {
    fn decode(&mut self, _: &[u8], _: i64) -> filmcraft_codecs::Result<Vec<DecodedFrame>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(filmcraft_codecs::CodecError::Decode("strict baseline unexpectedly called native decode".into()))
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "strict baseline stand-in (not hardware)"
    }
}

fn fixture_stream(root: &std::path::Path, signal: &str) -> (Stream, Arc<[u8]>) {
    let actual_path = root.join(format!("{signal}-actual.mp4"));
    let donor = read_stream(&root.join(format!("{signal}-donor.mp4")));
    let mut actual = read_stream(&actual_path);
    let CodecConfig::Av1(donor_cfg) = &donor.entry.codec else { panic!("donor is not AV1") };
    let CodecConfig::Av1(actual_cfg) = &mut actual.entry.codec else { panic!("actual is not AV1") };
    // Preserve fixed av1C declarations, actual nclx, and every actual compressed sample.
    actual_cfg.config_obus = donor_cfg.config_obus.clone();
    let info = FrameStreamInfo::from_entry(&actual.entry).expect("AV1 info").expect("configuration parsed");
    let first = &actual.samples.first().expect("first sample").0;
    assert!(matches!(info.av1_parameter_difference(first).expect("checked difference"), Av1ParameterDifference::ColorDescription { .. }));
    assert!(info.initial_av1_color_candidate(first).expect("checked candidate").is_some(), "{signal}: encoder emitted an unsupported initial shape");

    let bytes = std::fs::read(&actual_path).expect("actual file");
    assert!(bytes.len() <= 8 << 20);
    let file = filmcraft_isobmff::open(bytes.clone()).expect("actual MP4");
    let track_index = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("video track");
    let track = &file.tracks[track_index];
    assert_eq!(track.samples.len(), 48);
    assert!(track.timescale > 0);
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).expect("writer");
    let out_track = mux.add_track(TrackConfig::new(actual.entry.clone(), track.timescale)).expect("track");
    for (index, sample) in track.samples.iter().enumerate() {
        // AV1 ISOBMFF has no composition offsets; fail if the generator violates this premise.
        assert_eq!(sample.pts, sample.dts);
        let payload = file.read_sample(&bytes, track_index, index).expect("sample");
        mux.write_sample(out_track, WriteSample { data: &payload, duration: sample.duration, composition_offset: 0, is_sync: sample.is_sync })
            .expect("mux sample");
    }
    let output: Arc<[u8]> = mux.finish().expect("finish").into_inner().into();
    assert!(output.len() <= 8 << 20);
    (actual, output)
}

fn expected(signal: &str) -> ColorInfo {
    match signal {
        "pq" => ColorInfo { primaries: Primaries::Bt2020, transfer: Transfer::Pq, matrix: Matrix::Bt2020Ncl, range: Range::Limited },
        "hlg" => ColorInfo { primaries: Primaries::Bt2020, transfer: Transfer::Hlg, matrix: Matrix::Bt2020Ncl, range: Range::Limited },
        _ => ColorInfo::REC709,
    }
}

#[test]
#[ignore = "requires generated issue1678 fixtures and a Windows AV1 hardware decoder"]
fn issue1678_generated_av1_initial_metadata() {
    let root = std::path::PathBuf::from(std::env::var_os("FILMCRAFT_ISSUE1678_FIXTURE_DIR").expect("fixture directory required"));
    // Only this test binary/test thread should run: counters and decoder registry are global.
    filmcraft_codecs::register_video_decoder(filmcraft_platform::media_foundation_factory);
    filmcraft_codecs::hw::set_hardware_decoding(true);
    for signal in ["sdr", "pq", "hlg"] {
        let (stream, remuxed) = fixture_stream(&root, signal);
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&stream.entry).expect("software").as_mut(), &stream.samples);
        assert_eq!(reference.len(), 48);
        assert!(reference.iter().all(|frame| frame.frame.color == expected(signal)));
        let calls = Arc::new(AtomicUsize::new(0));
        let info = FrameStreamInfo::from_entry(&stream.entry).unwrap().unwrap();
        let mut strict = filmcraft_platform::HybridDecoder::new(Box::new(StrictStandIn(calls.clone())), stream.entry.clone(), info);
        let baseline_before = hw_stats();
        assert_same("strict baseline replay", &decode_all(&mut strict, &stream.samples), &reference);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "baseline preflight must reject before native input");
        assert_eq!(hw_stats().fallbacks - baseline_before.fallbacks, 1);
        assert!(!strict.is_hardware());

        let before = hw_stats();
        let mut hardware = filmcraft_platform::media_foundation_factory(&stream.entry)
            .expect("REQUIRED: MF AV1 factory declined; this is not a passing skip")
            .expect("MF factory error");
        let output = decode_all(hardware.as_mut(), &stream.samples);
        assert_eq!(output.len(), 48);
        assert_same(signal, &output, &reference);
        assert!(hardware.name().starts_with("Media Foundation"));
        let after = hw_stats();
        assert!(after.sessions > before.sessions);
        assert_eq!(after.frames - before.frames, 48);
        assert_eq!(after.fallbacks, before.fallbacks);
        assert_eq!(after.declined, before.declined);

        let sync = (1..stream.samples.len()).find(|&i| stream.sync[i]).expect("second GOP");
        hardware.reset();
        let mut software = filmcraft_codecs::software_video_decoder(&stream.entry).unwrap();
        let tail = &stream.samples[sync..];
        assert_same("reset/reseek", &decode_all(hardware.as_mut(), tail), &decode_all(software.as_mut(), tail));
        assert!(hardware.name().starts_with("Media Foundation"));
        assert_eq!(hw_stats().fallbacks, before.fallbacks);

        // Production MP4/GOP path, including container nclx override. The raw decoded colour
        // comparison above is insufficient by itself because GopCache can retag YUV output.
        let source_before = hw_stats();
        let source = filmcraft_codecs::Mp4Source::open("generated-issue1678.mp4", remuxed).expect("source");
        assert_eq!(source.info().video.as_ref().unwrap().color, expected(signal));
        let timescale = filmcraft_isobmff::open(std::fs::read(root.join(format!("{signal}-actual.mp4"))).unwrap()).unwrap();
        let ti = timescale.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        for index in [0, 1, 25, 3, 47] {
            let pts = stream.samples[index].1;
            let frame = source
                .video_frame(FrameRequest { time: filmcraft_time::Tick::from_units(pts, i64::from(timescale.tracks[ti].timescale)), scale: 1.0 })
                .expect("source frame");
            assert_eq!(frame.color, expected(signal));
            let want = reference.iter().find(|picture| picture.pts == pts).expect("reference PTS");
            let got = DecodedFrame { pts, frame: (*frame).clone(), draft: false };
            let expected_frame = DecodedFrame { pts, frame: want.frame.clone(), draft: false };
            assert_same("production MP4/GOP", &[got], &[expected_frame]);
        }
        let source_after = hw_stats();
        assert!(source_after.sessions > source_before.sessions);
        assert!(source_after.frames > source_before.frames);
        assert_eq!(source_after.fallbacks, source_before.fallbacks);
        assert_eq!(source_after.declined, source_before.declined);
        eprintln!(
            "ISSUE1678_ACCEPTANCE signal={signal} frames=48 raw_hw_frames={} source_hw_frames={} fallbacks=0 generated_fixture_only=true",
            after.frames - before.frames,
            source_after.frames - source_before.frames
        );
    }
}

/// Own bounded low-overhead OBU fixture edit: remove sequence OBUs without changing compressed
/// picture bytes. This is not a codec implementation or a decoder acceptance proof.
fn strip_sequence_obus(sample: &[u8]) -> Vec<u8> {
    assert!(sample.len() <= 8 << 20);
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut count = 0usize;
    while let Some(&header) = sample.get(offset) {
        count += 1;
        assert!(count <= 4096);
        assert_eq!(header & 0x81, 0);
        let begin = offset;
        offset = offset.checked_add(1).expect("OBU offset");
        if header & 4 != 0 {
            let extension = *sample.get(offset).expect("extension");
            assert_eq!(extension & 7, 0);
            offset = offset.checked_add(1).expect("extension offset");
        }
        let end = if header & 2 != 0 {
            let mut length = 0u64;
            let mut ended = false;
            for shift in 0..8 {
                let byte = *sample.get(offset).expect("OBU length");
                offset = offset.checked_add(1).expect("length offset");
                length |= u64::from(byte & 127) << (shift * 7);
                if byte & 128 == 0 {
                    ended = true;
                    break;
                }
            }
            assert!(ended);
            let length = usize::try_from(length).expect("addressable OBU length");
            offset.checked_add(length).expect("payload end")
        } else {
            sample.len()
        };
        let unit = sample.get(begin..end).expect("complete OBU payload");
        if (header >> 3) & 15 != 1 {
            out.extend_from_slice(unit);
        }
        offset = end;
    }
    out
}

/// AV1 software replaces only the native session. Hybrid compaction, replay and colour metadata
/// are real; this test needs generated compressed samples but never requires a GPU.
struct AdoptedAv1Replay {
    inner: Box<dyn VideoDecoder>,
    expected_initial_sample: Vec<u8>,
    accepted_initialization: Vec<u8>,
    adopted: bool,
    fail_at: usize,
    fed: usize,
}

impl VideoDecoder for AdoptedAv1Replay {
    fn software_replay_initialization(&self) -> Option<&[u8]> {
        self.adopted.then_some(self.accepted_initialization.as_slice())
    }
    fn try_adopt_parameter_change(&mut self, sample: &[u8]) -> filmcraft_codecs::Result<bool> {
        assert_eq!(sample, self.expected_initial_sample);
        assert!(!self.accepted_initialization.is_empty() && self.accepted_initialization.len() <= 65_536);
        self.adopted = true;
        Ok(true)
    }
    fn decode(&mut self, sample: &[u8], pts: i64) -> filmcraft_codecs::Result<Vec<DecodedFrame>> {
        assert!(self.adopted, "first discrepant header must be adopted before native decode");
        let index = self.fed;
        self.fed += 1;
        if index == self.fail_at {
            return Err(filmcraft_codecs::CodecError::Decode("native AV1 failure after headerless restart (test)".into()));
        }
        self.inner.decode(sample, pts)
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.inner.flush()
    }
    fn reset(&mut self) {
        self.inner.reset();
        self.fed = 0;
    }
    fn name(&self) -> &str {
        "software-backed AV1 native replay stand-in"
    }
}

#[test]
#[ignore = "requires generated issue1678 AV1 fixtures; uses a software-backed native stand-in"]
fn issue1678_adopted_av1_header_survives_headerless_restart_and_native_failure() {
    let root = std::path::PathBuf::from(std::env::var_os("FILMCRAFT_ISSUE1678_FIXTURE_DIR").expect("fixture directory required"));
    for signal in ["sdr", "pq", "hlg"] {
        let (mut stream, _) = fixture_stream(&root, signal);
        let info = FrameStreamInfo::from_entry(&stream.entry).unwrap().unwrap();
        let initial_sample = stream.samples[0].0.clone();
        let candidate = info.initial_av1_color_candidate(&initial_sample).unwrap().expect("initial candidate");
        let second_gop = (1..stream.samples.len()).find(|&index| stream.sync[index]).expect("second GOP");
        assert!(info.carries_sequence_header(&stream.samples[second_gop].0));
        for (sample, _) in &mut stream.samples[second_gop..] {
            *sample = strip_sequence_obus(sample);
            assert!(!info.carries_sequence_header(sample));
        }
        assert!(info.is_random_access(&stream.samples[second_gop].0), "replay compacts at this headerless key frame");
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&stream.entry).unwrap().as_mut(), &stream.samples);
        assert_eq!(reference.len(), 48);
        assert!(reference.iter().all(|frame| frame.frame.color == expected(signal)));
        let native = AdoptedAv1Replay {
            inner: filmcraft_codecs::software_video_decoder(&stream.entry).unwrap(),
            expected_initial_sample: initial_sample,
            accepted_initialization: candidate.config_obus,
            adopted: false,
            fail_at: second_gop + 2,
            fed: 0,
        };
        let mut hybrid = filmcraft_platform::HybridDecoder::new(Box::new(native), stream.entry.clone(), info);
        assert_same("accepted sequence survives compacted headerless restart", &decode_all(&mut hybrid, &stream.samples), &reference);
        assert!(!hybrid.is_hardware());
        hybrid.reset();
        let reset_pts = stream.samples[second_gop].1;
        let reference_start = reference.iter().position(|frame| frame.pts == reset_pts).expect("second GOP reference PTS");
        assert_same(
            "accepted replay sequence survives software reset to headerless GOP",
            &decode_all(&mut hybrid, &stream.samples[second_gop..]),
            &reference[reference_start..],
        );
    }
}

#[test]
#[ignore = "requires generated issue1678 fixtures and a Windows AV1 hardware decoder"]
fn issue1678_real_native_failure_preserves_headerless_replay_and_reset() {
    let root = std::path::PathBuf::from(std::env::var_os("FILMCRAFT_ISSUE1678_FIXTURE_DIR").expect("fixture directory required"));
    filmcraft_codecs::hw::set_hardware_decoding(true);
    for signal in ["sdr", "pq", "hlg"] {
        let (mut stream, _) = fixture_stream(&root, signal);
        let info = FrameStreamInfo::from_entry(&stream.entry).unwrap().unwrap();
        let second_gop = (1..stream.samples.len()).find(|&index| stream.sync[index]).expect("second GOP");
        for (sample, _) in &mut stream.samples[second_gop..] {
            *sample = strip_sequence_obus(sample);
        }
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&stream.entry).unwrap().as_mut(), &stream.samples);
        assert_eq!(reference.len(), 48);
        let mut native = filmcraft_platform::media_foundation::MfDecoder::from_entry(&stream.entry).expect("REQUIRED: real AV1 DXVA decoder");
        native.fail_after(u64::try_from(second_gop + 2).unwrap());
        let before = hw_stats();
        let mut hybrid = filmcraft_platform::HybridDecoder::new(Box::new(native), stream.entry.clone(), info);
        assert_same("real native failure preserves accepted AV1 header", &decode_all(&mut hybrid, &stream.samples), &reference);
        let after = hw_stats();
        assert!(after.frames > before.frames && after.frames - before.frames < 48);
        assert_eq!(after.fallbacks - before.fallbacks, 1);
        assert!(!hybrid.is_hardware());
        hybrid.reset();
        let reset_pts = stream.samples[second_gop].1;
        let start = reference.iter().position(|frame| frame.pts == reset_pts).unwrap();
        assert_same("real native failure then software reset", &decode_all(&mut hybrid, &stream.samples[second_gop..]), &reference[start..]);
        eprintln!("ISSUE1678_FORCED_FALLBACK signal={signal} hardware_frames={} fallbacks=1 pixels_color_pts_reset_match=true", after.frames - before.frames);
    }
}

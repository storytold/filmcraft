//! Controlled hardware stand-ins cover decode/drain failure, missing output, unknown input, parameter changes and reset.
//! Reference pixels/order and reported errors stay exact.

use crate::common;

use common::*;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_codecs::{DecodedFrame, Result, VideoDecoder};
use filmcraft_platform::HybridDecoder;

/// Decodes like the software decoder until `fail_after` samples were fed, then fails every call
/// (its pending pictures stay retrievable through `flush`, as with a real session).
struct Failing {
    inner: Box<dyn VideoDecoder>,
    fail_after: usize,
    fed: usize,
}

impl VideoDecoder for Failing {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.fed += 1;
        if self.fed > self.fail_after {
            return Err(filmcraft_codecs::CodecError::Decode("session invalidated (test)".into()));
        }
        self.inner.decode(sample, pts)
    }
    fn flush(&mut self) -> filmcraft_codecs::Result<Vec<DecodedFrame>> {
        self.inner.flush()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn name(&self) -> &str {
        "stand-in hardware"
    }
}

fn hybrid(s: &Stream, fail_after: usize) -> HybridDecoder {
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let inner = filmcraft_codecs::software_video_decoder(&s.entry).unwrap();
    HybridDecoder::new(Box::new(Failing { inner, fail_after, fed: 0 }), s.entry.clone(), info)
}

#[test]
fn header_output_intent_matches_single_picture_fixtures() {
    use filmcraft_codecs::hw::{NalCodec, OutputInspector, SampleOutput};

    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let s = read_stream(&named(&ff, name).expect("fixture"));
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
        let reference_pts: std::collections::BTreeSet<_> = reference.iter().map(|frame| frame.pts).collect();
        let byte_limit = s.samples.iter().map(|(data, _)| data.len()).sum::<usize>() + info.parameter_sets.iter().map(Vec::len).sum::<usize>();
        let mut inspector = OutputInspector::new(info.clone().into(), byte_limit);
        let mut known = 0;
        for (index, (data, pts)) in s.samples.iter().enumerate() {
            match inspector.inspect(data) {
                Some(plan) => {
                    assert_eq!(plan.pictures, usize::from(reference_pts.contains(pts)), "{name} sample {index}: header intent vs software reference");
                    known += 1;
                }
                None => {
                    let types = info.nal_types(data);
                    assert!(
                        info.codec == NalCodec::Hevc && types.iter().any(|t| matches!(t, 8 | 9 | 36 | 37)),
                        "{name} sample {index}: ordinary supported picture unexpectedly unknown: {types:?}"
                    );
                }
            }
        }
        assert_eq!(known, s.samples.len(), "{name}: every supported fixture input must be accounted for");
        if info.codec == NalCodec::Hevc {
            let seek = (1..s.samples.len()).find(|&index| s.sync[index]).expect("second random-access point");
            let tail = &s.samples[seek..];
            let after_seek = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), tail);
            assert!(after_seek.len() < tail.len(), "{name}: CRA fixture must exercise skipped leading pictures after seek");
            let after_seek_pts: std::collections::BTreeSet<_> = after_seek.iter().map(|frame| frame.pts).collect();
            inspector.reset();
            for (data, pts) in tail {
                let plan = inspector.inspect(data).expect("known run-start RASL disposition");
                assert_eq!(plan.pictures, usize::from(after_seek_pts.contains(pts)), "{name}: seek-start intent vs software reference");
            }
        }
        let aud = match info.codec {
            NalCodec::H264 => vec![0, 0, 0, 1, 9],
            NalCodec::Hevc => vec![0, 0, 0, 2, 35 << 1, 1],
        };
        inspector.reset();
        assert_eq!(inspector.inspect(&aud), Some(SampleOutput::default()), "{name}: AUD has no current picture");
        let mut multiple = s.samples[0].0.clone();
        multiple.extend_from_slice(&s.samples[1].0);
        assert_eq!(inspector.inspect(&multiple), None, "{name}: multiple pictures require software");
        assert_eq!(inspector.inspect(&s.samples[0].0), None, "{name}: unknown syntax invalidates the run until reset");
        inspector.reset();
        let mut truncated = s.samples[0].0.clone();
        truncated.push(0);
        assert_eq!(inspector.inspect(&truncated), None, "{name}: trailing partial length cannot disappear");
        assert_eq!(OutputInspector::new(info.into(), 0).inspect(&s.samples[0].0), None, "{name}: input budget is checked before parsing");
        eprintln!("{name}: {known}/{} inputs classified", s.samples.len());
    }
}

#[test]
fn output_headers_hevc_do_not_require_software_pixel_support() {
    check_header_only_format(filmcraft_codecs::hw::NalCodec::Hevc);
}

#[test]
fn output_headers_h264_do_not_require_software_pixel_support() {
    check_header_only_format(filmcraft_codecs::hw::NalCodec::H264);
}

fn check_header_only_format(codec: filmcraft_codecs::hw::NalCodec) {
    use filmcraft_codecs::hw::NalCodec;

    // 64 samples align to the largest HEVC CTU and whole AVC macroblocks; one IDR isolates header intent.
    const SIDE: u32 = 64;
    const PICTURES: usize = 1;
    let (encoder, pixel_format, profile, bits, parameter_flag, parameters) = match codec {
        NalCodec::Hevc => ("libx265", "yuv422p10le", "main422-10", 10, "-x265-params", format!("keyint=1:ctu={SIDE}:log-level=error")),
        NalCodec::H264 => ("libx264", "yuv422p", "high422", 8, "-x264-params", "keyint=1".into()),
    };
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = filmcraft_testkit::temp_path(&filmcraft_testkit::fixtures_dir("platform").join(format!("output_header_{profile}.mp4")));
    let generated = std::process::Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={SIDE}x{SIDE}:rate=1"))
        .args(["-frames:v", &PICTURES.to_string(), "-c:v", encoder, "-pix_fmt", pixel_format, "-profile:v", profile, parameter_flag])
        .arg(parameters)
        .arg(&path)
        .output()
        .expect("fixture encoder");
    assert!(generated.status.success(), "fixture generation failed: {}", String::from_utf8_lossy(&generated.stderr));
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    assert_eq!(info.codec, codec);
    assert_eq!(info.coded, (SIDE, SIDE));
    assert_eq!(info.chroma_format_idc, 2, "AVC/HEVC chroma_format_idc 2 is 4:2:2");
    assert_eq!(info.bit_depth_luma, bits);
    assert_eq!(s.samples.len(), PICTURES);
    let pixel_unsupported = match codec {
        NalCodec::Hevc => {
            let nal = info.parameter_sets.iter().find(|nal| filmcraft_hevc::slice::NalHeader::parse(nal).is_ok_and(|header| header.nal_type == 33)).unwrap();
            filmcraft_hevc::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(&nal[2..])).unwrap().check_supported().is_err()
        }
        NalCodec::H264 => {
            let nal = info.parameter_sets.iter().find(|nal| nal.first().is_some_and(|header| header & 0x1f == 7)).unwrap();
            filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(&nal[1..])).unwrap().check_supported().is_err()
        }
    };
    assert!(pixel_unsupported, "negative control: software pixels do not support this format");
    let byte_limit = s.samples.iter().map(|(data, _)| data.len()).sum::<usize>() + info.parameter_sets.iter().map(Vec::len).sum::<usize>();
    let mut inspector = filmcraft_codecs::hw::OutputInspector::new(info.into(), byte_limit);
    eprintln!("{codec:?} header fixture: {SIDE}x{SIDE}, 4:2:2, {bits}-bit, {PICTURES} IDR");
    let output = inspector.inspect(&s.samples[0].0).expect("pixel support must not gate header-only output intent");
    assert_eq!(output.pictures, PICTURES);
    assert!(output.restart);
}

struct BufferedHardware {
    decode_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    flush_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl VideoDecoder for BufferedHardware {
    fn decode(&mut self, _sample: &[u8], _pts: i64) -> Result<Vec<DecodedFrame>> {
        self.decode_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Vec::new())
    }
    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        self.flush_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(filmcraft_codecs::CodecError::Decode("unknown input must not publish a speculative hardware drain (test)".into()))
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "buffered stand-in hardware"
    }
}

#[test]
fn unknown_input_selects_software_without_draining_pending_hardware() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let name = FIXTURES[0].0;
    let s = read_stream(&named(&ff, name).expect("fixture"));
    // One known picture stays pending; all remaining access units share one input and timestamp.
    let remaining: Vec<u8> = s.samples[1..].iter().flat_map(|(bytes, _)| bytes.iter().copied()).collect();
    let last_pts = s.samples.iter().map(|(_, pts)| *pts).max().expect("nonempty fixture");
    let samples = vec![s.samples[0].clone(), (remaining, last_pts)];
    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &samples);
    assert_eq!(reference.len(), s.samples.len(), "software preserves every picture in the multi-picture input");
    let decode_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let flush_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hw = BufferedHardware { decode_calls: decode_calls.clone(), flush_calls: flush_calls.clone() };
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut hybrid = HybridDecoder::new(Box::new(hw), s.entry.clone(), info);
    let out = decode_all(&mut hybrid, &samples);
    assert!(!hybrid.is_hardware());
    assert_eq!(decode_calls.load(std::sync::atomic::Ordering::Relaxed), 1, "known input responds through hardware before the unknown input");
    assert_eq!(flush_calls.load(std::sync::atomic::Ordering::Relaxed), 0, "unknown prior-output effects must not trigger hardware drain");
    assert_same(name, &out, &reference);
}

struct DrainFailure {
    inner: Box<dyn VideoDecoder>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    withheld: Option<Vec<DecodedFrame>>,
}

struct MissingPicture {
    frames: std::collections::VecDeque<DecodedFrame>,
    submitted: std::collections::BTreeSet<i64>,
    fail_after: usize,
    fed: usize,
    missing: i64,
    later_output: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl MissingPicture {
    fn ready(&mut self) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        while self.frames.front().is_some_and(|frame| self.submitted.contains(&frame.pts)) {
            if let Some(frame) = self.frames.pop_front()
                && frame.pts != self.missing
            {
                out.push(frame);
            }
        }
        out
    }
}

impl VideoDecoder for MissingPicture {
    fn decode(&mut self, _sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.fed += 1;
        if self.fed > self.fail_after {
            return Err(filmcraft_codecs::CodecError::Decode("session invalidated after missing output (test)".into()));
        }
        self.submitted.insert(pts);
        let frames = self.ready();
        if frames.iter().any(|frame| frame.pts > self.missing) {
            self.later_output.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(frames)
    }
    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        Ok(self.ready())
    }
    fn reset(&mut self) {
        self.frames.clear();
        self.submitted.clear();
        self.fed = 0;
        self.later_output.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    fn name(&self) -> &str {
        "hardware omitting one visible picture"
    }
}

#[test]
fn a_missing_picture_is_recovered_before_newer_output_is_published() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let name = FIXTURES[0].0;
    let s = read_stream(&named(&ff, name).expect("fixture"));
    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
    assert_eq!(reference.len(), s.samples.len(), "one displayed picture per access unit");
    // Omit a picture in the first quarter; invalidate halfway through, after later output exists.
    let missing = reference[reference.len() / 4].pts;
    let later_output = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hw = MissingPicture {
        // Predecoded oracle pictures remove worker timing; publish only pictures whose input arrived.
        frames: decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples).into(),
        submitted: Default::default(),
        fail_after: s.samples.len() / 2,
        fed: 0,
        missing,
        later_output: later_output.clone(),
    };
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut hybrid = HybridDecoder::new(Box::new(hw), s.entry.clone(), info);
    let out = decode_all(&mut hybrid, &s.samples);
    assert!(later_output.load(std::sync::atomic::Ordering::Relaxed), "fault gate: decode returned a newer picture before failure, not only at drain");
    assert!(!hybrid.is_hardware(), "missing output must use software recovery");
    assert_eq!(
        out.iter().map(|f| f.pts).collect::<Vec<_>>(),
        reference.iter().map(|f| f.pts).collect::<Vec<_>>(),
        "missing picture recovered before publishing newer output"
    );
    assert_same(name, &out, &reference);
}

impl VideoDecoder for DrainFailure {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let frames = self.inner.decode(sample, pts)?;
        if let Some(withheld) = &mut self.withheld {
            withheld.extend(frames);
            Ok(Vec::new())
        } else {
            Ok(frames)
        }
    }
    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _consumed = self.inner.flush()?;
        Err(filmcraft_codecs::CodecError::Decode("injected hardware drain failure".into()))
    }
    fn reset(&mut self) {
        self.inner.reset();
        if let Some(withheld) = &mut self.withheld {
            withheld.clear();
        }
    }
    fn name(&self) -> &str {
        "failing hardware drain"
    }
}

#[test]
fn failed_drain_replays_unreturned_pictures_across_restart_points() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let name = FIXTURES[0].0;
    let s = read_stream(&named(&ff, name).expect("fixture"));
    assert!(s.sync.iter().filter(|&&sync| sync).count() > 1, "fixture must cross restart points");
    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
    assert_eq!(reference.len(), s.samples.len(), "fixture has one picture per access unit");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Decode every input but defer publication until drain; timing cannot retire the replay prefix.
    let hw = DrainFailure { inner: filmcraft_codecs::software_video_decoder(&s.entry).unwrap(), calls: calls.clone(), withheld: Some(Vec::new()) };
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut hybrid = HybridDecoder::new(Box::new(hw), s.entry.clone(), info);
    for (data, pts) in &s.samples {
        assert!(hybrid.decode(data, *pts).unwrap().is_empty(), "no pictures published before drain");
    }
    let out = hybrid.flush().unwrap();
    assert!(!hybrid.is_hardware());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1, "failed drain must not be retried");
    assert_same(name, &out, &reference);
}

#[test]
fn failed_hardware_drain_replays_once_without_losing_or_repeating_pictures() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let path = named(&ff, name).expect("fixture");
        let s = read_stream(&path);
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hw = DrainFailure { inner: filmcraft_codecs::software_video_decoder(&s.entry).unwrap(), calls: calls.clone(), withheld: None };
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        let mut hybrid = HybridDecoder::new(Box::new(hw), s.entry.clone(), info);
        let mut out = Vec::new();
        for (data, pts) in &s.samples {
            out.extend(hybrid.decode(data, *pts).unwrap());
        }
        assert!(hybrid.is_hardware(), "{name}: failure starts at drain");
        out.extend(hybrid.flush().unwrap());
        assert!(!hybrid.is_hardware(), "{name}: replay used software");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1, "{name}: failed drain must not be called twice");
        assert_eq!(out.iter().map(|f| f.pts).collect::<Vec<_>>(), reference.iter().map(|f| f.pts).collect::<Vec<_>>(), "{name}: every picture once in order");
        assert_same(name, &out, &reference);
    }
}

#[test]
fn failed_hardware_drain_reports_unavailable_software_replay() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = named(&ff, FIXTURES[0].0).expect("fixture");
    let s = read_stream(&path);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hw = DrainFailure { inner: filmcraft_codecs::software_video_decoder(&s.entry).unwrap(), calls: calls.clone(), withheld: None };
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut unsupported = s.entry.clone();
    unsupported.codec = filmcraft_isobmff::CodecConfig::Unknown { fourcc: filmcraft_isobmff::FourCc(*b"none"), raw: Vec::new() };
    let mut hybrid = HybridDecoder::new(Box::new(hw), unsupported, info);
    let error = hybrid.flush().err().expect("hardware drain failure has no software decoder").to_string();
    assert!(error.contains("injected hardware drain failure") && error.contains("no software decoder"), "{error}");
    assert!(hybrid.flush().is_err(), "missing decoder cannot become successful empty output");
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1, "hardware drain is not retried");
}

#[test]
fn failures_anywhere_continue_with_the_software_output() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let syncs: Vec<usize> = (0..s.samples.len()).filter(|&i| s.sync[i]).collect();
        let [_, k, .., last] = syncs[..] else { panic!("{name}: three GOPs or more") };
        // The stream up to a few samples into its last GOP: past the last failure point the
        // hybrid only passes the software decoder's output on.
        let samples = &s.samples[..(last + 6).min(s.samples.len())];
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), samples);
        // The hybrid's paths differ by what the replay log holds and what the hardware had
        // pending: in the first GOP (nothing out yet, then pictures held for reordering), at and
        // just after the second restart point (the log starts over, or an HEVC CRA keeps the GOP
        // before it), at the last one (a CRA drops the GOP before the previous one), at the last
        // sample and never. Failing anywhere else takes one of those paths.
        let points = [0, 1, 3, 7, k, k + 1, k + 2, k + 5, last, last + 2, samples.len() - 1, samples.len()];
        for fail_after in points {
            let mut d = hybrid(&s, fail_after);
            let out = decode_all(&mut d, samples);
            assert_eq!(d.is_hardware(), fail_after >= samples.len(), "{name}: fell back at {fail_after}");
            assert_same(&format!("{name} failing after {fail_after} samples"), &out, &reference);
        }
        // a failure after a seek (reset) into the second GOP
        let tail = &samples[k..];
        let expect = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), tail);
        let mut d = hybrid(&s, k + 4);
        decode_all(&mut d, &samples[..k]);
        d.reset();
        assert_same(&format!("{name} failing after a seek"), &decode_all(&mut d, tail), &expect);
    }
}

#[test]
fn changed_in_band_parameter_sets_switch_to_software() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = named(&ff, "h264_high.mp4") else { return };
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let sps = info.parameter_sets[0].clone();
    let with_sps = |sps: &[u8], sample: &[u8]| {
        let mut v = (sps.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(sps);
        v.extend_from_slice(sample);
        v
    };
    // the same SPS repeated in band at every sync sample: stays in "hardware"
    let same: Vec<_> = s.samples.iter().zip(&s.sync).map(|((x, p), &k)| (if k { with_sps(&sps, x) } else { x.clone() }, *p)).collect();
    let mut d = hybrid(&s, usize::MAX);
    let out = decode_all(&mut d, &same);
    assert!(d.is_hardware());
    assert_same("repeated SPS", &out, &decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &same));
    // a different SPS (same values, another nal_ref_idc: a trailing zero byte is not a change) at
    // the second sync sample: software
    let mut changed_sps = sps.clone();
    changed_sps[0] ^= 0x40;
    let k = (1..s.samples.len()).find(|&i| s.sync[i]).unwrap();
    let changed: Vec<_> = s.samples.iter().enumerate().map(|(i, (x, p))| (if i == k { with_sps(&changed_sps, x) } else { x.clone() }, *p)).collect();
    let mut d = hybrid(&s, usize::MAX);
    let out = decode_all(&mut d, &changed);
    assert!(!d.is_hardware(), "changed SPS: software");
    assert_same("changed SPS", &out, &decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &changed));
}

/// Feed `samples` then flush, carrying on past samples the decoder rejects (as playback does);
/// returns the pictures and the indices of the rejected samples.
fn decode_lenient(d: &mut dyn VideoDecoder, samples: &[(Vec<u8>, i64)]) -> (Vec<DecodedFrame>, Vec<usize>) {
    let (mut out, mut rejected) = (Vec::new(), Vec::new());
    for (i, (s, p)) in samples.iter().enumerate() {
        match d.decode(s, *p) {
            Ok(f) => out.extend(f),
            Err(_) => rejected.push(i),
        }
    }
    out.extend(d.flush().unwrap());
    (out, rejected)
}

/// forbidden_zero_bit=1 is invalid in both H.264 and HEVC; leave length prefixes/payload unchanged.
fn damaged_header(sample: &[u8], length_size: usize) -> Vec<u8> {
    let mut v = sample.to_vec();
    *v.get_mut(length_size).expect("fixture NAL header") |= 0x80;
    v
}

#[test]
fn unclassifiable_headers_follow_the_software_error_contract() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let mut checked = 0;
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        // the second restart point, and a disposable picture before it that is not the first sample
        let Some(next) = (1..s.samples.len()).find(|&i| s.sync[i]) else { continue };
        let Some(d) = (2..next.saturating_sub(2)).find(|&i| info.is_disposable(&s.samples[i].0)) else { continue };
        // the first two GOPs: the replay and the restart after it
        let end = (next + 1..s.samples.len()).find(|&i| s.sync[i]).unwrap_or(s.samples.len());
        let mut stream = s.samples[..end].to_vec();
        stream[d].0 = damaged_header(&s.samples[d].0, info.length_size);
        let mut inspector = filmcraft_codecs::hw::OutputInspector::new(info.into(), stream.iter().map(|(data, _)| data.len()).sum());
        assert_eq!(inspector.inspect(&stream[d].0), None, "{name}: forbidden header bit must select software");
        let (reference, rejected) = decode_lenient(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &stream);
        assert_eq!(rejected, [d], "{name}: the software decoder rejects the damaged sample (and only it)");
        // Immediate hardware failure and healthy hardware both obey the same unknown-header policy.
        for fail_after in [0, stream.len()] {
            let mut h = hybrid(&s, fail_after);
            let (out, errors) = decode_lenient(&mut h, &stream);
            let what = format!("{name}: sample {d} damaged, hardware failing after {fail_after}");
            assert!(!h.is_hardware(), "{what}: fell back");
            assert_eq!(errors, rejected, "{what}: report the software reference's errors");
            let pts: Vec<i64> = out.iter().map(|f| f.pts).collect();
            assert!(pts.windows(2).all(|w| w[0] < w[1]), "{what}: pictures once and in order: {pts:?}");
            assert_same(&what, &out, &reference);
        }
        checked += 1;
    }
    assert_eq!(checked, FIXTURES.len(), "every fixture has a damaged-sample case");
}

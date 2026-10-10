//! [`HybridDecoder`] fallback on every platform, with a stand-in "hardware" decoder (our software
//! decoder that starts failing after N samples, as a lost hardware session does): the output must
//! be exactly the software decoder's, whatever sample the failure hits. Also: in-band parameter
//! sets that differ from the sample entry's switch to software; identical ones do not; and a
//! damaged sample that the hardware decoded but the software decoder rejects, inside the replayed
//! run, loses no picture.

mod common;

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
    fn flush(&mut self) -> Vec<DecodedFrame> {
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
fn failures_anywhere_continue_with_the_software_output() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
        let syncs: Vec<usize> = (0..s.samples.len()).filter(|&i| s.sync[i]).collect();
        let mut points: Vec<usize> = vec![0, 1, 3, 7, s.samples.len() - 1, s.samples.len()];
        for &k in &syncs {
            points.extend([k, k + 1, k + 2, k + 5]);
        }
        for fail_after in points {
            let mut d = hybrid(&s, fail_after);
            let out = decode_all(&mut d, &s.samples);
            assert_eq!(d.is_hardware(), fail_after >= s.samples.len(), "{name}: fell back at {fail_after}");
            assert_same(&format!("{name} failing after {fail_after} samples"), &out, &reference);
        }
        // a failure after a seek (reset) into the second GOP
        if let Some(&k) = syncs.get(1) {
            let tail = &s.samples[k..];
            let expect = {
                let mut sw = filmcraft_codecs::software_video_decoder(&s.entry).unwrap();
                decode_all(sw.as_mut(), tail)
            };
            let mut d = hybrid(&s, k + 4);
            decode_all(&mut d, &s.samples[..k]);
            d.reset();
            assert_same(&format!("{name} failing after a seek"), &decode_all(&mut d, tail), &expect);
        }
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
    // a different SPS (same values, trailing zero byte) at the second sync sample: software
    let mut changed_sps = sps.clone();
    changed_sps.push(0);
    let k = (1..s.samples.len()).find(|&i| s.sync[i]).unwrap();
    let changed: Vec<_> = s.samples.iter().enumerate().map(|(i, (x, p))| (if i == k { with_sps(&changed_sps, x) } else { x.clone() }, *p)).collect();
    let mut d = hybrid(&s, usize::MAX);
    let out = decode_all(&mut d, &changed);
    assert!(!d.is_hardware(), "changed SPS: software");
    assert_same("changed SPS", &out, &decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &changed));
}

/// Like [`Failing`], but tolerant of one damaged sample the way a GPU can be: at `damaged_at` it
/// decodes the intact sample instead of the one it is given (which the software decoder rejects).
struct Tolerant {
    inner: Box<dyn VideoDecoder>,
    intact: Vec<u8>,
    damaged_at: usize,
    fail_after: usize,
    fed: usize,
}

impl VideoDecoder for Tolerant {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let i = self.fed;
        self.fed += 1;
        if self.fed > self.fail_after {
            return Err(filmcraft_codecs::CodecError::Decode("session invalidated (test)".into()));
        }
        self.inner.decode(if i == self.damaged_at { &self.intact } else { sample }, pts)
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.inner.flush()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn name(&self) -> &str {
        "stand-in hardware"
    }
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
    out.extend(d.flush());
    (out, rejected)
}

/// The sample's NAL unit with its header kept and its payload overwritten: the slice header no
/// longer parses, so the software decoder rejects the sample.
fn damaged(sample: &[u8], nal_header: usize) -> Vec<u8> {
    let mut v = sample.to_vec();
    let start = (4 + nal_header).min(v.len());
    v[start..].fill(0xff);
    v
}

/// A replay (after the hardware fails) that meets a sample the software decoder rejects must go
/// on to the end of the replayed run, like decoding in software does, and must not lose the
/// pictures it decoded before that sample: every picture the software decoder outputs for the
/// stream is output, once and in order, and from the next restart point on the pictures are the
/// software decoder's.
#[test]
fn a_sample_software_rejects_during_the_replay_loses_no_pictures() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let mut checked = 0;
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        let header = if name.starts_with("hevc") { 2 } else { 1 };
        // the second restart point, and a disposable picture before it that is not the first sample
        let Some(next) = (1..s.samples.len()).find(|&i| s.sync[i]) else { continue };
        let Some(d) = (2..next.saturating_sub(2)).find(|&i| info.is_disposable(&s.samples[i].0)) else { continue };
        let mut stream = s.samples.clone();
        stream[d].0 = damaged(&s.samples[d].0, header);
        let (reference, rejected) = decode_lenient(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &stream);
        assert_eq!(rejected, [d], "{name}: the software decoder rejects the damaged sample (and only it)");
        let restart = stream[next..].iter().map(|x| x.1).min().unwrap();
        // failing at the damaged sample itself (the software decoder rejects the failed sample
        // too), and after the hardware had decoded it
        for fail_after in [d, d + 1, d + 2, next - 1] {
            let tolerant = Tolerant {
                inner: filmcraft_codecs::software_video_decoder(&s.entry).unwrap(),
                intact: s.samples[d].0.clone(),
                damaged_at: d,
                fail_after,
                fed: 0,
            };
            let mut h = HybridDecoder::new(Box::new(tolerant), s.entry.clone(), info.clone());
            let (out, errors) = decode_lenient(&mut h, &stream);
            let what = format!("{name}: sample {d} damaged, hardware failing after {fail_after}");
            assert!(!h.is_hardware(), "{what}: fell back");
            // the damaged sample is reported as the software decoder reports it, unless the
            // hardware had already decoded it
            assert_eq!(errors, if fail_after == d { vec![d] } else { vec![] }, "{what}: rejected samples");
            let pts: Vec<i64> = out.iter().map(|f| f.pts).collect();
            assert!(pts.windows(2).all(|w| w[0] < w[1]), "{what}: pictures once and in order: {pts:?}");
            let lost: Vec<i64> = reference.iter().map(|f| f.pts).filter(|p| !pts.contains(p)).collect();
            assert!(lost.is_empty(), "{what}: lost pictures at pts {lost:?}");
            let from = |v: &[DecodedFrame]| v.iter().position(|f| f.pts >= restart).unwrap_or(v.len());
            assert_same(&format!("{what}, from the next restart point"), &out[from(&out)..], &reference[from(&reference)..]);
        }
        checked += 1;
    }
    assert_eq!(checked, FIXTURES.len(), "every fixture has a damaged-sample case");
}

//! [`HybridDecoder`]: a hardware decoder that turns into the software decoder for the same stream
//! when it fails mid-stream (decode error, session lost to a GPU change or sleep, an in-band
//! parameter-set change it was not set up for), without the caller noticing.
//!
//! The hybrid keeps the samples fed since the last point decoding can restart from (the run's
//! first sample, then every IDR / IRAP access unit). On a failure it builds the software decoder
//! ([`filmcraft_codecs::software_video_decoder`], skipping registered factories), replays those
//! samples through it and carries on, dropping pictures the hardware already returned and keeping
//! the ones it had decoded but not yet returned, so the output is the software decoder's own
//! sequence. That replay log is bounded; past the bound a failure is reported as an error once
//! and the instance continues in software from the next seek ([`VideoDecoder::reset`]).

use std::collections::{BTreeMap, BTreeSet};

use filmcraft_codecs::hw::StreamInfo;
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_isobmff::SampleEntry;

/// Replay log bounds: samples and bytes since the last restart point.
const LOG_MAX_SAMPLES: usize = 600;
const LOG_MAX_BYTES: usize = 256 << 20;
/// Presentation times remembered as returned (older ones are forgotten first).
const EMITTED_MAX: usize = 4096;
/// Initialization consists only of bounded accepted sequence configuration, never media samples.
const INITIALIZATION_MAX_BYTES: usize = 65_536;

/// A hardware decoder with a transparent software fallback.
pub struct HybridDecoder {
    hw: Option<Box<dyn VideoDecoder>>,
    sw: Option<Box<dyn VideoDecoder>>,
    software_initialization: Option<Vec<u8>>,
    needs_software_initialization: bool,
    entry: SampleEntry,
    info: StreamInfo,
    draft: bool,
    /// Samples (and pts) since the last restart point; `None` once the bound was passed.
    log: Option<Vec<(Vec<u8>, i64)>>,
    log_bytes: usize,
    /// Index in the log of the most recent restart point.
    last_irap: usize,
    /// Pictures returned since the last reset (by pts).
    emitted: BTreeSet<i64>,
    /// Pictures the hardware had decoded but not returned when it failed.
    carry: BTreeMap<i64, DecodedFrame>,
}

impl HybridDecoder {
    /// Wrap `hw`, a decoder for `entry` (whose stream is described by `info`).
    pub fn new(hw: Box<dyn VideoDecoder>, entry: SampleEntry, info: impl Into<StreamInfo>) -> Self {
        let info = info.into();
        filmcraft_codecs::hw::note_hw_session();
        Self {
            hw: Some(hw),
            sw: None,
            software_initialization: None,
            needs_software_initialization: false,
            entry,
            info,
            draft: false,
            log: Some(Vec::new()),
            log_bytes: 0,
            last_irap: 0,
            emitted: BTreeSet::new(),
            carry: BTreeMap::new(),
        }
    }

    /// Whether the hardware decoder is still in use (false after a fallback).
    pub fn is_hardware(&self) -> bool {
        self.hw.is_some()
    }

    fn remember(&mut self, sample: &[u8], pts: i64) {
        if self.info.is_irap(sample) {
            // An HEVC CRA keeps the previous restart point: replaying from the CRA itself would
            // treat it as a first picture and drop its RASL pictures, which the hardware decodes.
            let cra = self.info.keeps_previous_restart(sample);
            match self.log.as_mut() {
                Some(log) if cra => {
                    log.drain(..self.last_irap.min(log.len()));
                    self.log_bytes = log.iter().map(|s| s.0.len()).sum();
                }
                _ => {
                    self.log = Some(Vec::new());
                    self.log_bytes = 0;
                }
            }
            self.last_irap = self.log.as_ref().map_or(0, Vec::len);
        }
        let Some(log) = self.log.as_mut() else { return };
        self.log_bytes = self.log_bytes.saturating_add(sample.len());
        if log.len() >= LOG_MAX_SAMPLES || self.log_bytes > LOG_MAX_BYTES {
            self.log = None;
            return;
        }
        log.push((sample.to_vec(), pts));
    }

    fn note_emitted(&mut self, out: &[DecodedFrame]) {
        for f in out {
            self.emitted.insert(f.pts);
        }
        while self.emitted.len() > EMITTED_MAX {
            self.emitted.pop_first();
        }
    }

    /// In-band parameter sets (sequence header, key frame format) that differ from the sample
    /// entry's (the hardware session was set up from those).
    fn parameter_sets_changed(&self, sample: &[u8]) -> bool {
        self.info.parameters_changed(sample)
    }

    /// Software output after a fallback: pictures already returned are dropped, carried hardware
    /// pictures are merged in presentation order (a software picture replaces the carried one with
    /// the same pts).
    fn merge(&mut self, frames: Vec<DecodedFrame>) -> Vec<DecodedFrame> {
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            if self.emitted.contains(&f.pts) {
                self.carry.remove(&f.pts);
                continue;
            }
            while let Some(e) = self.carry.first_entry() {
                if *e.key() >= f.pts {
                    break;
                }
                out.push(e.remove());
            }
            self.carry.remove(&f.pts);
            out.push(f);
        }
        self.note_emitted(&out);
        out
    }

    fn initialize_software(&mut self) -> Result<()> {
        if !self.needs_software_initialization {
            return Ok(());
        }
        let Some(initialization) = self.software_initialization.as_deref() else {
            return Err(CodecError::Decode("software replay initialization is missing".into()));
        };
        let Some(sw) = self.sw.as_mut() else {
            return Err(CodecError::Decode("no software decoder to initialize for replay".into()));
        };
        let frames = sw.decode(initialization, 0).map_err(|e| CodecError::Decode(format!("cannot initialize software replay: {e}")))?;
        if !frames.is_empty() {
            return Err(CodecError::Decode("software replay initialization unexpectedly produced pictures".into()));
        }
        self.needs_software_initialization = false;
        Ok(())
    }

    /// Switch to the software decoder after `err`, replaying the log (which ends with the sample
    /// that failed). The replay goes on past samples the software decoder rejects, as decoding in
    /// software from the restart point would: one that the hardware had already decoded is logged
    /// (its call returned long ago), and if the failed sample itself is rejected its error is this
    /// call's, while the pictures replayed before it are kept for the next output.
    fn fall_back(&mut self, err: CodecError) -> Result<Vec<DecodedFrame>> {
        // Keep the native owner available if preserving its accepted configuration fails. The
        // original entry/info remain unchanged; one bounded copy also survives software reset.
        let initialization = self
            .hw
            .as_ref()
            .and_then(|hw| hw.software_replay_initialization())
            .map(|bytes| {
                if bytes.is_empty() || bytes.len() > INITIALIZATION_MAX_BYTES {
                    return Err(CodecError::Decode("software replay initialization must contain 1..=64 KiB".into()));
                }
                let mut copy = Vec::new();
                copy.try_reserve_exact(bytes.len()).map_err(|e| CodecError::Decode(format!("cannot retain software replay initialization: {e}")))?;
                copy.extend_from_slice(bytes);
                Ok(copy)
            })
            .transpose()
            .map_err(|e| CodecError::Decode(format!("{err}; {e}")))?;
        let Some(mut hw) = self.hw.take() else { return Err(err) };
        log::warn!("{} failed ({err}); continuing with the software decoder", hw.name());
        filmcraft_codecs::hw::note_hw_fallback();
        for f in hw.flush() {
            if !self.emitted.contains(&f.pts) {
                self.carry.insert(f.pts, f);
            }
        }
        let mut sw = filmcraft_codecs::software_video_decoder(&self.entry).map_err(|e| CodecError::Decode(format!("{err}; no software decoder: {e}")))?;
        sw.set_draft(self.draft);
        self.sw = Some(sw);
        self.software_initialization = initialization;
        self.needs_software_initialization = self.software_initialization.is_some();
        self.initialize_software()?;
        drop(hw);
        let Some(log) = self.log.take() else {
            // Too far from a restart point to replay: the next seek restarts in software.
            self.carry.clear();
            return Err(CodecError::Decode(format!("{err} (continuing in software after the next seek)")));
        };
        let mut frames = Vec::new();
        let mut rejected = None;
        let last = log.len().saturating_sub(1);
        for (i, (s, p)) in log.iter().enumerate() {
            let Some(sw) = self.sw.as_mut() else { break };
            match sw.decode(s, *p) {
                Ok(out) => frames.extend(out),
                Err(e) if i == last => rejected = Some(e),
                Err(e) => log::warn!("software replay after the hardware decoder failed: {e}"),
            }
        }
        self.log = Some(log);
        match rejected {
            None => Ok(self.merge(frames)),
            Some(e) => {
                for f in frames {
                    if !self.emitted.contains(&f.pts) {
                        self.carry.insert(f.pts, f);
                    }
                }
                Err(e)
            }
        }
    }
}

impl VideoDecoder for HybridDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if self.sw.is_some() {
            self.initialize_software()?;
            let Some(sw) = self.sw.as_mut() else { return Err(CodecError::Decode("no software decoder".into())) };
            let frames = sw.decode(sample, pts)?;
            return Ok(self.merge(frames));
        }
        self.remember(sample, pts);
        if self.parameter_sets_changed(sample) {
            // The native owner may prepare a compatible change. Keep the original entry/info
            // and logged sample untouched so a later failure replays the software stream.
            let adoption = self.hw.as_mut().map_or(Ok(false), |hw| hw.try_adopt_parameter_change(sample));
            match adoption {
                Ok(true) => {}
                Ok(false) => return self.fall_back(CodecError::Decode("in-band parameter sets differ from the sample entry".into())),
                Err(e) => return self.fall_back(e),
            }
        }
        let Some(hw) = self.hw.as_mut() else {
            return Err(CodecError::Decode("no decoder".into()));
        };
        match hw.decode(sample, pts) {
            Ok(out) => {
                filmcraft_codecs::hw::note_hw_frames(out.len());
                self.note_emitted(&out);
                Ok(out)
            }
            Err(e) => self.fall_back(e),
        }
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        if let Some(hw) = self.hw.as_mut() {
            let out = hw.flush();
            filmcraft_codecs::hw::note_hw_frames(out.len());
            self.note_emitted(&out);
            return out;
        }
        let frames = self.sw.as_mut().map(|d| d.flush()).unwrap_or_default();
        let mut out = self.merge(frames);
        out.extend(std::mem::take(&mut self.carry).into_values());
        out
    }

    fn reset(&mut self) {
        if let Some(hw) = self.hw.as_mut() {
            hw.reset();
        }
        if let Some(sw) = self.sw.as_mut() {
            sw.reset();
            self.needs_software_initialization = self.software_initialization.is_some();
        }
        self.log = Some(Vec::new());
        self.log_bytes = 0;
        self.last_irap = 0;
        self.emitted.clear();
        self.carry.clear();
    }

    fn name(&self) -> &str {
        match (&self.hw, &self.sw) {
            (Some(hw), _) => hw.name(),
            (None, Some(sw)) => sw.name(),
            (None, None) => "hardware decoder",
        }
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        self.info.is_random_access(sample)
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.info.is_disposable(sample)
    }

    fn set_draft(&mut self, on: bool) {
        self.draft = on;
        if let Some(sw) = self.sw.as_mut() {
            sw.set_draft(on);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_codecs::hw::{FrameCodec, FrameStreamInfo};

    enum PrimerOutcome {
        Error,
        Picture,
    }

    struct ReplayProbe {
        initialization: Vec<u8>,
        outcome: PrimerOutcome,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl VideoDecoder for ReplayProbe {
        fn software_replay_initialization(&self) -> Option<&[u8]> {
            Some(&self.initialization)
        }
        fn decode(&mut self, _: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match self.outcome {
                PrimerOutcome::Error => Err(CodecError::Decode("initialization rejected (test)".into())),
                PrimerOutcome::Picture => Ok(vec![DecodedFrame { pts, frame: filmcraft_frame::VideoFrame::rgba8(1, 1, vec![0, 0, 0, 255]), draft: false }]),
            }
        }
        fn flush(&mut self) -> Vec<DecodedFrame> {
            Vec::new()
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "replay initialization probe"
        }
    }

    fn with_probe(initialization: Vec<u8>, outcome: PrimerOutcome) -> HybridDecoder {
        HybridDecoder::new(
            Box::new(ReplayProbe { initialization, outcome, calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)) }),
            SampleEntry::av1(filmcraft_isobmff::Av1Config::default(), 2, 2),
            FrameStreamInfo::for_tests(FrameCodec::Av1, 0, 8, (1, 1), (2, 2)),
        )
    }

    #[test]
    fn software_replay_initialization_cap_rejects_before_releasing_native_owner() {
        for bytes in [Vec::new(), vec![0; 65_537]] {
            let mut hybrid = with_probe(bytes, PrimerOutcome::Error);
            assert!(hybrid.fall_back(CodecError::Decode("native failure".into())).is_err());
            assert!(hybrid.is_hardware(), "invalid initialization must not discard native owner");
        }
    }

    #[test]
    fn software_replay_initialization_errors_and_pictures_cannot_be_silently_discarded() {
        for outcome in [PrimerOutcome::Error, PrimerOutcome::Picture] {
            let mut hybrid = with_probe(Vec::new(), PrimerOutcome::Error);
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            hybrid.sw = Some(Box::new(ReplayProbe { initialization: Vec::new(), outcome, calls: calls.clone() }));
            hybrid.software_initialization = Some(vec![0x0a, 0]);
            hybrid.needs_software_initialization = true;
            assert!(hybrid.initialize_software().is_err());
            assert!(hybrid.needs_software_initialization, "failed initialization cannot authorize sample replay");
            let before_reset = calls.load(std::sync::atomic::Ordering::Relaxed);
            hybrid.reset();
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), before_reset, "reset must defer fallible priming until decode");
            assert!(hybrid.needs_software_initialization);
            assert!(hybrid.decode(&[0x12, 0], 1).is_err(), "first decode after reset reports initialization failure");
        }
    }

    #[test]
    fn software_replay_malformed_av1_initialization_returns_error_without_output() {
        let mut hybrid = with_probe(vec![0x0a, 0xff], PrimerOutcome::Error); // Truncated OBU length.
        let error = hybrid.fall_back(CodecError::Decode("native failure".into()));
        assert!(error.is_err());
        assert!(hybrid.needs_software_initialization, "malformed initialization cannot authorize replay");
        assert!(hybrid.flush().is_empty());
    }
}

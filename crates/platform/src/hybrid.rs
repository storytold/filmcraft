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

/// A hardware decoder with a transparent software fallback.
pub struct HybridDecoder {
    hw: Option<Box<dyn VideoDecoder>>,
    sw: Option<Box<dyn VideoDecoder>>,
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

    /// Switch to the software decoder after `err`, replaying the log (which ends with the sample
    /// that failed). The replay goes on past samples the software decoder rejects, as decoding in
    /// software from the restart point would: one that the hardware had already decoded is logged
    /// (its call returned long ago), and if the failed sample itself is rejected its error is this
    /// call's, while the pictures replayed before it are kept for the next output.
    fn fall_back(&mut self, err: CodecError) -> Result<Vec<DecodedFrame>> {
        let Some(mut hw) = self.hw.take() else { return Err(err) };
        log::warn!("{} failed ({err}); continuing with the software decoder", hw.name());
        filmcraft_codecs::hw::note_hw_fallback();
        for f in hw.flush() {
            if !self.emitted.contains(&f.pts) {
                self.carry.insert(f.pts, f);
            }
        }
        drop(hw);
        let mut sw = filmcraft_codecs::software_video_decoder(&self.entry).map_err(|e| CodecError::Decode(format!("{err}; no software decoder: {e}")))?;
        sw.set_draft(self.draft);
        self.sw = Some(sw);
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
        if let Some(sw) = self.sw.as_mut() {
            let frames = sw.decode(sample, pts)?;
            return Ok(self.merge(frames));
        }
        self.remember(sample, pts);
        if self.parameter_sets_changed(sample) {
            return self.fall_back(CodecError::Decode("in-band parameter sets differ from the sample entry".into()));
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

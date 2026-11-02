//! Hardware output is published only after its input obligations are satisfied.
//! Unknown syntax, missing pictures or failed drains switch to software while restart input remains available.

mod replay;

use filmcraft_codecs::hw::{OutputInspector, StreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_isobmff::SampleEntry;

use replay::{Ledger, ReplayTail};

enum Run {
    Hardware { decoder: Box<dyn VideoDecoder>, ledger: Ledger },
    Software { decoder: Box<dyn VideoDecoder>, tail: ReplayTail },
    Unavailable,
}

enum Recovery {
    SelectSoftware,
    DrainHardware,
    AlreadyDrained,
}

/// Hardware decoding with bounded input accounting and transparent software recovery.
pub struct HybridDecoder {
    run: Run,
    entry: SampleEntry,
    inspector: OutputInspector,
    draft: bool,
}

impl HybridDecoder {
    /// hw: decoder for entry; unknown output intent selects software before hardware submission.
    pub fn new(hw: Box<dyn VideoDecoder>, entry: SampleEntry, info: impl Into<StreamInfo>) -> Self {
        filmcraft_codecs::hw::note_hw_session();
        Self {
            run: Run::Hardware { decoder: hw, ledger: Ledger::default() },
            entry,
            inspector: OutputInspector::new(info.into(), replay::MAX_BYTES),
            draft: false,
        }
    }

    pub fn is_hardware(&self) -> bool {
        matches!(self.run, Run::Hardware { .. })
    }

    fn software_decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let Run::Software { decoder, tail } = &mut self.run else { return Err(CodecError::Decode("no software decoder".into())) };
        tail.merge(decoder.decode(sample, pts)?)
    }

    fn software_flush(&mut self) -> Result<Vec<DecodedFrame>> {
        let Run::Software { decoder, tail } = &mut self.run else { return Err(CodecError::Decode("no software decoder".into())) };
        tail.finish(decoder.flush()?)
    }

    /// Unknown syntax uses software authority; classified hardware failure may salvage pending pixels.
    fn recover(
        &mut self,
        mut hw: Box<dyn VideoDecoder>,
        ledger: Ledger,
        error: CodecError,
        mut ready: Vec<DecodedFrame>,
        recovery: Recovery,
    ) -> Result<Vec<DecodedFrame>> {
        log::warn!("{}: {error}; continuing in software", hw.name());
        filmcraft_codecs::hw::note_hw_fallback();
        if matches!(recovery, Recovery::DrainHardware) {
            match hw.flush() {
                Ok(frames) => replay::retain(&mut ready, frames)?,
                Err(error) => log::warn!("{} drain failed ({error}); replaying retained input", hw.name()),
            }
        }
        drop(hw);
        let mut decoder =
            filmcraft_codecs::software_video_decoder(&self.entry).map_err(|cause| CodecError::Decode(format!("{error}; no software decoder: {cause}")))?;
        decoder.set_draft(self.draft);
        let (inputs, mut tail) = ledger.into_replay(ready)?;
        let result = (|| {
            let mut frames = Vec::new();
            let mut rejected = None;
            let last = inputs.len().saturating_sub(1);
            for (index, input) in inputs.iter().enumerate() {
                match decoder.decode(&input.data, input.pts) {
                    Ok(output) => replay::retain(&mut frames, output)?,
                    Err(error) if index == last => rejected = Some(error),
                    Err(error) => {
                        log::warn!("software replay reported an error while feeding PTS {}: {error}", input.pts);
                        if matches!(recovery, Recovery::SelectSoftware) && rejected.is_none() {
                            rejected = Some(error);
                        }
                    }
                }
            }
            match rejected {
                Some(error) => {
                    tail.stage(frames)?;
                    Err(error)
                }
                None => tail.merge(frames),
            }
        })();
        self.run = Run::Software { decoder, tail };
        result
    }
}

impl VideoDecoder for HybridDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if matches!(self.run, Run::Software { .. }) {
            return self.software_decode(sample, pts);
        }
        let run = std::mem::replace(&mut self.run, Run::Unavailable);
        let Run::Hardware { mut decoder, mut ledger } = run else { return Err(CodecError::Decode("no decoder".into())) };
        let plan = self.inspector.inspect(sample);
        let prior_pending = ledger.pending();
        let ambiguous_pts = ledger.ambiguous_pts(pts);
        if !ledger.remember(sample, pts, plan) {
            // Switch while bounded history is intact; do not disable recovery at the cap.
            let mut out =
                self.recover(decoder, ledger, CodecError::Decode("hardware replay input reached its bound".into()), Vec::new(), Recovery::DrainHardware)?;
            out.extend(self.software_decode(sample, pts)?);
            return Ok(out);
        }
        let needs_software = match plan {
            None => true,
            Some(plan) => plan.pictures > 1 || (plan.pictures != 0 && ambiguous_pts) || (plan.may_discard_prior && prior_pending),
        };
        if needs_software {
            // Unknown syntax can cancel earlier output; let software interpret it before publishing any drain.
            return self.recover(decoder, ledger, CodecError::Decode("input needs software output accounting".into()), Vec::new(), Recovery::SelectSoftware);
        }
        ledger.submit();
        match decoder.decode(sample, pts) {
            Ok(frames) => {
                filmcraft_codecs::hw::note_hw_frames(frames.len());
                if !ledger.publish(&frames, false) {
                    return self.recover(
                        decoder,
                        ledger,
                        CodecError::Decode("hardware output did not satisfy presentation-order input obligations".into()),
                        frames,
                        Recovery::DrainHardware,
                    );
                }
                self.run = Run::Hardware { decoder, ledger };
                Ok(frames)
            }
            Err(error) => self.recover(decoder, ledger, error, Vec::new(), Recovery::DrainHardware),
        }
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        if matches!(self.run, Run::Software { .. }) {
            return self.software_flush();
        }
        let run = std::mem::replace(&mut self.run, Run::Unavailable);
        let Run::Hardware { mut decoder, mut ledger } = run else { return Err(CodecError::Decode("no decoder".into())) };
        let recovered = match decoder.flush() {
            Ok(frames) => {
                filmcraft_codecs::hw::note_hw_frames(frames.len());
                if ledger.publish(&frames, true) {
                    self.run = Run::Hardware { decoder, ledger };
                    return Ok(frames);
                }
                self.recover(
                    decoder,
                    ledger,
                    CodecError::Decode("hardware drain left unresolved picture obligations".into()),
                    frames,
                    Recovery::AlreadyDrained,
                )?
            }
            Err(error) => self.recover(decoder, ledger, error, Vec::new(), Recovery::AlreadyDrained)?,
        };
        let mut out = recovered;
        out.extend(self.software_flush()?);
        Ok(out)
    }

    fn reset(&mut self) {
        self.inspector.reset();
        match &mut self.run {
            Run::Hardware { decoder, ledger } => {
                decoder.reset();
                *ledger = Ledger::default();
            }
            Run::Software { decoder, tail } => {
                decoder.reset();
                *tail = ReplayTail::default();
            }
            Run::Unavailable => {}
        }
    }

    fn name(&self) -> &str {
        match &self.run {
            Run::Hardware { decoder, .. } | Run::Software { decoder, .. } => decoder.name(),
            Run::Unavailable => "hardware decoder",
        }
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        self.inspector.stream().is_random_access(sample)
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.inspector.stream().is_disposable(sample)
    }

    fn set_draft(&mut self, on: bool) {
        self.draft = on;
        if let Run::Software { decoder, .. } = &mut self.run {
            decoder.set_draft(on);
        }
    }
}

#[cfg(test)]
#[path = "hybrid/tests.rs"]
mod tests;

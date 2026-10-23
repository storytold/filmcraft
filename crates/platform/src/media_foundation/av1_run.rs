//! The empty-run AV1 metadata adoption contract, independent of COM and GPU state.

use filmcraft_codecs::Result;
use filmcraft_codecs::hw::{Av1ParameterDifference, FrameStreamInfo};

/// Active native interpretation, distinct from Hybrid's immutable replay configuration.
pub(super) struct Av1RunMetadata {
    active: FrameStreamInfo,
    adoption_open: bool,
}

impl Av1RunMetadata {
    pub(super) fn new(active: FrameStreamInfo) -> Self {
        Self { active, adoption_open: true }
    }

    pub(super) fn active(&self) -> &FrameStreamInfo {
        &self.active
    }

    /// Guard every sample against the active header, even when Hybrid sees no change against
    /// its original av1C. Only an initial checked colour restart in an empty native run may
    /// change active metadata. The native Plan/Spec and original container context must both
    /// permit a candidate through `eligible` before metadata is committed.
    ///
    /// `true` also permits unchanged headers and headerless continuation; neither authorizes
    /// a new adoption or calls `eligible`. The owner closes adoption before submitting input.
    pub(super) fn prepare(&mut self, sample: &[u8], pending: usize, eligible: impl FnOnce(&FrameStreamInfo) -> bool) -> Result<bool> {
        match self.active.av1_parameter_difference(sample)? {
            Av1ParameterDifference::Unchanged => return Ok(true),
            Av1ParameterDifference::ColorDescription { .. } if self.adoption_open && pending == 0 => {}
            _ => return Ok(false),
        }
        let Some(candidate) = self.active.initial_av1_color_candidate(sample)? else {
            return Ok(false);
        };
        if !eligible(&candidate) {
            return Ok(false);
        }
        self.active = candidate;
        // Repeated calls for the accepted first sample stay idempotent; another colour cannot
        // replace it, even before its first native input has been submitted.
        self.close_adoption();
        Ok(true)
    }

    /// Close before any native submission (including headerless input) and before drain.
    pub(super) fn close_adoption(&mut self) {
        self.adoption_open = false;
    }

    /// Reopen only after a successful reset discarded native references/output, or a fresh
    /// MFT will be created before input. An empty pending list or failed flush alone is no proof.
    pub(super) fn restart_empty(&mut self) {
        self.adoption_open = true;
    }
}

#[cfg(test)]
#[path = "av1_run_tests.rs"]
mod tests;

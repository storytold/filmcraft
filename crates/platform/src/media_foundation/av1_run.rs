//! The empty-run AV1 metadata adoption contract, independent of COM and GPU state.

use filmcraft_codecs::Result;
use filmcraft_codecs::hw::{Av1ParameterDifference, Av1ReplayConfigurationRefresh, FrameStreamInfo};

/// Active native interpretation, distinct from Hybrid's immutable replay configuration.
#[derive(Clone, Debug)]
pub struct Av1RunMetadata {
    active: FrameStreamInfo,
    adoption_open: bool,
    ever_adopted: bool,
}

impl Av1RunMetadata {
    pub(super) fn new(active: FrameStreamInfo) -> Self {
        Self { active, adoption_open: true, ever_adopted: false }
    }

    pub(super) fn active(&self) -> &FrameStreamInfo {
        &self.active
    }

    pub(super) fn software_replay_initialization(&self) -> Option<&[u8]> {
        self.ever_adopted.then_some(self.active.config_obus.as_slice())
    }

    /// Guard every sample against the active header, even when Hybrid sees no change against
    /// its original av1C. Only an initial checked colour restart in an empty native run may
    /// change active metadata. The native Plan/Spec and original container context must both
    /// permit a candidate through `eligible` before metadata is committed.
    ///
    /// `true` also permits unchanged format/colour and headerless continuation. After adoption,
    /// a changed coding-tool sequence must pass the supported independent-restart proof so replay
    /// retains current configuration. This refresh never changes colour, calls `eligible`, or
    /// reopens adoption. The owner closes adoption before submitting input.
    pub(super) fn prepare(&mut self, sample: &[u8], pending: usize, eligible: impl FnOnce(&FrameStreamInfo) -> bool) -> Result<bool> {
        match self.active.av1_parameter_difference(sample)? {
            Av1ParameterDifference::Unchanged => {
                if self.ever_adopted {
                    match self.active.av1_replay_configuration_refresh(sample)? {
                        Av1ReplayConfigurationRefresh::NotNeeded => {}
                        Av1ReplayConfigurationRefresh::Compatible(candidate) => self.active = candidate,
                        Av1ReplayConfigurationRefresh::Unsupported => return Ok(false),
                    }
                }
                return Ok(true);
            }
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
        self.ever_adopted = true;
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

//! Native Premiere project and effect-preset import, observed from user-authored files.
//!
//! Both formats contain a PremiereData v3 object graph. Projects may wrap that XML in gzip.
//! This module reads data files only; it does not load Adobe plug-ins or copy their code.
//! Unknown effects and approximated interpolation are named in the returned report.

mod effects;
mod graph;
mod presets;
mod project;

use filmcraft_project::EffectInstance;
use filmcraft_time::Tick;

pub use presets::import_presets;
pub use project::import_project;

/// How the original preset places its keyframes when applied to another clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetTiming {
    Scale,
    AnchorToIn,
    AnchorToOut,
}

/// A format-neutral effect preset. The engine maps this into its persistent preset library.
#[derive(Clone, Debug)]
pub struct ImportedPreset {
    pub name: String,
    pub description: String,
    pub timing: PresetTiming,
    pub source_duration: Tick,
    pub source_size: (u32, u32),
    pub effects: Vec<EffectInstance>,
    /// Saved native transition duration; a transition is applied at an edit point, not as a clip filter.
    pub transition_duration: Option<Tick>,
}

/// Native documents are bounded before decompression and before parsing their object graph.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
/// Leave headroom for subsequent edit and parameter interpolation arithmetic.
pub(super) const MAX_TIME_TICKS: i64 = i64::MAX / 4;

pub(crate) fn sniff(bytes: &[u8]) -> bool {
    use std::io::Read;
    let mut prefix = Vec::new();
    if bytes.starts_with(&[0x1f, 0x8b]) {
        if flate2::read::GzDecoder::new(bytes).take(4096).read_to_end(&mut prefix).is_err() {
            return false;
        }
    } else {
        prefix.extend_from_slice(bytes.get(..bytes.len().min(4096)).unwrap_or_default());
    }
    let text = String::from_utf8_lossy(&prefix);
    text.contains("<PremiereData") && text.contains("<Project ")
}

//! Persistent workflow records. MIT OR Apache-2.0.
//!
//! Import records describe the immutable input, not the current edit. Relinking updates current
//! media references without rewriting these records. Their presence requires the workflow file tag.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub version: u32,
    #[serde(default)]
    pub imports: Vec<std::sync::Arc<PremiereImport>>,
}
impl Default for Workflow {
    fn default() -> Self {
        Self { version: 1, imports: Vec::new() }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PremiereImport {
    pub source_path: String,
    pub source_sha256: String,
    pub snapshot_path: String,
    pub media_mode: String,
    pub media: Vec<SourceMedia>,
    pub source_tree: serde_json::Value,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceMedia {
    pub original_path: String,
    pub imported_path: String,
    /// Import-time digest, present for copied media (and older linked imports). New linked
    /// imports defer it; later inspection must not backfill a hash observed at a different time.
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    /// Import-time size/head/tail fingerprint, not a full-file cryptographic digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<crate::MediaIdentity>,
}

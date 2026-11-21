//! Application-neutral identity of an externally rendered project object.
use serde::{Deserialize, Serialize};

/// The item remains the same when its disposable render cache changes. Executable paths are
/// deliberately absent: opening an untrusted project must never choose a program to execute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectLink {
    pub version: u32,
    pub provider: String,
    pub project_path: String,
    /// Provider-owned stable identifier (composition, document, artboard…).
    pub object: String,
    pub object_name: String,
    /// Last successful full-quality render revision. Empty until one completes.
    pub revision: String,
    /// Explicit refresh invalidates environmental inputs (for example changed system fonts).
    #[serde(default)]
    pub cache_generation: u64,
    #[serde(default)]
    pub baked: bool,
}

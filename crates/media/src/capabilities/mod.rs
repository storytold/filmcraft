//! Portable media V1 capability descriptions.
//!
//! Available means an implementation candidate. Exact stream/export
//! validation is still required. Listed formats are a confirmed V1 subset,
//! not a promise that every unlisted configuration is unsupported.
//! None means unknown; Some(empty) means a known empty set.
//! Deserialize only through decode_snapshot when reading untrusted JSON.

mod validation;
mod wire;
pub use wire::{decode_snapshot, encode_snapshot};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_GPUS: usize = 32;
pub const MAX_CAPABILITIES: usize = 4096;
pub const MAX_FORMATS: usize = 128;
pub const MAX_ISSUES: usize = 128;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_NAME_BYTES: usize = 512;
pub const MAX_REASON_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Decode,
    Encode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccelerationMode {
    Software,
    Hardware,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoProfile {
    H264ConstrainedBaseline,
    H264Main,
    H264High,
    HevcMain,
    HevcMain10,
    Av1Main,
}

impl VideoProfile {
    pub fn codec(self) -> VideoCodec {
        match self {
            Self::H264ConstrainedBaseline | Self::H264Main | Self::H264High => VideoCodec::H264,
            Self::HevcMain | Self::HevcMain10 => VideoCodec::Hevc,
            Self::Av1Main => VideoCodec::Av1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromaSampling {
    C420,
    C422,
    C444,
    Monochrome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrMode {
    Sdr,
    Pq,
    Hlg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderControl {
    Cbr,
    VbrOnePass,
    VbrTwoPass,
    KeyframeInterval,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Available,
    NotImplemented,
    Unavailable,
    Unsupported,
    Unknown,
    QueryFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEvidence {
    ImplementationCoverage,
    ApiQuery,
    SessionValidation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryState {
    NotStarted,
    Checking,
    Complete,
    Partial,
    Failed,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuKind {
    Integrated,
    Discrete,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InclusiveRange {
    pub min: u32,
    pub max: u32,
    pub step: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RationalRate {
    pub num: u32,
    pub den: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityLimits {
    pub width: Option<InclusiveRange>,
    pub height: Option<InclusiveRange>,
    pub max_frame_rate: Option<RationalRate>,
    pub progressive_only: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatCapability {
    pub profile: VideoProfile,
    pub bit_depth: u8,
    pub chroma: ChromaSampling,
    pub limits: CapabilityLimits,
    pub hdr_modes: Option<Vec<HdrMode>>,
    pub encoder_controls: Option<Vec<EncoderControl>>,
}

impl FormatCapability {
    pub fn new(profile: VideoProfile, bit_depth: u8) -> Self {
        Self { profile, bit_depth, chroma: ChromaSampling::C420, limits: CapabilityLimits::default(), hdr_modes: None, encoder_controls: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodecCapability {
    pub codec: VideoCodec,
    pub operation: Operation,
    pub backend_id: String,
    pub mode: AccelerationMode,
    pub adapter_id: Option<String>,
    pub status: CapabilityStatus,
    pub evidence: Vec<CapabilityEvidence>,
    pub formats: Vec<FormatCapability>,
    pub requires_configuration_validation: bool,
    pub reason: Option<String>,
}

impl CodecCapability {
    pub fn software(codec: VideoCodec, operation: Operation, backend_id: &str, formats: Vec<FormatCapability>) -> Self {
        Self {
            codec,
            operation,
            backend_id: backend_id.into(),
            mode: AccelerationMode::Software,
            adapter_id: None,
            status: CapabilityStatus::Available,
            evidence: vec![CapabilityEvidence::ImplementationCoverage],
            formats,
            requires_configuration_validation: true,
            reason: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuInfo {
    pub model: Option<String>,
    pub architecture: Option<String>,
    pub logical_processors: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuInfo {
    pub id: String,
    pub name: Option<String>,
    pub vendor_id: Option<u32>,
    pub device_id: Option<u32>,
    pub driver: Option<String>,
    pub is_software: bool,
    pub kind: GpuKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryIssue {
    pub backend_id: Option<String>,
    pub adapter_id: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySnapshot {
    pub schema_version: u32,
    pub generation: u64,
    pub discovery: DiscoveryState,
    pub cpu: Option<CpuInfo>,
    pub gpus: Vec<GpuInfo>,
    pub capabilities: Vec<CodecCapability>,
    pub issues: Vec<DiscoveryIssue>,
    pub truncated: bool,
}

impl CapabilitySnapshot {
    pub fn new(generation: u64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            generation,
            discovery: DiscoveryState::NotStarted,
            cpu: None,
            gpus: Vec::new(),
            capabilities: Vec::new(),
            issues: Vec::new(),
            truncated: false,
        }
    }
}

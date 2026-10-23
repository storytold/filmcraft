use super::*;
use crate::{MediaError, Result};
use std::collections::BTreeSet;

fn invalid(field: &'static str) -> MediaError {
    MediaError::Unsupported(format!("invalid media capabilities: {field}"))
}

fn text(value: &str, max: usize, field: &'static str) -> Result<()> {
    if value.len() > max || value.trim().is_empty() {
        return Err(invalid(field));
    }
    Ok(())
}

fn identifier(value: &str, field: &'static str) -> Result<()> {
    text(value, MAX_ID_BYTES, field)?;
    if !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)) {
        return Err(invalid(field));
    }
    Ok(())
}

fn optional_text(value: &Option<String>, max: usize, field: &'static str) -> Result<()> {
    if let Some(value) = value {
        text(value, max, field)?;
    }
    Ok(())
}

fn unique<T: Ord>(values: &[T], max: usize, field: &'static str) -> Result<()> {
    if values.len() > max {
        return Err(invalid(field));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(invalid(field));
        }
    }
    Ok(())
}

impl CapabilityLimits {
    /// Reject invalid dimension ranges and zero frame-rate numerators or denominators.
    /// Unknown limits remain valid and do not establish configuration support.
    pub fn validate(&self) -> Result<()> {
        for range in [self.width, self.height].into_iter().flatten() {
            if range.min == 0 || range.max < range.min || range.step == 0 {
                return Err(invalid("dimension range"));
            }
        }
        if self.max_frame_rate.is_some_and(|r| r.num == 0 || r.den == 0) {
            return Err(invalid("frame rate"));
        }
        Ok(())
    }
}

impl CodecCapability {
    /// Check identifiers, availability evidence, adapter rules and the V1 format subset.
    /// This validates the description, not whether a particular codec session can run.
    pub fn validate(&self) -> Result<()> {
        identifier(&self.backend_id, "backend_id")?;
        if let Some(id) = &self.adapter_id {
            identifier(id, "adapter_id")?;
        }
        optional_text(&self.reason, MAX_REASON_BYTES, "reason")?;
        if self.mode == AccelerationMode::Software && self.adapter_id.is_some() {
            return Err(invalid("software adapter association"));
        }
        unique(&self.evidence, 3, "evidence")?;
        if self.status == CapabilityStatus::Available {
            if self.evidence.is_empty() || !self.requires_configuration_validation {
                return Err(invalid("available implementation evidence/validation"));
            }
            if self.formats.is_empty() {
                return Err(invalid("available formats"));
            }
            if self.mode == AccelerationMode::Hardware && self.adapter_id.is_none() {
                return Err(invalid("available hardware adapter"));
            }
        } else if self.reason.is_none() {
            return Err(invalid("unavailable/unverified reason"));
        }
        if self.status == CapabilityStatus::NotImplemented && !self.formats.is_empty() {
            return Err(invalid("unimplemented formats"));
        }
        if self.formats.len() > MAX_FORMATS {
            return Err(invalid("format count"));
        }
        let mut formats = BTreeSet::new();
        for format in &self.formats {
            let key = (format.profile, format.bit_depth, format.chroma);
            if !formats.insert(key) {
                return Err(invalid("duplicate format"));
            }
            if format.profile.codec() != self.codec {
                return Err(invalid("codec/profile"));
            }
            let depth_ok = match format.profile {
                VideoProfile::H264ConstrainedBaseline | VideoProfile::H264Main | VideoProfile::H264High | VideoProfile::HevcMain => format.bit_depth == 8,
                VideoProfile::HevcMain10 | VideoProfile::Av1Main => matches!(format.bit_depth, 8 | 10),
            };
            if !depth_ok || format.chroma != ChromaSampling::C420 {
                return Err(invalid("V1 profile/bit depth/chroma"));
            }
            format.limits.validate()?;
            if let Some(modes) = &format.hdr_modes {
                unique(modes, 3, "HDR modes")?;
            }
            if let Some(controls) = &format.encoder_controls {
                unique(controls, 4, "encoder controls")?;
                if self.operation == Operation::Decode && !controls.is_empty() {
                    return Err(invalid("decoder encoder controls"));
                }
            }
        }
        Ok(())
    }
}

impl CapabilitySnapshot {
    /// Check the schema, record bounds, discovery consistency and adapter associations.
    /// This validates structure and relationships, not the authenticity of reported support.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid("schema_version"));
        }
        if self.gpus.len() > MAX_GPUS || self.capabilities.len() > MAX_CAPABILITIES || self.issues.len() > MAX_ISSUES {
            return Err(invalid("snapshot record count"));
        }
        if self.truncated && self.issues.is_empty() {
            return Err(invalid("truncation reason"));
        }
        if self.truncated && !matches!(self.discovery, DiscoveryState::Partial | DiscoveryState::Failed | DiscoveryState::Stale) {
            return Err(invalid("truncated discovery state"));
        }
        if matches!(self.discovery, DiscoveryState::Partial | DiscoveryState::Failed | DiscoveryState::Stale) && self.issues.is_empty() {
            return Err(invalid("discovery failure reason"));
        }
        if let Some(cpu) = &self.cpu {
            optional_text(&cpu.model, MAX_NAME_BYTES, "CPU model")?;
            optional_text(&cpu.architecture, MAX_NAME_BYTES, "CPU architecture")?;
            if cpu.logical_processors == Some(0) {
                return Err(invalid("CPU parallelism"));
            }
        }
        let mut gpu_ids = BTreeSet::new();
        for gpu in &self.gpus {
            identifier(&gpu.id, "GPU id")?;
            if !gpu_ids.insert(gpu.id.as_str()) {
                return Err(invalid("duplicate GPU id"));
            }
            optional_text(&gpu.name, MAX_NAME_BYTES, "GPU name")?;
            optional_text(&gpu.driver, MAX_NAME_BYTES, "GPU driver")?;
            if gpu.is_software && gpu.kind != GpuKind::Unknown {
                return Err(invalid("software GPU physical classification"));
            }
        }
        let mut capability_ids = BTreeSet::new();
        for capability in &self.capabilities {
            capability.validate()?;
            let key = (capability.codec, capability.operation, capability.mode, capability.backend_id.as_str(), capability.adapter_id.as_deref());
            if !capability_ids.insert(key) {
                return Err(invalid("duplicate capability"));
            }
            if let Some(id) = capability.adapter_id.as_deref() {
                let gpu = self.gpus.iter().find(|g| g.id == id).ok_or_else(|| invalid("unknown GPU id"))?;
                if gpu.is_software && capability.status == CapabilityStatus::Available {
                    return Err(invalid("software adapter hardware acceleration"));
                }
            }
        }
        for issue in &self.issues {
            if let Some(id) = &issue.backend_id {
                identifier(id, "issue backend_id")?;
            }
            if let Some(id) = &issue.adapter_id {
                identifier(id, "issue adapter_id")?;
            }
            text(&issue.reason, MAX_REASON_BYTES, "issue reason")?;
        }
        Ok(())
    }
}

//! Narrow offline profiles derived from live, identity-matched AVB source descriptors.
//! No filesystem presence or decoder success is implied by this metadata.
use crate::{EngineError, Result};
use filmcraft_interchange::avid_catalog::full_identity;
use filmcraft_media::VideoStreamInfo;
use filmcraft_time::{FrameRate, Tick};
use serde_json::Value;
fn bad(s: &str) -> EngineError {
    EngineError::Other(format!("Avid offline media: {s}"))
}
pub fn descriptor(evidence: &Value, id: [u8; 32]) -> Result<&Value> {
    let entries = evidence["source_descriptors"].as_array().filter(|v| v.len() <= 64).ok_or_else(|| bad("no validated source descriptors"))?;
    let mut matching = entries.iter().filter(|d| full_identity(&d["id"]).ok() == Some(id));
    let d = &matching.next().ok_or_else(|| bad("no unique live source descriptor for missing media"))?["descriptor"];
    if matching.next().is_some() || d["mob_kind"] != 1 || d["is_omfi"] != 0 || d["locator"]["class"] != "MSML" {
        return Err(bad("unsupported or ambiguous managed source descriptor"));
    }
    let locator = &d["locator"]["properties"]["mob_id"];
    if full_identity(&serde_json::json!({"raw":locator["id_raw"],"byte_order":locator["byte_order"]})).ok() != Some(id) {
        return Err(bad("descriptor locator identity disagrees with source"));
    }
    Ok(d)
}
fn integer(v: &Value) -> Option<u32> {
    v.as_f64().filter(|n| n.is_finite() && *n >= 1.0 && *n <= f64::from(u32::MAX) && n.fract() == 0.0).map(|n| n as u32)
}
pub fn audio_rate(d: &Value, start: Tick, length: Tick) -> Result<u32> {
    let rate = integer(&d["sample_rate"]).filter(|r| (8000..=384000).contains(r)).ok_or_else(|| bad("unsupported audio sample rate"))?;
    let bits = d["quantization_bits"].as_u64().filter(|n| matches!(*n, 16 | 24 | 32)).ok_or_else(|| bad("unsupported PCM sample size"))?;
    if d["class"] != "PCMA"
        || d["channels"] != 1
        || d["coding_format"] != 1
        || integer(&d["edit_rate"]) != Some(rate)
        || d["block_align"].as_u64() != Some(bits / 8)
        || d["average_bps"].as_u64() != Some(u64::from(rate) * bits / 8)
    {
        return Err(bad("offline audio requires a complete mono PCM descriptor"));
    }
    let samples = d["length"].as_i64().filter(|n| *n >= 0).ok_or_else(|| bad("invalid audio descriptor duration"))?;
    let end = start.0.checked_add(length.0).filter(|n| *n >= 0 && *n <= Tick::MAX.0).ok_or_else(|| bad("audio source range overflow"))?;
    if i128::from(end) * i128::from(rate) > i128::from(samples) * i128::from(filmcraft_time::TICKS_PER_SECOND) {
        return Err(bad("audio range exceeds descriptor duration"));
    }
    Ok(rate)
}
pub fn picture(d: &Value, r: &Value, evidence: &Value, track: u32) -> Result<VideoStreamInfo> {
    let proof = &evidence["picture_rate_evidence"];
    let recorded = d["edit_rate"].as_f64();
    let frame_rate = if recorded == Some(24.0) {
        FrameRate::FPS_24
    } else if recorded == Some(29.97)
        && proof["basis"] == "automatic_adapter"
        && proof["rate"]["numerator"] == 30000
        && proof["rate"]["denominator"] == 1001
        && proof["source_track"].as_u64() == Some(u64::from(track))
    {
        FrameRate::new(30000, 1001)
    } else {
        return Err(bad("no validated exact picture clock for offline import"));
    };
    if d["class"] != "CDCI"
        || d["frame_layout"] != 0
        || d["component_width"] != 8
        || d["horizontal_subsampling"] != 2
        || d["vertical_subsampling"] != 1
        || d["alpha_sampled_width"] != 0
        || !filmcraft_interchange::avid_catalog::picture_rate_matches(r, frame_rate)
    {
        return Err(bad("unsupported offline picture descriptor"));
    }
    // Only the two observed display aspects are validated for this raster.
    let par = if d["aspect_ratio"] == serde_json::json!([16, 9]) {
        (1, 1)
    } else if d["aspect_ratio"] == serde_json::json!([149, 84]) {
        (447, 448)
    } else {
        return Err(bad("unsupported offline picture aspect ratio"));
    };
    for prefix in ["stored", "sampled", "display"] {
        if d[format!("{prefix}_width")] != 1920 || d[format!("{prefix}_height")] != 1080 {
            return Err(bad("unsupported offline picture raster"));
        }
    }
    for field in ["sampled_x_offset", "sampled_y_offset", "display_x_offset", "display_y_offset"] {
        if d[field] != 0 {
            return Err(bad("unsupported offline picture aperture offset"));
        }
    }
    let start = r["source_start"].as_u64().ok_or_else(|| bad("invalid picture source offset"))?;
    let length = r["length"].as_u64().ok_or_else(|| bad("invalid picture source length"))?;
    let available = d["length"].as_u64().ok_or_else(|| bad("invalid picture descriptor duration"))?;
    if start.checked_add(length).is_none_or(|n| n > available) {
        return Err(bad("picture range exceeds descriptor duration"));
    }
    Ok(VideoStreamInfo {
        width: 1920,
        height: 1080,
        frame_rate,
        par,
        codec: "Avid picture (unverified)".into(),
        pixel_format: "YUV 4:2:2 8-bit".into(),
        color: filmcraft_color::ColorInfo::REC709,
        has_alpha: false,
        bitrate: None,
        hdr: None,
    })
}

//! Link metadata stored on a FilmCraft media item.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One link from a FilmCraft media item to an EffectCraft comp.
///
/// Unknown JSON fields stay in [`LinkRecord::extra`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkRecord {
    pub v: u32,
    pub link_id: String,
    pub target_app: String,
    pub target_project: String,
    pub target_comp: String,
    pub nested_sequence: String,
    pub result_path: String,
    pub result_epoch: u64,
    pub format: Format,
    pub created: String,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

/// Size, rate, duration, and alpha of the linked result.
///
/// Unknown JSON fields stay in [`Format::extra`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Format {
    pub w: u64,
    pub h: u64,
    pub rate_num: u64,
    pub rate_den: u64,
    pub duration_ticks: u64,
    pub alpha: bool,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

impl LinkRecord {
    /// Return an error when a field breaks the link record rules.
    ///
    /// Deserialization can accept a record this check rejects.
    pub fn validate(&self) -> Result<(), String> {
        if self.v != 1 {
            return Err(format!("v is {}, want 1", self.v));
        }
        if !valid_uuid(&self.link_id) {
            return Err("link_id is not a uuid".to_string());
        }
        if self.target_app != "effectcraft" {
            return Err(format!("target_app is {:?}, want effectcraft", self.target_app));
        }
        require_nonempty("target_project", &self.target_project)?;
        require_nonempty("target_comp", &self.target_comp)?;
        require_nonempty("nested_sequence", &self.nested_sequence)?;
        require_nonempty("result_path", &self.result_path)?;
        require_at_least_one("format.w", self.format.w)?;
        require_at_least_one("format.h", self.format.h)?;
        require_at_least_one("format.rate_num", self.format.rate_num)?;
        require_at_least_one("format.rate_den", self.format.rate_den)?;
        if !valid_rfc3339(&self.created) {
            return Err("created is not an RFC 3339 date-time".to_string());
        }
        Ok(())
    }
}

fn require_nonempty(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() { Err(format!("{name} is empty")) } else { Ok(()) }
}

fn require_at_least_one(name: &str, value: u64) -> Result<(), String> {
    if value >= 1 { Ok(()) } else { Err(format!("{name} must be >= 1")) }
}

fn valid_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let hyphen = matches!(index, 8 | 13 | 18 | 23);
        if hyphen && *byte != b'-' {
            return false;
        }
        if !hyphen && !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// `YYYY-MM-DDTHH:MM:SS` with an optional fraction and a `Z` or numeric offset.
fn valid_rfc3339(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let (Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (two_digits(bytes, 5), two_digits(bytes, 8), two_digits(bytes, 11), two_digits(bytes, 14), two_digits(bytes, 17))
    else {
        return false;
    };
    if !four_digits(bytes, 0)
        || byte_at(bytes, 4) != Some(b'-')
        || byte_at(bytes, 7) != Some(b'-')
        || !matches!(byte_at(bytes, 10), Some(b'T' | b't'))
        || byte_at(bytes, 13) != Some(b':')
        || byte_at(bytes, 16) != Some(b':')
    {
        return false;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    let mut index = 19;
    if byte_at(bytes, index) == Some(b'.') {
        index += 1;
        let start = index;
        while byte_at(bytes, index).is_some_and(|byte| byte.is_ascii_digit()) {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    match byte_at(bytes, index) {
        Some(b'Z' | b'z') => index.checked_add(1) == Some(bytes.len()),
        Some(b'+' | b'-') => index.checked_add(1).is_some_and(|next| numeric_offset(bytes, next)),
        _ => false,
    }
}

fn numeric_offset(bytes: &[u8], index: usize) -> bool {
    let Some(end) = index.checked_add(5) else {
        return false;
    };
    let Some(colon) = index.checked_add(2) else {
        return false;
    };
    let Some(minutes) = index.checked_add(3) else {
        return false;
    };
    if bytes.len() != end || byte_at(bytes, colon) != Some(b':') {
        return false;
    }
    let (Some(hour), Some(minute)) = (two_digits(bytes, index), two_digits(bytes, minutes)) else {
        return false;
    };
    hour <= 23 && minute <= 59
}

fn four_digits(bytes: &[u8], index: usize) -> bool {
    let Some(end) = index.checked_add(4) else {
        return false;
    };
    bytes.get(index..end).is_some_and(|slice| slice.iter().all(u8::is_ascii_digit))
}

fn two_digits(bytes: &[u8], index: usize) -> Option<u16> {
    let end = index.checked_add(2)?;
    let slice = bytes.get(index..end)?;
    let hi = slice.first().copied()?;
    let lo = slice.get(1).copied()?;
    if !hi.is_ascii_digit() || !lo.is_ascii_digit() {
        return None;
    }
    Some(u16::from(hi - b'0') * 10 + u16::from(lo - b'0'))
}

fn byte_at(bytes: &[u8], index: usize) -> Option<u8> {
    bytes.get(index).copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;

    fn sample() -> LinkRecord {
        LinkRecord {
            v: 1,
            link_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            target_app: "effectcraft".to_string(),
            target_project: "links/shot.ecproj".to_string(),
            target_comp: "comp-1".to_string(),
            nested_sequence: "item-9".to_string(),
            result_path: "Project Links/550e8400-e29b-41d4-a716-446655440000.mov".to_string(),
            result_epoch: 7,
            format: Format { w: 1920, h: 1080, rate_num: 24, rate_den: 1, duration_ticks: 240, alpha: true, extra: Map::new() },
            created: "2026-10-11T15:04:05Z".to_string(),
            extra: Map::new(),
        }
    }

    #[test]
    fn schema_accepts_a_record_and_rejects_a_bad_token() {
        let record = sample();
        record.validate().expect("sample record");
        let round_trip: LinkRecord = serde_json::from_value(serde_json::to_value(&record).expect("serialize")).expect("deserialize");
        assert_eq!(round_trip, record);

        let mut value = serde_json::to_value(&record).expect("serialize");
        value["badge"] = Value::String("kept".into());
        value["format"]["tint"] = json!("warm");
        let restored: LinkRecord = serde_json::from_value(value).expect("unknown fields deserialize");
        assert_eq!(restored.extra.get("badge").and_then(Value::as_str), Some("kept"));
        assert_eq!(restored.format.extra.get("tint").and_then(Value::as_str), Some("warm"));
        restored.validate().expect("unknown fields stay valid");

        let serialized = serde_json::to_value(&restored).expect("serialize restored");
        assert_eq!(serialized.get("badge").and_then(Value::as_str), Some("kept"));
        assert_eq!(serialized["format"].get("tint").and_then(Value::as_str), Some("warm"));

        let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../../crates/ipc/schemas/link-record.schema.json");
        let shown = schema_path.display().to_string();
        assert!(schema_path.is_file(), "schema file missing: {shown}");
        let schema: Value = serde_json::from_slice(&std::fs::read(&schema_path).expect("read schema")).expect("parse schema");
        let validator = jsonschema::options().should_validate_formats(true).build(&schema).expect("compile schema");
        let errors: Vec<_> = validator.iter_errors(&serialized).map(|err| err.to_string()).collect();
        assert!(errors.is_empty(), "{errors:?}");

        let mut bad_app = serialized.clone();
        // The schema takes any app id (a producer can be any Craft app). It still refuses a malformed one.
        bad_app["target_app"] = Value::String("Film_Craft".into());
        assert!(!validator.is_valid(&bad_app));
        let mut other_app = serialized.clone();
        other_app["target_app"] = Value::String("filmcraft".into());
        assert!(validator.is_valid(&other_app), "the contract allows any producer app");
        let other: LinkRecord = serde_json::from_value(other_app).expect("deserializes");
        assert!(other.validate().is_err(), "this build links to EffectCraft only");

        let mut bad_v = serialized.clone();
        bad_v["v"] = json!(2);
        let bad: LinkRecord = serde_json::from_value(bad_v).expect("v 2 still deserializes");
        assert!(bad.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_fields() {
        let mut upper = sample();
        upper.link_id = "550E8400-E29B-41D4-A716-446655440000".to_string();
        upper.validate().expect("upper hex uuid");

        let mut zeros = sample();
        zeros.result_epoch = 0;
        zeros.format.duration_ticks = 0;
        zeros.validate().expect("zero epoch and duration");

        for created in ["2026-10-11T15:04:05.123Z", "2026-10-11T15:04:05+05:30", "2026-10-11T15:04:05-00:00", "2026-10-11t15:04:05z"] {
            let mut record = sample();
            record.created = created.to_string();
            record.validate().expect(created);
        }

        let mut bad_v = sample();
        bad_v.v = 2;
        bad_v.validate().expect_err("v 2");

        let mut bad_id = sample();
        bad_id.link_id = "not-a-uuid".to_string();
        bad_id.validate().expect_err("link id");

        let mut bad_app = sample();
        bad_app.target_app = "filmcraft".to_string();
        bad_app.validate().expect_err("target app");

        let mut empty_project = sample();
        empty_project.target_project.clear();
        empty_project.validate().expect_err("target_project");
        let mut empty_comp = sample();
        empty_comp.target_comp.clear();
        empty_comp.validate().expect_err("target_comp");
        let mut empty_nest = sample();
        empty_nest.nested_sequence.clear();
        empty_nest.validate().expect_err("nested_sequence");
        let mut empty_path = sample();
        empty_path.result_path.clear();
        empty_path.validate().expect_err("result_path");

        let mut zero_w = sample();
        zero_w.format.w = 0;
        zero_w.validate().expect_err("w");
        let mut zero_h = sample();
        zero_h.format.h = 0;
        zero_h.validate().expect_err("h");
        let mut zero_num = sample();
        zero_num.format.rate_num = 0;
        zero_num.validate().expect_err("rate_num");
        let mut zero_den = sample();
        zero_den.format.rate_den = 0;
        zero_den.validate().expect_err("rate_den");

        for created in ["", "2026-10-11", "2026-10-11T15:04:05", "2026-13-11T15:04:05Z", "2026-10-11T15:04:05.Z", "yesterday"] {
            let mut record = sample();
            record.created = created.to_string();
            record.validate().expect_err(created);
        }
    }
}

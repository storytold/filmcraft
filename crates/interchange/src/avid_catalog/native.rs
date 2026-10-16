//! Metadata-only native AVB transfer. This does not synthesize a complete media-scan report.
use super::*;
use crate::avid_bin::{self, AttributeValue, Bin, Decoded, Mob};
use serde_json::json;
mod graph;
pub use graph::{document as native_document, read_native, read_native_with_checkpoint};

struct Evidence<'a, 'b> {
    bin: &'b Bin<'a>,
    remaining: usize,
    bytes_left: usize,
    visiting: BTreeSet<u32>,
    checkpoint: &'b dyn Fn() -> Result<()>,
}
impl Evidence<'_, '_> {
    fn attributes(&mut self, id: u32, depth: usize) -> Result<Value> {
        if id == 0 {
            return Ok(json!({}));
        }
        // Optional metadata must never abort a bin. A reference to an object this reader does not
        // decode is recorded as evidence, exactly as a nested attribute reference already is.
        let object = self.bin.object(id)?.ok_or_else(|| bad("null attribute object"))?;
        if object.class != *b"ATTR" {
            return Ok(json!({"object_index":id,"class_raw":object.class,"unsupported":"native_attributes"}));
        }
        if depth > 16 || !self.visiting.insert(id) {
            return Err(bad("cyclic or excessively nested attributes"));
        }
        let Decoded::Attributes { entries } = self.bin.decode(id)? else {
            return Err(bad("attribute reference does not target ATTR"));
        };
        let mut values = serde_json::Map::new();
        for entry in entries {
            if self.remaining.is_multiple_of(256) {
                (self.checkpoint)()?;
            }
            self.remaining = self.remaining.checked_sub(1).ok_or_else(|| bad("attribute expansion budget exceeded"))?;
            let value_bytes = match &entry.value {
                AttributeValue::Text(Some(t)) => t.value.len(),
                AttributeValue::Bytes(b) => b.len().checked_mul(2).ok_or_else(|| bad("attribute size overflow"))?,
                _ => 64,
            };
            let cost = 128usize.checked_add(entry.name.value.len()).and_then(|v| v.checked_add(value_bytes)).ok_or_else(|| bad("attribute size overflow"))?;
            self.bytes_left = self.bytes_left.checked_sub(cost).ok_or_else(|| bad("attribute byte budget exceeded"))?;
            let name = entry.name.value.to_string();
            if name.len() > MAX_TEXT || name.contains('\0') {
                return Err(bad("invalid attribute name"));
            }
            let value = match entry.value {
                AttributeValue::Integer(i) => json!(i),
                AttributeValue::Text(t) => t.map_or(Value::Null, |t| json!(t.value)),
                AttributeValue::Bytes(b) => json!({"raw_hex":hex(b)}),
                AttributeValue::Reference(0) => Value::Null,
                AttributeValue::Reference(r) => {
                    let object = self.bin.object(r)?.ok_or_else(|| bad("null attribute object"))?;
                    if object.class == *b"ATTR" { self.attributes(r, depth + 1)? } else { json!({"object_index":r,"class_raw":object.class}) }
                }
            };
            values.insert(name, value);
        }
        self.visiting.remove(&id);
        Ok(Value::Object(values))
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn clip_evidence(m: &Mob<'_>, index: u32, bin: &Bin<'_>, attributes: Value) -> Result<Value> {
    let id = m.id.ok_or_else(|| bad("master has no verified full identity"))?;
    let name = m.common.name.as_ref().ok_or_else(|| bad("master has no name"))?.value.as_ref();
    if name.len() > MAX_TEXT || name.contains('\0') {
        return Err(bad("invalid master name"));
    }
    let (n, d) = m.common.rate.rational()?;
    let mut tracks = Vec::new();
    if m.tracks.len() > 64 {
        return Err(bad("too many master tracks"));
    }
    for t in &m.tracks {
        let index = t.index.filter(|i| *i > 0).ok_or_else(|| bad("invalid master track number"))?;
        let r = t.component.filter(|i| *i > 0).ok_or_else(|| bad("missing master track component"))?;
        let component = bin.decode(r)?;
        let common = match &component {
            Decoded::Sequence { common, .. } | Decoded::SourceClip { common, .. } | Decoded::Filler { common, .. } | Decoded::Timecode { common, .. } => common,
            _ => return Err(bad("unsupported master track component")),
        };
        let (num, den) = common.rate.rational()?;
        tracks.push(json!({"index":index,"status":"unresolved","references":[],
            "component":{"object_index":r,"media_kind":common.media_kind,"rate":{"numerator":num,"denominator":den}},
            "native_track":t}));
    }
    let user = match attributes.get("_USER") {
        None => json!({}),
        Some(v) if v.is_object() => v.clone(),
        _ => return Err(bad("invalid user metadata attributes")),
    };
    if user.get(SOURCE_KEY).is_some() {
        return Err(bad("user metadata conflicts with reserved provenance key"));
    }
    Ok(json!({"kind":"MasterMob","name":name,"user_placed":true,"object_index":index,
        "id":{"raw":hex(&id),"byte_order":"big"},"legacy_id_raw":hex(m.legacy_id_raw),
        "length":m.length,"rate":{"numerator":n,"denominator":d,"mantissa":m.common.rate.mantissa,"exponent":m.common.rate.exponent},
        "user":user,"attributes":attributes,"tracks":tracks,"availability":"unresolved","native_coverage":"metadata_only",
        "created_raw":m.created_raw,"modified_raw":m.modified_raw,"usage_code":m.usage,"descriptor_index":m.descriptor}))
}
/// Transfer current placed master names/columns into offline items. Sequence/descriptor/media
/// graph conversion is intentionally unavailable in this entry point and always reported.
pub fn plan_native(bytes: &[u8], path: &str) -> Result<Plan> {
    let bin = avid_bin::index(bytes)?;
    let membership = bin.membership()?;
    let name = bin_name(path)?;
    let mut evidence = Evidence { bin: &bin, remaining: 1_000_000, bytes_left: 64 * 1024 * 1024, visiting: BTreeSet::new(), checkpoint: &|| Ok(()) };
    let mut out = BinPlan {
        name: name.clone(),
        uid: format!("{:016x}", membership.uid),
        clips: Vec::new(),
        referenced: Vec::new(),
        subclips: Vec::new(),
        compositions: 0,
    };
    let mut warnings = vec!["Native AVB metadata import: source locations are not resolved, media remains offline, and sequences are not imported.".into()];
    let mut references = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut quantized = 0;
    for member in membership.members {
        if !references.insert(member.object_index) {
            return Err(bad("duplicate bin membership reference"));
        }
        if !member.user_placed {
            continue;
        }
        let Decoded::Mob(mob) = bin.decode(member.object_index)? else {
            return Err(bad("bin member is not a mob"));
        };
        let label = mob.common.name.as_ref().map(|s| s.value.as_ref()).unwrap_or("(unnamed)");
        if mob.kind != 2 || !matches!(mob.usage, 0 | 7) {
            if mob.kind == 1 {
                out.compositions += 1;
            }
            warnings.push(format!("{name}: {label} (mob type {}, usage {}) was not imported", mob.kind, mob.usage));
            continue;
        }
        let id = mob.id.ok_or_else(|| bad("master has no full identity"))?;
        if !identities.insert(id) {
            return Err(bad("duplicate placed master identity"));
        }
        let attributes = evidence.attributes(mob.common.attributes, 0)?;
        let value = clip_evidence(&mob, member.object_index, &bin, attributes)?;
        let (duration, q) = duration(&value)?;
        quantized += usize::from(q);
        out.clips.push(ClipPlan { name: label.into(), duration, evidence: value, source_descriptors: Vec::new(), picture_rate_evidence: None });
    }
    if quantized > 0 {
        warnings.push(format!("{quantized} durations were floored by less than one tick; raw rates and lengths are preserved."));
    }
    Ok(Plan { bins: vec![out], warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_non_attribute_metadata_reference_is_recorded_instead_of_failing_the_bin() {
        // Optional metadata referencing a class this reader does not decode is evidence, matching
        // how a nested attribute reference to another class is already reported.
        let bin = Bin {
            byte_order: avid_bin::ByteOrder::Little,
            root_index: 1,
            saved_at_raw: &[],
            creator_raw: &[],
            save_fields_raw: &[],
            reserved_raw: &[],
            objects: vec![avid_bin::Object { index: 1, class: *b"WINF", offset: 7, payload: &[0, 1] }],
        };
        let mut evidence = Evidence { bin: &bin, remaining: 8, bytes_left: 1024, visiting: BTreeSet::new(), checkpoint: &|| Ok(()) };
        let v = evidence.attributes(1, 0).unwrap();
        assert_eq!(v["class_raw"], json!(*b"WINF"));
        assert_eq!(v["object_index"], json!(1));
        assert_eq!(v["unsupported"], "native_attributes");
        // No user metadata is invented for it, and an absent reference stays absent.
        assert!(v.get("_USER").is_none());
        assert_eq!(evidence.attributes(0, 0).unwrap(), json!({}));
        assert!(evidence.attributes(2, 0).is_err());
    }
    #[test]
    #[ignore = "set FILMCRAFT_AVB_SEMANTIC_ORACLE and FILMCRAFT_AVB_CORPUS_MANIFEST to private evidence"]
    fn native_metadata_matches_oracle_and_transfers() {
        let files: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_SEMANTIC_ORACLE").unwrap()).unwrap()).unwrap();
        let manifest: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_CORPUS_MANIFEST").unwrap()).unwrap()).unwrap();
        let mut total = 0;
        for file in files.as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            let bytes = std::fs::read(path).unwrap();
            let plan = plan_native(&bytes, path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let membership = manifest.as_array().unwrap().iter().find(|v| v["path"] == file["path"]).unwrap();
            // The native membership is separately checked against the complete oracle reference list.
            let index = avid_bin::index(&bytes).unwrap();
            let members = index.membership().unwrap();
            assert_eq!(members.members.len() as u64, membership["members"].as_u64().unwrap());
            let objects: BTreeMap<_, _> = file["objects"].as_array().unwrap().iter().map(|o| (o["index"].as_u64().unwrap(), &o["properties"])).collect();
            let expected: Vec<_> = members
                .members
                .iter()
                .filter(|m| m.user_placed)
                .filter_map(|m| objects.get(&u64::from(m.object_index)))
                .filter(|m| m["mob_type_id"] == 2 && matches!(m["usage_code"].as_u64(), Some(0 | 7)))
                .collect();
            assert_eq!(plan.bins[0].clips.len(), expected.len(), "{path}");
            if path.contains("/Phase 6/") {
                assert_eq!(expected.len(), 1953);
            }
            for (clip, mob) in plan.bins[0].clips.iter().zip(expected) {
                assert_eq!(clip.name, mob["name"].as_str().unwrap());
                assert_eq!(clip.evidence["length"], mob["length"]);
                let raw = mob["mob_id"]["id_le"].as_str().unwrap();
                assert_eq!(full_identity(&clip.evidence["id"]).unwrap(), full_identity(&json!({"raw":raw,"byte_order":"little"})).unwrap());
                if let Some(attrs) = mob["attributes"]["ref"].as_u64().and_then(|r| objects.get(&r))
                    && let Some(user) = attrs["_USER"]["ref"].as_u64().and_then(|r| objects.get(&r))
                {
                    for (key, value) in user.as_object().unwrap() {
                        if value.is_string() || value.is_number() {
                            assert_eq!(&clip.evidence["user"][key], value, "{path} {key}");
                        }
                    }
                }
                total += 1;
            }
            let mut project = Project::default();
            let bins = apply(&mut project, plan, None).unwrap();
            assert_eq!(bins.len(), 1);
            assert_eq!(project.items.len(), bins[0].clips);
        }
        eprintln!("{total} native placed masters match names, full identities, lengths and scalar user columns");
    }
}

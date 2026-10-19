//! Normalize native objects into the existing transfer contract. No file access or availability inference.
use super::*;
use crate::avid_bin::{Component, DecimalRate, Descriptor, Field, Track};
fn rate(r: DecimalRate) -> Result<Value> {
    let (n, d) = r.rational()?;
    Ok(json!({"numerator":n,"denominator":d}))
}
fn id(v: Option<[u8; 32]>) -> Value {
    v.map_or(Value::Null, |v| json!({"raw":hex(&v),"byte_order":"big"}))
}
fn scalar_rate(r: DecimalRate) -> Result<Value> {
    let (n, d) = r.rational()?;
    Ok(if n % d == 0 { json!(n / d) } else { json!(n as f64 / d as f64) })
}
// Expansion budgets are independent of the borrowed AVB input and object index. The compact
// native path does not spend this budget on a duplicate master catalog.
const MAX_GRAPH_BYTES: usize = 512 * 1024 * 1024;
struct Graph<'a, 'b> {
    evidence: Evidence<'a, 'b>,
    nodes: usize,
    visiting: BTreeSet<u32>,
    bytes_left: usize,
}
impl Graph<'_, '_> {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        (self.evidence.checkpoint)()?;
        self.bytes_left = self.bytes_left.checked_sub(bytes).ok_or_else(|| bad("native graph byte budget exceeded"))?;
        Ok(())
    }
    fn reference(&self, r: u32) -> Result<Value> {
        Ok(match self.evidence.bin.object(r)? {
            None => Value::Null,
            Some(o) => json!({"object_index":r,"class_raw":o.class}),
        })
    }
    /// A rendered effect points at the precompute that holds its baked media, through a source
    /// clip. Carry that clip's identity, so a transfer can reach the render without decoding the
    /// bin again. Anything else stays the plain reference it already was.
    fn precompute(&self, r: u32) -> Result<Value> {
        let mut value = self.reference(r)?;
        if value["class_raw"] == json!(*b"SCLP")
            && let Ok(Decoded::SourceClip { length, id: source, track, start, .. }) = self.evidence.bin.decode(r)
        {
            value["source_id"] = id(source);
            value["length"] = json!(length);
            value["source_track"] = json!(track);
            value["start"] = json!(start);
        }
        Ok(value)
    }
    fn properties(&mut self, c: &Component<'_>) -> Result<Value> {
        Ok(json!({"effect_id":c.effect.as_ref().map(|t|t.value.as_ref()),"attributes":self.evidence.attributes(c.attributes,0)?,
            "name":c.name.as_ref().map(|t|t.value.as_ref()),"left_bob":self.reference(c.left)?,"right_bob":self.reference(c.right)?,
            "param_list":self.reference(c.parameters.unwrap_or(0))?,"precomputed":self.precompute(c.precomputed)?,
            "raw_rate":{"mantissa":c.rate.mantissa,"exponent":c.rate.exponent}}))
    }
    fn track(&mut self, t: &Track, depth: usize) -> Result<Value> {
        let mut p = json!({"index":t.index,"lock_number":t.lock});
        if let Some(r) = t.attributes {
            p["attributes"] = self.evidence.attributes(r, 0)?;
        }
        if let Some(r) = t.session_attributes {
            p["session_attr"] = self.evidence.attributes(r, 0)?;
        }
        if let Some(r) = t.filler_proxy {
            p["filler_proxy"] = self.reference(r)?;
        }
        if let Some(r) = t.bob_data {
            p["bob_data"] = self.reference(r)?;
        }
        if let Some(n) = t.start {
            p["start_pos"] = json!(n);
        }
        Ok(json!({"index":t.index,"properties":p,"component":self.component(t.component.ok_or_else(||bad("track has no component"))?,depth)?}))
    }
    fn field(&self, f: &Field<'_>) -> Result<Value> {
        Ok(match f {
            Field::Integer(v) => json!(v),
            Field::Boolean(v) => json!(v),
            Field::Ratio(v) => json!(v),
            Field::Float(v) => json!(v),
            Field::Reference(r) => self.reference(*r)?,
            Field::Bytes(b) => json!({"raw_hex":hex(b)}),
        })
    }
    fn component(&mut self, r: u32, depth: usize) -> Result<Value> {
        self.charge(512)?;
        self.nodes = self.nodes.checked_add(1).filter(|n| *n <= 200_000).ok_or_else(|| bad("native component expansion exceeds budget"))?;
        if depth > 16 || !self.visiting.insert(r) {
            return Err(bad("cyclic/excessive component graph"));
        }
        let object = self.evidence.bin.object(r)?.ok_or_else(|| bad("null component"))?;
        // Repeated references expand into independent JSON values. Charge variable payloads
        // before decoding/copying, including MacRoman-to-UTF-8 text and raw field encoding.
        self.charge(object.payload.len().checked_mul(8).ok_or_else(|| bad("component expansion overflow"))?)?;
        let mut value = match self.evidence.bin.decode(r)? {
            Decoded::Sequence { common, components } => {
                let mut children = Vec::new();
                let mut length = 0i64;
                for child in components {
                    let v = self.component(child, depth + 1)?;
                    length =
                        length.checked_add(v["length"].as_i64().ok_or_else(|| bad("missing child length"))?).ok_or_else(|| bad("sequence length overflow"))?;
                    children.push(v);
                }
                json!({"class":"SEQU","length":length,"rate":rate(common.rate)?,"media_kind":common.media_kind,"components":children})
            }
            Decoded::SourceClip { common, length, track, start, id: source, .. } => {
                json!({"class":"SCLP","length":length,"rate":rate(common.rate)?,"media_kind":common.media_kind,"source_id":id(source),"source_track":track,"start":start})
            }
            Decoded::Filler { common, length } => json!({"class":"FILL","length":length,"rate":rate(common.rate)?,"media_kind":common.media_kind}),
            Decoded::Timecode { common, length, flags, fps, start } => {
                json!({"class":"TCCP","length":length,"rate":rate(common.rate)?,"media_kind":common.media_kind,"flags":flags,"fps":fps,"start":start})
            }
            Decoded::Selector { common, mode, length, tracks, selection_raw, trailing_raw } => {
                let mut properties = self.properties(&common)?;
                properties["mc_mode"] = json!(mode);
                properties["selection_raw"] = json!(selection_raw);
                properties["trailing_raw"] = json!(trailing_raw);
                let alternatives = tracks.iter().map(|t| self.track(t, depth + 1)).collect::<Result<Vec<_>>>()?;
                // Alternatives are choices, not simultaneous inputs or consecutive edits.
                // In particular, best-effort effect bypass must not choose the first one.
                // The active-choice field is not yet established by paired Avid fixtures.
                json!({"class":"SLCT","length":length,"rate":rate(common.rate)?,"media_kind":common.media_kind,
                    "properties":properties,"alternatives":alternatives,"unsupported":"selector_selection"})
            }
            Decoded::Group(g) => {
                let mut properties = self.properties(&g.common)?;
                if g.common.effect.as_ref().is_some_and(|id| id.value.as_ref() == "EFF2_SBLEND")
                    && properties["attributes"]["_EFFECT_PLUGIN_NAME"] == "3DWarp"
                    && let Some(reference) = g.common.parameters
                    && let Some(params) = self.evidence.bin.numeric_parameters(reference)?
                {
                    self.charge(params.len().checked_mul(128).ok_or_else(|| bad("parameter expansion overflow"))?)?;
                    properties["numeric_parameters"] = json!(params);
                }
                properties["mc_mode"] = json!(g.mode);
                for (key, f) in &g.fields {
                    properties[*key] = self.field(f)?;
                }
                let inputs = g.tracks.iter().map(|t| self.track(t, depth + 1)).collect::<Result<Vec<_>>>()?;
                json!({"class":std::str::from_utf8(&g.class).map_err(|_|bad("invalid class label"))?,"length":g.length,"rate":rate(g.common.rate)?,"media_kind":g.common.media_kind,
                    "properties":properties,"input_tracks":inputs,"unsupported":"component_semantics"})
            }
            Decoded::Auxiliary { common, length, fields } => {
                let mut properties = self.properties(&common)?;
                for (key, f) in &fields {
                    properties[*key] = self.field(f)?;
                }
                json!({"class":std::str::from_utf8(&object.class).map_err(|_|bad("invalid class label"))?,"length":length,"rate":rate(common.rate)?,"media_kind":common.media_kind,"properties":properties,"unsupported":"component_semantics"})
            }
            _ => return Err(bad("unsupported component object")),
        };
        value["object_index"] = json!(r);
        self.visiting.remove(&r);
        Ok(value)
    }
    fn descriptor(&mut self, r: u32) -> Result<Value> {
        if r == 0 {
            return Ok(Value::Null);
        }
        let object = self.evidence.bin.object(r)?.ok_or_else(|| bad("null descriptor"))?;
        if !matches!(&object.class, b"MDES" | b"PCMA" | b"CDCI") {
            return Ok(json!({"class_raw":object.class,"object_index":r,"unsupported":"native_descriptor"}));
        }
        self.charge(object.payload.len().checked_mul(8).ok_or_else(|| bad("descriptor expansion overflow"))?)?;
        let Decoded::Descriptor(d) = self.evidence.bin.decode(r)? else {
            return Err(bad("invalid descriptor"));
        };
        if let Some(locator) = self.evidence.bin.object(d.locator)? {
            self.charge(locator.payload.len().checked_mul(8).ok_or_else(|| bad("locator expansion overflow"))?)?;
        }
        descriptor_value(self.evidence.bin, &d, object.class)
    }
}
fn descriptor_value(bin: &Bin<'_>, d: &Descriptor<'_>, class: [u8; 4]) -> Result<Value> {
    let mut v = json!({"class":std::str::from_utf8(&class).map_err(|_|bad("invalid descriptor class"))?,"mob_kind":d.mob_kind,"intermediate":d.intermediate,"physical_media_index":d.physical_media});
    if let Some(f) = &d.file {
        v["edit_rate"] = scalar_rate(f.rate)?;
        v["length"] = json!(f.length);
        v["is_omfi"] = json!(f.omfi);
        v["data_offset"] = json!(f.data_offset);
    }
    if let Some(p) = &d.pcm {
        let values = json!({"channels":p.channels,"quantization_bits":p.bits,"sample_rate":scalar_rate(p.rate)?,"coding_format":p.coding_format,"block_align":p.block_align,"average_bps":p.average_bps});
        v.as_object_mut().ok_or_else(|| bad("descriptor object"))?.extend(values.as_object().ok_or_else(|| bad("PCM object"))?.clone());
    }
    if let Some(p) = &d.picture {
        for (key, value) in &p.integers {
            v[*key] = json!(value);
        }
        for (key, value) in &p.booleans {
            v[*key] = json!(value);
        }
        v["aspect_ratio"] = json!(p.aspect);
        v["line_map"] = json!(p.line_map);
    }
    // Retain the full typed descriptor separately from the fields consumed by binding.
    v["native_fields"] = serde_json::to_value(d).map_err(|e| bad(e.to_string()))?;
    if d.locator != 0 {
        let object = bin.object(d.locator)?.ok_or_else(|| bad("null locator"))?;
        // A locator this reader does not decode is optional evidence, not a reason to lose the
        // whole bin. Record the class so binding treats the descriptor as unverified.
        if !matches!(&object.class, b"FILE" | b"MSML" | b"URLL" | b"WINF") {
            v["locator"] = json!({"class_raw":object.class,"object_index":d.locator,"unsupported":"native_locator"});
            return Ok(v);
        }
        let Decoded::Locator(l) = bin.decode(d.locator)? else {
            return Err(bad("unsupported descriptor locator"));
        };
        v["locator"] = json!({"class":std::str::from_utf8(&object.class).map_err(|_|bad("invalid locator class"))?,"properties":{
            "mob_id":l.id.map(|id|json!({"id_raw":hex(&id),"byte_order":"big"})),
            "path":l.path.as_ref().map(|t|t.value.as_ref()),"path_posix":l.posix.as_ref().map(|t|t.value.as_ref()),"path_utf8":l.utf8.as_ref().map(|t|t.value.as_ref())},"native_fields":l});
    }
    Ok(v)
}
fn value_cost(v: &Value, depth: usize) -> Result<usize> {
    if depth > 40 {
        return Err(bad("excessive normalized value depth"));
    }
    let mut total = 48usize;
    let mut add = |n: usize| -> Result<()> {
        total = total.checked_add(n).filter(|n| *n <= MAX_BYTES).ok_or_else(|| bad("normalized value exceeds byte budget"))?;
        Ok(())
    };
    match v {
        Value::String(s) => add(s.len())?,
        Value::Array(a) => {
            for child in a {
                add(value_cost(child, depth + 1)?)?;
            }
        }
        Value::Object(o) => {
            for (key, child) in o {
                add(key.len())?;
                add(value_cost(child, depth + 1)?)?;
            }
        }
        _ => {}
    }
    Ok(total)
}
fn sources(c: &Value, out: &mut Vec<Value>, depth: usize) -> Result<()> {
    if depth > 16 || out.len() > 64 {
        return Err(bad("source reference expansion exceeds bounds"));
    }
    if c["class"] == "SCLP" {
        out.push(json!({"source_id":c["source_id"],"source_track":c["source_track"],"source_start":c["start"],"length":c["length"],"rate":c["rate"],"status":"unresolved","locations":[]}));
    } else if c["class"] == "SEQU" {
        for child in c["components"].as_array().ok_or_else(|| bad("invalid sequence children"))? {
            sources(child, out, depth + 1)?;
        }
    }
    Ok(())
}
/// Native normalized catalog with an explicitly empty media scan. References authorize offline
/// structure only when a unique live source descriptor and its managed locator agree.
pub fn document(bytes: &[u8], path: &str) -> Result<Value> {
    let mut doc = read_native(bytes, path)?;
    let mut b = doc.bins.pop().ok_or_else(|| bad("missing native bin"))?;
    let mut budget = MAX_GRAPH_BYTES;
    let mut clips = Vec::new();
    for m in array(&b, "mobs")?.iter().filter(|m| m["kind"] == "MasterMob") {
        budget = budget.checked_sub(value_cost(m, 0)?).ok_or_else(|| bad("expanded master catalog exceeds byte budget"))?;
        clips.push(m.clone());
    }
    // Keep the expanded schema-2 export's original split: decoded mobs carry source evidence,
    // while the catalog copy carries derived availability and source-descriptor joins.
    for m in b["mobs"].as_array_mut().ok_or_else(|| bad("missing native mobs"))?.iter_mut().filter(|m| m["kind"] == "MasterMob") {
        let master = m.as_object_mut().ok_or_else(|| bad("invalid native master"))?;
        for key in ["availability", "location_scope", "workflow_evidence"] {
            master.remove(key);
        }
        for track in master.get_mut("tracks").and_then(Value::as_array_mut).ok_or_else(|| bad("missing master tracks"))? {
            let track = track.as_object_mut().ok_or_else(|| bad("invalid master track"))?;
            track.remove("status");
            track.remove("references");
        }
    }
    let mut catalog = json!({"summary":{"master_count":clips.len()}});
    catalog["clips"] = Value::Array(clips);
    b["catalog"] = catalog;
    let mut result = json!({"schema_version":2,"workflow":"avid_managed_mxf"});
    result["bins"] = Value::Array(vec![b]);
    Ok(result)
}
/// Native snapshots need no second copy of each master for external-report cross-checking.
pub fn read_native(bytes: &[u8], path: &str) -> Result<Document> {
    read_native_with_checkpoint(bytes, path, &|| Ok(()))
}
/// Same snapshot, with cancellation during object indexing, graph and attribute expansion.
pub fn read_native_with_checkpoint(bytes: &[u8], path: &str, checkpoint: &dyn Fn() -> Result<()>) -> Result<Document> {
    // A checkpoint failure must escape best-effort track recovery even if a callback only
    // reports it once. Do not classify cancellation by an error-message string.
    let checkpoint_failed = std::cell::Cell::new(false);
    let checked = || checkpoint().inspect_err(|_| checkpoint_failed.set(true));
    let bin = avid_bin::index_with_checkpoint(bytes, &checked)?;
    let membership = bin.membership()?;
    let mut graph = Graph {
        evidence: Evidence { bin: &bin, remaining: 1_000_000, bytes_left: 128 * 1024 * 1024, visiting: BTreeSet::new(), checkpoint: &checked },
        nodes: 0,
        bytes_left: MAX_GRAPH_BYTES,
        visiting: BTreeSet::new(),
    };
    let mut mobs = Vec::new();
    let mut seen = BTreeSet::new();
    for member in membership.members {
        checkpoint()?;
        if !seen.insert(member.object_index) {
            return Err(bad("duplicate bin member"));
        }
        let Decoded::Mob(m) = bin.decode(member.object_index)? else {
            return Err(bad("invalid bin member"));
        };
        let attrs = graph.evidence.attributes(m.common.attributes, 0)?;
        let mut decode_errors = Vec::new();
        let mut tracks = Vec::new();
        for t in &m.tracks {
            match graph.track(t, 0) {
                Ok(v) => tracks.push(v),
                Err(e) => {
                    // Cancellation is never converted into an unsupported track.
                    if checkpoint_failed.get() {
                        return Err(e);
                    }
                    // The composition/master's declared range bounds a failed top-level track.
                    // Keep other tracks and bin items available for best-effort conversion.
                    graph.visiting.clear();
                    graph.evidence.visiting.clear();
                    let decoded = t.component.and_then(|r| bin.decode(r).ok());
                    let common = match &decoded {
                        Some(
                            Decoded::Sequence { common, .. }
                            | Decoded::SourceClip { common, .. }
                            | Decoded::Filler { common, .. }
                            | Decoded::Timecode { common, .. }
                            | Decoded::Selector { common, .. }
                            | Decoded::Auxiliary { common, .. },
                        ) => Some(common),
                        Some(Decoded::Group(g)) => Some(&g.common),
                        _ => None,
                    };
                    let kind = common.map_or(0, |c| c.media_kind);
                    let recorded = common.map_or(m.common.rate, |c| c.rate);
                    let message = format!("Track {:?}: {e}", t.index);
                    decode_errors.push(message.clone());
                    tracks.push(
                        json!({"index":t.index,"properties":{"native_track":t},"component":{"class":"UNSUPPORTED","unsupported":message,"length":m.length,
                        "rate":rate(recorded)?,"media_kind":kind,"object_index":t.component}}),
                    );
                }
            }
        }
        let kind = match m.kind {
            1 => "CompositionMob",
            2 => "MasterMob",
            3 => "SourceMob",
            _ => "UnknownMob",
        };
        mobs.push(
            json!({"kind":kind,"id":id(m.id),"live":true,"user_placed":member.user_placed,"name":m.common.name.as_ref().map(|t|t.value.as_ref()).unwrap_or(""),
            "length":m.length,"rate":rate(m.common.rate)?,"attributes":attrs,"user":attrs.get("_USER").cloned().unwrap_or(json!({})),"tracks":tracks,
            "native_decode_errors":decode_errors,"descriptor":graph.descriptor(m.descriptor)?,"object_id":member.object_index,"usage_code":m.usage,"native_legacy_id_raw":hex(m.legacy_id_raw)}),
        );
    }
    let mut source_map = BTreeMap::new();
    for (index, m) in mobs.iter().enumerate().filter(|(_, m)| m["kind"] == "SourceMob") {
        if let Ok(key) = full_identity(&m["id"]) {
            source_map.entry(key).and_modify(|v| *v = None).or_insert(Some(index));
        }
    }
    for index in 0..mobs.len() {
        checkpoint()?;
        if mobs.get(index).is_none_or(|m| m["kind"] != "MasterMob") {
            continue;
        }
        let mut clip = mobs.get_mut(index).ok_or_else(|| bad("missing master"))?.take();
        let tracks = clip["tracks"].as_array_mut().ok_or_else(|| bad("invalid tracks"))?;
        for t in tracks {
            let mut refs = Vec::new();
            sources(&t["component"], &mut refs, 0)?;
            for r in &mut refs {
                if let Ok(key) = full_identity(&r["source_id"])
                    && let Some(Some(source)) = source_map.get(&key)
                {
                    let descriptor = &mobs.get(*source).ok_or_else(|| bad("missing source"))?["descriptor"];
                    let locator = &descriptor["locator"]["properties"]["mob_id"];
                    let locator_id = json!({"raw":locator["id_raw"],"byte_order":locator["byte_order"]});
                    if descriptor["locator"]["class"] == "MSML" && full_identity(&locator_id).ok() == Some(key) {
                        r["managed_descriptor_verified"] = json!(true);
                        graph.charge(value_cost(descriptor, 0)?)?;
                        r["source_descriptor"] = descriptor.clone();
                        r["location_scope"] = json!("no_media_locations_scanned");
                    }
                }
            }
            t["status"] = json!("unresolved");
            t["references"] = json!(refs);
        }
        if clip["tracks"].as_array().is_some_and(|ts| {
            !ts.is_empty()
                && ts.iter().all(|t| t["references"].as_array().is_some_and(|rs| !rs.is_empty() && rs.iter().all(|r| r["managed_descriptor_verified"] == true)))
        }) {
            clip["workflow_evidence"] = json!("managed_avb_source_descriptors");
        }
        clip["availability"] = json!("unresolved");
        clip["location_scope"] = json!("no_media_locations_scanned");
        *mobs.get_mut(index).ok_or_else(|| bad("missing master"))? = clip;
    }
    let mut result = json!({"path":path,"backend":"native_rust","status":"parsed","snapshot_stable":true,
        "diagnostics":[],"summary":{"bin_uid":format!("{:016x}",membership.uid)},
        "coverage":"supported_native_objects; unknown descriptor classes remain explicit","media_locations_scanned":[]});
    // json! serializes borrowed expressions: passing the entire tree there duplicates it.
    result["mobs"] = Value::Array(mobs);
    Ok(Document { schema_version: 2, workflow: "avid_managed_mxf".into(), compact_native: true, bins: vec![result] })
}

#[cfg(test)]
mod selector_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_component_text_is_charged_to_the_expansion_budget() {
        let mut payload = vec![2, 3];
        payload.extend_from_slice(&[0; 8]);
        payload.extend_from_slice(&7u16.to_le_bytes());
        payload.extend_from_slice(&24u32.to_le_bytes());
        payload.extend_from_slice(&[0; 2]);
        payload.extend_from_slice(&1024u16.to_le_bytes());
        payload.extend(std::iter::repeat_n(b'x', 1024));
        payload.extend_from_slice(&[255; 2]);
        payload.extend_from_slice(&[0; 12]);
        payload.extend_from_slice(&[2, 1]);
        payload.extend_from_slice(&24u32.to_le_bytes());
        payload.extend_from_slice(&[2, 1, 0, 0, 0, 0, 3]);
        let bin = Bin {
            byte_order: avid_bin::ByteOrder::Little,
            root_index: 1,
            saved_at_raw: &[],
            creator_raw: &[],
            save_fields_raw: &[],
            reserved_raw: &[],
            objects: vec![avid_bin::Object { index: 1, class: *b"TRKR", offset: 0, payload: &payload }],
        };
        let mut graph = Graph {
            evidence: Evidence { bin: &bin, remaining: 100, bytes_left: 4096, visiting: BTreeSet::new(), checkpoint: &|| Ok(()) },
            nodes: 0,
            bytes_left: 512 + payload.len() * 8 + 4096,
            visiting: BTreeSet::new(),
        };
        assert_eq!(graph.component(1, 0).unwrap()["properties"]["name"], "x".repeat(1024));
        // The same object is copied on each reference; only the first expansion fits.
        assert!(graph.component(1, 0).unwrap_err().to_string().contains("byte budget"));
    }
    #[test]
    fn an_undecodable_descriptor_locator_is_recorded_instead_of_failing_the_bin() {
        // Bins carry locator classes this reader does not decode; PVOL is the most common one in
        // the corpus. The locator is optional evidence for binding, so an unknown class must not
        // lose the bin.
        let payload = [0x11, 0x22];
        let bin = Bin {
            byte_order: avid_bin::ByteOrder::Little,
            root_index: 1,
            saved_at_raw: &[],
            creator_raw: &[],
            save_fields_raw: &[],
            reserved_raw: &[],
            objects: vec![avid_bin::Object { index: 1, class: *b"PVOL", offset: 119, payload: &payload }],
        };
        let d = Descriptor {
            mob_kind: 1,
            locator: 1,
            intermediate: false,
            physical_media: 0,
            uuid: None,
            plugin_raw: None,
            attributes: None,
            file: None,
            pcm: None,
            picture: None,
        };
        let v = descriptor_value(&bin, &d, *b"MDES").unwrap();
        assert_eq!(v["locator"]["class_raw"], json!(*b"PVOL"));
        assert_eq!(v["locator"]["object_index"], json!(1));
        assert_eq!(v["locator"]["unsupported"], "native_locator");
        // Nothing is invented: no decoded path or identity is reported for it.
        assert!(v["locator"]["properties"].is_null());
        // A null locator reference stays absent rather than becoming unsupported evidence.
        let none = Descriptor { locator: 0, ..d };
        assert!(descriptor_value(&bin, &none, *b"MDES").unwrap().get("locator").is_none());
    }
    #[test]
    fn cyclic_components_and_expansion_budgets_fail_without_panics() {
        let mut payload = vec![2, 3];
        payload.extend_from_slice(&[0; 8]);
        payload.extend_from_slice(&2u16.to_le_bytes());
        payload.extend_from_slice(&24u32.to_le_bytes());
        payload.extend_from_slice(&[0; 2]);
        payload.extend_from_slice(&[255; 4]);
        payload.extend_from_slice(&[0; 12]);
        payload.extend_from_slice(&[2, 3]);
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.push(3);
        let bin = Bin {
            byte_order: avid_bin::ByteOrder::Little,
            root_index: 1,
            saved_at_raw: &[],
            creator_raw: &[],
            save_fields_raw: &[],
            reserved_raw: &[],
            objects: vec![avid_bin::Object { index: 1, class: *b"SEQU", offset: 0, payload: &payload }],
        };
        for (nodes, bytes_left, expected) in [(0, 4096, "cyclic"), (200_000, 4096, "budget"), (0, 0, "budget")] {
            let mut graph = Graph {
                evidence: Evidence { bin: &bin, remaining: 100, bytes_left: 4096, visiting: BTreeSet::new(), checkpoint: &|| Ok(()) },
                nodes,
                bytes_left,
                visiting: BTreeSet::new(),
            };
            assert!(graph.component(1, 0).unwrap_err().to_string().contains(expected));
        }
        let mut deep = Value::Null;
        for _ in 0..42 {
            deep = json!([deep]);
        }
        assert!(value_cost(&deep, 0).is_err());
    }
}

//! Development bridge from the standalone Avid reader's JSON catalog.
//!
//! No AVB/PMR/MDB binary decoding or file I/O. Imported clips are metadata-only offline
//! placeholders until FilmCraft supports their independently located media components.
use std::collections::{BTreeMap, BTreeSet};

use filmcraft_media::{MediaInfo, MediaKind};
use filmcraft_project::{BinEntry, BinId, Interpretation, ItemKind, Label, MediaClip, MediaRef, Project};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::Serialize;
use serde_json::Value;

use crate::{Error, Result};

mod native;
mod subclips;
pub use native::{native_document, plan_native, read_native, read_native_with_checkpoint};
pub(super) use subclips::subclip_track;
use subclips::{referenced_subclips, subclip};

pub const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_BINS: usize = 1024;
const MAX_CLIPS: usize = 100_000;
const MAX_MOBS: usize = 200_000;
const MAX_TEXT: usize = 4096;
/// All supported source fields are copied here, not linked to the AVB/report.
pub const SOURCE_KEY: &str = "filmcraft:avid-source";
/// Sub-bin holding masters an imported bin's sequences reference without placing them. Kept out
/// of the bin's own listing so it still reflects what Avid shows.
pub const REFERENCED_BIN: &str = "Referenced Sources";

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    bins: Vec<BinPlan>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
struct BinPlan {
    pub name: String,
    pub uid: String,
    pub clips: Vec<ClipPlan>,
    /// Masters the bin's sequences reference without placing them in the bin. Not bin contents;
    /// FilmCraft still needs them as items because media binds to an item, never to a timeline
    /// clip. Populated for sequence transfers only.
    pub referenced: Vec<ClipPlan>,
    /// Placed subclips and referenced timeline dependencies, restricted to a master this
    /// bin also carries. Avid records that range in attributes, not in the component.
    pub subclips: Vec<SubclipPlan>,
    pub compositions: usize,
}
#[derive(Clone, Debug, Serialize)]
struct SubclipPlan {
    pub name: String,
    pub placed: bool,
    /// Identity of the master this subclip restricts. Must be imported for the bin.
    pub parent: [u8; 32],
    pub start: Tick,
    pub duration: Tick,
    pub evidence: Value,
}
#[derive(Clone, Debug, Serialize)]
struct ClipPlan {
    pub name: String,
    pub duration: Tick,
    pub evidence: Value,
    pub source_descriptors: Vec<Value>,
    pub picture_rate_evidence: Option<Value>,
}
#[derive(Debug, Serialize)]
pub struct ImportedBin {
    pub bin: BinId,
    pub name: String,
    pub clips: usize,
    /// Sequence sources imported into the sub-bin, not part of the Avid bin's own contents.
    pub referenced: usize,
    /// Subclips imported as bin contents, each restricting one of this bin's masters.
    pub subclips: usize,
    /// Unplaced subclip dependencies retained in the referenced-sources sub-bin.
    pub referenced_subclips: usize,
    pub compositions: usize,
}

/// Requested transfer stages. Media/sequence stages must be verified by the engine before commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Metadata,
    Media,
    Sequences,
}
fn bad(message: impl Into<String>) -> Error {
    Error::Other(format!("Avid catalog: {}", message.into()))
}
fn array<'a>(v: &'a Value, name: &str) -> Result<&'a Vec<Value>> {
    v.get(name).and_then(Value::as_array).ok_or_else(|| bad(format!("missing or invalid {name}")))
}
fn text<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    let s = v.get(name).and_then(Value::as_str).ok_or_else(|| bad(format!("missing or invalid {name}")))?;
    if s.len() > MAX_TEXT || s.contains('\0') {
        return Err(bad(format!("invalid {name} text")));
    }
    Ok(s)
}
/// The catalog contract carries binary IDs with explicit byte order, never display URNs.
/// Normalize the material-number fields for duplicate detection; preserve raw evidence.
fn identity_key(m: &Value) -> Result<Vec<u8>> {
    let id = m.get("id").ok_or_else(|| bad("missing source identity"))?;
    let raw = text(id, "raw")?;
    let order = text(id, "byte_order")?;
    if !matches!(raw.len(), 24 | 64) || !raw.bytes().all(|b| b.is_ascii_hexdigit()) || !matches!(order, "little" | "big") {
        return Err(bad("invalid source identity"));
    }
    let mut bytes = raw
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).map_err(|_| bad("invalid identity hex"))?;
            u8::from_str_radix(digits, 16).map_err(|_| bad("invalid identity hex"))
        })
        .collect::<Result<Vec<_>>>()?;
    if bytes.len() == 12 {
        if order != "little" || bytes.get(..4) != Some(&[42, 0, 0, 0]) {
            return Err(bad("unsupported short source identity"));
        }
    } else if order == "little" {
        for range in [16..20, 20..22, 22..24] {
            bytes.get_mut(range).ok_or_else(|| bad("invalid identity width"))?.reverse();
        }
    }
    Ok(bytes)
}
/// A rendered motion effect carries its retime in media of its own, produced when the effect was
/// rendered. Its component points at that precompute through a source clip, whose identity is the
/// precompute master. Find that master, so the effect can be transferred as the baked clip it is
/// and bound to the render like any other managed master.
fn precompute_master<'a>(m: &Value, mobs: &'a [Value]) -> Option<&'a Value> {
    fn find(v: &Value, out: &mut Vec<[u8; 32]>, depth: usize) {
        if depth > 24 || out.len() > 64 {
            return;
        }
        match v {
            Value::Object(o) => {
                if let Some(p) = o.get("properties").and_then(|p| p.get("precomputed"))
                    && let Some(id) = p.get("source_id").and_then(|i| full_identity(i).ok())
                {
                    out.push(id);
                }
                for x in o.values() {
                    find(x, out, depth + 1);
                }
            }
            Value::Array(a) => {
                for x in a {
                    find(x, out, depth + 1);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    find(m.get("tracks")?, &mut found, 0);
    // One effect, one render. Tracks naming different precomputes are not a single baked clip,
    // and picking one of them would bind the item to the wrong media.
    let id = *found.first()?;
    found.iter().all(|x| *x == id).then_some(())?;
    mobs.iter().find(|x| x["kind"] == "MasterMob" && full_identity(&x["id"]).ok() == Some(id))
}
fn membership(m: &Value) -> Result<bool> {
    m.get("user_placed").and_then(Value::as_bool).ok_or_else(|| bad("missing master bin membership"))
}

/// Canonical full identity for media binding; short-only evidence cannot authorize a file.
pub fn full_identity(id: &Value) -> Result<[u8; 32]> {
    identity_key(&serde_json::json!({"id":id}))?.try_into().map_err(|_| bad("media binding requires a full source identity"))
}

/// Compare recorded edit units with an independently corroborated clock. Recognized nominal
/// NTSC labels are explicit pairs, never a floating-point tolerance or blanket substitution.
/// This is not permission to substitute rates without opening the identified picture source.
pub fn picture_rate_matches(recorded: &Value, actual: filmcraft_time::FrameRate) -> bool {
    let Some(n) = recorded["rate"]["numerator"].as_i64().filter(|n| *n > 0) else { return false };
    let Some(d) = recorded["rate"]["denominator"].as_i64().filter(|d| *d > 0) else { return false };
    actual.num > 0
        && actual.den > 0
        && (i128::from(n) * i128::from(actual.den) == i128::from(d) * i128::from(actual.num)
            || [(23976, 1000, 24000), (2997, 100, 30000), (47952, 1000, 48000), (4795, 100, 48000), (5994, 100, 60000), (11988, 100, 120000)].iter().any(
                |&(rn, rd, an)| {
                    filmcraft_time::FrameRate::new(n, d) == filmcraft_time::FrameRate::new(rn, rd) && actual == filmcraft_time::FrameRate::new(an, 1001)
                },
            ))
}
/// Integer conversion with sub-tick flooring; original count/rate stays in evidence.
fn duration(v: &Value) -> Result<(Tick, bool)> {
    let count = v.get("length").and_then(Value::as_i64).filter(|n| *n >= 0).ok_or_else(|| bad("invalid clip length"))?;
    let rate = v.get("rate").ok_or_else(|| bad("missing clip rate"))?;
    let num = rate.get("numerator").and_then(Value::as_i64).filter(|n| *n > 0).ok_or_else(|| bad("invalid rate numerator"))?;
    let den = rate.get("denominator").and_then(Value::as_i64).filter(|n| *n > 0).ok_or_else(|| bad("invalid rate denominator"))?;
    let scaled =
        i128::from(count).checked_mul(i128::from(den)).and_then(|x| x.checked_mul(i128::from(TICKS_PER_SECOND))).ok_or_else(|| bad("timing overflow"))?;
    let quantized = scaled % i128::from(num) != 0;
    let ticks = i64::try_from(scaled / i128::from(num)).map_err(|_| bad("clip duration exceeds tick range"))?;
    if ticks > Tick::MAX.0 {
        return Err(bad("clip duration exceeds supported tick range"));
    }
    Ok((Tick(ticks), quantized))
}
/// Derive the bin label portably, including catalog paths recorded on Windows.
fn bin_name(path: &str) -> Result<String> {
    let file = path.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = file.get(..file.len().saturating_sub(4)).filter(|_| file.get(file.len().saturating_sub(4)..).is_some_and(|x| x.eq_ignore_ascii_case(".avb")));
    let name = stem.filter(|s| !s.trim().is_empty()).ok_or_else(|| bad("bin path must end in a nonempty .avb filename"))?;
    Ok(name.to_owned())
}
/// Validate the complete transfer before starting a project transaction.
pub fn plan(bytes: &[u8]) -> Result<Plan> {
    plan_scoped(bytes, TransferMode::Metadata)
}

/// Parsed catalog shared by catalog and sequence planning. Native imports keep masters once;
/// external schema-2 reports retain their independent mob/catalog cross-checks.
pub struct Document {
    schema_version: u32,
    workflow: String,
    pub(super) bins: Vec<Value>,
    compact_native: bool,
}
impl Document {
    /// Read-only normalized bins, for identity search before binding. No mutable access is exposed.
    pub fn bins(&self) -> &[Value] {
        &self.bins
    }
}
/// Parse external reports once. Unrelated scan inventories are skipped by serde.
pub fn read_document(bytes: &[u8]) -> Result<Document> {
    if bytes.len() > MAX_BYTES {
        return Err(bad("report exceeds 256 MiB"));
    }
    #[derive(serde::Deserialize)]
    struct Report {
        schema_version: u32,
        workflow: String,
        bins: Vec<Value>,
    }
    let report: Report = serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid JSON: {e}")))?;
    let doc = Document { schema_version: report.schema_version, workflow: report.workflow, bins: report.bins, compact_native: false };
    if doc.schema_version != 2 || doc.workflow != "avid_managed_mxf" {
        return Err(bad("expected schema-2 avid_managed_mxf catalog"));
    }
    if doc.bins.is_empty() || doc.bins.len() > MAX_BINS {
        return Err(bad("expected 1–1024 bins"));
    }
    Ok(doc)
}

/// Defer composition diagnostics only when the caller will run the sequence converter.
pub fn plan_scoped(bytes: &[u8], mode: TransferMode) -> Result<Plan> {
    plan_document(&read_document(bytes)?, mode)
}
/// Plan from a previously parsed snapshot without serializing or reparsing it.
pub fn plan_document(doc: &Document, mode: TransferMode) -> Result<Plan> {
    let bins = &doc.bins;
    let warnings =
        if mode == TransferMode::Metadata { vec!["Metadata-only import: media components remain unbound and clips are offline.".into()] } else { Vec::new() };
    let mut result = Plan { bins: Vec::new(), warnings };
    let mut count = 0usize;
    let mut mob_count = 0usize;
    let mut quantized_count = 0usize;
    for b in bins {
        if b.get("status").and_then(Value::as_str) != Some("parsed")
            || b.get("snapshot_stable") != Some(&Value::Bool(true))
            || !array(b, "diagnostics")?.is_empty()
        {
            return Err(bad("bin was not decoded completely from a stable snapshot"));
        }
        let name = bin_name(text(b, "path")?)?;
        let summary = b.get("summary").ok_or_else(|| bad("missing bin summary"))?;
        let uid = text(summary, "bin_uid")?.to_owned();
        let mobs = array(b, "mobs")?;
        let clips: Vec<&Value> = if doc.compact_native {
            mobs.iter().filter(|m| m["kind"] == "MasterMob").collect()
        } else {
            let catalog = b.get("catalog").ok_or_else(|| bad("missing catalog"))?;
            let clips = array(catalog, "clips")?;
            if catalog.get("summary").and_then(|s| s.get("master_count")).and_then(Value::as_u64) != Some(clips.len() as u64) {
                return Err(bad("incomplete master catalog"));
            }
            clips.iter().collect()
        };
        count = count.checked_add(clips.len()).filter(|n| *n <= MAX_CLIPS).ok_or_else(|| bad("too many clips"))?;
        let mut out = BinPlan { name, uid, clips: Vec::new(), referenced: Vec::new(), subclips: Vec::new(), compositions: 0 };
        // Cross-check the derived catalog against independently exported decoded mobs.
        // Its own master_count cannot prove that no master was omitted during assembly.
        mob_count = mob_count.checked_add(mobs.len()).filter(|n| *n <= MAX_MOBS).ok_or_else(|| bad("too many decoded mobs"))?;
        let mut decoded_masters = BTreeMap::new();
        for m in mobs {
            if m.get("kind").and_then(Value::as_str) == Some("MasterMob") {
                let key = identity_key(m)?;
                membership(m)?;
                if decoded_masters.insert(key, m).is_some() {
                    return Err(bad("duplicate decoded master identity in bin"));
                }
            }
        }
        // A placed motion effect is a baked clip: its retime lives in media the render produced.
        // Claim that precompute master for it, so the bin shows the effect at its baked length
        // bound to its own render, and the master is not also imported as a separate source.
        let mut baked: BTreeMap<[u8; 32], Option<String>> = BTreeMap::new();
        let mut unrendered = 0usize;
        let mut contested = 0usize;
        for m in mobs.iter().filter(|m| m["kind"] == "CompositionMob" && m["user_placed"] == true && m["usage_code"] == 6) {
            match precompute_master(m, mobs).and_then(|p| full_identity(&p["id"]).ok()) {
                Some(id) => {
                    // Two effects sharing one render cannot both be it; neither is claimed, and
                    // the precompute stays an ordinary source rather than taking one of the names.
                    if baked.insert(id, Some(text(m, "name")?.to_owned())).is_some() {
                        baked.insert(id, None);
                        contested += 1;
                    }
                }
                // Not rendered, so there is no baked media to show.
                None => unrendered += 1,
            }
        }
        if contested > 0 {
            result.warnings.push(format!(
                "{}: {contested} rendered media are each claimed by more than one motion effect; none was transferred as a baked clip.",
                out.name
            ));
        }
        if unrendered > 0 {
            result
                .warnings
                .push(format!("{}: {unrendered} motion effects have not been rendered, so they have no media of their own and were not imported.", out.name));
        }
        let rate_evidence = crate::avid_sequence::offline_rate_evidence(mobs)?;
        let mut sources = BTreeMap::new();
        for m in mobs.iter().filter(|m| m["kind"] == "SourceMob" && m["live"] == true) {
            if let Ok(id) = full_identity(&m["id"]) {
                // Duplicate live identities cannot authorize offline metadata.
                sources.entry(id).and_modify(|entry| *entry = None).or_insert(Some(m));
            }
        }
        let mut seen = BTreeSet::new();
        let mut transferred_baked = BTreeSet::new();
        for c in clips {
            if c.get("kind").and_then(Value::as_str) != Some("MasterMob") {
                return Err(bad("catalog entry is not a master clip"));
            }
            let name = text(c, "name")?.to_owned();
            let key = identity_key(c)?;
            if !seen.insert(key.clone()) {
                return Err(bad("duplicate master identity in bin"));
            }
            let placed = membership(c)?;
            let decoded = decoded_masters.remove(&key).ok_or_else(|| bad("catalog master absent from decoded bin"))?;
            if membership(decoded)? != placed {
                return Err(bad("catalog master identity or membership disagrees with decoded bin"));
            }
            for field in ["name", "user", "length", "rate", "attributes"] {
                if c.get(field) != decoded.get(field) {
                    return Err(bad(format!("catalog master {field} disagrees with decoded bin")));
                }
            }
            let decoded_tracks = array(decoded, "tracks")?;
            let catalog_tracks = array(c, "tracks")?;
            if catalog_tracks.len() != decoded_tracks.len()
                || catalog_tracks.iter().zip(decoded_tracks).any(|(a, b)| ["index", "component", "properties"].iter().any(|k| a.get(k) != b.get(k)))
            {
                return Err(bad("catalog master tracks disagree with decoded bin"));
            }
            let user = c.get("user").and_then(Value::as_object).ok_or_else(|| bad("missing user metadata"))?;
            if user.contains_key(SOURCE_KEY) {
                return Err(bad("user metadata conflicts with reserved provenance key"));
            }
            for key in user.keys() {
                if key.len() > MAX_TEXT || key.contains('\0') {
                    return Err(bad("invalid metadata key"));
                }
            }
            let tracks = array(c, "tracks")?;
            if tracks.len() > 64 {
                return Err(bad("too many master tracks"));
            }
            for t in tracks {
                if t.get("index").and_then(Value::as_u64).filter(|n| *n > 0 && *n <= 65535).is_none() {
                    return Err(bad("invalid master track number"));
                }
                if !matches!(t.get("status").and_then(Value::as_str), Some("present" | "absent" | "partial" | "ambiguous" | "unresolved")) {
                    return Err(bad("invalid track availability"));
                }
            }
            let (duration, quantized) = duration(c)?;
            if quantized {
                quantized_count = quantized_count.saturating_add(1);
            }
            // Only placed masters are bin contents. A sequence transfer additionally needs the
            // masters its compositions reference, which Avid keeps in the file without placing
            // them; they are kept apart from the bin's own contents.
            if placed || mode == TransferMode::Sequences {
                let mut source_descriptors = Vec::new();
                let mut descriptor_ids = BTreeSet::new();
                for r in tracks.iter().filter_map(|t| t["references"].as_array()).flatten() {
                    if let Ok(id) = full_identity(&r["source_id"])
                        && let Some(Some(m)) = sources.get(&id)
                        && m["descriptor"].is_object()
                    {
                        if r.get("source_descriptor").is_some_and(|d| d != &m["descriptor"]) {
                            return Err(bad("source descriptor disagrees with decoded live source mob"));
                        }
                        if descriptor_ids.insert(id) {
                            source_descriptors.push(serde_json::json!({"id":m["id"],"descriptor":m["descriptor"]}));
                        }
                    }
                }
                let picture_rate_evidence = full_identity(&c["id"]).ok().and_then(|id| rate_evidence.get(&id)).cloned().flatten();
                // A claimed precompute is the motion effect's own media: it belongs in the bin
                // under the effect's name, not among the sources the bin merely references.
                // A master the bin placed is already its own item and keeps its own name; only an
                // unplaced precompute is claimed, and only when exactly one effect claims it.
                let claimed = if placed { None } else { full_identity(&c["id"]).ok().and_then(|id| baked.get(&id)).cloned().flatten() };
                if claimed.is_some() {
                    transferred_baked.insert(full_identity(&c["id"])?);
                }
                let clip = ClipPlan { name: claimed.clone().unwrap_or(name), duration, evidence: c.clone(), source_descriptors, picture_rate_evidence };
                if placed || claimed.is_some() { out.clips.push(clip) } else { out.referenced.push(clip) }
            } else {
                result.warnings.push(format!("{}: referenced master {name} was not imported as a bin item", out.name));
            }
        }
        if !out.referenced.is_empty() {
            result.warnings.push(format!(
                "{}: {} masters referenced by this bin's sequences, but not placed in it, were imported into a \"{REFERENCED_BIN}\" sub-bin so those sequences can resolve and relink their sources.",
                out.name,
                out.referenced.len()
            ));
        }
        if !decoded_masters.is_empty() {
            return Err(bad("catalog omitted decoded masters"));
        }
        // Masters this bin carries, whether placed or referenced: a subclip may only restrict one
        // of these, because its parent has to exist as an item for the range to mean anything.
        let available: BTreeMap<[u8; 32], Tick> =
            out.clips.iter().chain(out.referenced.iter()).filter_map(|c| Some((full_identity(&c.evidence["id"]).ok()?, c.duration))).collect();
        let needed_subclips = if mode == TransferMode::Sequences { referenced_subclips(mobs)? } else { BTreeSet::new() };
        let mut clamped = 0usize;
        for m in mobs {
            let placed = m.get("user_placed") == Some(&Value::Bool(true));
            let needed = m["kind"] == "CompositionMob" && m["usage_code"] == 2 && full_identity(&m["id"]).is_ok_and(|id| needed_subclips.contains(&id));
            if (placed || needed) && m.get("kind").and_then(Value::as_str) != Some("MasterMob") {
                if m["kind"] == "CompositionMob" {
                    // A subclip is bin contents in its own right, not a composition to convert.
                    if m["usage_code"] == 2 {
                        match subclip(m, &available) {
                            Ok((plan, was_clamped)) => {
                                clamped += usize::from(was_clamped);
                                out.subclips.push(plan);
                                continue;
                            }
                            Err(e) => {
                                result.warnings.push(format!("{}: subclip {} was not imported: {e}", out.name, text(m, "name")?));
                                continue;
                            }
                        }
                    }
                    // Count only what sequence conversion will take. A saved effect template,
                    // group clip or motion effect is a composition mob but not a timeline, and
                    // counting one asks the engine to convert a bin that has nothing to convert.
                    if matches!(m["usage_code"].as_i64(), Some(3 | 4 | 6)) {
                        if matches!(m["usage_code"].as_i64(), Some(3 | 4)) {
                            let kind = if m["usage_code"] == 3 { "saved effect template" } else { "group clip" };
                            result.warnings.push(format!("{}: {kind} {} was not imported: unsupported bin item.", out.name, text(m, "name")?));
                        } else if !precompute_master(m, mobs).and_then(|p| full_identity(&p["id"]).ok()).is_some_and(|id| transferred_baked.contains(&id)) {
                            result.warnings.push(format!("{}: motion effect clip {} was not imported as a baked clip; its render was unavailable, unclaimed, or outside this transfer mode.", out.name, text(m, "name")?));
                        }
                        continue;
                    }
                    out.compositions += 1; // bounded by MAX_MOBS
                    if mode == TransferMode::Sequences {
                        continue;
                    }
                }
                result.warnings.push(format!("{}: unsupported bin item {} ({}) was not imported", out.name, text(m, "name")?, text(m, "kind")?));
            }
        }
        if out.subclips.len() > MAX_CLIPS {
            return Err(bad("too many subclips"));
        }
        if clamped > 0 {
            result.warnings.push(format!(
                "{}: {clamped} subclips record a range reaching past the end of the master they restrict; each was shortened to its master's extent and the original range is retained in source metadata.",
                out.name
            ));
        }
        result.bins.push(out);
    }
    if quantized_count > 0 {
        result.warnings.push(format!("For {quantized_count} clips, the recorded Avid rate gives a duration between FilmCraft ticks. Original counts and rates are retained in source metadata."));
    }
    Ok(result)
}
/// Validate depth and ID space before using the project's recursive bin helpers.
pub(crate) fn validate_project(project: &Project, parent: Option<BinId>) -> Result<()> {
    let mut stack = vec![(&project.root, 0usize)];
    let mut ids = BTreeSet::new();
    let mut found = parent.is_none();
    while let Some((bin, depth)) = stack.pop() {
        if depth >= 128 {
            return Err(bad("destination bin tree is too deep"));
        }
        if !ids.insert(bin.id.0) || bin.id.0 >= project.next_id {
            return Err(bad("inconsistent project bin IDs"));
        }
        if Some(bin.id) == parent {
            found = true;
        }
        for child in &bin.children {
            if let BinEntry::Bin(b) = child {
                stack.push((b, depth.saturating_add(1)));
            }
        }
    }
    if !found {
        return Err(bad("destination parent does not exist"));
    }
    if project.items.keys().any(|id| id.0 >= project.next_id || ids.contains(&id.0)) {
        return Err(bad("inconsistent project item IDs"));
    }
    Ok(())
}
/// The masters imported for one Avid bin, as `(placed contents, referenced sequence sources)`.
/// Media binding and sequence conversion must agree on the whole set, or a converted timeline
/// would hold clips that can never be relinked. They are returned apart because the two carry
/// different promises: the bin's own contents are what an import is accountable for, while a
/// referenced source that cannot be bound only costs its sequence clip a placeholder. Other
/// sub-bins are the user's and are never treated as sources.
pub struct ImportedItems {
    /// Masters the Avid bin itself placed. An import is accountable for these.
    pub placed: Vec<filmcraft_project::ItemId>,
    /// Masters its sequences reference without placing, in the referenced sub-bin.
    pub referenced: Vec<filmcraft_project::ItemId>,
    /// Subclips. These have no media of their own: they bind through the master they restrict,
    /// so offering one to media binding would fail the item and, strictly, the whole import.
    /// Sequence conversion still needs them, because a sequence can cut from a subclip.
    pub subclips: Vec<filmcraft_project::ItemId>,
}
pub fn imported_items(project: &Project, bin: BinId) -> Result<ImportedItems> {
    let b = project.root.find_bin(bin).ok_or_else(|| bad("missing imported bin"))?;
    let mut out = ImportedItems { placed: Vec::new(), referenced: Vec::new(), subclips: Vec::new() };
    let sort = |id: filmcraft_project::ItemId, into: &mut Vec<filmcraft_project::ItemId>, subclips: &mut Vec<filmcraft_project::ItemId>| match project
        .item(id)
        .map(|i| &i.kind)
    {
        Some(ItemKind::Media(_)) => into.push(id),
        Some(ItemKind::Subclip { .. }) => subclips.push(id),
        _ => {}
    };
    for child in &b.children {
        match child {
            BinEntry::Item(id) => sort(*id, &mut out.placed, &mut out.subclips),
            BinEntry::Bin(sub) if sub.name == REFERENCED_BIN => {
                for e in &sub.children {
                    if let BinEntry::Item(id) = e {
                        let (mut referenced, mut subclips) = (Vec::new(), Vec::new());
                        sort(*id, &mut referenced, &mut subclips);
                        out.referenced.append(&mut referenced);
                        out.subclips.append(&mut subclips);
                    }
                }
            }
            BinEntry::Bin(_) => {}
        }
    }
    Ok(out)
}
fn allocate_name(names: &mut BTreeSet<String>, base: &str) -> Result<String> {
    if names.insert(base.into()) {
        return Ok(base.into());
    }
    for n in 2..=names.len().saturating_add(2) {
        let name = format!("{base}_{n}");
        if names.insert(name.clone()) {
            return Ok(name);
        }
    }
    Err(bad("cannot allocate bin name"))
}
#[derive(Debug, Serialize)]
pub struct PreviewBin {
    pub name: String,
    pub clips: usize,
    pub referenced: usize,
    pub subclips: usize,
    /// Unplaced subclip dependencies retained in the referenced-sources sub-bin.
    pub referenced_subclips: usize,
}
/// Resolve all names together, accounting for other bins in this import without editing a project.
pub fn preview(project: &Project, plan: &Plan, parent: Option<BinId>) -> Result<Vec<PreviewBin>> {
    validate_project(project, parent)?;
    // One ID per bin and item, plus one for a referenced-sources sub-bin where it is needed.
    let required = plan
        .bins
        .iter()
        .try_fold(0u64, |n, b| {
            n.checked_add(1)?
                .checked_add(b.clips.len() as u64)?
                .checked_add(b.referenced.len() as u64)?
                .checked_add(b.subclips.len() as u64)?
                .checked_add(u64::from(!b.referenced.is_empty() || b.subclips.iter().any(|s| !s.placed)))
        })
        .ok_or_else(|| bad("too many items"))?;
    project.next_id.checked_add(required).ok_or_else(|| bad("project ID space exhausted"))?;
    let bin = match parent {
        Some(id) => project.root.find_bin(id).ok_or_else(|| bad("destination parent does not exist"))?,
        None => &project.root,
    };
    let mut names: BTreeSet<String> = bin.children.iter().filter_map(|x| if let BinEntry::Bin(b) = x { Some(b.name.clone()) } else { None }).collect();
    plan.bins
        .iter()
        .map(|b| {
            Ok(PreviewBin {
                name: allocate_name(&mut names, &b.name)?,
                clips: b.clips.len(),
                referenced: b.referenced.len(),
                subclips: b.subclips.iter().filter(|s| s.placed).count(),
                referenced_subclips: b.subclips.iter().filter(|s| !s.placed).count(),
            })
        })
        .collect()
}
/// Add one offline master as an item of `bin`. `placed` records whether the Avid bin itself
/// listed it, so a referenced-only source is never reported as bin contents.
fn add_master(project: &mut Project, bin: BinId, bin_name: &str, bin_uid: &str, c: ClipPlan, placed: bool) -> Result<filmcraft_project::ItemId> {
    // Deliberately no guessed file path: multi-component sources need a media binding.
    let has_video = c
        .evidence
        .get("tracks")
        .and_then(Value::as_array)
        .is_some_and(|ts| ts.iter().any(|t| t.pointer("/component/media_kind").and_then(Value::as_u64) == Some(1)));
    let info = MediaInfo {
        name: c.name.clone(),
        kind: if has_video { MediaKind::Movie } else { MediaKind::AudioOnly },
        duration: c.duration,
        video: None,
        audio_streams: Vec::new(),
        container: "Avid metadata (unbound)".into(),
        start_timecode: None,
        file_size: None,
    };
    let mid = project.add_item(
        &c.name,
        if placed { Label::Lavender } else { Label::Tan },
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: String::new() },
            info,
            interpret: Interpretation::default(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            offline: true,
            proxy: None,
            identity: None,
        }),
        Some(bin),
    );
    let item = project.item_mut(mid).ok_or_else(|| bad("new item missing"))?;
    if let Some(user) = c.evidence.get("user").and_then(Value::as_object) {
        for (k, v) in user {
            item.metadata.insert(k.clone(), v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()));
        }
    }
    let provenance = serde_json::json!({"version":1,"bin_name":bin_name,"bin_uid":bin_uid,"clip":c.evidence,"source_descriptors":c.source_descriptors,
        "picture_rate_evidence":c.picture_rate_evidence,"binding":"unbound","placed":placed});
    item.metadata.insert(SOURCE_KEY.into(), provenance.to_string());
    Ok(mid)
}
/// Apply a validated plan to a transaction-local project copy; no I/O or AVB dependency remains.
pub fn apply(project: &mut Project, plan: Plan, parent: Option<BinId>) -> Result<Vec<ImportedBin>> {
    let destinations = preview(project, &plan, parent)?;
    let mut imported = Vec::new();
    for (b, destination) in plan.bins.into_iter().zip(destinations) {
        let name = destination.name;
        let id = project.add_bin(&name, parent);
        let count = b.clips.len();
        let referenced = b.referenced.len();
        let subclips = b.subclips.iter().filter(|s| s.placed).count();
        let referenced_subclips = b.subclips.len() - subclips;
        // Identity to item, so a subclip can find the master it restricts. Placed and referenced
        // masters share one space: a subclip does not care which list its master came from.
        let mut masters = BTreeMap::new();
        for c in b.clips {
            let id_bytes = full_identity(&c.evidence["id"]).ok();
            let item = add_master(project, id, &b.name, &b.uid, c, true)?;
            if let Some(k) = id_bytes {
                masters.insert(k, item);
            }
        }
        let source_bin = if !b.referenced.is_empty() || referenced_subclips > 0 { Some(project.add_bin(REFERENCED_BIN, Some(id))) } else { None };
        if let Some(sources) = source_bin {
            // A separate sub-bin: these are sequence sources, not contents Avid showed in the bin.
            for c in b.referenced {
                let id_bytes = full_identity(&c.evidence["id"]).ok();
                let item = add_master(project, sources, &b.name, &b.uid, c, false)?;
                if let Some(k) = id_bytes {
                    masters.insert(k, item);
                }
            }
        }
        // After their masters, so every parent already exists.
        for sub in b.subclips {
            let parent = *masters.get(&sub.parent).ok_or_else(|| bad("subclip master missing after import"))?;
            let item = project.add_item(
                &sub.name,
                Label::Iris,
                ItemKind::Subclip { parent, range: filmcraft_time::TimeRange::new(sub.start, sub.duration), restrict_trims: false },
                Some(if sub.placed { id } else { source_bin.ok_or_else(|| bad("referenced subclip bin missing"))? }),
            );
            let item = project.item_mut(item).ok_or_else(|| bad("new subclip missing"))?;
            let provenance = serde_json::json!({"version":1,"bin_name":b.name,"bin_uid":b.uid,"clip":sub.evidence,
                "source_descriptors":[],"picture_rate_evidence":Value::Null,"binding":"through_parent","placed":sub.placed,"subclip":true});
            item.metadata.insert(SOURCE_KEY.into(), provenance.to_string());
        }
        imported.push(ImportedBin { bin: id, name, clips: count, referenced, subclips, referenced_subclips, compositions: b.compositions });
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::ItemKind;
    #[test]
    fn picture_rate_reconciliation_is_narrow_and_requires_actual_rate() {
        let actual = filmcraft_time::FrameRate::new(30000, 1001);
        for (n, d, expected) in [(2997, 100, true), (30000, 1001, true), (60000, 2002, true), (2998, 100, false), (30, 1, false), (0, 1, false), (24, 0, false)]
        {
            assert_eq!(picture_rate_matches(&serde_json::json!({"rate":{"numerator":n,"denominator":d}}), actual), expected);
        }
        assert!(!picture_rate_matches(&serde_json::json!({"rate":{"numerator":2997,"denominator":100}}), filmcraft_time::FrameRate::FPS_24));
    }
    use serde_json::json;
    fn fixture() -> Value {
        let mut v = json!({"schema_version":2,"workflow":"avid_managed_mxf","bins":[{
            "path":"C:\\Bins\\音声.avb","status":"parsed","snapshot_stable":true,"diagnostics":[],
            "summary":{"bin_uid":"synthetic"},"mobs":[],"catalog":{"summary":{"master_count":1},"clips":[{
                "name":"Source","kind":"MasterMob","user_placed":true,"id":{"raw":"01".repeat(32),"byte_order":"little"},
                "length":1,"rate":{"numerator":2997,"denominator":125},"user":{},"tracks":[]
            }]}
        }]});
        v["bins"][0]["mobs"] = json!([v["bins"][0]["catalog"]["clips"][0].clone()]);
        v
    }
    fn decode(v: &Value) -> Result<Plan> {
        plan(&serde_json::to_vec(v).unwrap())
    }
    /// Avid writes a subclip's range two ways and reading either alone is wrong somewhere. The
    /// numbers here are from a fixture whose in and out points a person set deliberately: a
    /// 334-frame master with a subclip over frames 47..83, which both forms must reproduce.
    fn subclip_fixture(attribute_form: bool) -> Value {
        let mut v = fixture();
        let master = v["bins"][0]["catalog"]["clips"][0].clone();
        v["bins"][0]["catalog"]["clips"][0]["length"] = json!(334);
        v["bins"][0]["mobs"][0]["length"] = json!(334);
        let rate = master["rate"].clone();
        // Attribute form: the component spans the whole master and the mob's length is the
        // master's, so only _START/_END describe the subclip. Component form: the reverse.
        let (component_start, mob_length, attributes) =
            if attribute_form { (0, 334, json!({"_START": 47, "_END": 83})) } else { (47, 36, json!({"_START": 0, "_END": 36})) };
        let sub = json!({"kind":"CompositionMob","name":"B.sub.01","live":true,"user_placed":true,
            "id":{"raw":"03".repeat(32),"byte_order":"little"},"length":mob_length,"rate":rate,"usage_code":2,
            "attributes":attributes,
            "tracks":[{"index":1,"component":{"class":"SCLP","media_kind":1,"length":mob_length,"rate":master["rate"],
                "source_id":master["id"],"source_track":1,"start":component_start}}]});
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(sub);
        v
    }
    #[test]
    fn a_subclip_imports_the_range_avid_recorded_in_either_form() {
        for attribute_form in [true, false] {
            let v = subclip_fixture(attribute_form);
            let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
            assert_eq!(plan.bins[0].subclips.len(), 1, "attribute_form={attribute_form}");
            let mut project = Project::default();
            let bins = apply(&mut project, plan, None).unwrap();
            assert_eq!((bins[0].clips, bins[0].subclips), (1, 1));
            let (parent, range) = project
                .items
                .values()
                .find_map(|i| match &i.kind {
                    ItemKind::Subclip { parent, range, .. } => Some((*parent, *range)),
                    _ => None,
                })
                .expect("a subclip item");
            // The parent is the master, and the range is 47..83 of its 334 frames, whichever way
            // Avid recorded it. Expected ticks are computed from the fixture's 2997/125 rate
            // directly, so this checks the range that was chosen rather than the conversion.
            let at = |frames: i128| (frames * 125 * i128::from(TICKS_PER_SECOND) / 2997) as i64;
            assert_eq!(range.start.0, at(47), "start, attribute_form={attribute_form}");
            assert_eq!(range.duration.0, at(36), "duration, attribute_form={attribute_form}");
            let master = project.item(parent).unwrap().as_media().unwrap();
            assert!(range.end() <= master.info.duration, "the range stays inside its master");
        }
    }
    /// A rendered motion effect: the placed effect points through a source clip at a precompute
    /// master, which is what carries the baked media.
    fn motion_effect_fixture(rendered: bool) -> Value {
        let mut v = fixture();
        let master = v["bins"][0]["catalog"]["clips"][0].clone();
        let rate = master["rate"].clone();
        let precompute = json!({"kind":"MasterMob","name":"Clip,Motion_Effect+1","user_placed":false,
            "id":{"raw":"0a".repeat(32),"byte_order":"little"},"length":24,"rate":rate,"usage_code":1,
            "user":{},"tracks":[{"index":1,"status":"unresolved","component":{"class":"SCLP","media_kind":1,"length":24,"rate":rate},"references":[]}]});
        let pointer = if rendered { json!({"class_raw":[83,67,76,80],"object_index":9,"source_id":precompute["id"]}) } else { Value::Null };
        let effect = json!({"kind":"CompositionMob","name":"Fast (48.00 FPS)","live":true,"user_placed":true,
            "id":{"raw":"0b".repeat(32),"byte_order":"little"},"length":24,"rate":rate,"usage_code":6,
            "tracks":[{"index":1,"component":{"class":"SPED","media_kind":1,"length":24,"rate":rate,
                "properties":{"effect_id":"EFF_ADV_MOTION_CTL","precomputed":pointer}}}]});
        v["bins"][0]["catalog"]["clips"].as_array_mut().unwrap().push(precompute.clone());
        v["bins"][0]["catalog"]["summary"]["master_count"] = json!(2);
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(precompute);
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(effect);
        v
    }
    #[test]
    fn a_rendered_motion_effect_is_its_baked_clip() {
        // The effect is a baked clip: it belongs in the bin at its retimed length, carrying the
        // media the render produced, so it binds and plays like any other managed master.
        let v = motion_effect_fixture(true);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        let mut project = Project::default();
        let bins = apply(&mut project, plan, None).unwrap();
        // The precompute is bin contents under the effect's name, not a referenced source.
        assert_eq!((bins[0].clips, bins[0].referenced), (2, 0));
        let item = project.items.values().find(|i| i.name == "Fast (48.00 FPS)").expect("the effect, named as the bin shows it");
        let media = item.as_media().expect("a media item carrying the baked render");
        // The baked length, from the fixture's 2997/125 rate: the retime is already in the media.
        assert_eq!(media.info.duration.0, (24i128 * 125 * i128::from(TICKS_PER_SECOND) / 2997) as i64, "the effect shows its retimed length");
        // Its evidence is the precompute's, so identity binding finds the rendered file.
        let provenance: Value = serde_json::from_str(&item.metadata[SOURCE_KEY]).unwrap();
        assert_eq!(provenance["clip"]["id"]["raw"], json!("0a".repeat(32)), "bound through the precompute master");
        assert!(project.items.values().all(|i| i.name != "Clip,Motion_Effect+1"), "the precompute is not also imported separately");
    }
    #[test]
    fn a_contested_or_placed_precompute_is_never_renamed_or_claimed() {
        // Two effects cannot both be one render: neither claims it, and it keeps its own name.
        let mut v = motion_effect_fixture(true);
        let mut second = v["bins"][0]["mobs"].as_array().unwrap().last().unwrap().clone();
        second["name"] = json!("Also fast");
        second["id"]["raw"] = json!("0c".repeat(32));
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(second);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("claimed by more than one motion effect")), "{:?}", plan.warnings);
        let mut project = Project::default();
        apply(&mut project, plan, None).unwrap();
        assert!(project.items.values().any(|i| i.name == "Clip,Motion_Effect+1"), "the render keeps its own name");
        for name in ["Fast (48.00 FPS)", "Also fast"] {
            assert!(project.items.values().all(|i| i.name != name), "{name} must not have claimed it");
        }

        // A master the bin placed keeps its own name even if an effect points at it.
        let mut v = motion_effect_fixture(true);
        let last = v["bins"][0]["catalog"]["clips"].as_array().unwrap().len() - 1;
        v["bins"][0]["catalog"]["clips"][last]["user_placed"] = json!(true);
        let mobs = v["bins"][0]["mobs"].as_array().unwrap().len();
        for m in 0..mobs {
            if v["bins"][0]["mobs"][m]["name"] == json!("Clip,Motion_Effect+1") {
                v["bins"][0]["mobs"][m]["user_placed"] = json!(true);
            }
        }
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        let mut project = Project::default();
        apply(&mut project, plan, None).unwrap();
        assert!(project.items.values().any(|i| i.name == "Clip,Motion_Effect+1"), "a placed master is not renamed");
    }
    #[test]
    fn tracks_naming_different_renders_are_not_one_baked_clip() {
        // Picking one of them would bind the item to the wrong media.
        let mut v = motion_effect_fixture(true);
        let effect = v["bins"][0]["mobs"].as_array().unwrap().len() - 1;
        let mut other = v["bins"][0]["mobs"][effect]["tracks"][0].clone();
        other["index"] = json!(2);
        other["component"]["properties"]["precomputed"]["source_id"]["raw"] = json!("0f".repeat(32));
        v["bins"][0]["mobs"][effect]["tracks"].as_array_mut().unwrap().push(other);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("have not been rendered")), "{:?}", plan.warnings);
        let mut project = Project::default();
        apply(&mut project, plan, None).unwrap();
        assert!(project.items.values().all(|i| i.name != "Fast (48.00 FPS)"));
    }
    #[test]
    fn an_unrendered_motion_effect_has_no_media_to_show() {
        let v = motion_effect_fixture(false);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("have not been rendered")), "{:?}", plan.warnings);
        let mut project = Project::default();
        let bins = apply(&mut project, plan, None).unwrap();
        assert!(project.items.values().all(|i| i.name != "Fast (48.00 FPS)"));
        // Nothing claimed the precompute, so it stays an ordinary referenced source.
        assert_eq!(bins[0].referenced, 1);
    }
    #[test]
    fn a_subclip_reaching_past_its_master_is_bounded_by_it() {
        // Recorded ranges do run past a master's end, by a frame or by dozens, in a real project.
        // The item must not reach past its own media.
        let mut v = subclip_fixture(true);
        v["bins"][0]["mobs"][1]["attributes"]["_END"] = json!(400); // master is 334 frames
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert_eq!(plan.bins[0].subclips.len(), 1);
        assert!(plan.warnings.iter().any(|w| w.contains("reaching past the end of the master")), "{:?}", plan.warnings);
        let mut project = Project::default();
        apply(&mut project, plan, None).unwrap();
        let (parent, range) = project
            .items
            .values()
            .find_map(|i| match &i.kind {
                ItemKind::Subclip { parent, range, .. } => Some((*parent, *range)),
                _ => None,
            })
            .expect("a subclip item");
        let master = project.item(parent).unwrap().as_media().unwrap();
        assert_eq!(range.end(), master.info.duration, "shortened to exactly the master's extent");
        assert!(range.duration > Tick::ZERO);
        // Starting past the end is refused outright rather than clamped to nothing.
        let mut v = subclip_fixture(true);
        v["bins"][0]["mobs"][1]["attributes"]["_START"] = json!(500);
        v["bins"][0]["mobs"][1]["attributes"]["_END"] = json!(600);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.bins[0].subclips.is_empty());
        assert!(plan.warnings.iter().any(|w| w.contains("starts after the end of its master")), "{:?}", plan.warnings);
        // An offset with no usable end is refused, never stretched to the master's length.
        let mut v = subclip_fixture(true);
        v["bins"][0]["mobs"][1]["attributes"]["_END"] = json!(47);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.bins[0].subclips.is_empty());
        assert!(plan.warnings.iter().any(|w| w.contains("records a start without a usable end")), "{:?}", plan.warnings);
    }
    #[test]
    fn a_composition_that_is_not_a_plain_range_is_reported_rather_than_approximated() {
        // No master in this bin to restrict.
        let mut v = subclip_fixture(true);
        v["bins"][0]["mobs"][1]["tracks"][0]["component"]["source_id"]["raw"] = json!("09".repeat(32));
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.bins[0].subclips.is_empty());
        assert!(plan.warnings.iter().any(|w| w.contains("its master is not in this bin")), "{:?}", plan.warnings);
        // An empty range is refused rather than imported as a zero-length item.
        let mut v = subclip_fixture(true);
        v["bins"][0]["mobs"][1]["attributes"]["_END"] = json!(47);
        v["bins"][0]["mobs"][1]["length"] = json!(0);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.bins[0].subclips.is_empty());
        assert!(plan.warnings.iter().any(|w| w.contains("subclip B.sub.01 was not imported")), "{:?}", plan.warnings);
        // Tracks disagreeing about which master, or where, is not one range.
        let mut v = subclip_fixture(true);
        let mut second = v["bins"][0]["mobs"][1]["tracks"][0].clone();
        second["index"] = json!(2);
        second["component"]["start"] = json!(9);
        v["bins"][0]["mobs"][1]["tracks"].as_array_mut().unwrap().push(second);
        let plan = plan_scoped(&serde_json::to_vec(&v).unwrap(), TransferMode::Sequences).unwrap();
        assert!(plan.bins[0].subclips.is_empty());
    }
    #[test]
    fn decimal_rate_is_preserved_with_explicit_subtick_quantization() {
        let v = fixture();
        let p = decode(&v).unwrap();
        assert_eq!(p.bins[0].clips[0].duration.0, (i128::from(TICKS_PER_SECOND) * 125 / 2997) as i64);
        assert_eq!(p.bins[0].clips[0].evidence["rate"], v["bins"][0]["catalog"]["clips"][0]["rate"]);
        assert!(p.warnings.iter().any(|w| w.contains("between FilmCraft ticks")));
        assert_eq!(p.bins[0].name, "音声");
    }
    #[test]
    fn invalid_catalogs_rejected_and_no_mutation_panics() {
        let cases = [
            ("/schema_version", json!(1)),
            ("/bins/0/snapshot_stable", json!(false)),
            ("/bins/0/diagnostics", json!(["incomplete"])),
            ("/bins/0/path", json!("😀")),
            ("/bins/0/catalog/summary/master_count", json!(2)),
            ("/bins/0/catalog/clips/0/length", json!(-1)),
            ("/bins/0/catalog/clips/0/length", json!(i64::MAX)),
            ("/bins/0/catalog/clips/0/rate/numerator", json!(0)),
            ("/bins/0/catalog/clips/0/rate/denominator", json!(0)),
            ("/bins/0/catalog/clips/0/id/raw", json!("urn:smpte:umid:01")),
            ("/bins/0/catalog/clips/0/id/byte_order", json!("middle")),
            ("/bins/0/catalog/clips/0/user", json!({SOURCE_KEY:"reserved"})),
            ("/bins/0/catalog/clips/0/user_placed", json!(null)),
            ("/bins/0/catalog/clips/0/tracks", json!([{"index":-1,"status":"present"}])),
        ];
        for (pointer, value) in cases {
            let mut v = fixture();
            *v.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&v).is_err(), "{pointer}");
        }
        let mut v = fixture();
        v["bins"][0]["catalog"]["clips"][0]["length"] = json!(i64::MAX);
        v["bins"][0]["catalog"]["clips"][0]["rate"]["denominator"] = json!(i64::MAX);
        assert!(decode(&v).is_err());
        let bytes = serde_json::to_vec(&fixture()).unwrap();
        for end in 0..bytes.len() {
            assert!(std::panic::catch_unwind(|| plan(&bytes[..end])).unwrap().is_err());
        }
        for pos in 0..bytes.len() {
            let mut changed = bytes.clone();
            changed[pos] ^= 0xff;
            assert!(std::panic::catch_unwind(|| plan(&changed)).is_ok());
        }
        let deep = format!("{}0{}", "[".repeat(256), "]".repeat(256));
        assert!(std::panic::catch_unwind(|| plan(deep.as_bytes())).unwrap().is_err());
    }
    #[test]
    fn empty_repeated_bins_and_unsupported_items_are_explicit() {
        let mut v = fixture();
        v["bins"][0]["catalog"]["clips"] = json!([]);
        v["bins"][0]["catalog"]["summary"]["master_count"] = json!(0);
        v["bins"][0]["mobs"] = json!([{"kind":"CompositionMob","name":"Sequence","user_placed":true}]);
        let b = v["bins"][0].clone();
        v["bins"].as_array_mut().unwrap().push(b);
        let p = decode(&v).unwrap();
        assert_eq!(p.warnings.len(), 3);
        let mut project = Project::new("Test");
        let out = apply(&mut project, p, None).unwrap();
        assert_eq!(out[0].name, "音声");
        assert_eq!(out[1].name, "音声_2");
        assert!(project.items.is_empty());
    }
    #[test]
    fn duplicate_master_identity_is_not_silently_deduplicated() {
        let mut v = fixture();
        let c = v["bins"][0]["catalog"]["clips"][0].clone();
        v["bins"][0]["catalog"]["clips"].as_array_mut().unwrap().push(c);
        v["bins"][0]["catalog"]["summary"]["master_count"] = json!(2);
        assert!(decode(&v).unwrap_err().to_string().contains("duplicate"));
    }
    #[test]
    fn catalog_must_account_for_decoded_masters_and_membership() {
        let mut v = fixture();
        v["bins"][0]["catalog"]["clips"] = json!([]);
        v["bins"][0]["catalog"]["summary"]["master_count"] = json!(0);
        assert!(decode(&v).is_err(), "a self-consistent count must not conceal an omitted master");
        let mut v = fixture();
        v["bins"][0]["catalog"]["clips"][0]["user_placed"] = json!(false);
        assert!(decode(&v).is_err(), "catalog membership must agree with the decoded bin");
    }
    #[test]
    fn external_reports_cannot_claim_native_validation_privileges() {
        let mut v = fixture();
        v["compact_native"] = serde_json::json!(true);
        v["bins"][0]["catalog"]["clips"] = serde_json::json!([]);
        v["bins"][0]["catalog"]["summary"]["master_count"] = serde_json::json!(0);
        let doc = read_document(&serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(plan_document(&doc, TransferMode::Sequences).is_err());
    }
    #[test]
    fn mixed_byte_order_duplicate_master_is_rejected() {
        let mut v = fixture();
        let raw = "060a2b340101010501010f101300000002385d50221306a105dc1c1dd3d90a1b";
        let canonical = "060a2b340101010501010f1013000000505d38021322a10605dc1c1dd3d90a1b";
        v["bins"][0]["catalog"]["clips"][0]["id"]["raw"] = json!(raw);
        let mut second = v["bins"][0]["catalog"]["clips"][0].clone();
        second["id"] = json!({"raw":canonical,"byte_order":"big"});
        v["bins"][0]["catalog"]["clips"].as_array_mut().unwrap().push(second);
        v["bins"][0]["catalog"]["summary"]["master_count"] = json!(2);
        v["bins"][0]["mobs"] = v["bins"][0]["catalog"]["clips"].clone();
        assert!(decode(&v).is_err(), "byte-order aliases must not import twice");
    }
    #[test]
    fn invalid_project_id_counter_is_rejected_before_changes() {
        for next in [0, u64::MAX] {
            let mut project = Project::new("Test");
            project.next_id = next;
            let before = project.clone();
            assert!(apply(&mut project, decode(&fixture()).unwrap(), None).is_err());
            assert_eq!(project, before);
        }
    }
}

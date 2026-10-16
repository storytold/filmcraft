//! The document knowledge the hub validates against (spec §3.6 phase A: `DocSchema`): which
//! kinds exist, which parents accept children, the keyed document lists that sync per entry with
//! their order, and — this app's own row in the adoption table — the **region locks of the
//! `TimelineRange` shape** (spec §6.7, §18.4 row 8).
//!
//! ## The lock shape (spec §6.7: locks sync as document properties)
//!
//! FilmCraft locks a *region of the timeline*, not an object: an editor holds a time range on
//! one track while they trim inside it. The lock set is one document property, `locks`, an array
//! of TimelineRange locks:
//!
//! ```json
//! [{ "track": 12, "start": 254016000000, "duration": 508032000000, "holder": 7 }]
//! ```
//!
//! * `track` — the track object's id (the locked region's track; a clip edit is scoped by the
//!   track its clip sits on, which the core's generic tree knows as the clip's parent).
//! * `start`, `duration` — sequence ticks, the document's canonical time (the same numbers the
//!   `start`/`duration` clip properties carry).
//! * `holder` — the holding client's id (its id space; `0` marks a lock loaded from a file,
//!   held by nobody in this session).
//!
//! The whole array is one last-writer-wins document property — the session resolves races by
//! first-holder-wins before it ever writes (spec §6.7's rule), so a concurrent `SetDoc` is only
//! ever a release or a re-acquire that lost the race. [`FilmSchema::validate`] refuses a clip
//! edit whose track and range intersect a lock held by another client: that is the region lock,
//! enforced where every batch already passes.
//!
//! ## The deferred LUT asset (spec §3.5: bytes never travel in batches)
//!
//! The LUT library is the one genuinely heavy payload embedded in a FilmCraft project (`.cube`
//! and `.3dl` text, routinely hundreds of kilobytes — "LUT files are embedded, so projects
//! render without the original files"). Each LUT therefore syncs as **two entries**: `Lut:<id>`
//! (the metadata: name, source path, format) and `LutText:<id>` whose value is an
//! [`craft_collab::value::AssetRef`] — the SHA-256 and byte length of the text, content-addressed,
//! riding the asset channel, never a batch. `FilmcraftHub::offer_asset` hands the adapter the
//! bytes when the channel delivers them; until then a `LutText` put is skipped (not corrupted),
//! and the session replays it after the asset lands. Media files themselves never entered the
//! document at all — they are paths (`media` property), read from disk.

use craft_collab::adapter::ValidationCtx;
use craft_collab::change::{Change, EntryId};
use craft_collab::limits::Limits;
use craft_collab::value::{AssetRef, Value};
use filmcraft_project::{Narration, Project, SourceGraphic, Transcript};
use serde::{Deserialize, Serialize};

use crate::props;
use crate::{CONTAINERS, KINDS, ROOT};

/// One region lock of the TimelineRange shape (the JSON form the `locks` property carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineLock {
    /// The track the lock covers (the track object's id).
    pub track: u64,
    /// Timeline start, in sequence ticks.
    pub start: i64,
    /// Covered duration, in sequence ticks.
    pub duration: i64,
    /// The holding client's id space (`0` = loaded from a file, held by nobody).
    pub holder: u32,
}

/// The filmcraft document schema.
#[derive(Debug, Default)]
pub struct FilmSchema {
    limits: Limits,
}

impl craft_collab::adapter::DocSchema for FilmSchema {
    fn validate(&self, change: &Change, ctx: &ValidationCtx<'_>) -> Result<(), String> {
        match change {
            Change::Create { kind, parent, props, .. } => {
                if !KINDS.contains(&kind.as_ref()) {
                    return Err(format!("unknown kind '{kind}'"));
                }
                if *parent == ROOT {
                    // The root bin is the file root: its children (bins and items) attach here.
                    if !crate::ROOT_CHILD_KINDS.contains(&kind.as_ref()) {
                        return Err(format!("'{kind}' does not attach to the file root; only bins and items do"));
                    }
                } else {
                    let p = ctx.replica.node(*parent).ok_or_else(|| format!("unknown parent {parent:?}"))?;
                    if !CONTAINERS.contains(&p.kind.as_ref()) {
                        return Err(format!("a '{}' accepts no children", p.kind));
                    }
                    if !crate::container_accepts(p.kind.as_ref(), kind.as_ref()) {
                        return Err(format!("a '{}' accepts no '{}' children", p.kind, kind));
                    }
                }
                for (p, _) in props {
                    if p.as_ref() != "parent" && p.as_ref() != "position" && !kind_props(kind.as_ref()).contains(&p.as_ref()) {
                        return Err(format!("unknown property '{p}' for a '{kind}'"));
                    }
                }
                // A clip-shaped creation must not land inside a foreign region lock: the Create's
                // parent is the track, its start/duration the range.
                if matches!(kind.as_ref(), "clip" | "transition" | "caption")
                    && let (Some(start), Some(duration)) = (prop_i64(props, "start"), prop_i64(props, "duration"))
                {
                    check_locks(*parent, start, duration, ctx)?;
                }
                Ok(())
            }
            Change::Set { id, prop, value, .. } => {
                if prop.as_ref() == "parent" || prop.as_ref() == "position" {
                    return Ok(());
                }
                let node = ctx.replica.node(*id).ok_or_else(|| format!("unknown object {id:?}"))?;
                if !kind_props(node.kind.as_ref()).contains(&prop.as_ref()) {
                    return Err(format!("unknown property '{prop}' for a '{}'", node.kind));
                }
                // A time edit inside a region held by someone else is the lock's whole point:
                // the touched range is the union of the current and the written one.
                if matches!(node.kind.as_ref(), "clip" | "transition" | "caption") {
                    let start = if prop.as_ref() == "start" { value_i64(value) } else { node.props.get("start").and_then(value_i64) };
                    let duration = if prop.as_ref() == "duration" { value_i64(value) } else { node.props.get("duration").and_then(value_i64) };
                    if let (Some(start), Some(duration)) = (start, duration) {
                        check_locks(node.parent, start, duration, ctx)?;
                    }
                }
                Ok(())
            }
            Change::Delete { id } => {
                let node = ctx.replica.node(*id).ok_or_else(|| format!("unknown object {id:?}"))?;
                if matches!(node.kind.as_ref(), "clip" | "transition" | "caption")
                    && let (Some(start), Some(duration)) = (node.props.get("start").and_then(value_i64), node.props.get("duration").and_then(value_i64))
                {
                    check_locks(node.parent, start, duration, ctx)?;
                }
                Ok(())
            }
            Change::SetDoc { prop, value } => {
                if props::DOC_PROPS.contains(&prop.as_ref()) || is_order_prop(prop) {
                    if prop.as_ref() == "locks" {
                        parse_locks(value).map(|_| ())?;
                    }
                    return Ok(());
                }
                Err(format!("unknown document property '{prop}' (value {value:?})"))
            }
            Change::Put { entry, .. } | Change::Remove { entry } => match entry_kind(&entry.0) {
                Some("lut") | Some("lutText") | Some("transcript") | Some("searchBin") | Some("sourceGraphic") | Some("narration") => Ok(()),
                _ => Err(format!("unknown entry kind in '{entry:?}'")),
            },
        }
    }

    fn recompute(&self, _touched: &[craft_collab::ids::NodeId]) -> Vec<Change> {
        // FilmCraft derives nothing automatically today: track order is derived from clip starts
        // (the document's own invariant, `Track::sort`), and the adapter restores that sort
        // inline on apply rather than emitting a follow-up batch, because the sort is
        // deterministic and local — every replica converges on it without a broadcast. Rendered
        // frames, waveforms, peak files and proxies are engine-side derived data that never
        // syncs (the sync matrix's derived row). When a derived property lands, its recompute
        // belongs here and nowhere else.
        Vec::new()
    }

    fn limits(&self) -> &Limits {
        &self.limits
    }
}

impl FilmSchema {
    // (the lock checks live in the free functions below; the schema itself is stateless)
}

/// Whether two tick ranges overlap (a zero duration still guards its start point).
fn overlaps(a_start: i64, a_dur: i64, b_start: i64, b_dur: i64) -> bool {
    a_start < b_start.saturating_add(b_dur.max(0)) && b_start < a_start.saturating_add(a_dur.max(0))
}

/// Refuse a touched range on `track` that intersects a foreign region lock (spec §6.7).
fn check_locks(track: craft_collab::ids::NodeId, start: i64, duration: i64, ctx: &ValidationCtx<'_>) -> Result<(), String> {
    let Ok(locks) = ctx.replica.doc_prop("locks").map_or_else(|| Ok(Vec::new()), parse_locks) else {
        return Ok(()); // a malformed lock set is the writer's problem, not this edit's
    };
    for lock in locks {
        if lock.holder != 0 && lock.holder != ctx.author_space && lock.track == track.0 && overlaps(lock.start, lock.duration, start, duration) {
            return Err(format!("region locked by client {} (track {}, {} ticks)", lock.holder, lock.track, lock.duration));
        }
    }
    Ok(())
}

/// An i64 property from a Create's props list.
fn prop_i64(props: &[(std::sync::Arc<str>, Value)], name: &str) -> Option<i64> {
    props.iter().find(|(p, _)| p.as_ref() == name).and_then(|(_, v)| value_i64(v))
}

/// An i64 out of a JSON value (numbers only; hostile input is refused, never coerced).
fn value_i64(v: &Value) -> Option<i64> {
    v.to_json().as_i64()
}

/// Parse the `locks` document property (wrong shapes are errors, never panics).
pub fn parse_locks(value: &Value) -> Result<Vec<TimelineLock>, String> {
    let json = value.to_json();
    let locks: Vec<TimelineLock> = serde_json::from_value(json).map_err(|e| format!("malformed lock set: {e}"))?;
    for l in &locks {
        if l.duration < 0 {
            return Err(format!("lock duration must not be negative ({l:?})"));
        }
    }
    Ok(locks)
}

/// The locks as their wire value.
pub fn locks_value(locks: &[TimelineLock]) -> Value {
    Value::Json(serde_json::to_value(locks).unwrap_or(serde_json::Value::Array(Vec::new())))
}

/// Which list an entry id belongs to.
fn entry_kind(entry: &str) -> Option<&'static str> {
    if entry.starts_with("LutText:") {
        Some("lutText")
    } else if entry.starts_with("Lut:") {
        Some("lut")
    } else if entry.starts_with("Transcript:") {
        Some("transcript")
    } else if entry.starts_with("SearchBin:") {
        Some("searchBin")
    } else if entry.starts_with("SourceGraphic:") {
        Some("sourceGraphic")
    } else if entry.starts_with("Narration:") {
        Some("narration")
    } else {
        None
    }
}

/// Whether `prop` is one of the per-list order properties.
fn is_order_prop(prop: &str) -> bool {
    matches!(prop, "lutOrder" | "searchBinOrder")
}

/// The property table of one kind (the item payload tables are per-kind).
fn kind_props(kind: &str) -> Vec<&'static str> {
    match kind {
        "bin" => props::BIN_PROPS.to_vec(),
        "media" | "sequence" | "subclip" | "adjustment" | "graphic" => props::item_props(&default_item_kind(kind)),
        "videoTrack" | "audioTrack" | "submixTrack" => props::TRACK_PROPS.to_vec(),
        "captionTrack" => props::CAPTION_TRACK_PROPS.to_vec(),
        "clip" => props::CLIP_PROPS.to_vec(),
        "transition" => props::TRANSITION_PROPS.to_vec(),
        "caption" => props::CAPTION_PROPS.to_vec(),
        _ => Vec::new(),
    }
}

/// A stand-in item kind for a kind string (schema-side only: it picks the payload table).
fn default_item_kind(kind: &str) -> filmcraft_project::ItemKind {
    match kind {
        "media" => filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::File { path: String::new() },
            info: filmcraft_media::MediaInfo {
                name: String::new(),
                kind: filmcraft_media::MediaKind::Still,
                duration: filmcraft_time::Tick::ZERO,
                video: None,
                audio_streams: Vec::new(),
                container: String::new(),
                start_timecode: None,
                file_size: None,
            },
            interpret: filmcraft_project::Interpretation::default(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            offline: false,
            proxy: None,
            identity: None,
        }),
        "sequence" => filmcraft_project::ItemKind::Sequence(std::sync::Arc::new(crate::empty_sequence())),
        "subclip" => filmcraft_project::ItemKind::Subclip {
            parent: filmcraft_project::ItemId(0),
            range: filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::Tick::ZERO),
            restrict_trims: false,
        },
        "adjustment" => filmcraft_project::ItemKind::AdjustmentLayer {
            width: 0,
            height: 0,
            rate: filmcraft_time::FrameRate::default(),
            duration: filmcraft_time::Tick::ZERO,
        },
        _ => filmcraft_project::ItemKind::Graphic { width: 0, height: 0, rate: filmcraft_time::FrameRate::default() },
    }
}

/// The LUT metadata that rides the `Lut:<id>` entry (the text never does — see the module docs).
#[derive(Serialize, Deserialize)]
struct LutMeta {
    id: String,
    name: String,
    source_path: Option<String>,
    format: String,
}

/// Upsert one document-level entry. The `LutText` kind needs the asset bytes: `assets` holds
/// what the session has fetched so far (`offer_asset`), and a text whose bytes have not arrived
/// yet is an error the caller counts as skipped — deferred, not lost.
pub fn put_entry(doc: &mut Project, entry: &EntryId, value: &Value, assets: &std::collections::BTreeMap<String, std::sync::Arc<str>>) -> Result<(), String> {
    let key = entry.0.as_ref();
    match entry_kind(key) {
        Some("lut") => {
            let meta: LutMeta = serde_json::from_value(value.to_json()).map_err(|e| e.to_string())?;
            let id = key.strip_prefix("Lut:").unwrap_or(key).to_string();
            let text = doc.luts.iter().find(|l| l.id == id).map_or_else(std::sync::Arc::default, |l| l.text.clone());
            let lut = filmcraft_project::ProjectLut { id: id.clone(), name: meta.name, source_path: meta.source_path, format: meta.format, text };
            upsert(doc, |d| &mut d.luts, |l| l.id.clone(), lut, &id)
        }
        Some("lutText") => {
            let Value::AssetRef(asset) = value else {
                return Err("the LUT text entry must be an asset reference".into());
            };
            let id = key.strip_prefix("LutText:").unwrap_or(key).to_string();
            let text = assets.get(&asset.sha256).ok_or_else(|| format!("LUT text asset {} has not arrived yet (deferred)", asset.sha256))?;
            let lut = doc.luts.iter_mut().find(|l| l.id == id).ok_or_else(|| format!("LUT '{id}' has no metadata entry yet"))?;
            lut.text = text.clone();
            Ok(())
        }
        Some("transcript") => {
            let n = key.strip_prefix("Transcript:").unwrap_or(key);
            let item = n.parse::<u64>().map_err(|e| e.to_string()).map(filmcraft_project::ItemId)?;
            let transcript: Transcript = serde_json::from_value(value.to_json()).map_err(|e| e.to_string())?;
            doc.transcripts.insert(item, std::sync::Arc::new(transcript));
            Ok(())
        }
        Some("searchBin") => {
            let bin: filmcraft_project::SearchBin = serde_json::from_value(value.to_json()).map_err(|e| e.to_string())?;
            let id = key.strip_prefix("SearchBin:").unwrap_or(key).to_string();
            upsert(doc, |d| &mut d.search_bins, |b| b.id.0.to_string(), bin, &id)
        }
        Some("sourceGraphic") => {
            let n = key.strip_prefix("SourceGraphic:").unwrap_or(key);
            let item = n.parse::<u64>().map_err(|e| e.to_string()).map(filmcraft_project::ItemId)?;
            let graphic: SourceGraphic = serde_json::from_value(value.to_json()).map_err(|e| e.to_string())?;
            doc.source_graphics.insert(item, graphic);
            Ok(())
        }
        Some("narration") => {
            let n = key.strip_prefix("Narration:").unwrap_or(key);
            let item = n.parse::<u64>().map_err(|e| e.to_string()).map(filmcraft_project::ItemId)?;
            let narration: Narration = serde_json::from_value(value.to_json()).map_err(|e| e.to_string())?;
            doc.narrations.insert(item, narration);
            Ok(())
        }
        _ => Err(format!("unknown entry kind in '{key}'")),
    }
}

/// Remove one document-level entry (absent entries are fine — the tolerant v2 rule). Removing a
/// `LutText` clears the text (the LUT renders identity until the asset lands again); removing a
/// `Lut` removes the whole LUT.
pub fn remove_entry(doc: &mut Project, entry: &EntryId) -> Result<(), String> {
    let key = entry.0.as_ref();
    match entry_kind(key) {
        Some("lut") => {
            let id = key.strip_prefix("Lut:").unwrap_or(key);
            doc.luts.retain(|l| l.id != id);
            Ok(())
        }
        Some("lutText") => {
            let id = key.strip_prefix("LutText:").unwrap_or(key).to_string();
            if let Some(lut) = doc.luts.iter_mut().find(|l| l.id == id) {
                lut.text = std::sync::Arc::default();
            }
            Ok(())
        }
        Some("transcript") => {
            let n = key.strip_prefix("Transcript:").unwrap_or(key);
            if let Ok(item) = n.parse::<u64>() {
                doc.transcripts.remove(&filmcraft_project::ItemId(item));
            }
            Ok(())
        }
        Some("searchBin") => {
            let n = key.strip_prefix("SearchBin:").unwrap_or(key);
            if let Ok(id) = n.parse::<u64>() {
                doc.search_bins.retain(|b| b.id.0 != id);
            }
            Ok(())
        }
        Some("sourceGraphic") => {
            let n = key.strip_prefix("SourceGraphic:").unwrap_or(key);
            if let Ok(item) = n.parse::<u64>() {
                doc.source_graphics.remove(&filmcraft_project::ItemId(item));
            }
            Ok(())
        }
        Some("narration") => {
            let n = key.strip_prefix("Narration:").unwrap_or(key);
            if let Ok(item) = n.parse::<u64>() {
                doc.narrations.remove(&filmcraft_project::ItemId(item));
            }
            Ok(())
        }
        _ => Err(format!("unknown entry kind in '{key}'")),
    }
}

/// Reorder one document-level list from an order property's value.
pub fn set_doc_order(doc: &mut Project, prop: &str, value: &Value) -> Option<Result<(), String>> {
    let order: Vec<String> = match serde_json::from_value(value.to_json()) {
        Ok(v) => v,
        Err(e) => return Some(Err(e.to_string())),
    };
    match prop {
        "lutOrder" => Some(reorder(&mut doc.luts, |l| l.id.clone(), &order)),
        "searchBinOrder" => Some(reorder_by_id(&mut doc.search_bins, &order)),
        _ => None,
    }
}

/// Reorder `list` so the entries whose keys appear in `order` come first in that order.
fn reorder<T, K: PartialEq + Clone>(list: &mut Vec<T>, key: fn(&T) -> K, order: &[K]) -> Result<(), String> {
    let mut decorated: Vec<(usize, T)> = std::mem::take(list).into_iter().map(|item| (order.len() + 1, item)).collect();
    for (slot, item) in decorated.iter_mut() {
        let k = key(item);
        if let Some(i) = order.iter().position(|o| *o == k) {
            *slot = i;
        }
    }
    decorated.sort_by_key(|(slot, _)| *slot);
    *list = decorated.into_iter().map(|(_, item)| item).collect();
    Ok(())
}

/// [`reorder`] for the search bins (keyed by their numeric id).
fn reorder_by_id(list: &mut Vec<filmcraft_project::SearchBin>, order: &[String]) -> Result<(), String> {
    let ids: Result<Vec<u64>, _> = order.iter().map(|k| k.parse::<u64>().map_err(|e| e.to_string())).collect();
    reorder(list, |b| b.id.0, &ids?)
}

/// Insert-or-replace by key: replace the entry with the same key, or append.
fn upsert<T>(doc: &mut Project, get: fn(&mut Project) -> &mut Vec<T>, key: fn(&T) -> String, item: T, want: &str) -> Result<(), String> {
    let list = get(doc);
    for slot in list.iter_mut() {
        if key(slot) == want {
            *slot = item;
            return Ok(());
        }
    }
    list.push(item);
    Ok(())
}

/// The SHA-256 of the LUT text, lowercase hex (the asset id the `LutText` entry references).
pub fn text_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The document-level entries of `doc` in order, as `(entry id, value)` pairs for the view: the
/// LUT metadata, the deferred LUT text refs, transcripts, search bins, source graphics and
/// narrations.
pub fn entry_values(doc: &Project) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for lut in &doc.luts {
        let meta = LutMeta { id: lut.id.clone(), name: lut.name.clone(), source_path: lut.source_path.clone(), format: lut.format.clone() };
        if let Some(v) = serde_json::to_value(&meta).ok().map(Value::Json) {
            out.push((format!("Lut:{}", lut.id), v));
        }
        let asset = AssetRef { sha256: text_sha256(&lut.text), bytes: lut.text.len() as u64 };
        out.push((format!("LutText:{}", lut.id), Value::AssetRef(asset)));
    }
    for (item, transcript) in &doc.transcripts {
        if let Some(v) = serde_json::to_value(transcript.as_ref()).ok().map(Value::Json) {
            out.push((format!("Transcript:{}", item.0), v));
        }
    }
    for bin in &doc.search_bins {
        if let Some(v) = serde_json::to_value(bin).ok().map(Value::Json) {
            out.push((format!("SearchBin:{}", bin.id.0), v));
        }
    }
    for (item, graphic) in &doc.source_graphics {
        if let Some(v) = serde_json::to_value(graphic).ok().map(Value::Json) {
            out.push((format!("SourceGraphic:{}", item.0), v));
        }
    }
    for (item, narration) in &doc.narrations {
        if let Some(v) = serde_json::to_value(narration).ok().map(Value::Json) {
            out.push((format!("Narration:{}", item.0), v));
        }
    }
    out
}

/// Diff the document-level lists between two documents: per-entry `Put`/`Remove` plus one order
/// property per ordered list whose order changed.
pub fn diff_entries(before: &Project, after: &Project) -> Vec<Change> {
    let mut out = Vec::new();
    // The LUTs: metadata puts, text-ref puts, removes, and the order.
    let b_keys: Vec<String> = before.luts.iter().map(|l| l.id.clone()).collect();
    let a_keys: Vec<String> = after.luts.iter().map(|l| l.id.clone()).collect();
    if b_keys != a_keys {
        out.push(Change::SetDoc { prop: "lutOrder".into(), value: Value::Json(serde_json::json!(a_keys)) });
    }
    for lut in &after.luts {
        let same = before.luts.iter().any(|b| b.id == lut.id && b.name == lut.name && b.source_path == lut.source_path && b.format == lut.format);
        if !same {
            let meta = LutMeta { id: lut.id.clone(), name: lut.name.clone(), source_path: lut.source_path.clone(), format: lut.format.clone() };
            if let Some(v) = serde_json::to_value(&meta).ok().map(Value::Json) {
                out.push(Change::Put { entry: EntryId(format!("Lut:{}", lut.id).into()), value: v });
            }
        }
        let text_changed = before.luts.iter().find(|b| b.id == lut.id).is_none_or(|b| b.text != lut.text);
        if text_changed {
            let asset = AssetRef { sha256: text_sha256(&lut.text), bytes: lut.text.len() as u64 };
            out.push(Change::Put { entry: EntryId(format!("LutText:{}", lut.id).into()), value: Value::AssetRef(asset) });
        }
    }
    for lut in &before.luts {
        if !a_keys.contains(&lut.id) {
            out.push(Change::Remove { entry: EntryId(format!("Lut:{}", lut.id).into()) });
            out.push(Change::Remove { entry: EntryId(format!("LutText:{}", lut.id).into()) });
        }
    }
    // The keyed maps: value puts for changed entries, removes for gone ones (no order — they are
    // maps, their order is the key order).
    diff_map(before, after, |d| d.transcripts.iter().map(|(i, t)| (i.0, serde_json::to_value(t.as_ref()).ok())).collect(), "Transcript", &mut out);
    diff_map(before, after, |d| d.source_graphics.iter().map(|(i, g)| (i.0, serde_json::to_value(g).ok())).collect(), "SourceGraphic", &mut out);
    diff_map(before, after, |d| d.narrations.iter().map(|(i, n)| (i.0, serde_json::to_value(n).ok())).collect(), "Narration", &mut out);
    // The search bins: puts, removes and the order.
    let b_bins: Vec<u64> = before.search_bins.iter().map(|b| b.id.0).collect();
    let a_bins: Vec<u64> = after.search_bins.iter().map(|b| b.id.0).collect();
    if b_bins != a_bins {
        out.push(Change::SetDoc { prop: "searchBinOrder".into(), value: Value::Json(serde_json::json!(a_bins)) });
    }
    for bin in &after.search_bins {
        let same = before.search_bins.iter().any(|b| b == bin);
        if !same && let Some(v) = serde_json::to_value(bin).ok().map(Value::Json) {
            out.push(Change::Put { entry: EntryId(format!("SearchBin:{}", bin.id.0).into()), value: v });
        }
    }
    for bin in &before.search_bins {
        if !a_bins.contains(&bin.id.0) {
            out.push(Change::Remove { entry: EntryId(format!("SearchBin:{}", bin.id.0).into()) });
        }
    }
    out
}

/// Diff one keyed map: value `Put`s for new or changed entries, `Remove`s for gone ones.
fn diff_map<F>(before: &Project, after: &Project, list: F, prefix: &str, out: &mut Vec<Change>)
where
    F: Fn(&Project) -> Vec<(u64, Option<serde_json::Value>)>,
{
    let a: Vec<(u64, Option<serde_json::Value>)> = list(after);
    let b: Vec<(u64, Option<serde_json::Value>)> = list(before);
    for (id, v) in &a {
        let same = b.iter().any(|(bid, bv)| *bid == *id && *bv == *v);
        if !same && let Some(v) = v.clone().map(Value::Json) {
            out.push(Change::Put { entry: EntryId(format!("{prefix}:{id}").into()), value: v });
        }
    }
    for (id, _) in &b {
        if !a.iter().any(|(aid, _)| aid == id) {
            out.push(Change::Remove { entry: EntryId(format!("{prefix}:{id}").into()) });
        }
    }
}

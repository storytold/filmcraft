//! Marker mapping from contributor-authored project XML. `MarkerOwner/Markers@ObjectRef` points
//! to a markers object whose `Markers` list entries point (`Second@ObjectRef`) to `Marker`
//! objects holding one `DVAMarker` JSON record. Original work, MIT OR Apache-2.0.
use super::{referred, spend, warning};
use crate::xml::{child, child_text};
use crate::{Report, Result};
use filmcraft_project::{Label, Marker, MarkerId, MarkerKind};
use filmcraft_time::Tick;
use roxmltree::Node;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_MARKERS: usize = 10_000;
const MAX_RECORD_BYTES: usize = 64 * 1024;

/// The markers of `owner` (a `Sequence`, or the `Clip` element of a master clip's clip), sorted
/// by start. Ids are left at zero for the caller to allocate.
pub(super) fn read<'a, 'i>(
    owner: Node<'a, 'i>,
    ids: &BTreeMap<&str, Node<'a, 'i>>,
    work: &mut usize,
    report: &mut Report,
    context: &str,
) -> Result<Vec<Marker>> {
    let Some(list) = referred(child(owner, "MarkerOwner").and_then(|m| child(m, "Markers")), ids, "ObjectRef") else { return Ok(Vec::new()) };
    spend(work, list)?;
    let mut out = Vec::new();
    for entry in child(list, "Markers").into_iter().flat_map(|l| l.children()).filter(Node::is_element) {
        let Some(marker) = referred(child(entry, "Second"), ids, "ObjectRef") else { continue };
        spend(work, marker)?;
        if out.len() >= MAX_MARKERS {
            warning(report, format!("{context}: more than {MAX_MARKERS} markers; the rest were skipped"));
            break;
        }
        let Some((marker, url)) = child_text(marker, "DVAMarker").filter(|r| r.len() <= MAX_RECORD_BYTES).and_then(record) else {
            warning(report, format!("{context}: a marker could not be read; marker skipped"));
            continue;
        };
        if url {
            warning(report, format!("{context}: marker {} URL retained in the original snapshot, not imported", marker.name));
        }
        out.push(marker);
    }
    out.sort_by_key(|m| m.start);
    Ok(out)
}

/// A cue-point colour is stored as `0xAABBGGRR`.
fn color(value: u64) -> Label {
    let rgb = [(value & 0xff) as i32, ((value >> 8) & 0xff) as i32, ((value >> 16) & 0xff) as i32];
    let distance = |label: &Label| label.marker_rgb().iter().zip(rgb).map(|(c, v)| (i32::from(*c) - v).pow(2)).sum::<i32>();
    Label::ALL.iter().copied().min_by_key(distance).unwrap_or(Label::Green)
}

/// The marker and whether it stores a URL, which FilmCraft markers do not carry.
fn record(json: &str) -> Option<(Marker, bool)> {
    let root: Value = serde_json::from_str(json).ok()?;
    let m = root.get("DVAMarker")?;
    let ticks = |key: &str| -> Option<i64> {
        let t = m.get(key)?.get("ticks")?;
        t.as_i64().or_else(|| t.as_str()?.trim().parse().ok())
    };
    let text = |key: &str| m.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let start = ticks("mStartTime").unwrap_or(0).clamp(0, Tick::MAX.0);
    let duration = ticks("mDuration").unwrap_or(0).clamp(0, Tick::MAX.0 - start);
    let kind = match m.get("mType").and_then(Value::as_str).unwrap_or("Comment") {
        "Chapter" => MarkerKind::Chapter,
        "Segmentation" => MarkerKind::Segmentation,
        "WebLink" => MarkerKind::WebLink,
        "FlashCuePoint" | "FlashCue" => MarkerKind::FlashCue,
        _ => MarkerKind::Comment,
    };
    let cue = m
        .get("mCuePointList")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("mValue").and_then(Value::as_str))
        .filter_map(|v| serde_json::from_str::<Value>(v).ok())
        .find_map(|v| v.get("color").and_then(Value::as_u64));
    let url = m.get("mLocation").and_then(Value::as_str).is_some_and(|u| !u.trim().is_empty());
    let marker = Marker {
        id: MarkerId(0),
        start: Tick(start),
        duration: Tick(duration),
        name: text("mName"),
        comment: text("mComment"),
        kind,
        color: cue.map(color).unwrap_or(Label::Green),
    };
    Some((marker, url))
}

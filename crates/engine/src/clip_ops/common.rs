use super::{BTreeSet, FrameRate, ItemId, ItemKind, MediaClip, Project, Session, Tick, TimeRange, TrackItem, Value, item_p, json};

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

/// The media item behind `id` (subclips resolve to their parent) and the subclip range.
pub(super) fn media_root(p: &Project, id: ItemId) -> Option<(ItemId, &MediaClip, Option<TimeRange>)> {
    p.resolve_media(id)
}

/// What the Source monitor shows for an item: the media-time span of its time ruler, its frame
/// rate, In / Out marks and markers.
///
/// A subclip that restricts trims shows only its range (the playhead can't leave it); one that
/// doesn't shows its parent's whole media with the subclip's range as In / Out. Either way a
/// subclip shows the markers of its parent media that fall inside its range (inherited: they
/// belong to the master clip, so adding one there shows on every subclip that covers it).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceView {
    pub item: ItemId,
    /// The media item whose frames are shown (the parent of a subclip).
    pub media: ItemId,
    pub start: Tick,
    pub end: Tick,
    pub rate: FrameRate,
    pub mark_in: Option<Tick>,
    pub mark_out: Option<Tick>,
    pub markers: Vec<filmcraft_project::Marker>,
    /// For a subclip: its range and whether trims are restricted to it.
    pub subclip: Option<(TimeRange, bool)>,
}

impl SourceView {
    /// The marked Source span. Unset marks mean the beginning/end of the source; Out is inclusive.
    pub fn selected_range(&self) -> TimeRange {
        if self.start.0 < 0 || self.end <= self.start {
            return TimeRange::new(self.start, Tick::ZERO);
        }
        let start = self.mark_in.unwrap_or(self.start).clamp(self.start, self.end);
        let end = self.mark_out.map(|o| Tick(o.0.saturating_add(self.rate.frame_duration().0))).unwrap_or(self.end).clamp(start, self.end);
        TimeRange::from_bounds(start, end)
    }

    pub fn to_json(&self, playhead: Tick) -> Value {
        json!({
            "item": self.item.0, "media": self.media.0, "start": self.start.0, "end": self.end.0, "fps": self.rate.as_f64(),
            "playhead": playhead.0, "markIn": self.mark_in.map(|t| t.0), "markOut": self.mark_out.map(|t| t.0),
            "markers": self.markers.iter().map(|m| json!({"id": m.id.0, "start": m.start.0, "name": m.name, "duration": m.duration.0})).collect::<Vec<_>>(),
            "subclip": self.subclip.map(|(r, restrict)| json!({"start": r.start.0, "end": r.end().0, "restrictTrims": restrict})),
        })
    }
}

/// The Source monitor view of `item` (see [`SourceView`]).
pub fn source_view(s: &Session, item: ItemId) -> Option<SourceView> {
    let pi = s.project.item(item)?;
    let still = |m: &MediaClip| matches!(m.info.kind, filmcraft_media::MediaKind::Still) || m.info.duration.0 <= 0;
    Some(match &pi.kind {
        ItemKind::Media(m) => {
            let rate = m.frame_rate();
            let end = if still(m) { s.prefs.timeline.still_duration(rate) } else { m.duration() };
            SourceView { item, media: item, start: Tick::ZERO, end, rate, mark_in: m.mark_in, mark_out: m.mark_out, markers: m.markers.clone(), subclip: None }
        }
        ItemKind::Sequence(q) => SourceView {
            item,
            media: item,
            start: Tick::ZERO,
            end: q.duration(),
            rate: q.settings.frame_rate,
            mark_in: q.mark_in,
            mark_out: q.mark_out,
            markers: q.markers.clone(),
            subclip: None,
        },
        ItemKind::Subclip { range, restrict_trims, .. } => {
            let (root, m, _) = media_root(&s.project, item)?;
            let rate = m.frame_rate();
            let fd = rate.frame_duration();
            let markers = m.markers.iter().filter(|k| k.start >= range.start && k.start < range.end()).cloned().collect();
            let full = if still(m) { range.end() } else { m.duration().max(range.end()) };
            let (start, end, mi, mo) =
                if *restrict_trims { (range.start, range.end(), None, None) } else { (Tick::ZERO, full, Some(range.start), Some(range.end() - fd)) };
            SourceView { item, media: root, start, end, rate, mark_in: mi, mark_out: mo, markers, subclip: Some((*range, *restrict_trims)) }
        }
        _ => SourceView {
            item,
            media: item,
            start: Tick::ZERO,
            end: pi.duration(),
            rate: pi.frame_rate(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            subclip: None,
        },
    })
}

/// Frame rate of an item, resolving subclips to their media.
pub(super) fn item_rate(p: &Project, id: ItemId) -> FrameRate {
    match media_root(p, id) {
        Some((_, m, _)) => m.frame_rate(),
        None => p.item(id).map(|i| i.frame_rate()).unwrap_or_default(),
    }
}

pub(super) fn items_p(s: &Session, p: &Value) -> Vec<ItemId> {
    match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => item_p(p, "item").map(|i| vec![i]).unwrap_or_else(|| s.state.project_selection.clone()),
    }
}

/// Fold the undo steps pushed since the history had `n0` entries into one step named `label`.
pub(crate) fn collapse_history(s: &mut Session, n0: usize, label: &str) {
    if s.history.undo.len() > n0 {
        let first = s.history.undo[n0].1.clone();
        s.history.undo.truncate(n0);
        s.history.undo.push((label.to_string(), first));
    }
}

/// Every item referenced by a clip in any sequence (subclips pull in their parents, nested
/// sequences count as used).
pub(super) fn used_items(p: &Project) -> BTreeSet<ItemId> {
    let mut used = BTreeSet::new();
    for it in p.items.values() {
        if let ItemKind::Sequence(q) = &it.kind {
            for t in q.all_tracks() {
                for i in &t.items {
                    used.insert(i.item);
                }
            }
            if let Some(m) = &q.merged {
                let v = serde_json::to_value(m).unwrap_or_default();
                collect_ids(&v, &mut used, p);
            }
        }
    }
    // subclip parents of anything kept
    let mut changed = true;
    while changed {
        changed = false;
        for it in p.items.values() {
            if let ItemKind::Subclip { parent, .. } = it.kind
                && (used.contains(&it.id) || !is_removable(p, it.id))
                && used.insert(parent)
            {
                changed = true;
            }
        }
    }
    used
}

/// Item ids mentioned in a JSON value (keys named `item`/`items`/`video`/`audio` holding ids).
pub(crate) fn collect_ids(v: &Value, out: &mut BTreeSet<ItemId>, p: &Project) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if matches!(k.as_str(), "item" | "items" | "video" | "audio") {
                    for n in x.as_array().cloned().unwrap_or_else(|| vec![x.clone()]) {
                        if let Some(id) = n.as_u64().map(ItemId).filter(|i| p.items.contains_key(i)) {
                            out.insert(id);
                        }
                    }
                }
                collect_ids(x, out, p);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_ids(x, out, p)),
        _ => {}
    }
}

/// Remove Unused removes clips only: never sequences or graphic sources.
pub(super) fn is_removable(p: &Project, id: ItemId) -> bool {
    p.item(id).is_some_and(|i| matches!(i.kind, ItemKind::Media(_) | ItemKind::Subclip { .. } | ItemKind::AdjustmentLayer { .. }))
}

pub(super) fn remove_items(p: &mut Project, ids: &[ItemId]) {
    for i in ids {
        p.items.remove(i);
        p.root.remove_item(*i);
    }
}

/// Keyframe times of an effect instance shifted / scaled from clip `from` to clip `to` (media time).
pub(super) fn retime_effect(e: &mut filmcraft_project::EffectInstance, from: &TrackItem, to: &TrackItem, scale: bool) {
    let (f0, f1) = (from.source_in, from.source_out());
    let (t0, t1) = (to.source_in, to.source_out());
    let map = |k: Tick| -> Tick {
        if scale && f1 > f0 {
            let r = (k - f0).0 as f64 / (f1 - f0).0 as f64;
            t0 + Tick((r * (t1 - t0).0 as f64).round() as i64)
        } else {
            t0 + (k - f0)
        }
    };
    for p in e.params.values_mut() {
        for k in &mut p.keyframes {
            k.time = map(k.time);
        }
    }
    for m in &mut e.masks {
        for p in [&mut m.path, &mut m.feather, &mut m.opacity, &mut m.expansion] {
            for k in &mut p.keyframes {
                k.time = map(k.time);
            }
        }
    }
}

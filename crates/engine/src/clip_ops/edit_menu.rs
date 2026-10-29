use super::common::{is_removable, remove_items, retime_effect, used_items};
use super::{BTreeMap, BTreeSet, ClipId, EngineError, ItemId, ItemKind, Label, MediaRef, Result, Session, TrackKind, Value, bool_p, clips_p, json, with_links};

// ---------------------------------------------------------------------------------------------
// Edit
// ---------------------------------------------------------------------------------------------

pub(super) fn apply_label(s: &mut Session, p: &Value, l: Label) -> Result<Value> {
    let items = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None if p.get("clips").is_some() => Vec::new(),
        None => s.state.project_selection.clone(),
    };
    let clips = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None if p.get("items").is_some() => Vec::new(),
        None => s.state.selection.clone(),
    };
    if items.is_empty() && clips.is_empty() {
        return Err(EngineError::Other("select clips or project items".into()));
    }
    let seq = s.state.active_sequence;
    let n = s.edit(&format!("Label {}", l.name()), |pr, _| {
        let mut n = 0;
        for i in &items {
            if let Some(it) = pr.item_mut(*i) {
                it.label = l;
                n += 1;
            }
        }
        if let Some(q) = seq.and_then(|q| pr.sequence_mut(q)) {
            for c in &clips {
                if let Some((_, it)) = q.find_item_mut(*c) {
                    it.label = l;
                    n += 1;
                }
            }
        }
        Ok(n)
    })?;
    Ok(json!({"label": l.name(), "labelled": n}))
}

pub(super) fn select_label_group(s: &mut Session, _: &Value) -> Result<Value> {
    if !s.state.selection.is_empty() {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let labels: BTreeSet<&str> = s.state.selection.iter().filter_map(|c| q.find_item(*c).map(|(_, i)| i.label.name())).collect();
        let sel: Vec<ClipId> = q.all_tracks().flat_map(|t| t.items.iter()).filter(|i| labels.contains(i.label.name())).map(|i| i.id).collect();
        s.state.selection = sel;
        return Ok(json!({"clips": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}));
    }
    let labels: BTreeSet<&str> = s.state.project_selection.iter().filter_map(|i| s.project.item(*i).map(|it| it.label.name())).collect();
    let mut all = Vec::new();
    s.project.root.all_items(&mut all);
    let sel: Vec<ItemId> = all.into_iter().filter(|i| s.project.item(*i).is_some_and(|it| labels.contains(it.label.name()))).collect();
    s.state.project_selection = sel;
    Ok(json!({"items": s.state.project_selection.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

pub(super) fn select_all_matching(s: &mut Session, p: &Value) -> Result<Value> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sources: BTreeSet<ItemId> = clips_p(s, p).iter().filter_map(|c| q.find_item(*c).map(|(_, i)| i.item)).collect();
    let sel: Vec<ClipId> = q.all_tracks().flat_map(|t| t.items.iter()).filter(|i| sources.contains(&i.item)).map(|i| i.id).collect();
    s.state.selection = sel;
    Ok(json!({"clips": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

pub(super) fn remove_unused(s: &mut Session, _: &Value) -> Result<Value> {
    let used = used_items(&s.project);
    let unused: Vec<ItemId> = s.project.items.keys().copied().filter(|i| is_removable(&s.project, *i) && !used.contains(i)).collect();
    if unused.is_empty() {
        return Ok(json!({"removed": []}));
    }
    let gone = unused.clone();
    s.edit("Remove Unused", |pr, st| {
        remove_items(pr, &gone);
        st.project_selection.retain(|i| !gone.contains(i));
        if st.source_item.is_some_and(|i| gone.contains(&i)) {
            st.source_item = None;
        }
        Ok(())
    })?;
    for i in &unused {
        s.media.remove(*i);
    }
    Ok(json!({"removed": unused.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// Media items that refer to the same file (or generator) with the same interpretation are
/// duplicates; the oldest one is kept and every reference to the others moves to it.
pub(super) fn consolidate_duplicates(s: &mut Session, _: &Value) -> Result<Value> {
    let mut groups: BTreeMap<String, Vec<ItemId>> = BTreeMap::new();
    for it in s.project.items.values() {
        if let ItemKind::Media(m) = &it.kind {
            let generator_rate = if matches!(&m.media, MediaRef::Generator(filmcraft_media::Generator::CountingLeader)) {
                m.info.video.as_ref().map(|v| v.frame_rate)
            } else {
                None
            };
            let shape = (m.info.video.is_some(), m.info.has_audio(), m.info.duration, generator_rate);
            let key = serde_json::to_string(&(&m.media, &m.interpret, m.offline, shape)).unwrap_or_default();
            groups.entry(key).or_default().push(it.id);
        }
    }
    let mut remap: BTreeMap<ItemId, ItemId> = BTreeMap::new();
    for ids in groups.values().filter(|g| g.len() > 1) {
        for d in &ids[1..] {
            remap.insert(*d, ids[0]);
        }
    }
    if remap.is_empty() {
        return Ok(json!({"removed": []}));
    }
    let map = remap.clone();
    s.edit("Consolidate Duplicates", |pr, st| {
        for it in pr.items.values_mut() {
            match &mut it.kind {
                ItemKind::Sequence(q) => {
                    let q = std::sync::Arc::make_mut(q);
                    for t in q.all_tracks_mut() {
                        for i in &mut t.items {
                            if let Some(k) = map.get(&i.item) {
                                i.item = *k;
                            }
                        }
                    }
                }
                ItemKind::Subclip { parent, .. } => {
                    if let Some(k) = map.get(parent) {
                        *parent = *k;
                    }
                }
                _ => {}
            }
        }
        // keep the marks / markers of the kept item; carry markers of duplicates over
        let dups: Vec<ItemId> = map.keys().copied().collect();
        for d in &dups {
            let markers = pr.item(*d).and_then(|i| i.as_media()).map(|m| m.markers.clone()).unwrap_or_default();
            if let Some(m) = pr.item_mut(map[d]).and_then(|i| i.as_media_mut()) {
                for mk in markers {
                    if !m.markers.iter().any(|x| x.start == mk.start && x.name == mk.name) {
                        m.markers.push(mk);
                    }
                }
                m.markers.sort_by_key(|x| x.start);
            }
        }
        remove_items(pr, &dups);
        for i in st.project_selection.iter_mut() {
            if let Some(k) = map.get(i) {
                *i = *k;
            }
        }
        st.project_selection.dedup();
        if let Some(k) = st.source_item.and_then(|i| map.get(&i)) {
            st.source_item = Some(*k);
        }
        Ok(())
    })?;
    for d in remap.keys() {
        s.media.remove(*d);
    }
    Ok(json!({"removed": remap.keys().map(|i| i.0).collect::<Vec<_>>(), "kept": remap.values().map(|i| i.0).collect::<BTreeSet<_>>()}))
}

/// Which attributes a Paste / Remove Attributes call touches.
struct AttrSel {
    intrinsic: Vec<&'static str>,
    /// None = every standard effect; Some(ids) = those effects (Some(empty) = none).
    effects: Option<Vec<String>>,
}

fn attr_sel(p: &Value) -> AttrSel {
    let on = |k: &str| bool_p(p, k).unwrap_or(true);
    let mut intrinsic = Vec::new();
    for (k, id) in [
        ("motion", "motion"),
        ("opacity", "opacity"),
        ("timeRemapping", "time_remap"),
        ("volume", "volume"),
        ("channelVolume", "channel_volume"),
        ("panner", "panner"),
    ] {
        if on(k) {
            intrinsic.push(id);
        }
    }
    let effects = match p.get("effects") {
        Some(Value::Bool(false)) => Some(Vec::new()),
        Some(Value::Array(a)) => Some(a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()),
        _ => None,
    };
    AttrSel { intrinsic, effects }
}

fn is_standard(e: &filmcraft_project::EffectInstance) -> bool {
    e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e) && !e.essential
}

fn wants_effect(sel: &AttrSel, e: &filmcraft_project::EffectInstance) -> bool {
    sel.effects.as_ref().is_none_or(|ids| ids.contains(&e.effect))
}

pub(super) fn paste_attributes(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = attr_sel(p);
    let scale = bool_p(p, "scaleTimes").unwrap_or(true);
    let src_v = s.state.clipboard.iter().find(|c| c.0 == TrackKind::Video).map(|c| c.2.clone());
    let src_a = s.state.clipboard.iter().find(|c| c.0 == TrackKind::Audio).map(|c| c.2.clone());
    let targets = with_links(s, &clips_p(s, p));
    let n = s.edit_sequence("Paste Attributes", |q, _, _| {
        let mut n = 0;
        for c in &targets {
            let Some((tid, _)) = q.find_item(*c) else { continue };
            let kind = q.track(tid).map(|t| t.kind).unwrap_or_default();
            let Some(src) = (if kind == TrackKind::Video { src_v.as_ref() } else { src_a.as_ref() }) else { continue };
            let Some((_, dst)) = q.find_item_mut(*c) else { continue };
            let before = dst.clone();
            for id in &sel.intrinsic {
                let Some(e) = src.effect(id) else { continue };
                let mut e = e.clone();
                retime_effect(&mut e, src, &before, scale);
                match dst.effects.iter_mut().find(|x| x.effect == *id) {
                    Some(slot) => *slot = e,
                    None => dst.effects.push(e),
                }
            }
            for e in src.effects.iter().filter(|e| is_standard(e) && wants_effect(&sel, e)) {
                let mut e = e.clone();
                retime_effect(&mut e, src, &before, scale);
                dst.effects.push(e);
            }
            if *dst != before {
                n += 1;
            }
        }
        Ok(n)
    })?;
    Ok(json!({"clips": n}))
}

pub(super) fn remove_attributes(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = attr_sel(p);
    let targets = with_links(s, &clips_p(s, p));
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = (seq.settings.width, seq.settings.height);
    let sizes: BTreeMap<ClipId, (u32, u32)> =
        targets.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| (*c, filmcraft_render::source_size(&s.project, i.item).unwrap_or(frame)))).collect();
    let n = s.edit_sequence("Remove Attributes", |q, _, _| {
        let mut n = 0;
        for c in &targets {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let before = it.clone();
            for id in &sel.intrinsic {
                if let Some(slot) = it.effects.iter_mut().find(|x| x.effect == *id)
                    && let Some(def) = slot.def()
                {
                    let mut fresh = def.instance();
                    filmcraft_project::resolve_auto_points(&mut fresh, frame, sizes.get(c).copied().unwrap_or(frame));
                    *slot = fresh;
                }
            }
            it.effects.retain(|e| !(is_standard(e) && wants_effect(&sel, e)));
            if *it != before {
                n += 1;
            }
        }
        Ok(n)
    })?;
    Ok(json!({"clips": n}))
}

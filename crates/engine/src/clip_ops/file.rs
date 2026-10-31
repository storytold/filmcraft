use super::common::{collapse_history, item_rate, items_p, media_root};
use super::enabled::is_clip_item;
use super::{
    AudioChannels, BTreeMap, BinEntry, EngineError, FrameRate, ItemId, ItemKind, Label, MediaClip, MediaRef, Project, Result, Session, Tick, TimeRange, Value,
    bad, bool_p, f64_p, json, parse_timecode, place_item, str_p, u64_p,
};

// ---------------------------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------------------------

pub(super) fn sequence_from_clip(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = items_p(s, p).into_iter().filter(|i| is_clip_item(&s.project, *i)).collect();
    let first = *items.first().ok_or_else(|| bad("file.newSequenceFromClip", "select a clip in the Project panel"))?;
    let pi = s.project.item(first).ok_or_else(|| bad("file.newSequenceFromClip", "no such item"))?.clone();
    let mut settings = match &pi.kind {
        ItemKind::Sequence(q) => q.settings.clone(),
        _ => media_root(&s.project, first).map(|(_, m, _)| crate::commands::default_seq_settings_for(m)).unwrap_or_default(),
    };
    if let ItemKind::AdjustmentLayer { width, height, rate, .. } = &pi.kind {
        settings.width = *width;
        settings.height = *height;
        settings.frame_rate = *rate;
    }
    let channels = media_root(&s.project, first).and_then(|(_, m, _)| m.info.audio().map(|a| a.channels)).unwrap_or(2);
    let n_audio = media_root(&s.project, first)
        .map(|(_, m, _)| {
            crate::commands::audio_placement_specs(m.interpret.audio_channels.as_ref().map_or(&[], |a| a.clips.as_slice()), m.info.audio_streams.len()).len()
        })
        .unwrap_or(1);
    let n0 = s.history.undo.len();
    let name = pi.name.clone();
    let bin = s.project.root.parent_of(first).filter(|b| *b != s.project.root.id);
    let seq = s.edit("New Sequence From Clip", |pr, st| {
        let id = pr.new_sequence(&name, settings, 1, n_audio.max(1), bin);
        if channels == 1
            && let Some(q) = pr.sequence_mut(id)
        {
            q.settings.audio_master = AudioChannels::Stereo;
        }
        st.active_sequence = Some(id);
        if !st.open_sequences.contains(&id) {
            st.open_sequences.push(id);
        }
        Ok(id)
    })?;
    let mut at = Tick::ZERO;
    let mut placed = Vec::new();
    for item in items {
        let still = s.prefs.timeline.still_duration(item_rate(&s.project, item));
        let Some(range) = item_range(&s.project, item, still) else { continue };
        let tg = s.targeting();
        let ids = place_item(
            s,
            crate::commands::ItemPlacement {
                item,
                range,
                at,
                vdest: tg.video_dest,
                adest: tg.audio_dest,
                insert: false,
                label: "New Sequence From Clip",
                audio_split: None,
            },
        )?;
        at = s.active_sequence().map(|q| q.duration()).unwrap_or(at);
        placed.extend(ids.iter().map(|c| c.0));
    }
    collapse_history(s, n0, "New Sequence From Clip");
    s.state.selection.clear();
    s.events.push(crate::Event::OpenSequence(seq));
    Ok(json!({"sequence": seq.0, "clips": placed}))
}

/// The media range an item edits in with: its In/Out marks, a subclip's range, or the whole item.
pub(crate) fn item_range(p: &Project, id: ItemId, still: Tick) -> Option<TimeRange> {
    let it = p.item(id)?;
    let rate = item_rate(p, id);
    Some(match &it.kind {
        ItemKind::Media(m) => {
            let is_still = matches!(m.info.kind, filmcraft_media::MediaKind::Still);
            let full = if m.info.duration.0 > 0 && !is_still { m.info.duration } else { still };
            let a = m.mark_in.unwrap_or(Tick::ZERO);
            let b = m.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(full);
            TimeRange::from_bounds(a, b.max(a + rate.frame_duration()))
        }
        ItemKind::Subclip { range, .. } => *range,
        ItemKind::Sequence(q) => {
            let a = q.mark_in.unwrap_or(Tick::ZERO);
            let b = q.mark_out.map(|o| o + q.settings.frame_rate.frame_duration()).unwrap_or(q.duration());
            TimeRange::from_bounds(a, b.max(a + q.settings.frame_rate.frame_duration()))
        }
        _ => TimeRange::new(Tick::ZERO, it.duration()),
    })
}

pub(super) fn bin_from_selection(s: &mut Session, p: &Value) -> Result<Value> {
    let items = items_p(s, p);
    if items.is_empty() {
        return Err(bad("file.newBinFromSelection", "select items in the Project panel"));
    }
    let parent = s.project.root.parent_of(items[0]).filter(|b| *b != s.project.root.id);
    let name = match str_p(p, "name") {
        Some(n) => n.to_string(),
        None => {
            let mut n = 1;
            let mut names = Vec::new();
            bin_names(&s.project.root, &mut names);
            while names.contains(&format!("Bin {n:02}")) {
                n += 1;
            }
            format!("Bin {n:02}")
        }
    };
    let bin = s.edit("New Bin From Selection", |pr, _| {
        let b = pr.add_bin(&name, parent);
        pr.move_to_bin(&items, Some(b));
        Ok(b)
    })?;
    Ok(json!({"bin": bin.0, "moved": items.len()}))
}

fn bin_names(b: &filmcraft_project::Bin, out: &mut Vec<String>) {
    for c in &b.children {
        if let BinEntry::Bin(x) = c {
            out.push(x.name.clone());
            bin_names(x, out);
        }
    }
}

pub(super) fn offline_file(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
    let has_v = bool_p(p, "video").unwrap_or(true);
    let has_a = bool_p(p, "audio").unwrap_or(true);
    if !has_v && !has_a {
        return Err(bad("file.newOfflineFile", "an offline file needs video or audio"));
    }
    let rate = f64_p(p, "fps").map(FrameRate::from_f64).unwrap_or(st.frame_rate);
    let width = u64_p(p, "width").map(|v| v as u32).unwrap_or(st.width);
    let height = u64_p(p, "height").map(|v| v as u32).unwrap_or(st.height);
    let secs = f64_p(p, "seconds").unwrap_or(10.0).max(rate.frame_duration().seconds());
    let duration = rate.snap_nearest(Tick::from_seconds_f64(secs)).max(rate.frame_duration());
    let start_tc = match str_p(p, "timecode") {
        Some(tc) => Some(parse_timecode(tc, rate, false, 0).map_err(|e| bad("file.newOfflineFile", format!("timecode: {e:?}")))?),
        None => None,
    };
    // A name, never a path: the item's media path is what Make Offline ▸ delete would remove.
    let file_name = str_p(p, "fileName")
        .and_then(|n| n.rsplit(['/', '\\', ':']).next())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .map(str::to_string)
        .unwrap_or_else(|| "Offline.mov".into());
    let name = str_p(p, "name").map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| file_name.clone());
    let info = filmcraft_media::MediaInfo {
        name: file_name.clone(),
        kind: if has_v { filmcraft_media::MediaKind::Movie } else { filmcraft_media::MediaKind::AudioOnly },
        duration,
        video: has_v.then(|| filmcraft_media::VideoStreamInfo {
            width,
            height,
            frame_rate: rate,
            par: (1, 1),
            codec: String::new(),
            pixel_format: String::new(),
            color: Default::default(),
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: has_a
            .then(|| filmcraft_media::AudioStreamInfo {
                sample_rate: u64_p(p, "sampleRate").unwrap_or(48_000) as u32,
                channels: u64_p(p, "channels").unwrap_or(2).max(1) as u32,
                codec: String::new(),
                bits_per_sample: None,
            })
            .into_iter()
            .collect(),
        container: String::new(),
        start_timecode: start_tc,
        file_size: None,
    };
    let mut meta = BTreeMap::new();
    if let Some(t) = str_p(p, "tapeName").filter(|t| !t.is_empty()) {
        meta.insert("Tape Name".to_string(), t.to_string());
    }
    if let Some(d) = str_p(p, "description").filter(|t| !t.is_empty()) {
        meta.insert("Description".to_string(), d.to_string());
    }
    let id = s.edit("New Offline File", |pr, stt| {
        let id = pr.add_item(
            &name,
            if has_v { Label::Iris } else { Label::Caribbean },
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::File { path: file_name },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: true,
                proxy: None,
                identity: None,
            })),
            None,
        );
        if let Some(it) = pr.item_mut(id) {
            it.metadata = meta;
        }
        stt.project_selection = vec![id];
        Ok(id)
    })?;
    Ok(json!({"item": id.0}))
}

pub(super) fn close_project(s: &mut Session, p: &Value) -> Result<Value> {
    if s.is_dirty() && !bool_p(p, "force").unwrap_or(false) {
        return Err(EngineError::Other("the project has unsaved changes: save it first, or pass {\"force\": true} to discard them".into()));
    }
    s.project = std::sync::Arc::new(Project::new("Untitled"));
    s.document_epoch = s.document_epoch.wrapping_add(1);
    s.history = Default::default();
    s.history.limit = 200;
    s.state = Session::default().state;
    if s.path.take().is_some() {
        s.previews.reset_temp();
    }
    s.revision += 1;
    s.saved_revision = s.revision;
    s.events.push(crate::Event::ProjectChanged { revision: s.revision });
    Ok(json!({"closed": true}))
}

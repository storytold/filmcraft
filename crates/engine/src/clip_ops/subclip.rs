use super::common::{item_rate, items_p, media_root};
use super::enabled::{selected_audio_clips, subclip_source};
use super::{
    AudioChannelMap, AudioChannels, ClipId, EngineError, ItemId, ItemKind, Label, Result, Session, Tick, TimeRange, Value, bad, bool_p, item_p, json,
    parse_timecode, str_p,
};

// ---------------------------------------------------------------------------------------------
// Clip: subclips, Modify
// ---------------------------------------------------------------------------------------------

/// What Make Subclip would subclip: (parent media item, media range, name, label).
fn subclip_plan(s: &Session, p: &Value) -> Result<(ItemId, TimeRange, String, Label)> {
    Ok(if let Some(c) = clip_p_or_selection(s, p) {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item(c).ok_or_else(|| bad("clip.makeSubclip", "no such clip"))?;
        let (root, _, _) = media_root(&s.project, it.item).ok_or_else(|| bad("clip.makeSubclip", "only media clips can be subclipped"))?;
        (root, TimeRange::from_bounds(it.source_in, it.source_out()), it.name.clone(), it.label)
    } else {
        let item = item_p(p, "item").or_else(|| subclip_source(s)).ok_or_else(|| bad("clip.makeSubclip", "open a clip in the Source monitor"))?;
        let (root, m, sub) = media_root(&s.project, item).ok_or_else(|| bad("clip.makeSubclip", "only media clips can be subclipped"))?;
        let pi = s.project.item(item).ok_or_else(|| EngineError::Other("no such item".into()))?;
        let rate = m.frame_rate();
        let still = s.prefs.timeline.still_duration(rate);
        let is_still = matches!(m.info.kind, filmcraft_media::MediaKind::Still);
        let full = sub.unwrap_or(TimeRange::new(Tick::ZERO, if m.info.duration.0 > 0 && !is_still { m.info.duration } else { still }));
        let range = if sub.is_some() {
            full
        } else {
            let a = m.mark_in.unwrap_or(full.start);
            let b = m.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(full.end());
            TimeRange::from_bounds(a, b)
        };
        (root, range, pi.name.clone(), pi.label)
    })
}

/// The Make Subclip dialog's defaults: `{name, startFrame, endFrame, fps}` (media frames).
pub fn subclip_defaults(s: &Session, p: &Value) -> Option<Value> {
    let (parent, range, base, _) = subclip_plan(s, p).ok()?;
    let rate = item_rate(&s.project, parent);
    Some(json!({"name": format!("{base}.Subclip"), "startFrame": rate.frame_at(range.start), "endFrame": rate.frame_at(range.end()), "fps": rate.as_f64()}))
}

pub(super) fn make_subclip(s: &mut Session, p: &Value) -> Result<Value> {
    let (parent, range, base, label) = subclip_plan(s, p)?;
    let rate = item_rate(&s.project, parent);
    let a = p
        .get("start")
        .and_then(Value::as_i64)
        .map(Tick)
        .or_else(|| p.get("startFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f)))
        .unwrap_or(range.start);
    let b =
        p.get("end").and_then(Value::as_i64).map(Tick).or_else(|| p.get("endFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f))).unwrap_or(range.end());
    if b <= a || a < Tick::ZERO {
        return Err(bad("clip.makeSubclip", "the subclip's end must be after its start"));
    }
    let restrict = bool_p(p, "restrictTrims").unwrap_or(true);
    let name = str_p(p, "name").map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("{base}.Subclip"));
    let bin = s.project.root.parent_of(parent).filter(|b| *b != s.project.root.id);
    let id = s.edit("Make Subclip", |pr, st| {
        let id = pr.add_item(&name, label, ItemKind::Subclip { parent, range: TimeRange::from_bounds(a, b), restrict_trims: restrict }, bin);
        st.project_selection = vec![id];
        Ok(id)
    })?;
    Ok(json!({"item": id.0}))
}

/// `clip` param, else the timeline selection when nothing is loaded in the Source monitor and no
/// `item` is given.
fn clip_p_or_selection(s: &Session, p: &Value) -> Option<ClipId> {
    if let Some(c) = crate::commands::clip_p(p, "clip") {
        return Some(c);
    }
    if item_p(p, "item").is_some() || subclip_source(s).is_some() {
        return None;
    }
    s.state.selection.first().copied()
}

pub(super) fn edit_subclip(s: &mut Session, p: &Value) -> Result<Value> {
    let item = item_p(p, "item")
        .or_else(|| s.state.project_selection.iter().copied().find(|i| s.project.item(*i).is_some_and(|it| matches!(it.kind, ItemKind::Subclip { .. }))))
        .ok_or_else(|| bad("clip.editSubclip", "select a subclip"))?;
    let Some(ItemKind::Subclip { parent, range, restrict_trims }) = s.project.item(item).map(|i| i.kind.clone()) else {
        return Err(bad("clip.editSubclip", "not a subclip"));
    };
    let rate = item_rate(&s.project, parent);
    let a = p
        .get("start")
        .and_then(Value::as_i64)
        .map(Tick)
        .or_else(|| p.get("startFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f)))
        .unwrap_or(range.start);
    let b =
        p.get("end").and_then(Value::as_i64).map(Tick).or_else(|| p.get("endFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f))).unwrap_or(range.end());
    if b <= a || a < Tick::ZERO {
        return Err(bad("clip.editSubclip", "the subclip's end must be after its start"));
    }
    let restrict = bool_p(p, "restrictTrims").unwrap_or(restrict_trims);
    let convert = bool_p(p, "convertToMaster").unwrap_or(false);
    let parent_media = s.project.item(parent).and_then(|i| i.as_media()).cloned();
    s.edit("Edit Subclip", |pr, _| {
        let it = pr.item_mut(item).ok_or_else(|| bad("clip.editSubclip", "no such item"))?;
        if convert {
            // a master clip of the whole media, marked with the subclip's range; the parent's
            // markers inside that range come along (they were the subclip's inherited markers)
            let mut m = parent_media.ok_or_else(|| bad("clip.editSubclip", "the subclip's media is gone"))?;
            let fd = m.frame_rate().frame_duration();
            m.mark_in = Some(a);
            m.mark_out = Some((b - fd).max(a));
            m.markers.retain(|k| k.start >= a && k.start < b);
            it.kind = ItemKind::Media(Box::new(m));
        } else {
            it.kind = ItemKind::Subclip { parent, range: TimeRange::from_bounds(a, b), restrict_trims: restrict };
        }
        Ok(())
    })?;
    if convert {
        s.media.remove(item);
    }
    Ok(json!({"item": item.0, "converted": convert}))
}

fn parse_format(f: &str) -> Option<AudioChannels> {
    match f.to_ascii_lowercase().as_str() {
        "mono" => Some(AudioChannels::Mono),
        "stereo" => Some(AudioChannels::Stereo),
        "5.1" | "surround51" | "surround" => Some(AudioChannels::Surround51),
        "adaptive" => Some(AudioChannels::Adaptive),
        _ => None,
    }
}

pub(super) fn audio_channels(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> =
        items_p(s, p).into_iter().filter(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some_and(|m| m.info.has_audio())).collect();
    // `channels` is the only thing the timeline-clip branch below reads, so it alone decides
    // which branch runs: no project item selected used to fall into it too (on the theory that
    // there was nothing else to do), but a `channels`-less call with no item selected (e.g. a
    // stray `format` with an empty project selection, #1845) then reset the selected clips'
    // `source_channels` to `[]` via `unwrap_or_default()` instead of being rejected.
    if items.is_empty() && p.get("channels").is_none() {
        return Err(bad("clip.audioChannels", "select a clip with audio, or a project item to set its format"));
    }
    if p.get("channels").is_some() {
        // timeline audio clips: which source channels each plays
        let clips = match p.get("clips").and_then(Value::as_array) {
            Some(a) if a.iter().all(Value::is_u64) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
            _ => selected_audio_clips(s),
        };
        let chans: Vec<u16> =
            p.get("channels").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_u64().map(|c| c as u16)).collect()).unwrap_or_default();
        if clips.is_empty() {
            return Err(bad("clip.audioChannels", "select a clip with audio"));
        }
        s.edit_sequence("Modify Audio Channels", |q, _, _| {
            for c in &clips {
                if let Some((_, it)) = q.find_item_mut(*c) {
                    it.source_channels = chans.clone();
                }
            }
            Ok(())
        })?;
        return Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "channels": chans}));
    }
    let format = match str_p(p, "format") {
        Some(f) => Some(parse_format(f).ok_or_else(|| bad("clip.audioChannels", "format is mono, stereo, 5.1 or adaptive"))?),
        None => None,
    };
    let custom: Option<Vec<Vec<u16>>> = p
        .get("clips")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|c| c.as_array().map(|x| x.iter().filter_map(|v| v.as_u64().map(|n| n as u16)).collect()).unwrap_or_default()).collect());
    let mut out = Vec::new();
    let mut maps = Vec::new();
    for i in &items {
        let Some(m) = s.project.item(*i).and_then(|it| it.as_media()) else { continue };
        let n = m.info.audio().map_or(2, |a| a.channels) as u16;
        let fmt = format.unwrap_or_else(|| m.interpret.audio_channels.as_ref().map_or(AudioChannels::Stereo, |a| a.format));
        let map = match &custom {
            Some(c) => {
                if c.is_empty() || c.iter().any(|x| x.is_empty() || x.iter().any(|ch| *ch >= n)) {
                    return Err(bad("clip.audioChannels", format!("each clip needs source channels between 0 and {}", n - 1)));
                }
                AudioChannelMap { format: fmt, clips: c.clone() }
            }
            None => AudioChannelMap::for_format(fmt, n),
        };
        out.push(json!({"item": i.0, "format": format!("{:?}", map.format), "clips": map.clips}));
        maps.push((*i, map));
    }
    s.edit("Modify Audio Channels", |pr, _| {
        for (i, map) in maps {
            if let Some(m) = pr.item_mut(i).and_then(|it| it.as_media_mut()) {
                // the default stereo pair is no override
                m.interpret.audio_channels = if map == AudioChannelMap::for_format(AudioChannels::Stereo, 2) { None } else { Some(map) };
            }
        }
        Ok(())
    })?;
    Ok(json!({"items": out}))
}

pub(super) fn modify_timecode(s: &mut Session, p: &Value) -> Result<Value> {
    let item = item_p(p, "item")
        .or_else(|| s.state.project_selection.iter().copied().find(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some()))
        .ok_or_else(|| bad("clip.modifyTimecode", "select a clip"))?;
    let m = s.project.item(item).and_then(|i| i.as_media()).ok_or_else(|| bad("clip.modifyTimecode", "not a media clip"))?;
    let rate = m.frame_rate();
    let reset = bool_p(p, "reset").unwrap_or(false);
    let frame = if let Some(tc) = str_p(p, "timecode") {
        Some(parse_timecode(tc, rate, false, 0).map_err(|e| bad("clip.modifyTimecode", format!("timecode: {e:?}")))?)
    } else {
        p.get("frame").and_then(Value::as_i64)
    };
    let tape = str_p(p, "tapeName").map(str::to_string);
    if frame.is_none() && tape.is_none() && !reset {
        return Err(bad("clip.modifyTimecode", "need `timecode`, `frame`, `tapeName` or `reset`"));
    }
    if frame.is_some_and(|f| f < 0) {
        return Err(bad("clip.modifyTimecode", "timecode must not be negative"));
    }
    const FILE_TC: &str = "File Timecode";
    let r = s.edit("Modify Timecode", |pr, _| {
        let it = pr.item_mut(item).ok_or_else(|| bad("clip.modifyTimecode", "no such item"))?;
        let cur = it.as_media().and_then(|m| m.info.start_timecode);
        if reset {
            // back to the file's own timecode ("none" = the file had none)
            if let Some(orig) = it.metadata.remove(FILE_TC)
                && let Some(m) = it.as_media_mut()
            {
                m.info.start_timecode = orig.parse::<i64>().ok();
            }
        } else if let Some(f) = frame {
            it.metadata.entry(FILE_TC.to_string()).or_insert_with(|| cur.map(|c| c.to_string()).unwrap_or_else(|| "none".into()));
            if let Some(m) = it.as_media_mut() {
                m.info.start_timecode = Some(f);
            }
        }
        if let Some(t) = &tape {
            if t.is_empty() {
                it.metadata.remove("Tape Name");
            } else {
                it.metadata.insert("Tape Name".into(), t.clone());
            }
        }
        Ok(it.as_media().and_then(|m| m.info.start_timecode))
    })?;
    Ok(json!({"item": item.0, "startTimecode": r}))
}

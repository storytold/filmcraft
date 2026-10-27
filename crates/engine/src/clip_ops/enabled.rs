use super::common::media_root;
use super::*;

// ---------------------------------------------------------------------------------------------
// enablement
// ---------------------------------------------------------------------------------------------

pub(super) fn can_label(s: &Session) -> std::result::Result<(), String> {
    if s.state.selection.is_empty() && s.state.project_selection.is_empty() { Err("select clips or project items".into()) } else { Ok(()) }
}

pub(super) fn has_project_items(s: &Session) -> std::result::Result<(), String> {
    if s.project.items.values().any(|i| !matches!(i.kind, ItemKind::Graphic { .. })) { Ok(()) } else { Err("the project is empty".into()) }
}

pub(super) fn is_clip_item(p: &Project, id: ItemId) -> bool {
    p.item(id).is_some_and(|i| matches!(i.kind, ItemKind::Media(_) | ItemKind::Subclip { .. } | ItemKind::Sequence(_) | ItemKind::AdjustmentLayer { .. }))
}

pub(super) fn has_clip_items(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| is_clip_item(&s.project, *i)) { Ok(()) } else { Err("select a clip in the Project panel".into()) }
}

pub(super) fn can_paste_attributes(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.state.clipboard.is_empty() { Err("copy a clip first".into()) } else { Ok(()) }
}

pub(super) fn subclip_source(s: &Session) -> Option<ItemId> {
    s.state
        .source_item
        .filter(|i| media_root(&s.project, *i).is_some())
        .or_else(|| s.state.project_selection.iter().copied().find(|i| media_root(&s.project, *i).is_some()))
}

pub(super) fn can_make_subclip(s: &Session) -> std::result::Result<(), String> {
    if subclip_source(s).is_some() || !s.state.selection.is_empty() { Ok(()) } else { Err("open a clip in the Source monitor or select one".into()) }
}

pub(super) fn has_subclip(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| s.project.item(*i).is_some_and(|it| matches!(it.kind, ItemKind::Subclip { .. }))) {
        Ok(())
    } else {
        Err("select a subclip in the Project panel".into())
    }
}

pub(super) fn has_media_selection(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some()) {
        Ok(())
    } else {
        Err("select a clip in the Project panel".into())
    }
}

fn audio_items(s: &Session) -> Vec<ItemId> {
    s.state.project_selection.iter().copied().filter(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some_and(|m| m.info.has_audio())).collect()
}

pub(super) fn has_audio_items(s: &Session) -> std::result::Result<(), String> {
    if audio_items(s).is_empty() { Err("select a clip with audio in the Project panel".into()) } else { Ok(()) }
}

pub(super) fn selected_audio_clips(s: &Session) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    s.state.selection.iter().copied().filter(|c| q.audio_tracks.iter().any(|t| t.item(*c).is_some())).collect()
}

pub(super) fn has_audio_target(s: &Session) -> std::result::Result<(), String> {
    if audio_items(s).is_empty() && selected_audio_clips(s).is_empty() { Err("select a clip with audio".into()) } else { Ok(()) }
}

pub(super) fn extract_items(s: &Session) -> Vec<ItemId> {
    let mut out: Vec<ItemId> = s.state.project_selection.clone();
    if out.is_empty()
        && let Some(q) = s.active_sequence()
    {
        for c in &s.state.selection {
            if let Some((_, i)) = q.find_item(*c)
                && !out.contains(&i.item)
            {
                out.push(i.item);
            }
        }
    }
    out.retain(|i| media_root(&s.project, *i).is_some_and(|(_, m, _)| m.info.has_audio()));
    out
}

pub(super) fn has_extractable(s: &Session) -> std::result::Result<(), String> {
    if extract_items(s).is_empty() { Err("select a clip with audio".into()) } else { Ok(()) }
}

pub(super) fn can_replace_from_source(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    s.state.source_item.map(|_| ()).ok_or_else(|| "open a clip in the Source monitor".into())
}

pub(super) fn can_replace_from_bin(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.state.project_selection.iter().any(|i| is_clip_item(&s.project, *i)) { Ok(()) } else { Err("select a clip in the Project panel".into()) }
}

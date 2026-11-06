//! `link.editIn` on a clip that plays a linked result: reopen the link, nest nothing.
//!
//! A linked item stands for one EffectCraft comp. Nesting its clip again would build a second
//! item for a result that nothing ever rendered, so the call asks EffectCraft for the comp that
//! exists. EffectCraft answers from the comp it still holds, or from the sidecar project it wrote
//! at the first hand-off. When it has neither, the nest that the first hand-off built is exported
//! again under the same `link_id`, and the link record keeps its item.
//!
//! The project does not change on a reopen, so there is no undo step and no banner. The one
//! exception is a rebuild that makes EffectCraft name a new comp or sidecar: the record on the
//! linked item then follows.

use filmcraft_engine::project::{ItemId, TrackKind};
use filmcraft_engine::{EngineError, Session};
use serde_json::{Value, json};

use crate::edit_in::{Attached, EditInError, EditInOptions, LinkPeer, activate_peer, export_otio, failed, new_uuid, open_params, place_view, required_text};
use crate::link_ops::{LINK_KEY, record_of};
use crate::place::Place;
use crate::record::LinkRecord;

/// The linked item that the selected video clips play, and the record on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Linked {
    pub media: ItemId,
    pub record: LinkRecord,
}

/// The link that every selected video clip plays.
///
/// - `Ok(None)`: no selected video clip is linked. `link.editIn` nests them.
/// - `Ok(Some)`: every selected video clip plays the same linked item.
/// - `Err`: the selection mixes linked and unlinked clips, or clips of two links, or a record is
///   damaged. The text is one line for the disabled menu entry.
pub fn linked_selection(session: &Session) -> Result<Option<Linked>, String> {
    let Some(sequence) = session.active_sequence() else { return Ok(None) };
    let mut found: Option<Linked> = None;
    let mut plain = 0usize;
    for clip_id in &session.state.selection {
        let Some((track_id, clip)) = sequence.find_item(*clip_id) else { continue };
        if sequence.track(track_id).is_none_or(|track| track.kind != TrackKind::Video) {
            continue;
        }
        match record_of(&session.project, clip.item)? {
            None => plain += 1,
            Some(record) => match &found {
                Some(first) if first.record.link_id != record.link_id => return Err(MORE_THAN_ONE_LINK.to_string()),
                Some(_) => {}
                None => found = Some(Linked { media: clip.item, record }),
            },
        }
    }
    match (found, plain) {
        (Some(_), 1..) => Err(MIXED_SELECTION.to_string()),
        (found, _) => Ok(found),
    }
}

/// Why the menu entry is grey when linked and unlinked clips are selected together. The engine
/// repeats this string, because it cannot call this crate.
pub const MIXED_SELECTION: &str = "the selection mixes linked and unlinked clips";

/// Why the menu entry is grey when the selection holds clips of two links.
pub const MORE_THAN_ONE_LINK: &str = "the selection holds clips of more than one link";

/// Bring the comp of `linked` up in EffectCraft and activate it.
///
/// The reply has `reopened: true` and no `lifted_effects` or `unsupported`, so no banner is
/// raised and Undo is not offered. `rebuilt: true` says EffectCraft had no comp and no sidecar,
/// and the comp was imported again from the nest.
pub fn reopen(
    session: &mut Session,
    peer: &mut dyn LinkPeer,
    backend: &dyn craft_ipc::activate::ActivationBackend,
    opts: &EditInOptions,
    linked: &Linked,
) -> Result<Value, EditInError> {
    let record = &linked.record;
    let mut reply = match peer.attach(&record.link_id, &record.target_project, &record.target_comp)? {
        Attached::Reopened => reply_of(linked, peer, false),
        Attached::Missing(why) => {
            rebuild(session, peer, opts, linked, &why)?;
            reply_of(linked, peer, true)
        }
    };
    activate_peer(peer, backend, &mut reply);
    Ok(reply)
}

fn reply_of(linked: &Linked, peer: &dyn LinkPeer, rebuilt: bool) -> Value {
    json!({
        "reopened": true,
        "rebuilt": rebuilt,
        "activated": false,
        "link_id": linked.record.link_id,
        "item": linked.media.0,
        "sequence": linked.record.nested_sequence.parse::<u64>().ok(),
        "result_path": linked.record.result_path,
        "instance": peer.instance(),
    })
}

/// Open the link again from its nest, with the same `link_id` and result path.
fn rebuild(session: &mut Session, peer: &mut dyn LinkPeer, opts: &EditInOptions, linked: &Linked, why: &str) -> Result<(), EditInError> {
    let record = &linked.record;
    let nest = record
        .nested_sequence
        .parse::<u64>()
        .ok()
        .map(ItemId)
        .filter(|id| session.project.sequence(*id).is_some())
        .ok_or_else(|| EditInError::Failed(format!("EffectCraft cannot reopen the comp ({why}) and the nest of this link is no longer in the project")))?;
    let place = Place::capture(session, place_view(session)).map_err(|err| EditInError::Failed(err.to_string()))?;
    // Sources with no file are rendered first, like the first hand-off does.
    let exported = crate::prepare::resolve(session, nest, &crate::prepare::PrepareOptions { cache_dir: opts.cache_dir.clone(), wait: opts.wait })?;
    let data = export_otio(&exported, nest)?;
    let params = open_params(&record.link_id, &data, &place, &record.result_path, &new_uuid()?, nest);
    let opened = peer.open(params)?;
    let target_comp = required_text(&opened, "comp")?;
    let target_project = required_text(&opened, "project")?;
    if target_comp == record.target_comp && target_project == record.target_project {
        return Ok(());
    }
    // EffectCraft named the comp or the sidecar differently this time. Follow it, or the next
    // reopen asks for a comp that is not there.
    let mut moved = record.clone();
    moved.target_comp = target_comp;
    moved.target_project = target_project;
    moved.validate().map_err(EditInError::Failed)?;
    let text = serde_json::to_string(&moved).map_err(|err| EditInError::Failed(err.to_string()))?;
    let media = linked.media;
    let written = session.edit("link.reopen", move |project, _state| {
        let item = project.item_mut(media).ok_or_else(|| EngineError::Other("the linked item is missing".into()))?;
        item.metadata.insert(LINK_KEY.to_string(), text);
        Ok(())
    });
    if let Err(err) = written {
        let err = failed(err);
        return Err(match peer.close(&record.link_id) {
            Ok(_) => err,
            Err(close) => EditInError::Failed(format!("{err}; closing the EffectCraft link also failed: {close}")),
        });
    }
    Ok(())
}

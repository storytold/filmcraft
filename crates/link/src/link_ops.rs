//! The commands that act on an existing link: `focus`, `unlink`, `refresh`.
//!
//! Each finds its link by `link_id` in `ProjectItem.metadata["craft.link"]`. `unlink` also accepts
//! the selection, so the menu entry needs no parameters.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use craft_ipc::activate;
use filmcraft_engine::panels::Level;
use filmcraft_engine::project::{ClipId, ItemId, ItemKind, MediaRef, Project};
use filmcraft_engine::time::Tick;
use filmcraft_engine::{EngineError, Session, relink};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::edit_in::LinkPeer;
use crate::edit_in::activated_json;
use crate::host::{Host, host};
use crate::peer::{PeerChoice, Selection, SocketPeer, select_instance};
use crate::place::{Place, PlaceView, SelectionRange};
use crate::record::LinkRecord;

/// Metadata key of the link record on the linked media item.
pub const LINK_KEY: &str = "craft.link";

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.to_string(), msg: msg.into() }
}

fn text_param<'a>(cmd: &str, params: &'a Value, key: &str) -> Result<Option<&'a str>, EngineError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text)),
        Some(_) => Err(bad(cmd, format!("`{key}` must be a non-empty string"))),
    }
}

/// The record on `item`, or `None` when it carries none. A record that does not parse is an error:
/// the user must hear that the link is damaged.
pub fn record_of(project: &Project, item: ItemId) -> Result<Option<LinkRecord>, String> {
    let Some(text) = project.item(item).and_then(|it| it.metadata.get(LINK_KEY)) else { return Ok(None) };
    let record: LinkRecord = serde_json::from_str(text).map_err(|err| format!("the link record on item {} is damaged: {err}", item.0))?;
    record.validate().map_err(|err| format!("the link record on item {} is invalid: {err}", item.0))?;
    Ok(Some(record))
}

/// The media item that carries link `link_id`.
///
/// A damaged record elsewhere does not hide a good one. If nothing matches and some record was
/// damaged, that is the answer, because the missing link may be the damaged one.
pub fn find_link(project: &Project, link_id: &str) -> Result<Option<(ItemId, LinkRecord)>, String> {
    let mut damaged = None;
    for (id, item) in &project.items {
        if !item.metadata.contains_key(LINK_KEY) {
            continue;
        }
        match record_of(project, *id) {
            Ok(Some(record)) if record.link_id == link_id => return Ok(Some((*id, record))),
            Ok(_) => {}
            Err(err) => {
                damaged.get_or_insert(err);
            }
        }
    }
    match damaged {
        Some(err) => Err(err),
        None => Ok(None),
    }
}

/// The link named by `link_id`, else the link of the first selected clip.
fn resolve(session: &Session, cmd: &str, params: &Value) -> Result<(ItemId, LinkRecord), EngineError> {
    if let Some(link_id) = text_param(cmd, params, "link_id")? {
        return match find_link(&session.project, link_id).map_err(EngineError::Other)? {
            Some(found) => Ok(found),
            None => Err(EngineError::Other(format!("link_unknown: no item carries link {link_id}"))),
        };
    }
    let sequence = session.active_sequence().ok_or(EngineError::NoSequence)?;
    for clip in &session.state.selection {
        let Some((_, item)) = sequence.find_item(*clip) else { continue };
        if let Some(record) = record_of(&session.project, item.item).map_err(EngineError::Other)? {
            return Ok((item.item, record));
        }
    }
    Err(EngineError::Other("select a clip that is linked to EffectCraft, or give `link_id`".to_string()))
}

/// Every sequence with a clip of `item`, and those clips.
fn clips_of(project: &Project, item: ItemId) -> BTreeMap<ItemId, Vec<ClipId>> {
    let mut found = BTreeMap::new();
    for (id, entry) in &project.items {
        let ItemKind::Sequence(sequence) = &entry.kind else { continue };
        let clips: Vec<ClipId> = sequence.all_tracks().flat_map(|track| track.items.iter()).filter(|clip| clip.item == item).map(|clip| clip.id).collect();
        if !clips.is_empty() {
            found.insert(*id, clips);
        }
    }
    found
}

// ---------------------------------------------------------------- focus

/// The place EffectCraft sends with `link.focus`. Every field is optional: a missing one leaves
/// that part of the editor where it is.
#[derive(Debug, Default, Deserialize)]
struct WirePlace {
    playhead: Option<Tick>,
    seconds: Option<f64>,
    #[serde(default)]
    selection: Vec<SelectionRange>,
    view: Option<WireView>,
}

#[derive(Debug, Default, Deserialize)]
struct WireView {
    zoom: Option<f64>,
    scroll_s: Option<f64>,
}

/// `link.focus {link_id, place, activation?}`: open the sequence that holds the link, move to
/// `place`, and (when the sender gave a token) take focus. It also takes the result file in
/// ([`bring_online`]), because Back sends no `link.refresh`: an offline item comes online, and an
/// online one whose file changed is reopened.
///
/// The playhead is `place.playhead` ticks, else `place.seconds`. The selection maps by time. The
/// view is applied only when `zoom` and `scroll_s` both come, because only FilmCraft's own
/// round trip defines `zoom` as pixels per second.
pub fn focus(session: &mut Session, params: &Value) -> Result<Value, EngineError> {
    const CMD: &str = "link.focus";
    let link_id = text_param(CMD, params, "link_id")?.ok_or_else(|| bad(CMD, "`link_id` is required"))?;
    let (media, _record) = match find_link(&session.project, link_id).map_err(EngineError::Other)? {
        Some(found) => found,
        None => return Err(EngineError::Other(format!("link_unknown: no item carries link {link_id}"))),
    };
    let wire: WirePlace = match params.get("place") {
        None | Some(Value::Null) => WirePlace::default(),
        Some(value) => serde_json::from_value(value.clone()).map_err(|err| bad(CMD, format!("`place` is invalid: {err}")))?,
    };
    if wire.seconds.is_some_and(|s| !s.is_finite())
        || wire.view.as_ref().is_some_and(|v| v.zoom.is_some_and(|z| !z.is_finite()) || v.scroll_s.is_some_and(|s| !s.is_finite()))
    {
        return Err(bad(CMD, "`place` holds a number that is not finite"));
    }
    let sequence = open_sequence_of(session, media)?;
    let playhead = match (wire.playhead, wire.seconds) {
        (Some(ticks), _) => ticks,
        (None, Some(seconds)) => Tick::from_seconds_f64(seconds),
        (None, None) => session.playhead(),
    };
    let view = applied_view(&wire);
    let place = Place { playhead, seconds: playhead.seconds(), selection: wire.selection, view: view.unwrap_or(PlaceView { zoom: 0.0, scroll_s: 0.0 }) };
    place.restore(session).map_err(|err| EngineError::Other(err.to_string()))?;
    if let Some(view) = view
        && let Some(shown) = session.state.timeline_views.get_mut(&sequence)
        && view.zoom > 0.0
    {
        shown.pps = view.zoom;
        shown.scroll = view.scroll_s.max(0.0);
    }
    let mut reply = json!({ "sequence": sequence.0, "selected": session.state.selection.len() });
    // Back renders the result over the same path, then sends only this call. Take the new file in
    // now, so the monitor shows it and does not wait for the poller (offline item) or keep the
    // decoder of the file it replaced (online item).
    let readiness = bring_online(session, media)?;
    if let Some(map) = reply.as_object_mut() {
        map.insert("online".to_string(), Value::Bool(readiness.is_online()));
        if let Readiness::Unreadable(why) = &readiness {
            map.insert("online_error".to_string(), Value::String(why.clone()));
        }
    }
    if let Some(activation) = params.get("activation") {
        let token = activation.get("token").and_then(Value::as_str);
        let activated = activate::apply_activation(&*host().backend, token);
        if let Some(map) = reply.as_object_mut() {
            map.insert("activated".to_string(), activated_json(activated));
        }
    }
    Ok(reply)
}

fn applied_view(wire: &WirePlace) -> Option<PlaceView> {
    let view = wire.view.as_ref()?;
    Some(PlaceView { zoom: view.zoom?, scroll_s: view.scroll_s? })
}

/// Make the sequence that holds `media` the active one. The active sequence wins when it holds it.
fn open_sequence_of(session: &mut Session, media: ItemId) -> Result<ItemId, EngineError> {
    let holders = clips_of(&session.project, media);
    let active = session.state.active_sequence;
    let target = match active.filter(|id| holders.contains_key(id)) {
        Some(id) => id,
        None => *holders.keys().next().ok_or_else(|| EngineError::Other("no sequence has a clip of the linked item".to_string()))?,
    };
    if active != Some(target) {
        session.state.active_sequence = Some(target);
    }
    if !session.state.open_sequences.contains(&target) {
        session.state.open_sequences.push(target);
    }
    Ok(target)
}

// ---------------------------------------------------------------- unlink

/// `link.unlink`: swap every clip of the linked item back to the nested sequence, in one undo step.
///
/// The linked media item goes too, once no clip uses it. Then EffectCraft is told to close the
/// link if it runs. A peer that is not running, or ambiguous, is reported in the reply and does not
/// undo the unlink: the project no longer points at it either way.
pub fn unlink(session: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let (media, record) = resolve(session, "link.unlink", params)?;
    let nested = ItemId(
        record.nested_sequence.parse::<u64>().map_err(|_| EngineError::Other(format!("nested_sequence {:?} is not an item id", record.nested_sequence)))?,
    );
    if session.project.sequence(nested).is_none() {
        return Err(EngineError::Other("the nested sequence is gone, so the clips cannot be restored".to_string()));
    }
    let swapped = session.edit("Unlink from EffectCraft", move |project, _state| swap_back(project, media, nested))?;
    let host = host();
    let (peer_closed, peer_note) = close_peer(&host, &record);
    Ok(json!({ "link_id": record.link_id, "restored": swapped, "peer_closed": peer_closed, "peer_note": peer_note }))
}

fn swap_back(project: &mut Project, media: ItemId, nested: ItemId) -> Result<usize, EngineError> {
    let (name, label) = {
        let item = project.item(nested).ok_or_else(|| EngineError::Other("the nested sequence is gone".to_string()))?;
        (item.name.clone(), item.label)
    };
    let holders = clips_of(project, media);
    let mut swapped = 0usize;
    for (sequence_id, clips) in holders {
        let sequence = project.sequence_mut(sequence_id).ok_or(EngineError::NoSequence)?;
        for clip_id in clips {
            let Some((_, clip)) = sequence.find_item_mut(clip_id) else { continue };
            clip.item = nested;
            clip.name = name.clone();
            clip.label = label;
            clip.source_in = Tick::ZERO;
            clip.frame_hold = None;
            clip.multicam = None;
            swapped += 1;
        }
        sequence.check().map_err(EngineError::Other)?;
    }
    if swapped == 0 {
        return Err(EngineError::Other("no clip uses the linked item".to_string()));
    }
    if clips_of(project, media).is_empty() {
        project.items.remove(&media);
        project.root.remove_item(media);
    }
    Ok(swapped)
}

/// Tell a running EffectCraft that holds the link's project to drop the binding.
fn close_peer(host: &Host, record: &LinkRecord) -> (bool, String) {
    let dir = match host.runtime_dir() {
        Ok(dir) => dir,
        Err(err) => return (false, format!("could not look for EffectCraft: {err}")),
    };
    let records = match craft_ipc::Discovery::scan(&dir) {
        Ok(records) => records,
        Err(err) => return (false, format!("could not look for EffectCraft: {err}")),
    };
    let choice = PeerChoice { instance: None, holds_project: Some(&record.target_project) };
    let selected = match select_instance(&records, &choice) {
        Ok(Selection::One(found)) => *found,
        Ok(Selection::Start) => return (false, "EffectCraft is not running".to_string()),
        Ok(Selection::Choose(_)) => return (false, "several EffectCraft windows are open and none holds the project".to_string()),
        Err(err) => return (false, err.to_string()),
    };
    let mut peer = match SocketPeer::connect(host, &PeerChoice { instance: Some(&selected.instance), holds_project: None }, false) {
        Ok(peer) => peer,
        Err(err) => return (false, err.to_string()),
    };
    match peer.close(&record.link_id) {
        Ok(_) => (true, "EffectCraft closed the link".to_string()),
        Err(err) => (false, err.to_string()),
    }
}

// ---------------------------------------------------------------- refresh

static EPOCHS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// The newest epoch `link.refresh` has seen for `link_id`. 0 before the first.
pub fn epoch_of(link_id: &str) -> u64 {
    EPOCHS.lock().unwrap_or_else(PoisonError::into_inner).get(link_id).copied().unwrap_or(0)
}

/// `link.refresh {link_id, epoch?}`: EffectCraft changed the comp. Remember the epoch, bring the
/// linked item online when its result file exists, delete the render previews that cover the
/// linked clips, and bump the view so the frame cache refetches.
///
/// An epoch not newer than the last one seen changes no previews and says so. It still brings an
/// offline item online, because the file may have arrived since the last refresh. The reply says
/// `online`, and `online_error` when the file exists but cannot be opened yet.
pub fn refresh(session: &mut Session, params: &Value) -> Result<Value, EngineError> {
    const CMD: &str = "link.refresh";
    let link_id = text_param(CMD, params, "link_id")?.ok_or_else(|| bad(CMD, "`link_id` is required"))?;
    let (media, _record) = match find_link(&session.project, link_id).map_err(EngineError::Other)? {
        Some(found) => found,
        None => return Err(EngineError::Other(format!("link_unknown: no item carries link {link_id}"))),
    };
    let epoch = match params.get("epoch") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| bad(CMD, "`epoch` must be a non-negative integer"))?),
    };
    let current = epoch_of(link_id);
    let next = epoch.unwrap_or_else(|| current.saturating_add(1));
    let stale = next <= current;
    // A newer epoch means EffectCraft wrote the result file again. The pool still holds the
    // decoder of the file it replaced, so the source is opened anew.
    let readiness = sync_result(session, media, !stale)?;
    let mut reply = json!({ "online": readiness.is_online() });
    if let (Readiness::Unreadable(why), Some(map)) = (&readiness, reply.as_object_mut()) {
        map.insert("online_error".to_string(), Value::String(why.clone()));
    }
    let deleted = if stale {
        0
    } else {
        EPOCHS.lock().unwrap_or_else(PoisonError::into_inner).insert(link_id.to_string(), next);
        let deleted = delete_stale_previews(session, media);
        session.bump_view();
        deleted
    };
    if let Some(map) = reply.as_object_mut() {
        map.insert("link_id".to_string(), Value::String(link_id.to_string()));
        map.insert("epoch".to_string(), Value::from(if stale { current } else { next }));
        map.insert("stale".to_string(), Value::Bool(stale));
        map.insert("previews_deleted".to_string(), Value::from(deleted));
    }
    Ok(reply)
}

// ---------------------------------------------------------------- result online

/// Where a linked result file stands, as [`bring_online`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The item was online already.
    Online,
    /// The item was offline, the file opens, and the item now plays it.
    BroughtOnline,
    /// The item was online, the file changed, and the item now plays the new file.
    Reopened,
    /// The file is not there yet.
    Waiting,
    /// The file is there and cannot be opened (EffectCraft may still be writing it).
    Unreadable(String),
}

impl Readiness {
    /// True when the item plays the file.
    pub fn is_online(&self) -> bool {
        matches!(self, Self::Online | Self::BroughtOnline | Self::Reopened)
    }
}

/// Bring the linked media item `media` online when its result file exists and opens.
///
/// `link.editIn` makes the item with `file.newOfflineFile`, which sets the offline flag: the file
/// does not exist yet. Nothing clears the flag when EffectCraft writes the file, and the media pool
/// keys its sources on that flag, so the item would show the offline slate for good. This does what
/// `media.relink` does for one item, with the file's own properties (codec, size, duration).
///
/// It is a state sync and not a user edit, so it leaves no undo step and keeps the redo steps.
/// The project is marked as changed, so a save records the item as online.
pub fn bring_online(session: &mut Session, media: ItemId) -> Result<Readiness, EngineError> {
    sync_result(session, media, false)
}

/// [`bring_online`], and for an item that is online already: reopen the file when `reopen` is set
/// or when the file's identity (size and head/tail fingerprint) differs from the one the item
/// recorded. The media pool keys its sources on the path, so a result file that EffectCraft writes
/// again under the same path would otherwise keep playing through the old decoder.
fn sync_result(session: &mut Session, media: ItemId, reopen: bool) -> Result<Readiness, EngineError> {
    let item = session.project.item(media).ok_or_else(|| EngineError::Other(format!("item {} is gone", media.0)))?;
    let clip = item.as_media().ok_or_else(|| EngineError::Other(format!("item {} is not media", media.0)))?;
    let MediaRef::File { path } = &clip.media else {
        return Err(EngineError::Other(format!("item {} is not a file", media.0)));
    };
    let was_offline = clip.offline;
    let recorded = clip.identity;
    let path = path.clone();
    if !was_offline && !reopen {
        let now = relink::identity_of(&*session.services, &path).ok();
        if now.is_none() || now == recorded {
            return Ok(Readiness::Online);
        }
    }
    if !session.services.file_size(&path).is_ok_and(|bytes| bytes > 0) {
        return Ok(Readiness::Waiting);
    }
    let source = match session.media.open_file(&path, &*session.services) {
        Ok(source) => source,
        Err(err) => return Ok(Readiness::Unreadable(err.to_string())),
    };
    let info = source.info().clone();
    let identity = relink::identity_of(&*session.services, &path).ok();
    // `edit` pushes one undo step and clears redo. Take both back after it.
    let redo = std::mem::take(&mut session.history.redo);
    session.history.limit += 1;
    let edited = session.edit("Link Result Online", |project, _state| {
        let clip = project.item_mut(media).and_then(|item| item.as_media_mut()).ok_or_else(|| EngineError::Other(format!("item {} is gone", media.0)))?;
        clip.offline = false;
        clip.info = info;
        if identity.is_some() {
            clip.identity = identity;
        }
        Ok(())
    });
    session.history.limit -= 1;
    session.history.redo = redo;
    edited?;
    session.history.undo.pop();
    session.media.insert_file(media, &path, source);
    // A live link wraps the new file source again.
    crate::manager::after_open(session, media);
    session.offline.missing.retain(|id| *id != media);
    if was_offline {
        return Ok(Readiness::BroughtOnline);
    }
    session.bump_view();
    Ok(Readiness::Reopened)
}

/// The linked items that are still offline, with their result paths.
fn offline_links(project: &Project) -> Vec<(ItemId, String)> {
    project
        .items
        .iter()
        .filter(|(_, item)| item.metadata.contains_key(LINK_KEY))
        .filter_map(|(id, item)| match &item.as_media()?.media {
            MediaRef::File { path } if item.as_media()?.offline => Some((*id, path.clone())),
            _ => None,
        })
        .collect()
}

/// Bring every offline linked item online whose result file now opens. Returns how many came online.
pub fn heal_offline_links(session: &mut Session) -> usize {
    let mut healed = 0;
    for (item, _) in offline_links(&session.project) {
        match bring_online(session, item) {
            Ok(Readiness::BroughtOnline) => healed += 1,
            Ok(_) => {}
            Err(err) => session.log.push(Level::Warning, "link.refresh", err.to_string()),
        }
    }
    healed
}

/// How often the UI looks for result files that have appeared.
pub const RESULT_POLL: Duration = Duration::from_secs(1);

/// Watches for result files of offline linked items, at most once per [`RESULT_POLL`].
///
/// A file that exists but cannot be opened is tried again only when its size changes, so a bad
/// file costs one open and one warning, not one per second.
#[derive(Debug, Default)]
pub struct ResultPoller {
    last: Option<Instant>,
    gave_up: BTreeMap<ItemId, u64>,
}

impl ResultPoller {
    /// Look at the offline linked items if [`RESULT_POLL`] has passed since the last look.
    /// Returns true while some linked item is still offline, so the caller polls again.
    pub fn poll(&mut self, session: &mut Session, now: Instant) -> bool {
        let waiting = offline_links(&session.project);
        if self.last.is_some_and(|last| now.saturating_duration_since(last) < RESULT_POLL) {
            return !waiting.is_empty();
        }
        self.last = Some(now);
        self.gave_up.retain(|item, _| waiting.iter().any(|(id, _)| id == item));
        let mut still_offline = false;
        for (item, path) in waiting {
            let Ok(size) = session.services.file_size(&path) else {
                still_offline = true;
                continue;
            };
            if self.gave_up.get(&item) == Some(&size) {
                still_offline = true;
                continue;
            }
            match bring_online(session, item) {
                Ok(Readiness::BroughtOnline | Readiness::Online | Readiness::Reopened) => {}
                Ok(Readiness::Waiting) => still_offline = true,
                Ok(Readiness::Unreadable(why)) => {
                    self.gave_up.insert(item, size);
                    still_offline = true;
                    session.log.push(Level::Warning, "link.refresh", format!("the result {path} cannot be opened yet: {why}"));
                }
                Err(err) => {
                    still_offline = true;
                    session.log.push(Level::Warning, "link.refresh", err.to_string());
                }
            }
        }
        still_offline
    }
}

static POLLER: Mutex<Option<ResultPoller>> = Mutex::new(None);

/// [`ResultPoller::poll`] on the process poller. The UI calls this once per frame.
pub fn poll_results(session: &mut Session) -> bool {
    let mut slot = POLLER.lock().unwrap_or_else(PoisonError::into_inner);
    slot.get_or_insert_with(ResultPoller::default).poll(session, Instant::now())
}

/// Render previews are named by a hash of the clips and not of the file contents, so a changed
/// comp leaves a stale preview for every segment that shows the linked item.
fn delete_stale_previews(session: &Session, media: ItemId) -> usize {
    let project = std::sync::Arc::clone(&session.project);
    let mut hashes: BTreeSet<String> = BTreeSet::new();
    for (sequence, clips) in clips_of(&project, media) {
        for segment in session.previews.segments(&project, sequence).iter() {
            if segment.clips.iter().any(|clip| clips.contains(clip)) {
                hashes.insert(segment.hash.clone());
            }
        }
    }
    if hashes.is_empty() {
        return 0;
    }
    let hashes: Vec<String> = hashes.into_iter().collect();
    session.previews.delete(Some(&hashes))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::fixtures::{linked_offline, record};

    fn offline(session: &mut Session, name: &str) -> ItemId {
        let made = session
            .execute(
                "file.newOfflineFile",
                json!({"name": name, "fileName": "/tmp/x.mov", "video": true, "audio": false, "width": 320, "height": 180, "fps": 24.0, "seconds": 1.0}),
            )
            .expect("offline item");
        ItemId(made["item"].as_u64().expect("item id"))
    }

    const GOOD: &str = "0b9f6c1e-6a52-4f43-9d8e-4b7d3f0a1c22";

    const ONLINE: &str = "7c1d2e3f-0a4b-4c5d-8e6f-1a2b3c4d5e6f";
    const JUNK: &str = "8d2e3f40-1b5c-4d6e-9f70-2b3c4d5e6f70";
    const APPEAR: &str = "9e3f4051-2c6d-4e7f-a081-3c4d5e6f7081";

    fn status_of(session: &mut Session, item: ItemId) -> String {
        let rows = session.execute("media.status", json!({"item": item.0})).expect("media.status");
        rows[0]["status"].as_str().expect("status").to_string()
    }

    #[test]
    fn refresh_brings_the_result_online_once_the_file_exists() {
        let dir = std::env::temp_dir().join(format!("fc-link-online-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        let mut session = Session::default();
        crate::commands::install(&mut session);
        let item = linked_offline(&mut session, ONLINE, &path);

        // No file yet: the refresh says so and the item stays offline.
        let early = session.execute("plugin.craft.link.refresh", json!({"link_id": ONLINE, "epoch": 1})).expect("refresh");
        assert_eq!(status_of(&mut session, item), "offline");
        assert_ne!(early["online"], true, "{early}");

        // EffectCraft writes the result, then tells FilmCraft the epoch moved.
        crate::fixtures::write_movie(&path, 160, 90, 24);
        let undo = session.history.undo.len();
        let revision = session.revision;
        let reply = session.execute("plugin.craft.link.refresh", json!({"link_id": ONLINE, "epoch": 2})).expect("refresh");
        assert_eq!(status_of(&mut session, item), "online", "the result must come online without media.relink: {reply}");
        assert_eq!(reply["online"], true, "{reply}");
        let media = session.project.item(item).unwrap().as_media().unwrap().clone();
        assert!(!media.offline);
        assert_eq!(media.info.video.as_ref().map(|v| (v.width, v.height)), Some((160, 90)));
        assert!(media.info.video.as_ref().is_some_and(|v| !v.codec.is_empty()), "the item must take the real file's properties");
        assert!(session.revision > revision, "the preview must refetch");
        assert_eq!(session.history.undo.len(), undo, "bringing a result online is not a user edit");
        // The pool serves the file, not the offline slate.
        let source = session.media.source_for(&session.project, item, &*session.services).expect("source");
        assert!(source.info().video.as_ref().is_some_and(|v| !v.codec.is_empty()));
        assert!(session.media.offline_status(item).is_none());

        // A repeated refresh of the same epoch is still stale and changes nothing.
        let revision = session.revision;
        let again = session.execute("plugin.craft.link.refresh", json!({"link_id": ONLINE, "epoch": 2})).expect("refresh");
        assert_eq!(again["stale"], true);
        assert_eq!(session.revision, revision);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn source_size(session: &mut Session, item: ItemId) -> (u32, u32) {
        let source = session.media.source_for(&session.project, item, &*session.services).expect("source");
        let video = source.info().video.clone().expect("video");
        (video.width, video.height)
    }

    /// EffectCraft writes the second result over the first, under the same path.
    #[test]
    fn a_second_back_plays_the_new_file_and_not_the_old_decoder() {
        const SECOND: &str = "c1627384-5f90-4aa1-b3b4-6f708192a3b4";
        let dir = std::env::temp_dir().join(format!("fc-link-second-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        let mut session = Session::default();
        crate::commands::install(&mut session);
        let item = linked_offline(&mut session, SECOND, &path);
        crate::fixtures::write_movie(&path, 160, 90, 24);
        session.execute("plugin.craft.link.refresh", json!({"link_id": SECOND, "epoch": 1})).expect("first refresh");
        assert_eq!(source_size(&mut session, item), (160, 90));

        crate::fixtures::write_movie(&path, 96, 54, 12);
        let undo = session.history.undo.len();
        let revision = session.revision;
        let reply = session.execute("plugin.craft.link.refresh", json!({"link_id": SECOND, "epoch": 2})).expect("second refresh");
        assert_eq!(reply["online"], true, "{reply}");
        assert_eq!(source_size(&mut session, item), (96, 54), "the pool still plays the file the second Back replaced");
        let media = session.project.item(item).unwrap().as_media().unwrap().clone();
        assert_eq!(media.info.video.as_ref().map(|v| (v.width, v.height)), Some((96, 54)), "the item keeps the old file's properties");
        assert!(session.revision > revision, "the preview must refetch");
        assert_eq!(session.history.undo.len(), undo, "reopening a result is not a user edit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same, when the epoch does not move (a refresh without a newer epoch) but the file did.
    #[test]
    fn a_changed_file_is_reopened_even_when_the_epoch_is_stale() {
        const SAME: &str = "d2738495-60a1-4bb2-84c5-708192a3b4c5";
        let dir = std::env::temp_dir().join(format!("fc-link-samepoch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        let mut session = Session::default();
        crate::commands::install(&mut session);
        let item = linked_offline(&mut session, SAME, &path);
        crate::fixtures::write_movie(&path, 160, 90, 24);
        session.execute("plugin.craft.link.refresh", json!({"link_id": SAME, "epoch": 1})).expect("first refresh");
        assert_eq!(source_size(&mut session, item), (160, 90));
        // The file is the same, so a stale refresh keeps the open source.
        let kept = session.media.cached(item).expect("cached");
        session.execute("plugin.craft.link.refresh", json!({"link_id": SAME, "epoch": 1})).expect("stale refresh");
        assert!(std::sync::Arc::ptr_eq(&kept, &session.media.cached(item).expect("cached")), "an unchanged file must not be reopened");
        crate::fixtures::write_movie(&path, 96, 54, 12);
        let again = session.execute("plugin.craft.link.refresh", json!({"link_id": SAME, "epoch": 1})).expect("stale refresh");
        assert_eq!(again["stale"], true);
        assert_eq!(source_size(&mut session, item), (96, 54));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// As EffectCraft's Back sends it: `link.focus` with no epoch and no `link.refresh`.
    #[test]
    fn a_second_focus_after_back_plays_the_new_file() {
        const BACK: &str = "f4950617-82c3-4dd4-a6e7-92a3b4c5d6e7";
        let dir = std::env::temp_dir().join(format!("fc-link-back-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        let mut session = crate::fixtures::video_session().0;
        crate::commands::install(&mut session);
        let item = linked_offline(&mut session, BACK, &path);
        session.execute("timeline.place", json!({"item": item.0, "frame": 0})).expect("a clip of the linked item");
        crate::fixtures::write_movie(&path, 160, 90, 24);
        let first = session.execute("plugin.craft.link.focus", json!({"link_id": BACK, "place": {"seconds": 0.5}})).expect("first back");
        assert_eq!(first["online"], true, "{first}");
        assert_eq!(source_size(&mut session, item), (160, 90));

        // Back again with nothing changed: the open source stays.
        let kept = session.media.cached(item).expect("cached");
        session.execute("plugin.craft.link.focus", json!({"link_id": BACK})).expect("same file");
        assert!(std::sync::Arc::ptr_eq(&kept, &session.media.cached(item).expect("cached")), "an unchanged result must not be reopened");

        // EffectCraft renders over the same path and sends only link.focus.
        crate::fixtures::write_movie(&path, 96, 54, 12);
        let revision = session.revision;
        let second = session.execute("plugin.craft.link.focus", json!({"link_id": BACK, "place": {"seconds": 0.5}})).expect("second back");
        assert_eq!(second["online"], true, "{second}");
        assert_eq!(source_size(&mut session, item), (96, 54), "the pool still plays the file the second Back replaced");
        assert!(session.revision > revision, "the preview must refetch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_result_that_appears_without_a_refresh_comes_online() {
        let dir = std::env::temp_dir().join(format!("fc-link-appear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        let mut session = Session::default();
        let item = linked_offline(&mut session, APPEAR, &path);
        let revision = session.revision;
        assert_eq!(heal_offline_links(&mut session), 0, "no file, nothing to heal");
        assert_eq!(session.revision, revision, "a missing file must not bump the view");
        crate::fixtures::write_movie(&path, 160, 90, 24);
        assert_eq!(heal_offline_links(&mut session), 1);
        assert_eq!(status_of(&mut session, item), "online");
        assert_eq!(heal_offline_links(&mut session), 0, "an online item is left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_result_that_cannot_be_opened_stays_offline_and_says_why() {
        let dir = std::env::temp_dir().join(format!("fc-link-junk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        std::fs::write(&path, b"not a movie").unwrap();
        let mut session = Session::default();
        crate::commands::install(&mut session);
        let item = linked_offline(&mut session, JUNK, &path);
        let reply = session.execute("plugin.craft.link.refresh", json!({"link_id": JUNK, "epoch": 1})).expect("refresh");
        assert_eq!(status_of(&mut session, item), "offline");
        assert_eq!(reply["online"], false, "{reply}");
        assert!(reply["online_error"].as_str().is_some_and(|text| !text.is_empty()), "{reply}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_poller_looks_once_a_second_and_follows_a_bad_file_to_a_good_one() {
        const POLLED: &str = "af405162-3d7e-4f80-b192-4d5e6f708192";
        let dir = std::env::temp_dir().join(format!("fc-link-poll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        std::fs::write(&path, b"half written").unwrap();
        let mut session = Session::default();
        let item = linked_offline(&mut session, POLLED, &path);
        let mut poller = ResultPoller::default();
        let start = Instant::now();
        assert!(poller.poll(&mut session, start), "a bad file leaves the item offline, so keep polling");
        // The file is finished half a second later. The poller has looked a moment ago.
        crate::fixtures::write_movie(&path, 160, 90, 24);
        assert!(poller.poll(&mut session, start + Duration::from_millis(500)), "inside the interval the poller does not look");
        assert_eq!(status_of(&mut session, item), "offline");
        assert!(!poller.poll(&mut session, start + RESULT_POLL), "the finished file comes online, nothing is left to wait for");
        assert_eq!(status_of(&mut session, item), "online");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_record_does_not_hide_a_good_one() {
        let mut session = Session::default();
        let bad = offline(&mut session, "bad");
        let good = offline(&mut session, "good");
        session.project_mut().item_mut(bad).unwrap().metadata.insert(LINK_KEY.to_string(), "{not json".to_string());
        session.project_mut().item_mut(good).unwrap().metadata.insert(LINK_KEY.to_string(), serde_json::to_string(&record(GOOD)).unwrap());
        let (found, got) = find_link(&session.project, GOOD).unwrap().expect("the good link");
        assert_eq!((found, got.link_id.as_str()), (good, GOOD));
    }

    #[test]
    fn a_missing_link_next_to_a_damaged_record_reports_the_damage() {
        let mut session = Session::default();
        let bad = offline(&mut session, "bad");
        session.project_mut().item_mut(bad).unwrap().metadata.insert(LINK_KEY.to_string(), "{not json".to_string());
        let err = find_link(&session.project, GOOD).expect_err("the damage must surface");
        assert!(err.contains("damaged"), "{err}");
        // No damage and no match is a plain miss.
        let clean = Session::default();
        assert_eq!(find_link(&clean.project, GOOD).unwrap(), None);
    }

    #[test]
    fn record_with_a_rule_broken_is_invalid_not_silently_accepted() {
        let mut session = Session::default();
        let item = offline(&mut session, "x");
        let mut broken = record(GOOD);
        broken.target_app = "photocraft".to_string();
        session.project_mut().item_mut(item).unwrap().metadata.insert(LINK_KEY.to_string(), serde_json::to_string(&broken).unwrap());
        let err = record_of(&session.project, item).expect_err("invalid record");
        assert!(err.contains("invalid"), "{err}");
    }
}

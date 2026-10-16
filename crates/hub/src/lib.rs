//! # filmcraft-hub — FilmCraft's CraftHub client path
//!
//! The [`HubAdapter`] that maps FilmCraft's [`Project`](filmcraft_project::Project) onto
//! `craft-collab`'s generalized object/property model (the CraftHub specification §3.6 phase B,
//! §18.4 row 8). FilmCraft is a timeline app: what collaborates is the *timeline* — tracks,
//! clips, transitions — beside the bin tree that holds the media.
//!
//! ## The honest mapping (studied from the real model)
//!
//! FilmCraft's document is a bin tree plus a flat item map: a `Project` owns bins, items (media,
//! sequences, subclips, adjustment layers, graphics), and each sequence item owns video, audio,
//! submix and caption tracks holding clips, transitions and captions. All six id types draw from
//! **one monotonic counter**, so a single 64-bit id names any of them — they map onto
//! `NodeId` directly, space 0, exactly like a legacy file's ids (spec §5.1 P2). The root bin
//! (`BinId(0)`) *is* the file root: its children hang under the generic root and its name syncs
//! as the document property `rootName`.
//!
//! * **bins and items** are objects with the bin tree as their parent axis;
//! * **tracks** are objects whose parent is their sequence, in four lists (video, submix, audio,
//!   caption) — the sibling order is the concatenation of the four, and each list keeps its own
//!   stack; the track kind rides the node kind (`videoTrack`/`audioTrack`/`submixTrack`/
//!   `captionTrack`);
//! * **clips, transitions and captions** are objects whose parent is their track.
//!
//! ### Clip order is time, not paint order
//!
//! A track's items are sorted by `start` (the document's own invariant, `Track::sort`), so the
//! sibling keys are subordinate to it: the adapter assigns keys by current order (the first-sync
//! rule), the LIS discipline keeps them under edits, and `apply` **re-sorts every touched track
//! by start at the end of the batch** — the timeline's order is derived from synced `start`
//! values, deterministic on every replica, which is why the schema's `recompute` is empty and
//! the sort happens inline.
//!
//! ### The timeline's own sync decisions (§18.4 row 8)
//!
//! * **Region locks** — the `TimelineRange` shape, documented and validated in [`schema`]: locks
//!   sync as the `locks` document property, and a clip edit inside a foreign lock is refused.
//! * **Presence carries the playhead** — in sequence ticks; the fps resolution is document data
//!   (the sequence's `settings` property). See [`presence`].
//! * **Heavy media is deferred** — media files never entered the document (they are paths); the
//!   one heavy embedded payload, the LUT library, syncs as content-addressed
//!   [`AssetRef`](craft_collab::value::AssetRef) entries, never inside batches. See [`schema`].
//!
//! What deliberately does **not** live here: networking, sessions, the UI. The session wiring (a
//! `NetLink` speaking protocol v3, installed by the engine behind the app's `crafthub` feature
//! flag, LAN mode untouched) lands as the follow-up once this contract is reviewed; nothing here
//! changes any user-visible behavior.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod commands;
pub mod deeplink;
pub mod presence;
pub mod props;
pub mod schema;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use craft_collab::adapter::{ApplyReport, DocumentView, HubAdapter, UiState, ViewNode};
use craft_collab::change::Change;
use craft_collab::frac;
use craft_collab::ids::NodeId as HubId;
use craft_collab::value::Value;
use filmcraft_project::{Bin, BinEntry, ItemId, ItemKind, Project, TrackId};

/// The root every bin entry hangs under (the root bin is the file root). Space 0, counter 0 —
/// never a real id: FilmCraft's ids start at 1 (`next_id` starts at 1).
pub const ROOT: HubId = HubId(0);

/// The object kinds: the real kind discriminants of the model.
pub const KINDS: &[&str] = &[
    "bin",
    "media",
    "sequence",
    "subclip",
    "adjustment",
    "graphic",
    "videoTrack",
    "audioTrack",
    "submixTrack",
    "captionTrack",
    "clip",
    "transition",
    "caption",
];

/// The kinds that accept children.
pub const CONTAINERS: &[&str] = &["bin", "sequence", "videoTrack", "audioTrack", "submixTrack", "captionTrack"];

/// The kinds that attach to the file root (the root bin's own children).
pub const ROOT_CHILD_KINDS: &[&str] = &["bin", "media", "sequence", "subclip", "adjustment", "graphic"];

/// Which children a container accepts (the schema's table, mirrored for `apply`'s routing).
#[must_use]
pub fn container_accepts(parent: &str, child: &str) -> bool {
    match parent {
        "bin" => ROOT_CHILD_KINDS.contains(&child),
        "sequence" => matches!(child, "videoTrack" | "audioTrack" | "submixTrack" | "captionTrack"),
        "videoTrack" | "audioTrack" | "submixTrack" => matches!(child, "clip" | "transition"),
        "captionTrack" => child == "caption",
        _ => false,
    }
}

/// Which of a sequence's three ordinary track lists a track id lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackList {
    /// `sequence.video_tracks`.
    Video,
    /// `sequence.audio_tracks`.
    Audio,
    /// `sequence.submix_tracks`.
    Submix,
}

/// The adapter: document binding, sibling keys, the region-lock set and the fetched asset texts
/// in one value.
///
/// The sibling position keys are adapter state (spec §2.3: "existing documents take keys by their
/// current order at first sync"), exactly as the pilot kept them beside the document. The
/// [`schema::TimelineLock`] set is the §6.7 document property `locks` (the session acquires and
/// releases; the adapter stores what arrived), and the asset texts are what
/// [`FilmcraftHub::offer_asset`] has fetched so far — a `LutText` entry whose bytes have not
/// arrived is skipped, not corrupted, and replays after the asset lands.
pub struct FilmcraftHub {
    schema: schema::FilmSchema,
    positions: Mutex<BTreeMap<u64, String>>,
    locks: Mutex<Vec<schema::TimelineLock>>,
    assets: Mutex<BTreeMap<String, Arc<str>>>,
}

impl FilmcraftHub {
    /// A fresh adapter with the default limits and empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: schema::FilmSchema::default(),
            positions: Mutex::new(BTreeMap::new()),
            locks: Mutex::new(Vec::new()),
            assets: Mutex::new(BTreeMap::new()),
        }
    }

    /// Hand the adapter the bytes of an asset the channel delivered (spec §3.5): the session
    /// calls this when the deferred LUT text arrives, and the skipped `LutText` puts replay.
    pub fn offer_asset(&self, sha256: &str, text: &str) {
        self.assets.lock().unwrap_or_else(|e| e.into_inner()).insert(sha256.to_lowercase(), Arc::from(text));
    }

    /// The sibling key of `id`, if one has been assigned.
    #[must_use]
    pub fn position_key(&self, id: u64) -> Option<String> {
        self.positions.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }

    /// The region-lock set as it last synced (the `locks` document property's value).
    #[must_use]
    pub fn region_locks(&self) -> Vec<schema::TimelineLock> {
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The generic id of a document id (same 64-bit packing; ids are unique across all six id
    /// types because they share one counter).
    #[must_use]
    pub fn hub_id(id: u64) -> HubId {
        HubId(id)
    }

    /// The document id of a generic id.
    #[must_use]
    pub fn doc_id(id: HubId) -> u64 {
        id.0
    }

    // ---------------------------------------------------------------------------------------------
    // Where things live (the model is a flat map plus trees; these find things by id)
    // ---------------------------------------------------------------------------------------------

    /// Find a bin anywhere in the tree.
    fn find_bin(doc: &Project, id: u64) -> Option<&Bin> {
        fn walk(bin: &Bin, id: u64) -> Option<&Bin> {
            if bin.id.0 == id {
                return Some(bin);
            }
            bin.children.iter().find_map(|c| if let BinEntry::Bin(b) = c { walk(b, id) } else { None })
        }
        walk(&doc.root, id)
    }

    /// A mutable bin by id.
    fn find_bin_mut(doc: &mut Project, id: u64) -> Option<&mut Bin> {
        doc.root.find_bin_mut(filmcraft_project::BinId(id))
    }

    /// The bin that contains the entry named `id` (`None` = the root bin → tree parent ROOT).
    fn bin_parent_of(doc: &Project, id: u64) -> Option<u64> {
        fn walk(bin: &Bin, id: u64) -> Option<u64> {
            for c in &bin.children {
                match c {
                    BinEntry::Item(i) if i.0 == id => return Some(bin.id.0),
                    BinEntry::Bin(b) if b.id.0 == id => return Some(bin.id.0),
                    BinEntry::Bin(b) => {
                        if let Some(p) = walk(b, id) {
                            return Some(p);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&doc.root, id)
    }

    /// Which sequence and list a track id lives in.
    fn find_track_list(doc: &Project, id: u64) -> Option<(ItemId, TrackList)> {
        for item in doc.items.values() {
            if let ItemKind::Sequence(seq) = &item.kind {
                if seq.video_tracks.iter().any(|t| t.id.0 == id) {
                    return Some((item.id, TrackList::Video));
                }
                if seq.audio_tracks.iter().any(|t| t.id.0 == id) {
                    return Some((item.id, TrackList::Audio));
                }
                if seq.submix_tracks.iter().any(|t| t.id.0 == id) {
                    return Some((item.id, TrackList::Submix));
                }
            }
        }
        None
    }

    /// The sequence that owns a caption track id.
    fn find_caption_track(doc: &Project, id: u64) -> Option<ItemId> {
        doc.items.values().find(|i| i.as_sequence().is_some_and(|s| s.caption_tracks.iter().any(|t| t.id.0 == id))).map(|i| i.id)
    }

    /// The sequence and track that own a clip id.
    fn find_clip_home(doc: &Project, id: u64) -> Option<(ItemId, TrackId)> {
        for item in doc.items.values() {
            if let ItemKind::Sequence(seq) = &item.kind {
                for t in seq.all_tracks().chain(&seq.submix_tracks) {
                    if t.item(filmcraft_project::ClipId(id)).is_some() {
                        return Some((item.id, t.id));
                    }
                }
            }
        }
        None
    }

    /// The sequence and track that own a transition id.
    fn find_transition_home(doc: &Project, id: u64) -> Option<(ItemId, TrackId)> {
        for item in doc.items.values() {
            if let ItemKind::Sequence(seq) = &item.kind {
                for t in seq.all_tracks().chain(&seq.submix_tracks) {
                    if t.transitions.iter().any(|tr| tr.id.0 == id) {
                        return Some((item.id, t.id));
                    }
                }
            }
        }
        None
    }

    /// The sequence and caption track that own a caption id.
    fn find_caption_home(doc: &Project, id: u64) -> Option<(ItemId, TrackId)> {
        for item in doc.items.values() {
            if let ItemKind::Sequence(seq) = &item.kind {
                for t in &seq.caption_tracks {
                    if t.captions.iter().any(|c| c.id.0 == id) {
                        return Some((item.id, t.id));
                    }
                }
            }
        }
        None
    }

    /// Find an ordinary track by id.
    fn find_track(doc: &Project, id: u64) -> Option<&filmcraft_project::Track> {
        for item in doc.items.values() {
            if let ItemKind::Sequence(seq) = &item.kind
                && let Some(t) = seq.all_tracks().chain(&seq.submix_tracks).find(|t| t.id.0 == id)
            {
                return Some(t);
            }
        }
        None
    }

    /// Find a caption track by id.
    fn find_caption_track_ref(doc: &Project, id: u64) -> Option<&filmcraft_project::CaptionTrack> {
        doc.items.values().filter_map(|i| i.as_sequence()).flat_map(|s| s.caption_tracks.iter()).find(|t| t.id.0 == id)
    }

    /// Find a clip by id.
    fn find_clip(doc: &Project, id: u64) -> Option<&filmcraft_project::TrackItem> {
        Self::find_clip_home(doc, id)
            .and_then(|(item, track)| doc.sequence(item).and_then(|s| s.track(track)).and_then(|t| t.item(filmcraft_project::ClipId(id))))
    }

    /// Find a transition by id.
    fn find_transition(doc: &Project, id: u64) -> Option<&filmcraft_project::Transition> {
        Self::find_transition_home(doc, id)
            .and_then(|(item, track)| doc.sequence(item).and_then(|s| s.track(track)).and_then(|t| t.transitions.iter().find(|tr| tr.id.0 == id)))
    }

    /// Find a caption by id.
    fn find_caption(doc: &Project, id: u64) -> Option<&filmcraft_project::Caption> {
        Self::find_caption_home(doc, id)
            .and_then(|(item, track)| doc.sequence(item).and_then(|s| s.caption_track(track)).and_then(|t| t.caption(filmcraft_project::ClipId(id))))
    }

    /// The node kind of an id, if the id names a synced object.
    fn kind_of(doc: &Project, id: u64) -> Option<&'static str> {
        if let Some(item) = doc.items.get(&ItemId(id)) {
            return Some(match &item.kind {
                ItemKind::Media(_) => "media",
                ItemKind::Sequence(_) => "sequence",
                ItemKind::Subclip { .. } => "subclip",
                ItemKind::AdjustmentLayer { .. } => "adjustment",
                ItemKind::Graphic { .. } => "graphic",
            });
        }
        if Self::find_bin(doc, id).is_some() {
            return Some("bin");
        }
        if let Some((_, list)) = Self::find_track_list(doc, id) {
            return Some(match list {
                TrackList::Video => "videoTrack",
                TrackList::Audio => "audioTrack",
                TrackList::Submix => "submixTrack",
            });
        }
        if Self::find_caption_track(doc, id).is_some() {
            return Some("captionTrack");
        }
        if Self::find_clip_home(doc, id).is_some() {
            return Some("clip");
        }
        if Self::find_transition_home(doc, id).is_some() {
            return Some("transition");
        }
        if Self::find_caption_home(doc, id).is_some() {
            return Some("caption");
        }
        None
    }

    /// The properties of one node, by its kind's table (the view's payload).
    fn node_props(doc: &Project, id: u64) -> Option<Vec<(Arc<str>, Value)>> {
        let mut out: Vec<(Arc<str>, Value)> = Vec::new();
        if let Some(item) = doc.items.get(&ItemId(id)) {
            for p in props::item_props(&item.kind) {
                if let Some(v) = props::any_item_value(item, p) {
                    out.push((Arc::from(p), v));
                }
            }
            return Some(out);
        }
        if let Some(bin) = Self::find_bin(doc, id) {
            for p in props::BIN_PROPS {
                if let Some(v) = props::bin_value(bin, p) {
                    out.push((Arc::from(*p), v));
                }
            }
            return Some(out);
        }
        if let Some(track) = Self::find_track(doc, id) {
            for p in props::TRACK_PROPS {
                if let Some(v) = props::track_value(track, p) {
                    out.push((Arc::from(*p), v));
                }
            }
            return Some(out);
        }
        if let Some(ct) = Self::find_caption_track_ref(doc, id) {
            for p in props::CAPTION_TRACK_PROPS {
                if let Some(v) = props::caption_track_value(ct, p) {
                    out.push((Arc::from(*p), v));
                }
            }
            return Some(out);
        }
        if let Some(clip) = Self::find_clip(doc, id) {
            for p in props::CLIP_PROPS {
                if let Some(v) = props::clip_value(clip, p) {
                    out.push((Arc::from(*p), v));
                }
            }
            return Some(out);
        }
        if let Some(tr) = Self::find_transition(doc, id) {
            for p in props::TRANSITION_PROPS {
                if let Some(v) = props::transition_value(tr, p) {
                    out.push((Arc::from(*p), v));
                }
            }
            return Some(out);
        }
        if let Some(cap) = Self::find_caption(doc, id) {
            for p in props::CAPTION_PROPS {
                if let Some(v) = props::caption_value(cap, p) {
                    out.push((Arc::from(*p), v));
                }
            }
            return Some(out);
        }
        None
    }

    // ---------------------------------------------------------------------------------------------
    // Children, keys, order
    // ---------------------------------------------------------------------------------------------

    /// The children of `parent` in canonical order: the root bin's entries under ROOT; a bin's
    /// entries; a sequence's four track lists concatenated (video, submix, audio, caption); a
    /// track's clips then transitions; a caption track's captions.
    fn children_of(doc: &Project, parent: HubId) -> Vec<u64> {
        if parent == ROOT {
            return doc.root.children.iter().map(entry_id).collect();
        }
        let id = parent.0;
        if let Some(bin) = Self::find_bin(doc, id) {
            return bin.children.iter().map(entry_id).collect();
        }
        if let Some(item) = doc.items.get(&ItemId(id))
            && let ItemKind::Sequence(seq) = &item.kind
        {
            return seq
                .video_tracks
                .iter()
                .chain(&seq.submix_tracks)
                .chain(&seq.audio_tracks)
                .map(|t| t.id.0)
                .chain(seq.caption_tracks.iter().map(|t| t.id.0))
                .collect();
        }
        if let Some(track) = Self::find_track(doc, id) {
            return track.items.iter().map(|i| i.id.0).chain(track.transitions.iter().map(|t| t.id.0)).collect();
        }
        if let Some(ct) = Self::find_caption_track_ref(doc, id) {
            return ct.captions.iter().map(|c| c.id.0).collect();
        }
        Vec::new()
    }

    /// The tree parent of an id (its bin, its sequence, its track; ROOT for the root bin's own
    /// children).
    fn parent_of(doc: &Project, id: u64) -> HubId {
        if let Some(bin) = Self::bin_parent_of(doc, id) {
            return Self::hub_id(bin);
        }
        if doc.items.contains_key(&ItemId(id)) {
            return ROOT; // an item no bin lists rides the root (the app keeps them listed)
        }
        if let Some((item, _)) = Self::find_track_list(doc, id) {
            return Self::hub_id(item.0);
        }
        if let Some(item) = Self::find_caption_track(doc, id) {
            return Self::hub_id(item.0);
        }
        if let Some((_, track)) = Self::find_clip_home(doc, id) {
            return Self::hub_id(track.0);
        }
        if let Some((_, track)) = Self::find_transition_home(doc, id) {
            return Self::hub_id(track.0);
        }
        if let Some((_, track)) = Self::find_caption_home(doc, id) {
            return Self::hub_id(track.0);
        }
        ROOT
    }

    /// Assign sibling keys to every node that has none, by current order (the first-sync rule).
    fn first_sync_keys(&self, doc: &Project) {
        let mut positions = self.positions.lock().unwrap_or_else(|e| e.into_inner());
        let mut parents: Vec<HubId> = vec![ROOT];
        parents.extend(Self::all_nodes(doc).into_iter().map(Self::hub_id));
        for parent in parents {
            let missing: Vec<u64> = Self::children_of(doc, parent).into_iter().filter(|c| !positions.contains_key(c)).collect();
            if !missing.is_empty()
                && let Ok(keys) = frac::n_between(None, None, missing.len())
            {
                for (id, key) in missing.into_iter().zip(keys) {
                    positions.insert(id, key);
                }
            }
        }
    }

    /// Every synced node id in canonical tree order (parents first): the root bin's entries
    /// depth-first, then each sequence's tracks, clips, transitions and captions.
    fn all_nodes(doc: &Project) -> Vec<u64> {
        let mut out: Vec<u64> = Vec::new();
        fn walk_bin(bin: &Bin, out: &mut Vec<u64>) {
            for c in &bin.children {
                match c {
                    BinEntry::Item(i) => out.push(i.0),
                    BinEntry::Bin(b) => {
                        out.push(b.id.0);
                        walk_bin(b, out);
                    }
                }
            }
        }
        walk_bin(&doc.root, &mut out);
        // Items first (their bins reference them), then each sequence's subtree.
        let items: Vec<ItemId> = out.iter().copied().map(ItemId).collect();
        for item in items {
            let Some(seq) = doc.sequence(item) else { continue };
            for t in seq.video_tracks.iter().chain(&seq.submix_tracks).chain(&seq.audio_tracks) {
                out.push(t.id.0);
                out.extend(t.items.iter().map(|i| i.id.0));
                out.extend(t.transitions.iter().map(|tr| tr.id.0));
            }
            for t in &seq.caption_tracks {
                out.push(t.id.0);
                out.extend(t.captions.iter().map(|c| c.id.0));
            }
        }
        out
    }

    /// Sort one parent's children by `(key, id)` after a key change, in the document itself.
    /// Tracks sort each of their lists (clips, transitions) and sequences each of theirs; the
    /// start-sort that the timeline's own invariant demands runs after the whole batch (see
    /// [`FilmcraftHub::cleanup_sequences`]).
    fn reorder_children(doc: &mut Project, parent: HubId, keys: &BTreeMap<u64, String>) {
        let rank = |id: &u64| (keys.get(id).cloned().unwrap_or_default(), *id);
        if parent == ROOT {
            doc.root.children.sort_by_key(|c| rank(&entry_id(c)));
            return;
        }
        let id = parent.0;
        if Self::find_bin(doc, id).is_some() {
            let mut children = Self::find_bin(doc, id).map_or_else(Vec::new, |b| b.children.clone());
            children.sort_by_key(|c| rank(&entry_id(c)));
            if let Some(b) = Self::find_bin_mut(doc, id) {
                b.children = children;
            }
            return;
        }
        if doc.items.get(&ItemId(id)).is_some_and(|i| matches!(i.kind, ItemKind::Sequence(_))) {
            let seq = doc.items.get(&ItemId(id)).and_then(|i| i.as_sequence());
            let mut video = seq.map_or_else(Vec::new, |s| s.video_tracks.clone());
            let mut submix = seq.map_or_else(Vec::new, |s| s.submix_tracks.clone());
            let mut audio = seq.map_or_else(Vec::new, |s| s.audio_tracks.clone());
            let mut captions = seq.map_or_else(Vec::new, |s| s.caption_tracks.clone());
            for list in [&mut video, &mut submix, &mut audio] {
                list.sort_by_key(|t| rank(&t.id.0));
            }
            captions.sort_by_key(|t| rank(&t.id.0));
            if let Some(seq) = doc.sequence_mut(ItemId(id)) {
                seq.video_tracks = video;
                seq.submix_tracks = submix;
                seq.audio_tracks = audio;
                seq.caption_tracks = captions;
            }
            return;
        }
        if let Some((item, list)) = Self::find_track_list(doc, id) {
            let (mut items, mut transitions) = Self::find_track(doc, id).map_or_else(|| (Vec::new(), Vec::new()), |t| (t.items.clone(), t.transitions.clone()));
            items.sort_by_key(|i| rank(&i.id.0));
            transitions.sort_by_key(|t| rank(&t.id.0));
            if let Some(seq) = doc.sequence_mut(item) {
                let list = match list {
                    TrackList::Video => &mut seq.video_tracks,
                    TrackList::Audio => &mut seq.audio_tracks,
                    TrackList::Submix => &mut seq.submix_tracks,
                };
                if let Some(t) = list.iter_mut().find(|t| t.id.0 == id) {
                    t.items = items;
                    t.transitions = transitions;
                }
            }
            return;
        }
        if let Some(item) = Self::find_caption_track(doc, id) {
            let mut captions = Self::find_caption_track_ref(doc, id).map_or_else(Vec::new, |t| t.captions.clone());
            captions.sort_by_key(|c| rank(&c.id.0));
            if let Some(seq) = doc.sequence_mut(item)
                && let Some(t) = seq.caption_track_mut(TrackId(id))
            {
                t.captions = captions;
            }
        }
    }

    /// Record `key` for `id`.
    fn set_key(&self, id: u64, key: String) {
        self.positions.lock().unwrap_or_else(|e| e.into_inner()).insert(id, key);
    }

    /// Drop the keys of a subtree (before the document forgets it).
    fn drop_keys(&self, doc: &Project, id: u64) {
        let mut doomed = vec![id];
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            for c in Self::children_of(doc, Self::hub_id(cur)) {
                if !doomed.contains(&c) {
                    doomed.push(c);
                    stack.push(c);
                }
            }
        }
        let mut positions = self.positions.lock().unwrap_or_else(|e| e.into_inner());
        for d in doomed {
            positions.remove(&d);
        }
    }

    /// Diff one parent's child order between two documents, assigning fresh keys to the moved
    /// and inserted children while the longest rising run of old keys keeps its keys (the
    /// pilot's LIS discipline: only the children that must move get new keys, so an insertion in
    /// the middle costs one key, not a renumbering). Emits `Set(position)` and `Set(parent)`
    /// changes into `out`.
    fn diff_order(&self, before: &Project, after: &Project, parent: HubId, out: &mut Vec<Change>) {
        let old = Self::children_of(before, parent);
        let new = Self::children_of(after, parent);
        if old == new {
            return;
        }
        let mut positions = self.positions.lock().unwrap_or_else(|e| e.into_inner());
        // First-sync rule: children of the before-order without keys take them by order.
        let unkeyed: Vec<u64> = old.iter().copied().filter(|c| !positions.contains_key(c)).collect();
        if !unkeyed.is_empty()
            && let Ok(fresh) = frac::n_between(None, None, unkeyed.len())
        {
            for (id, key) in unkeyed.into_iter().zip(fresh) {
                positions.insert(id, key);
            }
        }
        // The longest strictly-rising run of keys over the new order, counting only children
        // that already have keys AND already belonged to this parent (a child new to this
        // parent — created or moved in — always takes a fresh key).
        let has_key = |c: &u64| positions.contains_key(c);
        let old_member = |c: &u64| old.contains(c);
        let n = new.len();
        let mut len = vec![0usize; n];
        let mut prev = vec![usize::MAX; n];
        let mut best_end = None;
        let mut best_len = 0usize;
        for i in 0..n {
            if has_key(&new[i]) && old_member(&new[i]) {
                len[i] = 1;
                for j in 0..i {
                    if has_key(&new[j])
                        && old_member(&new[j])
                        && positions.get(&new[j]).is_some_and(|a| positions.get(&new[i]).is_some_and(|b| a < b))
                        && len[j] + 1 > len[i]
                    {
                        len[i] = len[j] + 1;
                        prev[i] = j;
                    }
                }
                if len[i] > best_len {
                    best_len = len[i];
                    best_end = Some(i);
                }
            }
        }
        let mut kept = vec![false; n];
        let mut walk = best_end;
        while let Some(i) = walk {
            kept[i] = true;
            walk = if prev[i] == usize::MAX { None } else { Some(prev[i]) };
        }
        // Walk the new order; assign fresh keys to runs of not-kept children between the kept
        // neighbours (the key of the kept child before the run and the one after it).
        fn flush(pending: &mut Vec<u64>, prev_key: Option<&str>, next_key: Option<&str>, positions: &mut BTreeMap<u64, String>, out: &mut Vec<Change>) {
            if pending.is_empty() {
                return;
            }
            let count = pending.len();
            if let Ok(fresh) = frac::n_between(prev_key, next_key, count) {
                for (id, key) in pending.drain(..).zip(fresh) {
                    positions.insert(id, key.clone());
                    out.push(Change::Set { id: HubId(id), prop: Arc::from("position"), value: Value::Json(serde_json::Value::String(key)) });
                }
            }
        }
        let mut last_kept_key: Option<String> = None;
        let mut pending: Vec<u64> = Vec::new();
        for (i, c) in new.iter().enumerate() {
            if kept[i] {
                let next_key = positions.get(c).cloned();
                flush(&mut pending, last_kept_key.as_deref(), next_key.as_deref(), &mut positions, out);
                last_kept_key = positions.get(c).cloned();
            } else {
                pending.push(*c);
            }
        }
        flush(&mut pending, last_kept_key.as_deref(), None, &mut positions, out);
        // Children that changed parent get the parent write after their key.
        for c in &new {
            if Self::parent_of(before, *c) != Self::parent_of(after, *c) {
                let p = Self::parent_of(after, *c);
                out.push(Change::Set { id: Self::hub_id(*c), prop: Arc::from("parent"), value: Value::Json(serde_json::Value::Number(p.0.into())) });
            }
        }
    }

    /// Sort every touched sequence's tracks by start and drop transitions whose clips went away
    /// (the document's own invariants, restored after a batch — see the crate docs).
    fn cleanup_sequences(doc: &mut Project, touched: &[ItemId]) {
        for item in touched {
            let Some(seq) = doc.sequence_mut(*item) else { continue };
            for t in seq.all_tracks_mut() {
                t.sort();
            }
            for t in &mut seq.submix_tracks {
                t.sort();
            }
            for t in &mut seq.caption_tracks {
                t.sort();
            }
            for t in seq.all_tracks_mut() {
                let live: Vec<filmcraft_project::ClipId> = t.items.iter().map(|i| i.id).collect();
                t.transitions.retain(|tr| tr.from.is_none_or(|f| live.contains(&f)) && tr.to.is_none_or(|f| live.contains(&f)));
            }
        }
    }
}

/// The u64 id of a bin entry (item or bin).
fn entry_id(entry: &BinEntry) -> u64 {
    match entry {
        BinEntry::Item(i) => i.0,
        BinEntry::Bin(b) => b.id.0,
    }
}

impl Default for FilmcraftHub {
    fn default() -> Self {
        Self::new()
    }
}

impl HubAdapter for FilmcraftHub {
    type Doc = Project;

    fn view(&self, doc: &Project) -> DocumentView {
        self.first_sync_keys(doc);
        let positions = self.positions.lock().unwrap_or_else(|e| e.into_inner());
        let key = |id: &u64| positions.get(id).cloned().unwrap_or_default();
        let mut nodes = Vec::new();
        // Parents-first over the canonical order.
        for id in Self::all_nodes(doc) {
            let Some(kind) = Self::kind_of(doc, id) else { continue };
            let props = Self::node_props(doc, id).unwrap_or_default();
            nodes.push(ViewNode { id: Self::hub_id(id), kind: Arc::from(kind), parent: Self::parent_of(doc, id), position: key(&id), props });
        }
        let entries = schema::entry_values(doc);
        let locks = schema::locks_value(&self.locks.lock().unwrap_or_else(|e| e.into_inner()));
        let doc_props = props::DOC_PROPS.iter().filter_map(|p| props::doc_prop_value(doc, p, &locks).map(|v| ((*p).to_owned(), v))).collect();
        DocumentView { nodes, entries, doc: doc_props }
    }

    fn apply(&self, doc: &mut Project, changes: &[Change]) -> ApplyReport {
        let mut report = ApplyReport::default();
        let mut touched: Vec<ItemId> = Vec::new();
        let note_touched = |touched: &mut Vec<ItemId>, doc: &Project, id: u64| {
            let homes = [
                Self::find_track_list(doc, id).map(|(i, _)| i),
                Self::find_caption_track(doc, id),
                Self::find_clip_home(doc, id).map(|(i, _)| i),
                Self::find_transition_home(doc, id).map(|(i, _)| i),
                Self::find_caption_home(doc, id).map(|(i, _)| i),
            ];
            for home in homes.into_iter().flatten() {
                if !touched.contains(&home) {
                    touched.push(home);
                }
            }
        };
        for change in changes {
            match change {
                Change::Create { id, kind, parent, position, props } => {
                    let doc_id = Self::doc_id(*id);
                    match self.create(doc, doc_id, kind, *parent, position, props) {
                        Ok(()) => {
                            report.applied += 1;
                            note_touched(&mut touched, doc, doc_id);
                        }
                        Err(_) => report.skipped += 1,
                    }
                }
                Change::Set { id, prop, value } => {
                    let doc_id = Self::doc_id(*id);
                    if prop.as_ref() == "position" {
                        if let Value::Json(serde_json::Value::String(key)) = value {
                            if Self::kind_of(doc, doc_id).is_none() {
                                // A position write on an object that isn't there reorders nothing
                                // (a skip, never an accidental sort of the root by empty keys).
                                report.skipped += 1;
                                continue;
                            }
                            let parent = Self::parent_of(doc, doc_id);
                            self.set_key(doc_id, key.clone());
                            let keys = self.positions.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            Self::reorder_children(doc, parent, &keys);
                            report.applied += 1;
                            note_touched(&mut touched, doc, doc_id);
                        } else {
                            report.skipped += 1;
                        }
                    } else if prop.as_ref() == "parent" {
                        match value.to_json().as_u64().map(HubId) {
                            Some(p) => match Self::move_node(doc, doc_id, p) {
                                Ok(()) => {
                                    // The moved node keeps the key its position write gave it
                                    // (order in the batch: positions before parents); the new
                                    // parent's list takes it in key order, exactly like a
                                    // Create — a move to the end would leave the receiver's list
                                    // order drifting from the sender's.
                                    let keys = self.positions.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    Self::reorder_children(doc, p, &keys);
                                    report.applied += 1;
                                    note_touched(&mut touched, doc, doc_id);
                                }
                                Err(()) => report.skipped += 1,
                            },
                            _ => report.skipped += 1,
                        }
                    } else if Self::set_node_prop(doc, doc_id, prop, value) {
                        report.applied += 1;
                        note_touched(&mut touched, doc, doc_id);
                    } else {
                        report.skipped += 1;
                    }
                }
                Change::Delete { id } => {
                    let doc_id = Self::doc_id(*id);
                    if Self::kind_of(doc, doc_id).is_some() {
                        note_touched(&mut touched, doc, doc_id);
                        self.drop_keys(doc, doc_id);
                        if Self::delete_node(doc, doc_id) {
                            report.applied += 1;
                        } else {
                            report.skipped += 1;
                        }
                    }
                    // Deleting a deleted node does nothing (v2 tolerance, kept).
                }
                Change::SetDoc { prop, value } => {
                    if prop.as_ref() == "locks" {
                        match schema::parse_locks(value) {
                            Ok(locks) => {
                                *self.locks.lock().unwrap_or_else(|e| e.into_inner()) = locks;
                                report.applied += 1;
                            }
                            Err(_) => report.skipped += 1,
                        }
                        continue;
                    }
                    let routed = props::set_doc_prop(doc, prop, value).or_else(|| schema::set_doc_order(doc, prop, value));
                    match routed {
                        Some(Ok(())) => report.applied += 1,
                        Some(Err(_)) | None => report.skipped += 1,
                    }
                }
                Change::Put { entry, value } => {
                    let assets = self.assets.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    match schema::put_entry(doc, entry, value, &assets) {
                        Ok(()) => report.applied += 1,
                        Err(_) => report.skipped += 1,
                    }
                }
                Change::Remove { entry } => match schema::remove_entry(doc, entry) {
                    Ok(()) => report.applied += 1,
                    Err(_) => report.skipped += 1,
                },
            }
        }
        Self::cleanup_sequences(doc, &touched);
        report
    }

    fn diff(&self, before: &Project, after: &Project) -> Vec<Change> {
        let mut sets = Vec::new();
        let mut creates = Vec::new();

        // Sequences whose Arc is shared skip their whole subtree (the model's copy-on-write makes
        // this the cheap diff: an edit detaches only the sequences it touched).
        let unchanged: Vec<u64> = before
            .items
            .iter()
            .filter(|(id, b)| {
                after.items.get(id).is_some_and(|a| match (&b.kind, &a.kind) {
                    (ItemKind::Sequence(x), ItemKind::Sequence(y)) => Arc::ptr_eq(x, y),
                    _ => false,
                })
            })
            .map(|(id, _)| id.0)
            .collect();

        // Field sets and creations, walking the after-tree parents-first (the DFS order the
        // batch must carry: a `Create` never precedes its parent's).
        for id in Self::all_nodes(after) {
            if unchanged.contains(&id) {
                continue;
            }
            let Some(kind) = Self::kind_of(after, id) else { continue };
            if Self::kind_of(before, id).is_none() {
                // A creation: the whole node (the order pass below assigns its key).
                let props_vec = Self::node_props(after, id).unwrap_or_default();
                creates.push(Change::Create {
                    id: Self::hub_id(id),
                    kind: Arc::from(kind),
                    parent: Self::parent_of(after, id),
                    position: String::new(),
                    props: props_vec,
                });
                continue;
            }
            for prop in node_table(after, id, kind) {
                if node_differs(before, after, id, kind, prop)
                    && let Some(v) = Self::node_props(after, id).and_then(|ps| ps.into_iter().find(|(p, _)| p.as_ref() == prop).map(|(_, v)| v))
                {
                    sets.push(Change::Set { id: Self::hub_id(id), prop: Arc::from(prop), value: v });
                }
            }
        }

        // Deletions: one Delete per subtree root gone from the after tree — a container's
        // children leave with it, so a node whose (before-)parent is also gone needs no Delete of
        // its own (the inverted filter would ship Deletes for ghosts and none for roots).
        let gone: Vec<u64> = Self::all_nodes(before).into_iter().filter(|id| Self::kind_of(after, *id).is_none()).collect();
        let deletes: Vec<Change> = gone
            .iter()
            .filter(|id| {
                let parent = Self::parent_of(before, **id);
                parent == ROOT || !gone.contains(&parent.0)
            })
            .map(|id| Change::Delete { id: Self::hub_id(*id) })
            .collect();

        // Order and moves per parent (root first, then every parent that still exists).
        let mut order_changes = Vec::new();
        let mut parents: Vec<HubId> = vec![ROOT];
        parents.extend(Self::all_nodes(before).into_iter().map(Self::hub_id).filter(|i| *i != ROOT));
        for id in Self::all_nodes(after) {
            let h = Self::hub_id(id);
            if h != ROOT && !parents.contains(&h) {
                parents.push(h);
            }
        }
        for parent in parents {
            if parent == ROOT || Self::kind_of(after, parent.0).is_some() {
                self.diff_order(before, after, parent, &mut order_changes);
            }
        }

        // The creations' keys, now that the order pass has assigned them.
        let positions = self.positions.lock().unwrap_or_else(|e| e.into_inner());
        for create in &mut creates {
            if let Change::Create { id, position, .. } = create {
                *position = positions.get(&Self::doc_id(*id)).cloned().unwrap_or_default();
            }
        }

        // Document-level scalars (never `locks` — the session owns lock lifecycle) and entries.
        let mut doc_level = Vec::new();
        let no_locks = schema::locks_value(&[]);
        for prop in props::DOC_PROPS {
            if *prop == "locks" {
                continue;
            }
            if props::doc_prop_value(before, prop, &no_locks) != props::doc_prop_value(after, prop, &no_locks)
                && let Some(v) = props::doc_prop_value(after, prop, &no_locks)
            {
                doc_level.push(Change::SetDoc { prop: Arc::from(*prop), value: v });
            }
        }
        doc_level.extend(schema::diff_entries(before, after));

        // Batch order (§2.3): field writes, creations (parents first), moves, deletes, doc level.
        let mut batch = sets;
        batch.append(&mut creates);
        batch.append(&mut order_changes);
        batch.extend(deletes);
        batch.append(&mut doc_level);
        batch
    }

    fn presence(&self, _doc: &Project, ui: &UiState) -> Option<craft_collab::presence::PresenceMsg> {
        // Nothing to project when the UI is quiet: no sequence open, nothing selected, no
        // playhead, no drag in flight.
        if ui.page.is_none() && ui.selection.is_empty() && ui.playhead.is_none() && ui.cursor.is_none() && ui.viewport.is_none() && ui.track_range.is_none() {
            return None;
        }
        Some(craft_collab::presence::PresenceMsg {
            cursor: ui.cursor,
            page: ui.page, // the active sequence (the playhead's frame of reference)
            selection: ui.selection.clone(),
            viewport: ui.viewport,
            editing_text: None,    // FilmCraft edits text in dialogs, not a live canvas caret
            playhead: ui.playhead, // sequence ticks; peers resolve frames with the sequence's settings
            track_range: ui.track_range,
            generating: None, // FilmCraft runs no generation jobs (ArtCraft's field)
        })
    }

    fn schema(&self) -> &dyn craft_collab::adapter::DocSchema {
        &self.schema
    }
}

/// The property table of one node (by kind), for the diff walk.
fn node_table(doc: &Project, id: u64, kind: &str) -> Vec<&'static str> {
    match kind {
        "bin" => props::BIN_PROPS.to_vec(),
        "media" | "sequence" | "subclip" | "adjustment" | "graphic" => doc.items.get(&ItemId(id)).map(|i| props::item_props(&i.kind)).unwrap_or_default(),
        "videoTrack" | "audioTrack" | "submixTrack" => props::TRACK_PROPS.to_vec(),
        "captionTrack" => props::CAPTION_TRACK_PROPS.to_vec(),
        "clip" => props::CLIP_PROPS.to_vec(),
        "transition" => props::TRANSITION_PROPS.to_vec(),
        _ => props::CAPTION_PROPS.to_vec(),
    }
}

/// Whether one property of one node differs between two documents.
fn node_differs(before: &Project, after: &Project, id: u64, kind: &str, prop: &str) -> bool {
    let both = |b: Option<bool>| b.unwrap_or(false);
    match kind {
        "bin" => both(FilmcraftHub::find_bin(before, id).zip(FilmcraftHub::find_bin(after, id)).and_then(|(b, a)| props::bin_differs(b, a, prop))),
        "media" | "sequence" | "subclip" | "adjustment" | "graphic" => {
            both(before.items.get(&ItemId(id)).zip(after.items.get(&ItemId(id))).and_then(|(b, a)| props::any_item_differs(b, a, prop)))
        }
        "videoTrack" | "audioTrack" | "submixTrack" => {
            both(FilmcraftHub::find_track(before, id).zip(FilmcraftHub::find_track(after, id)).and_then(|(b, a)| props::track_differs(b, a, prop)))
        }
        "captionTrack" => both(
            FilmcraftHub::find_caption_track_ref(before, id)
                .zip(FilmcraftHub::find_caption_track_ref(after, id))
                .and_then(|(b, a)| props::caption_track_differs(b, a, prop)),
        ),
        "clip" => both(FilmcraftHub::find_clip(before, id).zip(FilmcraftHub::find_clip(after, id)).and_then(|(b, a)| props::clip_differs(b, a, prop))),
        "transition" => both(
            FilmcraftHub::find_transition(before, id).zip(FilmcraftHub::find_transition(after, id)).and_then(|(b, a)| props::transition_differs(b, a, prop)),
        ),
        _ => both(FilmcraftHub::find_caption(before, id).zip(FilmcraftHub::find_caption(after, id)).and_then(|(b, a)| props::caption_differs(b, a, prop))),
    }
}

impl FilmcraftHub {
    /// Create one node of any kind (the `apply` side of the tree): build with the kind's
    /// defaults, apply the properties, insert — in that order, so a bad property leaves no
    /// half-built node behind.
    fn create(&self, doc: &mut Project, id: u64, kind: &str, parent: HubId, position: &str, props: &[(Arc<str>, Value)]) -> Result<(), String> {
        let apply_props = |set: &mut dyn FnMut(&str, &Value) -> Result<(), String>| -> Result<(), String> {
            for (p, v) in props {
                if p.as_ref() != "parent" && p.as_ref() != "position" {
                    set(p, v)?;
                }
            }
            Ok(())
        };
        match kind {
            "bin" => {
                let mut bin = Bin { id: filmcraft_project::BinId(id), name: String::new(), children: Vec::new() };
                apply_props(&mut |p, v| match props::bin_set(&mut bin, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown bin property '{p}'")),
                })?;
                if parent == ROOT {
                    doc.root.children.push(BinEntry::Bin(bin));
                } else if let Some(b) = Self::find_bin_mut(doc, parent.0) {
                    b.children.push(BinEntry::Bin(bin));
                } else {
                    return Err(format!("parent bin {parent:?} does not exist"));
                }
            }
            "media" | "sequence" | "subclip" | "adjustment" | "graphic" => {
                let mut item = default_item(id, kind);
                apply_props(&mut |p, v| match props::any_item_set(&mut item, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown item property '{p}'")),
                })?;
                doc.items.insert(ItemId(id), item);
                if parent == ROOT {
                    doc.root.children.push(BinEntry::Item(ItemId(id)));
                } else if let Some(b) = Self::find_bin_mut(doc, parent.0) {
                    b.children.push(BinEntry::Item(ItemId(id)));
                } else {
                    return Err(format!("parent bin {parent:?} does not exist"));
                }
            }
            "videoTrack" | "audioTrack" | "submixTrack" => {
                let track_kind = if kind == "videoTrack" { filmcraft_project::TrackKind::Video } else { filmcraft_project::TrackKind::Audio };
                let mut track = filmcraft_project::Track::new(TrackId(id), track_kind, String::new());
                apply_props(&mut |p, v| match props::track_set(&mut track, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown track property '{p}'")),
                })?;
                let Some(seq) = doc.sequence_mut(ItemId(parent.0)) else {
                    return Err(format!("parent sequence {parent:?} does not exist"));
                };
                let list = match kind {
                    "videoTrack" => &mut seq.video_tracks,
                    "audioTrack" => &mut seq.audio_tracks,
                    _ => &mut seq.submix_tracks,
                };
                list.push(track);
            }
            "captionTrack" => {
                let mut ct = filmcraft_project::CaptionTrack::new(TrackId(id), String::new(), filmcraft_project::CaptionFormat::Subtitle);
                apply_props(&mut |p, v| match props::caption_track_set(&mut ct, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown caption track property '{p}'")),
                })?;
                let Some(seq) = doc.sequence_mut(ItemId(parent.0)) else {
                    return Err(format!("parent sequence {parent:?} does not exist"));
                };
                seq.caption_tracks.push(ct);
            }
            "clip" => {
                let mut clip = default_clip(id);
                apply_props(&mut |p, v| match props::clip_set(&mut clip, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown clip property '{p}'")),
                })?;
                let Some(track) = Self::track_of_mut(doc, parent.0) else {
                    return Err(format!("parent track {parent:?} does not exist"));
                };
                track.items.push(clip);
            }
            "transition" => {
                let mut tr = default_transition(id);
                apply_props(&mut |p, v| match props::transition_set(&mut tr, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown transition property '{p}'")),
                })?;
                let Some(track) = Self::track_of_mut(doc, parent.0) else {
                    return Err(format!("parent track {parent:?} does not exist"));
                };
                track.transitions.push(tr);
            }
            "caption" => {
                let mut cap = default_caption(id);
                apply_props(&mut |p, v| match props::caption_set(&mut cap, p, v) {
                    Some(Ok(())) => Ok(()),
                    Some(Err(e)) => Err(e),
                    None => Err(format!("unknown caption property '{p}'")),
                })?;
                let Some(item) = Self::find_caption_track(doc, parent.0) else {
                    return Err(format!("parent caption track {parent:?} does not exist"));
                };
                let Some(seq) = doc.sequence_mut(item) else {
                    return Err(format!("parent caption track {parent:?} does not exist"));
                };
                let Some(t) = seq.caption_track_mut(TrackId(parent.0)) else {
                    return Err(format!("parent caption track {parent:?} does not exist"));
                };
                t.captions.push(cap);
            }
            _ => return Err(format!("unknown kind '{kind}'")),
        }
        self.set_key(id, position.to_string());
        let keys = self.positions.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Self::reorder_children(doc, parent, &keys);
        Ok(())
    }

    /// A mutable ordinary track by id (video, audio or submix).
    fn track_of_mut(doc: &mut Project, id: u64) -> Option<&mut filmcraft_project::Track> {
        let (item, list) = Self::find_track_list(doc, id)?;
        let seq = doc.sequence_mut(item)?;
        let list = match list {
            TrackList::Video => &mut seq.video_tracks,
            TrackList::Audio => &mut seq.audio_tracks,
            TrackList::Submix => &mut seq.submix_tracks,
        };
        list.iter_mut().find(|t| t.id.0 == id)
    }

    /// Write one property of one node (any kind); `false` when the node or property is unknown
    /// or the value was the wrong shape (skipped, never a panic).
    fn set_node_prop(doc: &mut Project, id: u64, prop: &str, value: &Value) -> bool {
        if let Some(item) = doc.items.get_mut(&ItemId(id)) {
            return props::any_item_set(item, prop, value).is_some_and(|r| r.is_ok());
        }
        if Self::find_bin(doc, id).is_some() {
            let Some(bin) = Self::find_bin_mut(doc, id) else { return false };
            return props::bin_set(bin, prop, value).is_some_and(|r| r.is_ok());
        }
        if Self::find_track_list(doc, id).is_some() {
            let Some(track) = Self::track_of_mut(doc, id) else { return false };
            return props::track_set(track, prop, value).is_some_and(|r| r.is_ok());
        }
        if Self::find_caption_track(doc, id).is_some() {
            let Some(item) = Self::find_caption_track(doc, id) else { return false };
            let Some(seq) = doc.sequence_mut(item) else { return false };
            let Some(ct) = seq.caption_track_mut(TrackId(id)) else { return false };
            return props::caption_track_set(ct, prop, value).is_some_and(|r| r.is_ok());
        }
        if let Some((item, track)) = Self::find_clip_home(doc, id) {
            let Some(seq) = doc.sequence_mut(item) else { return false };
            let Some(t) = seq.track_mut(track) else { return false };
            let Some(clip) = t.item_mut(filmcraft_project::ClipId(id)) else { return false };
            return props::clip_set(clip, prop, value).is_some_and(|r| r.is_ok());
        }
        if let Some((item, track)) = Self::find_transition_home(doc, id) {
            let Some(seq) = doc.sequence_mut(item) else { return false };
            let Some(t) = seq.track_mut(track) else { return false };
            let Some(tr) = t.transitions.iter_mut().find(|tr| tr.id.0 == id) else { return false };
            return props::transition_set(tr, prop, value).is_some_and(|r| r.is_ok());
        }
        if let Some((item, track)) = Self::find_caption_home(doc, id) {
            let Some(seq) = doc.sequence_mut(item) else { return false };
            let Some(t) = seq.caption_track_mut(track) else { return false };
            let Some(cap) = t.captions.iter_mut().find(|c| c.id.0 == id) else { return false };
            return props::caption_set(cap, prop, value).is_some_and(|r| r.is_ok());
        }
        false
    }

    /// Move a node to a new parent (the `Set(parent)` path); `Err` when it cannot be done (the
    /// lenient rule: skipped, never a panic).
    fn move_node(doc: &mut Project, id: u64, parent: HubId) -> Result<(), ()> {
        let Some(kind) = Self::kind_of(doc, id) else { return Err(()) };
        let accepted = if parent == ROOT {
            ROOT_CHILD_KINDS.contains(&kind)
        } else {
            Self::kind_of(doc, parent.0).is_some_and(|tk| CONTAINERS.contains(&tk) && container_accepts(tk, kind))
        };
        if !accepted {
            return Err(());
        }
        match kind {
            "bin" | "media" | "sequence" | "subclip" | "adjustment" | "graphic" => {
                let bin = if parent == ROOT { None } else { Some(filmcraft_project::BinId(parent.0)) };
                doc.move_to_bin(&[ItemId(id)], bin).map(|_| ()).ok_or(())
            }
            "captionTrack" => {
                let Some(old) = Self::find_caption_track(doc, id) else { return Err(()) };
                let ct =
                    doc.sequence_mut(old).and_then(|s| s.caption_tracks.iter().position(|t| t.id.0 == id).map(|at| s.caption_tracks.remove(at))).ok_or(())?;
                let Some(seq) = doc.sequence_mut(ItemId(parent.0)) else { return Err(()) };
                seq.caption_tracks.push(ct);
                Ok(())
            }
            "videoTrack" | "audioTrack" | "submixTrack" => {
                let Some((old_item, list)) = Self::find_track_list(doc, id) else { return Err(()) };
                let track = doc
                    .sequence_mut(old_item)
                    .and_then(|s| {
                        let list = match list {
                            TrackList::Video => &mut s.video_tracks,
                            TrackList::Audio => &mut s.audio_tracks,
                            TrackList::Submix => &mut s.submix_tracks,
                        };
                        list.iter().position(|t| t.id.0 == id).map(|at| list.remove(at))
                    })
                    .ok_or(())?;
                let Some(seq) = doc.sequence_mut(ItemId(parent.0)) else { return Err(()) };
                let list = match kind {
                    "videoTrack" => &mut seq.video_tracks,
                    "audioTrack" => &mut seq.audio_tracks,
                    _ => &mut seq.submix_tracks,
                };
                list.push(track);
                Ok(())
            }
            "clip" => {
                let Some((old_item, old_track)) = Self::find_clip_home(doc, id) else { return Err(()) };
                let clip = doc
                    .sequence_mut(old_item)
                    .and_then(|s| s.track_mut(old_track))
                    .and_then(|t| t.items.iter().position(|i| i.id.0 == id).map(|at| t.items.remove(at)))
                    .ok_or(())?;
                let Some(track) = Self::track_of_mut(doc, parent.0) else { return Err(()) };
                track.items.push(clip);
                Ok(())
            }
            "transition" => {
                let Some((old_item, old_track)) = Self::find_transition_home(doc, id) else { return Err(()) };
                let tr = doc
                    .sequence_mut(old_item)
                    .and_then(|s| s.track_mut(old_track))
                    .and_then(|t| t.transitions.iter().position(|i| i.id.0 == id).map(|at| t.transitions.remove(at)))
                    .ok_or(())?;
                let Some(track) = Self::track_of_mut(doc, parent.0) else { return Err(()) };
                track.transitions.push(tr);
                Ok(())
            }
            "caption" => {
                let Some((old_item, old_track)) = Self::find_caption_home(doc, id) else { return Err(()) };
                let cap = doc
                    .sequence_mut(old_item)
                    .and_then(|s| s.caption_track_mut(old_track))
                    .and_then(|t| t.captions.iter().position(|c| c.id.0 == id).map(|at| t.captions.remove(at)))
                    .ok_or(())?;
                let Some(item) = Self::find_caption_track(doc, parent.0) else { return Err(()) };
                let Some(seq) = doc.sequence_mut(item) else { return Err(()) };
                seq.caption_track_mut(TrackId(parent.0)).ok_or(())?.captions.push(cap);
                Ok(())
            }
            _ => Err(()),
        }
    }

    /// Delete a node (subtree); `false` when the id names nothing.
    fn delete_node(doc: &mut Project, id: u64) -> bool {
        let Some(kind) = Self::kind_of(doc, id) else { return false };
        match kind {
            "bin" => {
                // Remove the whole subtree: child bins, the items they list, and those items'
                // sequences with everything in them.
                let mut doomed_items = Vec::new();
                fn walk(bin: &Bin, items: &mut Vec<u64>) {
                    for c in &bin.children {
                        match c {
                            BinEntry::Item(i) => items.push(i.0),
                            BinEntry::Bin(b) => walk(b, items),
                        }
                    }
                }
                if let Some(b) = Self::find_bin(doc, id) {
                    walk(b, &mut doomed_items);
                }
                if doc.root.remove_bin(filmcraft_project::BinId(id)).is_none() {
                    return false;
                }
                for i in doomed_items {
                    doc.items.remove(&ItemId(i));
                }
                true
            }
            "media" | "sequence" | "subclip" | "adjustment" | "graphic" => doc.items.remove(&ItemId(id)).is_some() && doc.root.remove_item(ItemId(id)),
            "videoTrack" | "audioTrack" | "submixTrack" => {
                let Some((item, list)) = Self::find_track_list(doc, id) else { return false };
                let Some(seq) = doc.sequence_mut(item) else { return false };
                let list = match list {
                    TrackList::Video => &mut seq.video_tracks,
                    TrackList::Audio => &mut seq.audio_tracks,
                    TrackList::Submix => &mut seq.submix_tracks,
                };
                let before = list.len();
                list.retain(|t| t.id.0 != id);
                list.len() != before
            }
            "captionTrack" => {
                let Some(item) = Self::find_caption_track(doc, id) else { return false };
                let Some(seq) = doc.sequence_mut(item) else { return false };
                let before = seq.caption_tracks.len();
                seq.caption_tracks.retain(|t| t.id.0 != id);
                seq.caption_tracks.len() != before
            }
            "clip" => {
                let Some((item, track)) = Self::find_clip_home(doc, id) else { return false };
                let Some(seq) = doc.sequence_mut(item) else { return false };
                let Some(t) = seq.track_mut(track) else { return false };
                let before = t.items.len();
                t.items.retain(|i| i.id.0 != id);
                t.items.len() != before
            }
            "transition" => {
                let Some((item, track)) = Self::find_transition_home(doc, id) else { return false };
                let Some(seq) = doc.sequence_mut(item) else { return false };
                let Some(t) = seq.track_mut(track) else { return false };
                let before = t.transitions.len();
                t.transitions.retain(|tr| tr.id.0 != id);
                t.transitions.len() != before
            }
            "caption" => {
                let Some((item, track)) = Self::find_caption_home(doc, id) else { return false };
                let Some(seq) = doc.sequence_mut(item) else { return false };
                let Some(t) = seq.caption_track_mut(track) else { return false };
                let before = t.captions.len();
                t.captions.retain(|c| c.id.0 != id);
                t.captions.len() != before
            }
            _ => false,
        }
    }
}

/// A fresh project item of `kind` with the given id (the Create's defaults before props land).
fn default_item(id: u64, kind: &str) -> filmcraft_project::ProjectItem {
    let item_kind = match kind {
        "media" => ItemKind::Media(filmcraft_project::MediaClip {
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
        "sequence" => ItemKind::Sequence(Arc::new(empty_sequence())),
        "subclip" => ItemKind::Subclip {
            parent: ItemId(0),
            range: filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::Tick::ZERO),
            restrict_trims: false,
        },
        "adjustment" => ItemKind::AdjustmentLayer { width: 0, height: 0, rate: filmcraft_time::FrameRate::default(), duration: filmcraft_time::Tick::ZERO },
        _ => ItemKind::Graphic { width: 0, height: 0, rate: filmcraft_time::FrameRate::default() },
    };
    filmcraft_project::ProjectItem {
        id: ItemId(id),
        name: String::new(),
        label: filmcraft_project::Label::ALL[0],
        kind: item_kind,
        metadata: BTreeMap::new(),
        created: 0,
        split: filmcraft_project::SplitMarks::default(),
    }
}

/// An empty sequence (the same shape `Project::new_sequence` builds).
pub(crate) fn empty_sequence() -> filmcraft_project::Sequence {
    filmcraft_project::Sequence {
        settings: filmcraft_project::SequenceSettings::default(),
        video_tracks: Vec::new(),
        audio_tracks: Vec::new(),
        markers: Vec::new(),
        mark_in: None,
        mark_out: None,
        work_area: None,
        start_timecode: 0,
        master_volume_db: 0.0,
        master_effects: Vec::new(),
        master_mixer: filmcraft_project::MixerStrip::default(),
        submix_tracks: Vec::new(),
        caption_tracks: Vec::new(),
        multicam: None,
        merged: None,
        split: filmcraft_project::SplitMarks::default(),
    }
}

/// A fresh clip (the Create's defaults before props land).
fn default_clip(id: u64) -> filmcraft_project::TrackItem {
    filmcraft_project::TrackItem {
        id: filmcraft_project::ClipId(id),
        item: ItemId(0),
        name: String::new(),
        label: filmcraft_project::Label::ALL[0],
        start: filmcraft_time::Tick::ZERO,
        duration: filmcraft_time::Tick::ZERO,
        source_in: filmcraft_time::Tick::ZERO,
        speed: 1.0,
        reverse: false,
        enabled: true,
        link: None,
        group: None,
        effects: Vec::new(),
        markers: Vec::new(),
        gain_db: 0.0,
        frame_hold: None,
        scale_to_frame: false,
        essential: None,
        multicam: None,
        time_interpolation: filmcraft_project::TimeInterpolation::default(),
        hold_filters: false,
        field_options: None,
        source_channels: Vec::new(),
        audio_stream: 0,
        graphic: None,
    }
}

/// A fresh transition (the Create's defaults before props land).
fn default_transition(id: u64) -> filmcraft_project::Transition {
    filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(id),
        effect: filmcraft_project::EffectInstance {
            effect: String::new(),
            enabled: true,
            params: BTreeMap::new(),
            masks: Vec::new(),
            post_fader: false,
            essential: false,
            layer: None,
        },
        start: filmcraft_time::Tick::ZERO,
        duration: filmcraft_time::Tick::ZERO,
        from: None,
        to: None,
        align: filmcraft_project::TransitionAlign::default(),
        reverse: false,
    }
}

/// A fresh caption (the Create's defaults before props land).
fn default_caption(id: u64) -> filmcraft_project::Caption {
    filmcraft_project::Caption {
        id: filmcraft_project::ClipId(id),
        start: filmcraft_time::Tick::ZERO,
        duration: filmcraft_time::Tick::ZERO,
        text: String::new(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    }
}

//! The adapter's ported convergence suite (CraftHub spec §3.6 phase C: the ported tests, the
//! family's most valuable testing asset) over FilmCraft's real project — bins, items, sequences,
//! tracks, clips, transitions, captions and the LUT library — plus this row's own timeline tests:
//! a clip move that touches only that clip, the region locks of the `TimelineRange` shape, and
//! the deferred LUT asset (bytes never inside batches).

// Tests may unwrap: a failing assertion is the point of a test (the family's clippy rule).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use craft_collab::adapter::HubAdapter;
use craft_collab::change::Change;
use craft_collab::ids::NodeId as HubId;
use craft_collab::value::Value;
use filmcraft_hub::FilmcraftHub;
use filmcraft_media::{MediaInfo, MediaKind};
use filmcraft_project::{
    BinEntry, Caption, CaptionFormat, CaptionTrack, ClipId, EffectInstance, ItemId, ItemKind, Label, MediaClip, MediaRef, Project, ProjectLut,
    SequenceSettings, Track, TrackItem, TrackKind, Transition, TransitionAlign,
};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

/// One second, in the document's canonical time.
const S: i64 = TICKS_PER_SECOND;

/// A media item of `dur` seconds, imported into `bin` (the honest import path).
fn media_item(doc: &mut Project, name: &str, bin: Option<filmcraft_project::BinId>, dur: i64) -> ItemId {
    doc.add_item(
        name,
        Label::ALL[0],
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: format!("/media/{name}.mov") },
            info: MediaInfo {
                name: name.into(),
                kind: MediaKind::Movie,
                duration: Tick(dur * S),
                video: None,
                audio_streams: Vec::new(),
                container: "mov".into(),
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
        bin,
    )
}

/// One clip on a track (the full literal, so a model field cannot drift past the tests).
fn clip(id: u64, item: u64, start: i64, dur: i64) -> TrackItem {
    TrackItem {
        id: ClipId(id),
        item: ItemId(item),
        name: String::new(),
        label: Label::ALL[0],
        start: Tick(start * S),
        duration: Tick(dur * S),
        source_in: Tick::ZERO,
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

/// A cross-dissolve between two clips (the full literal).
fn transition(id: u64, start: i64, dur: i64, from: u64, to: u64) -> Transition {
    Transition {
        id: filmcraft_project::TransitionId(id),
        effect: EffectInstance {
            effect: "crossDissolve".into(),
            enabled: true,
            params: std::collections::BTreeMap::new(),
            masks: Vec::new(),
            post_fader: false,
            essential: false,
            layer: None,
        },
        start: Tick(start * S),
        duration: Tick(dur * S),
        from: Some(ClipId(from)),
        to: Some(ClipId(to)),
        align: TransitionAlign::CenterAtCut,
        reverse: false,
    }
}

/// One caption block.
fn caption(id: u64, start: i64, dur: i64, text: &str) -> Caption {
    Caption { id: ClipId(id), start: Tick(start * S), duration: Tick(dur * S), text: text.into(), speaker: None, cue_id: None, settings: String::new() }
}

/// The sample project: a bin tree with media, a sequence with two video tracks, one audio track,
/// a submix and a caption track, four clips on the first video track, a transition, captions, and
/// a LUT — the shape of a small real edit.
fn sample() -> Project {
    let mut doc = Project::new("Wave Test");
    let footage = doc.add_bin("Footage", None);
    let interviews = doc.add_bin("Interviews", Some(footage));
    let a = media_item(&mut doc, "A roll", Some(footage), 30);
    let b = media_item(&mut doc, "Interview", Some(interviews), 120);
    let seq = doc.new_sequence("Main", SequenceSettings::default(), 2, 1, Some(footage));

    let (v1, a1) = {
        let s = doc.sequence_mut(seq).unwrap();
        (s.video_tracks[0].id, s.audio_tracks[0].id)
    };
    let _submix = {
        let id = filmcraft_project::TrackId(doc.alloc_id());
        let s = doc.sequence_mut(seq).unwrap();
        s.submix_tracks.push(Track::new(id, TrackKind::Audio, "Submix 1".into()));
        id
    };
    let cc = {
        let id = filmcraft_project::TrackId(doc.alloc_id());
        let s = doc.sequence_mut(seq).unwrap();
        s.caption_tracks.push(CaptionTrack::new(id, "CC1".into(), CaptionFormat::Subtitle));
        id
    };
    // Four clips on the first video track, at distinct starts (the timeline's own invariant).
    let clip_ids: Vec<u64> = [0, 4, 8, 12]
        .iter()
        .map(|&start| {
            let id = doc.alloc_id();
            let s = doc.sequence_mut(seq).unwrap();
            s.track_mut(v1).unwrap().items.push(clip(id, if start == 8 { b.0 } else { a.0 }, start, 4));
            id
        })
        .collect();
    // A cross-dissolve between the first two clips.
    let tr = doc.alloc_id();
    doc.sequence_mut(seq).unwrap().track_mut(v1).unwrap().transitions.push(transition(tr, 4, 1, clip_ids[0], clip_ids[1]));
    // Two captions.
    let c1 = doc.alloc_id();
    let c2 = doc.alloc_id();
    {
        let s = doc.sequence_mut(seq).unwrap();
        s.caption_track_mut(cc).unwrap().captions.push(caption(c1, 0, 4, "Hello."));
        s.caption_track_mut(cc).unwrap().captions.push(caption(c2, 4, 4, "Welcome back."));
    }
    // One audio clip and one on the second video track, so the track lists all hold something.
    let au = doc.alloc_id();
    doc.sequence_mut(seq).unwrap().track_mut(a1).unwrap().items.push(clip(au, b.0, 0, 16));
    let v2 = doc.sequence_mut(seq).unwrap().video_tracks[1].id;
    let vc = doc.alloc_id();
    doc.sequence_mut(seq).unwrap().track_mut(v2).unwrap().items.push(clip(vc, a.0, 0, 4));

    // The LUT library: metadata + embedded text (the deferred asset).
    doc.luts.push(ProjectLut {
        id: "lut-1".into(),
        name: "Teal Night".into(),
        source_path: Some("/lut/teal.cube".into()),
        format: "cube".into(),
        text: Arc::from("TITLE \"Teal Night\"\nLUT_3D_SIZE 2\n0 0 0\n"),
    });
    doc
}

/// Find a bin by name, anywhere in the tree (the test-side lookup).
fn bin_by_name(bin: &filmcraft_project::Bin, name: &str) -> Option<filmcraft_project::BinId> {
    if bin.name == name {
        return Some(bin.id);
    }
    bin.children.iter().find_map(|c| match c {
        BinEntry::Bin(b) => bin_by_name(b, name),
        _ => None,
    })
}

/// Where the sample's pieces live, for the tests to reach them.
struct Handle {
    seq: ItemId,
    v1: filmcraft_project::TrackId,
    a1: filmcraft_project::TrackId,
    cc: filmcraft_project::TrackId,
    clips: Vec<u64>,
    captions: Vec<u64>,
    footage: filmcraft_project::BinId,
    interviews: filmcraft_project::BinId,
    interview_item: ItemId,
}

fn handle(doc: &Project) -> Handle {
    let seq = doc.items.values().find(|i| i.as_sequence().is_some()).map(|i| i.id).unwrap();
    let s = doc.sequence(seq).unwrap();
    Handle {
        seq,
        v1: s.video_tracks[0].id,
        a1: s.audio_tracks[0].id,
        cc: s.caption_tracks[0].id,
        clips: s.track(s.video_tracks[0].id).unwrap().items.iter().map(|i| i.id.0).collect(),
        captions: s.caption_tracks[0].captions.iter().map(|c| c.id.0).collect(),
        footage: bin_by_name(&doc.root, "Footage").unwrap(),
        interviews: bin_by_name(&doc.root, "Interviews").unwrap(),
        interview_item: doc.items.values().find(|i| i.name == "Interview").map(|i| i.id).unwrap(),
    }
}

/// Two documents must view identically: same nodes (id, kind, parent, position, props), same
/// document properties, same entries.
fn assert_same_state(hub: &FilmcraftHub, a: &Project, b: &Project, what: &str) {
    let (va, vb) = (hub.view(a), hub.view(b));
    assert_eq!(va.nodes.len(), vb.nodes.len(), "{what}: node count");
    for (na, nb) in va.nodes.iter().zip(vb.nodes.iter()) {
        assert_eq!(na.id, nb.id, "{what}: node id");
        assert_eq!(na.kind, nb.kind, "{what}: kind of {na:?}");
        assert_eq!(na.parent, nb.parent, "{what}: parent of {na:?}");
        assert_eq!(na.position, nb.position, "{what}: position of {na:?}");
        assert_eq!(na.props, nb.props, "{what}: props of {na:?}");
    }
    assert_eq!(va.doc, vb.doc, "{what}: doc props");
    assert_eq!(va.entries, vb.entries, "{what}: entries");
}

/// The core promise, kept: applying the diff of two projects to the first makes it the second —
/// clip edits and moves, creations, deletions, bin moves, track additions, captions, LUT
/// metadata, and the document level.
#[test]
fn apply_diff_roundtrip() {
    let hub = FilmcraftHub::new();
    let a = sample();
    let h = handle(&a);
    let mut b = a.clone();

    // A clip rename and a time move (the last clip to the middle of the track).
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().item_mut(ClipId(h.clips[0])).unwrap().name = "Establishing".into();
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().item_mut(ClipId(h.clips[3])).unwrap().start = Tick(2 * S);
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().sort();

    // A creation in the middle, a deletion, a transition that leaves with its clip.
    let fresh = b.alloc_id();
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().items.push(clip(fresh, h.clips[1], 6, 2));
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().items.retain(|i| i.id.0 != h.clips[2]);
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().transitions.clear();

    // A submix deletion, a caption edit, a new audio track, a track rename.
    b.sequence_mut(h.seq).unwrap().submix_tracks.clear();
    b.sequence_mut(h.seq).unwrap().caption_track_mut(h.cc).unwrap().caption_mut(ClipId(h.captions[1])).unwrap().text = "Welcome back!!".into();
    let new_track = filmcraft_project::TrackId(b.alloc_id());
    b.sequence_mut(h.seq).unwrap().audio_tracks.push(Track::new(new_track, TrackKind::Audio, "Audio 2".into()));
    b.sequence_mut(h.seq).unwrap().track_mut(h.a1).unwrap().name = "Dialogue".into();

    // A bin rename, an item moved between bins, a media rename.
    b.root.find_bin_mut(h.interviews).unwrap().name = "Soundbites".into();
    b.move_to_bin(&[h.interview_item], None).unwrap();
    b.items.get_mut(&h.interview_item).unwrap().name = "The Interview".into();

    // The LUT's metadata, and the document level.
    b.luts[0].name = "Teal Day".into();
    b.name = "Wave Test 2".into();
    b.root.name = "Root bin".into();

    let changes = hub.diff(&a, &b);
    assert!(!changes.is_empty());

    let mut applied = a.clone();
    hub.apply(&mut applied, &changes);
    assert_same_state(&hub, &applied, &b, "roundtrip");
    assert!(hub.diff(&applied, &b).is_empty(), "the applied state diffs clean against the target");
}

/// An insertion in the middle of a track costs one position key — the sibling clips keep theirs.
#[test]
fn insertion_in_the_middle_costs_one_key() {
    let hub = FilmcraftHub::new();
    let a = sample();
    let h = handle(&a);
    let mut b = a.clone();

    let fresh = b.alloc_id();
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().items.push(clip(fresh, h.clips[1], 6, 2));
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().sort();

    let changes = hub.diff(&a, &b);
    let positions = changes.iter().filter(|c| matches!(c, Change::Set { prop, .. } if prop.as_ref() == "position")).count();
    let creates = changes.iter().filter(|c| matches!(c, Change::Create { .. })).count();
    assert_eq!(creates, 1, "one created clip");
    assert_eq!(positions, 1, "exactly one position write: the inserted clip ({changes:?})");

    let mut applied = a.clone();
    hub.apply(&mut applied, &changes);
    assert_same_state(&hub, &applied, &b, "insert");
    // And the inserted clip really sits in the middle, by start.
    let starts: Vec<i64> = applied.sequence(h.seq).unwrap().track(h.v1).unwrap().items.iter().map(|i| i.start.0).collect();
    assert_eq!(starts, [0, 4 * S, 6 * S, 8 * S, 12 * S], "start-sorted with the new clip third");
}

/// Two clients editing the same sequence converge (the server orders the batches; the playhead
/// never enters it — presence is ephemeral).
#[test]
fn two_clients_converge() {
    let hub = FilmcraftHub::new();
    let base = sample();
    let h = handle(&base);

    let mut local = base.clone();
    let mut remote = base.clone();
    local.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().item_mut(ClipId(h.clips[0])).unwrap().name = "Local name".into();
    remote.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().item_mut(ClipId(h.clips[0])).unwrap().duration = Tick(2 * S);

    let local_batch = hub.diff(&base, &local);
    let remote_batch = hub.diff(&base, &remote);
    assert_eq!(local_batch.len(), 1, "one field write locally: {local_batch:?}");
    assert_eq!(remote_batch.len(), 1, "one field write remotely: {remote_batch:?}");

    hub.apply(&mut local, &remote_batch);
    hub.apply(&mut remote, &local_batch);
    assert_same_state(&hub, &local, &remote, "convergence");
}

/// A deleted bin is one change — its items, and their sequences with everything in them, leave
/// with it — and deleting a deleted object does nothing.
#[test]
fn subtree_delete_is_one_change_and_tolerant() {
    let hub = FilmcraftHub::new();
    let a = sample();
    let h = handle(&a);
    let mut b = a.clone();

    // Delete the Footage bin the way the app does: the bin, and every item it listed.
    let mut doomed = Vec::new();
    b.root.find_bin(h.footage).unwrap().all_items(&mut doomed);
    b.root.remove_bin(h.footage);
    for i in doomed {
        b.items.remove(&i);
    }

    let changes = hub.diff(&a, &b);
    let deletes = changes.iter().filter(|c| matches!(c, Change::Delete { .. })).count();
    assert_eq!(deletes, 1, "one Delete for the whole bin subtree ({changes:?})");

    let mut applied = a.clone();
    hub.apply(&mut applied, &changes);
    assert_same_state(&hub, &applied, &b, "delete");
    assert!(!applied.items.contains_key(&h.seq), "the sequence item left with the bin");
    // Tolerant re-apply: deleting a deleted object does nothing (skipped, not an error).
    let report = hub.apply(&mut applied, &changes);
    assert_eq!(report.applied, 0);
}

/// The timeline's own edit: a clip dragged along the time axis touches only that clip — its
/// `start` and (because the track's order is derived from the starts) its own position key. No
/// write lands on the track, on a sibling clip, or on the sequence.
#[test]
fn a_clip_move_touches_only_that_clip() {
    let hub = FilmcraftHub::new();
    let a = sample();
    let h = handle(&a);
    let mut b = a.clone();

    // Drag the last clip (start 12s) to the front of the track (before the first).
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().item_mut(ClipId(h.clips[3])).unwrap().start = Tick(-4 * S);
    b.sequence_mut(h.seq).unwrap().track_mut(h.v1).unwrap().sort();

    let changes = hub.diff(&a, &b);
    assert!(!changes.is_empty());
    for c in &changes {
        match c {
            Change::Set { id, prop, .. } => {
                assert_eq!(id.0, h.clips[3], "a write on another object: {c:?}");
                assert!(prop.as_ref() == "start" || prop.as_ref() == "position", "an unexpected property: {c:?}");
            }
            other => panic!("unexpected change: {other:?}"),
        }
    }
    let json = serde_json::to_string(&changes).unwrap();
    assert!(json.len() < 256, "the move batch is a few bytes ({} bytes)", json.len());

    let mut applied = a.clone();
    hub.apply(&mut applied, &changes);
    assert_same_state(&hub, &applied, &b, "clip move");
    let starts: Vec<i64> = applied.sequence(h.seq).unwrap().track(h.v1).unwrap().items.iter().map(|i| i.start.0).collect();
    assert_eq!(starts, [-4 * S, 0, 4 * S, 8 * S], "the receiver's track is start-sorted like the sender's");
}

/// The region locks of the TimelineRange shape (§6.7, this row's own rule): a clip edit whose
/// track and time range intersect a lock held by another client is refused where every batch
/// already passes; the holder's own edit, and an edit outside the range, are not.
#[test]
fn region_locks_refuse_foreign_edits() {
    use craft_collab::adapter::DocSchema;
    use craft_collab::replica::Replica;

    let schema = filmcraft_hub::schema::FilmSchema::default();
    let mut replica = Replica::default();
    let track_id = HubId(12);
    let clip_id = HubId(100);
    replica.create(track_id, Arc::from("videoTrack"), filmcraft_hub::ROOT, "a0".into(), vec![]).unwrap();
    replica
        .create(
            clip_id,
            Arc::from("clip"),
            track_id,
            "a1".into(),
            vec![(Arc::from("start"), Value::Json(serde_json::json!(3 * S))), (Arc::from("duration"), Value::Json(serde_json::json!(2 * S)))],
        )
        .unwrap();
    // A lock held by client 7 on track 12, covering seconds 0..2.
    replica.set_doc(
        "locks",
        Value::Json(serde_json::json!([
            { "track": 12, "start": 0, "duration": 2 * S, "holder": 7 }
        ])),
    );

    let foreign = craft_collab::adapter::ValidationCtx { replica: &replica, author_space: 1, limits: &craft_collab::Limits::default() };
    let holder = craft_collab::adapter::ValidationCtx { replica: &replica, author_space: 7, limits: &craft_collab::Limits::default() };

    // The holder drags a clip into their own region: allowed.
    let into_the_lock = Change::Set { id: clip_id, prop: Arc::from("start"), value: Value::Json(serde_json::json!(S)) };
    assert!(schema.validate(&into_the_lock, &holder).is_ok(), "the holder edits inside their lock");
    // The same drag from another client, landing in the locked range: refused.
    assert!(schema.validate(&into_the_lock, &foreign).is_err(), "a foreign edit inside the lock is refused");
    // A foreign edit entirely outside the locked range: allowed.
    let outside = Change::Set { id: clip_id, prop: Arc::from("start"), value: Value::Json(serde_json::json!(10 * S)) };
    assert!(schema.validate(&outside, &foreign).is_ok(), "a foreign edit outside the lock is fine");
    // A foreign creation landing in the locked range: refused (the Create's parent is the track).
    let create = Change::Create {
        id: HubId(101),
        kind: Arc::from("clip"),
        parent: track_id,
        position: "a2".into(),
        props: vec![(Arc::from("start"), Value::Json(serde_json::json!(S))), (Arc::from("duration"), Value::Json(serde_json::json!(S)))],
    };
    assert!(schema.validate(&create, &foreign).is_err(), "a foreign clip landing in the lock is refused");
    assert!(schema.validate(&create, &holder).is_ok(), "the holder's clip lands");
}

/// The heavy payload is a deferred asset, not a batch: a LUT's changed text rides as a
/// content-addressed `AssetRef` whose bytes never appear in the change list, a put whose asset
/// has not arrived yet is skipped (deferred, not corrupted), and the replay after
/// `offer_asset` lands it.
#[test]
fn lut_text_rides_the_asset_channel() {
    let hub = FilmcraftHub::new();
    let a = sample();
    let mut b = a.clone();
    let lut_text = "TITLE \"Teal Night v2\"\nLUT_3D_SIZE 33\n<dense table, hundreds of KB>";
    b.luts[0].text = Arc::from(lut_text);

    let changes = hub.diff(&a, &b);
    let puts: Vec<&Value> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Put { entry, value } if entry.0.starts_with("LutText:") => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(puts.len(), 1, "one LutText entry ({changes:?})");
    let Value::AssetRef(asset) = puts[0] else { panic!("the LUT text must be an asset reference, not bytes") };
    assert_eq!(asset.sha256, filmcraft_hub::schema::text_sha256(lut_text), "content-addressed");
    assert_eq!(asset.bytes, lut_text.len() as u64, "the byte length rides the ref");
    let json = serde_json::to_string(&changes).unwrap();
    assert!(!json.contains("LUT_3D_SIZE 33"), "the LUT bytes never travel inside the batch");

    // Without the bytes the put is deferred (skipped), and the document keeps the old text.
    let mut applied = a.clone();
    let report = hub.apply(&mut applied, &changes);
    assert!(report.skipped >= 1, "the LutText put is skipped until the asset arrives");
    assert_ne!(applied.luts[0].text.as_ref(), lut_text);

    // The session hands the adapter the bytes; the replay lands them.
    hub.offer_asset(&asset.sha256, lut_text);
    let report = hub.apply(&mut applied, &changes);
    assert_eq!(applied.luts[0].text.as_ref(), lut_text, "the text lands after the asset ({report:?})");
    assert_same_state(&hub, &applied, &b, "deferred asset");
}

/// The command catalog resolves the v2 names onto their `hub.*` twins, and the deep link parses
/// the family format with the filmcraft scheme.
#[test]
fn catalog_and_deeplink() {
    assert_eq!(filmcraft_hub::commands::resolve("collab.share").map(|c| c.name), Some("hub.share"));
    assert_eq!(filmcraft_hub::commands::resolve("hub.peers").map(|c| c.name), Some("hub.peers"));
    assert!(filmcraft_hub::commands::resolve("collab.spotlight").is_some(), "the v2 spotlight maps onto hub.follow");
    let link = filmcraft_hub::deeplink::JoinLink::parse("filmcraft://join?code=AB12CD34&server=wss%3A%2F%2Fhub.example.com");
    assert_eq!(link.map(|l| (l.code, l.server)), Some((Some("AB12CD34".into()), Some("wss://hub.example.com".into()))));
}

/// The presence projection carries the playhead in sequence ticks beside the active sequence,
/// and a quiet UI declines to speak (presence is never a document write).
#[test]
fn presence_carries_the_playhead() {
    let hub = FilmcraftHub::new();
    let doc = sample();
    let h = handle(&doc);
    let ui = craft_collab::adapter::UiState {
        cursor: None,
        page: Some(h.seq.0),
        selection: vec![HubId(h.clips[0])],
        viewport: None,
        editing_text: None,
        playhead: Some(3 * S as u64),
        track_range: Some((0, S as u64, 2 * S as u64)),
        generating: None,
    };
    let msg = hub.presence(&doc, &ui).unwrap();
    assert_eq!(msg.playhead, Some(3 * S as u64), "the playhead rides as sequence ticks");
    assert_eq!(msg.page, Some(h.seq.0), "the active sequence is the frame of reference");
    assert_eq!(msg.track_range, Some((0, S as u64, 2 * S as u64)));
    assert!(msg.generating.is_none(), "FilmCraft runs no generation jobs");

    // The fps resolution is document data, not presence: the peer view resolves ticks to frames
    // with the sequence's own rate (24000/1001 here — SequenceSettings::default).
    let peer = filmcraft_hub::presence::PeerView::from_ui(&ui);
    let ticks_per_frame = (S as f64 / (24_000.0 / 1001.0)).round() as u64;
    assert_eq!(peer.playhead_frame(ticks_per_frame), Some(3 * S as u64 / ticks_per_frame));

    // A quiet UI says nothing.
    assert!(hub.presence(&doc, &craft_collab::adapter::UiState::default()).is_none());
}

/// A document that was never viewed still takes its keys at first contact (the first-sync rule
/// on the apply side), so a fresh replica catching up orders correctly.
#[test]
fn first_sync_on_a_fresh_replica() {
    let hub = FilmcraftHub::new();
    let a = sample();
    let h = handle(&a);
    let mut b = a.clone();
    // Reorder the root bin's children: the interview media item first.
    b.move_to_bin(&[h.interview_item], None).unwrap();
    b.root.children.rotate_right(1);

    let changes = hub.diff(&a, &b);
    let mut applied = a.clone();
    // `applied` was never viewed — apply must assign its keys by current order first.
    hub.apply(&mut applied, &changes);
    assert_same_state(&hub, &applied, &b, "first sync");
}

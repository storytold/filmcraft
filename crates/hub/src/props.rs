//! The synchronized property table (spec §2.3: "every field the file saves is a property").
//!
//! FilmCraft's document is not one `Node` struct but a family of them — bins, project items,
//! tracks, clips, transitions, captions — each with its own fields. Every struct gets its own
//! table here with the same compile-time guard the pilot used: a struct gaining a field that
//! doesn't sync is a **build error**, because the guard destructures every field, synced or
//! skipped (the skipped ones are the tree itself: ids and child lists).
//!
//! Naming decisions worth knowing when diffing this against the model:
//!
//! * `TrackItem::item` and `ItemKind::Subclip::parent` both reference another project item, so
//!   both sync as `sourceItem` (the tree `parent` property is reserved for the object tree).
//! * `ProjectItem::split` (the Source monitor's split marks) syncs as `splitMarks`; a sequence
//!   item's own `Sequence::split` keeps the plain `split` — the two live on the same object and
//!   need distinct names.
//! * `Track::kind` is pure discriminant (it decides which list the track is in), so it rides the
//!   node kind, not a property.
//! * `SequenceSettings` and `ProjectSettings` sync as single values: they are dialog-level
//!   units, and last-writer-wins on the block is their honest granularity.
//! * Big list-valued fields (effects, markers, mixer strips, transcripts) ride as one
//!   last-writer-wins value — the pilot's decision for `fills` and `effects`, kept verbatim.

use craft_collab::Value;
use filmcraft_project::{Bin, Caption, CaptionTrack, ItemKind, Project, ProjectItem, Sequence, Track, TrackItem, Transition};

/// One property table: the value/get/set/diff quadruple over one struct, plus the guard.
macro_rules! props_table {
    ($const_name:ident, $ty:path, $get:ident, $set:ident, $diff:ident, $guard:ident,
     synced: [$(( $field:ident, $name:literal )),+ $(,)?], skipped: [$($skip:ident),* $(,)?] $(,)?) => {
        /// The synchronized fields of this struct, by property name.
        pub const $const_name: &[&str] = &[$($name),+];

        /// The value of property `prop`; `None` when `prop` is not one of this struct's fields.
        pub fn $get(node: &$ty, prop: &str) -> Option<Value> {
            match prop {
                $($name => Some(Value::Json(serde_json::to_value(&node.$field).ok()?)),)+
                _ => None,
            }
        }

        /// Write property `prop`; `None` when `prop` is not one of this struct's fields. A value
        /// of the wrong shape is an error, never a panic — shipped code never crashes.
        pub fn $set(node: &mut $ty, prop: &str, value: &Value) -> Option<std::result::Result<(), String>> {
            match prop {
                $($name => Some(
                    serde_json::from_value(value.to_json())
                        .map(|v| node.$field = v)
                        .map_err(|e| e.to_string()),
                ),)+
                _ => None,
            }
        }

        /// Whether property `prop` differs between `a` and `b`; `None` if not a field here.
        pub fn $diff(a: &$ty, b: &$ty, prop: &str) -> Option<bool> {
            match prop {
                $($name => Some(a.$field != b.$field),)+
                _ => None,
            }
        }

        /// Fails to compile when the struct gains a field that is neither synced above nor
        /// consciously skipped: a new saved field must decide how it syncs.
        #[allow(dead_code)]
        fn $guard(n: &$ty) {
            let $ty { $($field: _),+, $($skip: _),* } = n;
        }
    };
}

props_table! { BIN_PROPS, Bin, bin_value, bin_set, bin_differs, every_bin_field_is_a_property,
    synced: [(name, "name")],
    skipped: [id, children],
}

props_table! { TRACK_PROPS, Track, track_value, track_set, track_differs, every_track_field_is_a_property,
    synced: [
        (name, "name"),
        (locked, "locked"),
        (sync_lock, "syncLock"),
        (enabled, "enabled"),
        (muted, "muted"),
        (solo, "solo"),
        (channels, "channels"),
        (volume_db, "volumeDb"),
        (pan, "pan"),
        (effects, "effects"),
        (mixer, "mixer"),
    ],
    skipped: [id, kind, items, transitions],
}

props_table! { CAPTION_TRACK_PROPS, CaptionTrack, caption_track_value, caption_track_set,
    caption_track_differs, every_caption_track_field_is_a_property,
    synced: [
        (name, "name"),
        (format, "format"),
        (language, "language"),
        (style, "style"),
        (enabled, "enabled"),
        (locked, "locked"),
        (sync_lock, "syncLock"),
        (vtt_blocks, "vttBlocks"),
    ],
    skipped: [id, captions],
}

props_table! { CLIP_PROPS, TrackItem, clip_value, clip_set, clip_differs, every_track_item_field_is_a_property,
    synced: [
        (item, "sourceItem"),
        (name, "name"),
        (label, "label"),
        (start, "start"),
        (duration, "duration"),
        (source_in, "sourceIn"),
        (speed, "speed"),
        (reverse, "reverse"),
        (enabled, "enabled"),
        (link, "link"),
        (group, "group"),
        (effects, "effects"),
        (markers, "markers"),
        (gain_db, "gainDb"),
        (frame_hold, "frameHold"),
        (scale_to_frame, "scaleToFrame"),
        (essential, "essential"),
        (multicam, "multicam"),
        (time_interpolation, "timeInterpolation"),
        (hold_filters, "holdFilters"),
        (field_options, "fieldOptions"),
        (source_channels, "sourceChannels"),
        (audio_stream, "audioStream"),
        (graphic, "graphic"),
    ],
    skipped: [id],
}

props_table! { TRANSITION_PROPS, Transition, transition_value, transition_set, transition_differs,
    every_transition_field_is_a_property,
    synced: [
        (effect, "effect"),
        (start, "start"),
        (duration, "duration"),
        (from, "from"),
        (to, "to"),
        (align, "align"),
        (reverse, "reverse"),
    ],
    skipped: [id],
}

props_table! { CAPTION_PROPS, Caption, caption_value, caption_set, caption_differs,
    every_caption_field_is_a_property,
    synced: [
        (start, "start"),
        (duration, "duration"),
        (text, "text"),
        (speaker, "speaker"),
        (cue_id, "cueId"),
        (settings, "settings"),
    ],
    skipped: [id],
}

// The `ProjectItem` wrapper fields (shared by every item kind). The `kind` payload syncs
// per-kind — see [`MEDIA_PROPS`], [`SEQUENCE_PROPS`] and the variant tables below.
props_table! { ITEM_PROPS, ProjectItem, item_value, item_set, item_differs, every_project_item_field_is_a_property,
    synced: [
        (name, "name"),
        (label, "label"),
        (metadata, "metadata"),
        (created, "created"),
        (split, "splitMarks"),
    ],
    skipped: [id, kind],
}

// The media payload of an item ([`ItemKind::Media`]) — everything the file knows about the
// source except its pixels and samples, which never entered the document (they live in files at
// `media`'s path; see the crate docs for the deferred-asset decision).
props_table! { MEDIA_PROPS, filmcraft_project::MediaClip, media_value, media_set, media_differs,
    every_media_clip_field_is_a_property,
    synced: [
        (media, "media"),
        (info, "mediaInfo"),
        (interpret, "interpret"),
        (mark_in, "markIn"),
        (mark_out, "markOut"),
        (markers, "markers"),
        (offline, "offline"),
        (proxy, "proxy"),
        (identity, "identity"),
    ],
    skipped: [],
}

// The sequence payload of an item ([`ItemKind::Sequence`]) minus its four track lists, which
// are the object tree (tracks are children of the sequence, clips and transitions of tracks).
props_table! { SEQUENCE_PROPS, Sequence, sequence_value, sequence_set, sequence_differs,
    every_sequence_field_is_a_property,
    synced: [
        (settings, "settings"),
        (markers, "markers"),
        (mark_in, "markIn"),
        (mark_out, "markOut"),
        (work_area, "workArea"),
        (start_timecode, "startTimecode"),
        (master_volume_db, "masterVolumeDb"),
        (master_effects, "masterEffects"),
        (master_mixer, "masterMixer"),
        (multicam, "multicam"),
        (merged, "merged"),
        (split, "split"),
    ],
    skipped: [video_tracks, audio_tracks, submix_tracks, caption_tracks],
}

/// The subclip payload ([`ItemKind::Subclip`]): the source media item, its range and the trim
/// restriction. `parent` syncs as `sourceItem` (the tree `parent` property is reserved).
pub const SUBCLIP_PROPS: &[&str] = &["sourceItem", "range", "restrictTrims"];

/// The adjustment-layer payload ([`ItemKind::AdjustmentLayer`]).
pub const ADJUSTMENT_PROPS: &[&str] = &["width", "height", "rate", "duration"];

/// The source-graphic payload ([`ItemKind::Graphic`]); duration is unlimited, so it has none.
pub const GRAPHIC_PROPS: &[&str] = &["width", "height", "rate"];

/// Fails to compile when an `ItemKind` variant gains a field that no table above lists (the
/// struct payloads are guarded by their own destructures).
#[allow(dead_code)]
fn every_item_kind_variant_field_is_a_property(kind: &ItemKind) {
    match kind {
        ItemKind::Media(m) => every_media_clip_field_is_a_property(m),
        ItemKind::Sequence(s) => every_sequence_field_is_a_property(s),
        ItemKind::Subclip { parent: _, range: _, restrict_trims: _ } => {}
        ItemKind::AdjustmentLayer { width: _, height: _, rate: _, duration: _ } => {}
        ItemKind::Graphic { width: _, height: _, rate: _ } => {}
    }
}

/// Every property that can be read off one project item: the wrapper fields plus its kind's
/// payload table. `None` when `prop` is neither.
pub fn any_item_value(item: &ProjectItem, prop: &str) -> Option<Value> {
    if let Some(v) = item_value(item, prop) {
        return Some(v);
    }
    match &item.kind {
        ItemKind::Media(m) => media_value(m, prop),
        ItemKind::Sequence(s) => sequence_value(s, prop),
        ItemKind::Subclip { parent, range, restrict_trims } => subclip_value(prop, *parent, *range, *restrict_trims),
        ItemKind::AdjustmentLayer { width, height, rate, duration } => adjustment_value(prop, *width, *height, *rate, *duration),
        ItemKind::Graphic { width, height, rate } => graphic_value(prop, *width, *height, *rate),
    }
}

/// The payload tables of the three inline variants (their fields are enum payload fields, so the
/// macro's `node.$field` does not reach them; these are written out by hand, guarded by the
/// variant destructure above).
fn subclip_value(prop: &str, parent: filmcraft_project::ItemId, range: filmcraft_time::TimeRange, restrict: bool) -> Option<Value> {
    let v = match prop {
        "sourceItem" => serde_json::to_value(parent.0).ok()?,
        "range" => serde_json::to_value(range).ok()?,
        "restrictTrims" => serde_json::to_value(restrict).ok()?,
        _ => return None,
    };
    Some(Value::Json(v))
}

fn adjustment_value(prop: &str, width: u32, height: u32, rate: filmcraft_time::FrameRate, duration: filmcraft_time::Tick) -> Option<Value> {
    let v = match prop {
        "width" => serde_json::to_value(width).ok()?,
        "height" => serde_json::to_value(height).ok()?,
        "rate" => serde_json::to_value(rate).ok()?,
        "duration" => serde_json::to_value(duration).ok()?,
        _ => return None,
    };
    Some(Value::Json(v))
}

fn graphic_value(prop: &str, width: u32, height: u32, rate: filmcraft_time::FrameRate) -> Option<Value> {
    let v = match prop {
        "width" => serde_json::to_value(width).ok()?,
        "height" => serde_json::to_value(height).ok()?,
        "rate" => serde_json::to_value(rate).ok()?,
        _ => return None,
    };
    Some(Value::Json(v))
}

/// Write one property of a project item (wrapper or payload). Sequence payloads detach the
/// shared `Arc` first, exactly as the engine's mutators do.
pub fn any_item_set(item: &mut ProjectItem, prop: &str, value: &Value) -> Option<std::result::Result<(), String>> {
    if let Some(r) = item_set(item, prop, value) {
        return Some(r);
    }
    let json = value.to_json();
    let kind = &mut item.kind;
    Some(match kind {
        ItemKind::Media(m) => media_set(m, prop, value)?,
        ItemKind::Sequence(s) => sequence_set(std::sync::Arc::make_mut(s), prop, value)?,
        ItemKind::Subclip { parent, range, restrict_trims } => {
            let r: std::result::Result<(), String> = match prop {
                "sourceItem" => serde_json::from_value::<u64>(json).map(|v| *parent = filmcraft_project::ItemId(v)).map_err(|e| e.to_string()),
                "range" => serde_json::from_value(json).map(|v| *range = v).map_err(|e| e.to_string()),
                "restrictTrims" => serde_json::from_value(json).map(|v| *restrict_trims = v).map_err(|e| e.to_string()),
                _ => return None,
            };
            r
        }
        ItemKind::AdjustmentLayer { width, height, rate, duration } => {
            let r: std::result::Result<(), String> = match prop {
                "width" => serde_json::from_value(json).map(|v| *width = v).map_err(|e| e.to_string()),
                "height" => serde_json::from_value(json).map(|v| *height = v).map_err(|e| e.to_string()),
                "rate" => serde_json::from_value(json).map(|v| *rate = v).map_err(|e| e.to_string()),
                "duration" => serde_json::from_value(json).map(|v| *duration = v).map_err(|e| e.to_string()),
                _ => return None,
            };
            r
        }
        ItemKind::Graphic { width, height, rate } => {
            let r: std::result::Result<(), String> = match prop {
                "width" => serde_json::from_value(json).map(|v| *width = v).map_err(|e| e.to_string()),
                "height" => serde_json::from_value(json).map(|v| *height = v).map_err(|e| e.to_string()),
                "rate" => serde_json::from_value(json).map(|v| *rate = v).map_err(|e| e.to_string()),
                _ => return None,
            };
            r
        }
    })
}

/// Whether one property of a project item differs between `a` and `b` (`None` when not a field).
pub fn any_item_differs(a: &ProjectItem, b: &ProjectItem, prop: &str) -> Option<bool> {
    if let Some(d) = item_differs(a, b, prop) {
        return Some(d);
    }
    match (&a.kind, &b.kind) {
        (ItemKind::Media(x), ItemKind::Media(y)) => media_differs(x, y, prop),
        (ItemKind::Sequence(x), ItemKind::Sequence(y)) => sequence_differs(x, y, prop),
        (ItemKind::Subclip { parent: p1, range: r1, restrict_trims: t1 }, ItemKind::Subclip { parent: p2, range: r2, restrict_trims: t2 }) => {
            Some(match prop {
                "sourceItem" => p1 != p2,
                "range" => r1 != r2,
                "restrictTrims" => t1 != t2,
                _ => return None,
            })
        }
        (
            ItemKind::AdjustmentLayer { width: w1, height: h1, rate: rt1, duration: d1 },
            ItemKind::AdjustmentLayer { width: w2, height: h2, rate: rt2, duration: d2 },
        ) => Some(match prop {
            "width" => w1 != w2,
            "height" => h1 != h2,
            "rate" => rt1 != rt2,
            "duration" => d1 != d2,
            _ => return None,
        }),
        (ItemKind::Graphic { width: w1, height: h1, rate: r1 }, ItemKind::Graphic { width: w2, height: h2, rate: r2 }) => Some(match prop {
            "width" => w1 != w2,
            "height" => h1 != h2,
            "rate" => r1 != r2,
            _ => return None,
        }),
        // A kind change is not a property write; the wrapper table covers what syncs.
        _ => Some(false),
    }
}

/// The properties of one project item by kind: the wrapper fields plus the payload table (built
/// from the tables above, so a table edit cannot drift from this list).
pub fn item_props(kind: &ItemKind) -> Vec<&'static str> {
    let payload: &[&str] = match kind {
        ItemKind::Media(_) => MEDIA_PROPS,
        ItemKind::Sequence(_) => SEQUENCE_PROPS,
        ItemKind::Subclip { .. } => SUBCLIP_PROPS,
        ItemKind::AdjustmentLayer { .. } => ADJUSTMENT_PROPS,
        ItemKind::Graphic { .. } => GRAPHIC_PROPS,
    };
    ITEM_PROPS.iter().chain(payload).copied().collect()
}

/// The document-level properties: the project's own scalars plus the two root-level names and
/// the region-lock set (§6.7 — locks sync as document properties; see [`crate::schema`]).
pub const DOC_PROPS: &[&str] = &["name", "settings", "nextId", "notes", "rootName", "locks"];

/// Write a document-level property; `None` when `prop` is not one of the synced scalars. The
/// `locks` property is adapter state (the session owns lock lifecycle), routed by `apply`.
pub fn set_doc_prop(doc: &mut Project, prop: &str, value: &Value) -> Option<std::result::Result<(), String>> {
    let json = value.to_json();
    match prop {
        "name" => Some(serde_json::from_value::<String>(json).map(|v| doc.name = v).map_err(|e| e.to_string())),
        "settings" => Some(serde_json::from_value(json).map(|v| doc.settings = v).map_err(|e| e.to_string())),
        "nextId" => Some(serde_json::from_value::<u64>(json).map(|v| doc.next_id = doc.next_id.max(v)).map_err(|e| e.to_string())),
        "notes" => Some(serde_json::from_value::<String>(json).map(|v| doc.notes = v).map_err(|e| e.to_string())),
        "rootName" => Some(serde_json::from_value::<String>(json).map(|v| doc.root.name = v).map_err(|e| e.to_string())),
        // Locks are adapter state, not a document field; the adapter's apply handles them.
        "locks" => None,
        _ => None,
    }
}

/// The value of a document-level property; `None` when `prop` is not a synced scalar.
pub fn doc_prop_value(doc: &Project, prop: &str, locks: &Value) -> Option<Value> {
    match prop {
        "name" => Some(Value::Json(serde_json::to_value(&doc.name).ok()?)),
        "settings" => Some(Value::Json(serde_json::to_value(&doc.settings).ok()?)),
        "nextId" => Some(Value::Json(serde_json::to_value(doc.next_id).ok()?)),
        "notes" => Some(Value::Json(serde_json::to_value(&doc.notes).ok()?)),
        "rootName" => Some(Value::Json(serde_json::to_value(&doc.root.name).ok()?)),
        "locks" => Some(locks.clone()),
        _ => None,
    }
}

/// Fails to compile when `Project` gains a field that is neither a synced scalar, a keyed entry
/// list (see [`crate::schema`]) nor the object tree itself.
#[allow(dead_code)]
fn every_project_field_is_a_property(p: &Project) {
    let Project {
        name: _,            // DOC_PROPS["name"]
        settings: _,        // DOC_PROPS["settings"]
        root: _,            // the object tree (the root bin is the file root)
        items: _,           // the object tree
        next_id: _,         // DOC_PROPS["nextId"] (id allocation proper is the core's)
        luts: _,            // entries Lut:<id> + LutText:<id> (the deferred asset)
        transcripts: _,     // entries Transcript:<item>
        search_bins: _,     // entries SearchBin:<id> + the searchBinOrder property
        source_graphics: _, // entries SourceGraphic:<item>
        narrations: _,      // entries Narration:<item>
        notes: _,           // DOC_PROPS["notes"]
    } = p;
}

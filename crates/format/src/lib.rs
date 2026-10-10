//! `.fcproj` project files.
//!
//! A project file is UTF-8 JSON with an explicit schema version:
//!
//! ```json
//! { "format": "filmcraft.project", "schema_version": 2, "generator": "FilmCraft 0.1.0", "project": { … } }
//! ```
//!
//! - **Versioning.** [`SCHEMA_VERSION`] is the version this build writes. Older files are upgraded on
//!   load by a chain of single-step migrations ([`MIGRATIONS`]: v1→v2, v2→v3, …) operating on the
//!   JSON document, so every historical shape only needs one small function. Newer files are refused
//!   with [`FormatError::TooNew`] instead of being half-read (and later overwritten with data loss).
//! - **Atomic saves.** [`atomic_write`] writes a temp file in the destination directory, fsyncs it,
//!   renames it over the target and fsyncs the directory: a crash or power cut during a save leaves
//!   either the old file or the new one, never a torn mix.
//! - **Auto-save naming/rotation** lives in [`autosave`] (`<name>-YYYY-MM-DD_HH-MM-SS.fcproj` in an
//!   `Auto-Save` folder next to the project, oldest pruned beyond the version limit).
//!
//! Schema history:
//! - **v1** (M0–M11): the bare serialized `Project` object with a `"version": 1` field.
//! - **v2** (M11.1): the envelope above; `Project.version` dropped (the envelope carries it).
//! - **v3** (M10.2): graphic clips (`ItemKind::Graphic`, text/shape layers). The shape of older
//!   data is unchanged (no-op step); the bump makes builds without graphics refuse such files
//!   cleanly instead of failing to parse them.
//! - **v4** (M7.4): Essential Sound (clip audio types and settings, `EffectInstance::essential`).
//!   No-op step; older builds would otherwise drop these fields silently when saving.
//! - **v5** (M8.8): colour management (sequence working space, Interpret Footage colour space,
//!   project LUT library). No-op step, for the same reason.
//! - **v6** (M11.7/M11.9): media identity fingerprints and ingest settings. No-op step.
//! - **v7** (M5.5): effect masks (keyframable Bézier mask paths on effect instances). No-op step.
//! - **v8** (M12): multi-camera source sequences, multi-camera clips and merged clips. No-op step;
//!   older builds would drop the camera data when saving.
//! - **v12** (M10.7): graphics templates, rolls / crawls, responsive design (pins, intro / outro),
//!   per-character text styles (`TrackItem::graphic`, `EffectInstance::layer`) and source graphics
//!   (`Project::source_graphics`). No-op step; older builds would drop these fields when saving.
//! - **v13**: all container audio streams, and per-timeline-clip stream selection. Old single-stream
//!   `audio` objects read as stream zero; older builds must refuse new multi-stream projects.
//! - **v14**: generated media (`Project::generated`: the recipes of ComfyUI clips). No-op step;
//!   older builds would drop the recipes when saving.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod atomic;
pub mod autosave;

use filmcraft_project::{Project, ProjectView};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use atomic::atomic_write;

/// The `format` tag written in every envelope.
pub const FORMAT_ID: &str = "filmcraft.project";

/// A migration upgrades a whole document from schema `n` to `n + 1`.
pub type Migration = fn(Value) -> Result<Value, String>;

/// `MIGRATIONS[i]` upgrades schema `i + 1` to `i + 2`. Append one function per schema bump; never
/// edit a shipped one.
pub const MIGRATIONS: &[Migration] =
    &[v1_to_v2, v2_to_v3, v3_to_v4, v4_to_v5, v5_to_v6, v6_to_v7, v7_to_v8, v8_to_v9, v9_to_v10, v10_to_v11, v11_to_v12, v12_to_v13, v13_to_v14];

/// The schema version this build writes (and the newest it reads).
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32 + 1;

/// Oldest schema this build can still upgrade.
pub const OLDEST_SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("not a FilmCraft project: {0}")]
    NotAProject(String),
    #[error(
        "this project was saved by a newer version of FilmCraft (project schema v{found}); this build reads up to v{supported}. Update FilmCraft to open it."
    )]
    TooNew { found: u32, supported: u32 },
    #[error("project schema v{found} is too old for this build (oldest supported: v{oldest})")]
    TooOld { found: u32, oldest: u32 },
    #[error("upgrading project from schema v{from} to v{}: {msg}", from + 1)]
    Migration { from: u32, msg: String },
    #[error("project file is damaged: {0}")]
    Corrupt(String),
}

/// A project read from disk.
#[derive(Debug)]
pub struct Loaded {
    pub project: Project,
    /// Schema version found in the file (before migration).
    pub schema_version: u32,
    /// The `generator` string of the writer, if recorded.
    pub generator: Option<String>,
    /// What was open when the file was saved (sequence tabs and how each was shown), when the
    /// file records it and it can be read.
    pub view: Option<ProjectView>,
}

impl Loaded {
    /// True when the file was written in an older schema and upgraded on load.
    pub fn migrated(&self) -> bool {
        self.schema_version < SCHEMA_VERSION
    }
}

/// Writer identification stored in the envelope.
pub fn generator() -> String {
    format!("FilmCraft {}", env!("CARGO_PKG_VERSION"))
}

#[derive(Serialize)]
struct EnvelopeOut<'a> {
    format: &'static str,
    schema_version: u32,
    generator: String,
    project: &'a Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    view: Option<&'a ProjectView>,
}

#[derive(Deserialize)]
struct EnvelopeIn {
    #[serde(default)]
    generator: Option<String>,
    project: Project,
    /// Kept as JSON until the project itself has loaded: a view that cannot be read is dropped,
    /// it never stops a project from opening.
    #[serde(default)]
    view: Option<Value>,
}

impl EnvelopeIn {
    fn loaded(self, schema_version: u32) -> Loaded {
        let view = self.view.and_then(|v| serde_json::from_value(v).ok());
        Loaded { project: self.project, schema_version, generator: self.generator, view }
    }
}

/// Just enough of a document to know which schema it is.
#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    schema_version: Option<u32>,
    /// Bare (pre-envelope) project: its `version` field.
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    items: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    root: Option<serde::de::IgnoredAny>,
}

/// Serialize a project in the current schema. `pretty` = indented (human-diffable; used for the
/// project file itself), compact otherwise (recovery snapshots).
pub fn encode(project: &Project, pretty: bool) -> Vec<u8> {
    encode_with_view(project, None, pretty)
}

/// [`encode`], with what is open in the editor (`view`) stored beside the project. The view is
/// outside the schema's promises: builds that do not know it ignore it, and it is dropped rather
/// than migrated when it cannot be read.
pub fn encode_with_view(project: &Project, view: Option<&ProjectView>, pretty: bool) -> Vec<u8> {
    let env = EnvelopeOut { format: FORMAT_ID, schema_version: SCHEMA_VERSION, generator: generator(), project, view };
    let r = if pretty { serde_json::to_vec_pretty(&env) } else { serde_json::to_vec(&env) };
    // Serializing plain data (string keys, finite numbers or null) cannot fail.
    r.unwrap_or_default()
}

/// Detect the schema version of a parsed document.
pub fn detect_version(doc: &Value) -> Result<u32, FormatError> {
    let probe: Probe = serde_json::from_value(doc.clone()).map_err(|e| FormatError::NotAProject(e.to_string()))?;
    probe_version(&probe)
}

fn probe_version(p: &Probe) -> Result<u32, FormatError> {
    if let Some(v) = p.schema_version {
        if let Some(f) = &p.format
            && f != FORMAT_ID
        {
            return Err(FormatError::NotAProject(format!("format is `{f}`, expected `{FORMAT_ID}`")));
        }
        return Ok(v);
    }
    // Before the envelope (v1) a file was the bare Project object with `version`.
    // Its `version` was 1, or 2 on builds that briefly bumped it when caption tracks were added
    // (same shape; the new field loads with its default). Anything higher is from the future.
    if p.items.is_some() && p.root.is_some() {
        return Ok(match p.version.unwrap_or(1) {
            0..=2 => 1,
            v => v,
        });
    }
    Err(FormatError::NotAProject("no `schema_version` and not a legacy project".into()))
}

/// Read a project file (any supported schema).
pub fn decode(bytes: &[u8]) -> Result<Loaded, FormatError> {
    let probe: Probe = serde_json::from_slice(bytes)
        .map_err(|e| if e.is_syntax() || e.is_eof() { FormatError::Corrupt(e.to_string()) } else { FormatError::NotAProject(e.to_string()) })?;
    let found = probe_version(&probe)?;
    check_supported(found)?;
    if found == SCHEMA_VERSION {
        // Fast path: deserialize straight into the model.
        let env: EnvelopeIn = serde_json::from_slice(bytes).map_err(|e| FormatError::Corrupt(e.to_string()))?;
        return validate_loaded(env.loaded(found));
    }
    let doc: Value = serde_json::from_slice(bytes).map_err(|e| FormatError::Corrupt(e.to_string()))?;
    let doc = migrate_with(MIGRATIONS, doc, found)?;
    let env: EnvelopeIn = serde_json::from_value(doc).map_err(|e| FormatError::Corrupt(format!("after upgrading from schema v{found}: {e}")))?;
    validate_loaded(env.loaded(found))
}

/// Refuse only values that would overflow or exhaust memory later (`Sequence::check_bounds`);
/// structural rules such as overlaps are left to the editor, so projects that open today still open.
fn validate_loaded(mut loaded: Loaded) -> Result<Loaded, FormatError> {
    // Out-of-range audio stream data is repaired, not refused: failing the load would lose the
    // whole project over one bad index. A media item keeps its first `MAX_AUDIO_STREAMS` streams; a
    // clip pointing past that plays stream 0.
    let max = filmcraft_media::MAX_AUDIO_STREAMS;
    for item in loaded.project.items.values_mut() {
        let name = item.name.clone();
        match &mut item.kind {
            filmcraft_project::ItemKind::Media(m) if m.info.audio_streams.len() > max => {
                log::warn!("media `{name}`: {} audio streams, keeping the first {max}", m.info.audio_streams.len());
                m.info.audio_streams.truncate(max);
            }
            filmcraft_project::ItemKind::Sequence(sequence) => {
                for clip in sequence.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
                    if clip.audio_stream >= max {
                        log::warn!("sequence `{name}`: clip `{}` uses audio stream {}, reset to 0", clip.name, clip.audio_stream);
                        clip.audio_stream = 0;
                    }
                }
            }
            _ => {}
        }
    }
    for item in loaded.project.sequences() {
        if let filmcraft_project::ItemKind::Sequence(sequence) = &item.kind {
            sequence.check_bounds().map_err(|e| FormatError::Corrupt(format!("sequence `{}`: {e}", item.name)))?;
        }
    }
    Ok(loaded)
}

fn check_supported(found: u32) -> Result<(), FormatError> {
    if found > SCHEMA_VERSION {
        return Err(FormatError::TooNew { found, supported: SCHEMA_VERSION });
    }
    if found < OLDEST_SCHEMA {
        return Err(FormatError::TooOld { found, oldest: OLDEST_SCHEMA });
    }
    Ok(())
}

/// Run `table` from schema `from` up to `table.len() + 1`, stamping `schema_version` after each step.
pub fn migrate_with(table: &[Migration], mut doc: Value, from: u32) -> Result<Value, FormatError> {
    let target = table.len() as u32 + 1;
    if from > target {
        return Err(FormatError::TooNew { found: from, supported: target });
    }
    if from == 0 {
        return Err(FormatError::TooOld { found: 0, oldest: 1 });
    }
    for v in from..target {
        let step = table[(v - 1) as usize];
        doc = step(doc).map_err(|msg| FormatError::Migration { from: v, msg })?;
        match doc.as_object_mut() {
            Some(o) => {
                o.insert("schema_version".into(), Value::from(v + 1));
            }
            None => return Err(FormatError::Migration { from: v, msg: "migration produced a non-object document".into() }),
        }
    }
    Ok(doc)
}

/// v1 → v2: wrap the bare project in the envelope and drop the per-project `version` field.
fn v1_to_v2(doc: Value) -> Result<Value, String> {
    let Value::Object(mut project) = doc else { return Err("expected a JSON object".into()) };
    project.remove("version");
    Ok(serde_json::json!({
        "format": FORMAT_ID,
        "schema_version": 2,
        "generator": "FilmCraft (schema v1)",
        "project": Value::Object(project),
    }))
}

/// v2 → v3: graphic clips added; existing data needs no change.
fn v2_to_v3(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v3 → v4: Essential Sound fields added; existing data needs no change.
fn v3_to_v4(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v4 → v5: colour-management fields added; existing data needs no change.
fn v4_to_v5(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v5 → v6: media identity and ingest settings added; existing data needs no change.
fn v5_to_v6(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v6 → v7: effect masks (Bézier mask paths as keyframable parameters); existing data needs no change.
fn v6_to_v7(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v7 → v8: multi-camera source sequences / clips and merged clips added; existing data needs no
/// change.
fn v7_to_v8(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v8 → v9: per-media-item transcripts (`project.transcripts`) added; existing data needs no change.
fn v8_to_v9(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v9 → v10: clip time interpolation, Hold Filters, Field Options and audio source channels; the
/// Modify ▸ Audio Channels map; subclips' Restrict Trims flag. Existing data needs no change.
fn v9_to_v10(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v10 → v11: search bins (`project.search_bins`), Flash Cue markers, Project Settings safe areas,
/// capture format and scratch disks. Existing data needs no change.
fn v10_to_v11(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v11 → v12: graphics templates, rolls / crawls, responsive design and per-character text styles
/// on graphic clips, and source graphics. Existing data needs no change.
fn v11_to_v12(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v12 → v13: container audio streams and per-clip stream selection. `MediaInfo` reads the old
/// `audio` field as stream zero; `TrackItem::audio_stream` defaults to zero.
fn v12_to_v13(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v13 → v14: generated media items (the recipes of ComfyUI clips). Existing data needs no
/// change.
fn v13_to_v14(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_matches_table() {
        assert_eq!(SCHEMA_VERSION, 14);
    }

    #[test]
    fn damaged_sequence_settings_are_rejected_before_rendering() {
        let mut p = Project::new("Invalid sequence");
        let id = p.new_sequence("Bad", filmcraft_project::SequenceSettings::default(), 1, 1, None);
        for (width, height, sample_rate) in [(0, 90, 48000), (160, 0, 48000), (160, 90, 0), (65536, 90, 48000), (32768, 16384, 48000)] {
            let settings = &mut p.sequence_mut(id).unwrap().settings;
            (settings.width, settings.height, settings.sample_rate) = (width, height, sample_rate);
            assert!(matches!(decode(&encode(&p, false)), Err(FormatError::Corrupt(_))));
        }
    }

    /// #210 review: load-time validation refuses only overflowing values. A project that breaks a
    /// structural rule an older build may have written (overlapping or zero-length clips) still
    /// opens, as it did before; and 16K sequences are not "too large".
    #[test]
    fn structural_rule_breaks_still_open() {
        use filmcraft_project::{ItemKind, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, Tick, TimeRange};
        let mut p = Project::new("Legacy");
        let rate = FrameRate::FPS_24;
        let layer = p.add_item(
            "Layer",
            filmcraft_project::Label::Iris,
            ItemKind::AdjustmentLayer { width: 1920, height: 1080, rate, duration: rate.tick_of(48) },
            None,
        );
        let settings = SequenceSettings { width: 15360, height: 8640, ..Default::default() };
        let seq = p.new_sequence("Sequence 01", settings, 1, 1, None);
        let range = TimeRange::new(Tick::ZERO, rate.tick_of(48));
        let a = p.make_track_item(layer, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
        let b = p.make_track_item(layer, TrackKind::Video, rate.tick_of(24), range, rate).unwrap();
        let mut c = p.make_track_item(layer, TrackKind::Video, rate.tick_of(200), range, rate).unwrap();
        c.duration = Tick::ZERO;
        p.sequence_mut(seq).unwrap().video_tracks[0].items = vec![a, b, c];
        assert!(p.sequence(seq).unwrap().check().is_err(), "the sequence breaks the editor's invariants");
        let loaded = decode(&encode(&p, false)).unwrap_or_else(|e| panic!("a project that opened before is refused: {e}"));
        assert_eq!(loaded.project.sequence(seq).unwrap().video_tracks[0].items.len(), 3);
    }

    #[test]
    fn roundtrip_current() {
        let p = Project::new("Round Trip");
        for pretty in [true, false] {
            let b = encode(&p, pretty);
            let l = decode(&b).unwrap();
            assert_eq!(l.project, p);
            assert_eq!(l.schema_version, SCHEMA_VERSION);
            assert!(!l.migrated());
            assert_eq!(l.generator.as_deref(), Some(generator().as_str()));
        }
    }

    /// What is open is stored beside the project. A file without it, or with one that cannot be
    /// read, opens all the same.
    #[test]
    fn the_view_is_kept_beside_the_project_and_never_stops_a_file_from_opening() {
        use filmcraft_project::{ItemId, SequenceSettings, SequenceView};
        let mut p = Project::new("Tabs");
        let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 1, 1, None);
        let view = ProjectView {
            open_sequences: vec![seq],
            active_sequence: Some(seq),
            sequences: [(seq, SequenceView { pps: 80.0, scroll: 1.5, v_scroll: 0.0, a_scroll: 4.0, video_track_h: 60.0, audio_track_h: 56.0 })].into(),
            playheads: [(seq, filmcraft_time::Tick(3 * filmcraft_time::TICKS_PER_SECOND))].into(),
        };
        for pretty in [true, false] {
            let l = decode(&encode_with_view(&p, Some(&view), pretty)).unwrap();
            assert_eq!((l.view.as_ref(), &l.project), (Some(&view), &p));
            assert_eq!(decode(&encode(&p, pretty)).unwrap().view, None);
        }
        // the project is the same bytes with or without a view beside it
        let mut doc: Value = serde_json::from_slice(&encode_with_view(&p, Some(&view), false)).unwrap();
        assert_eq!(doc["project"], serde_json::from_slice::<Value>(&encode(&p, false)).unwrap()["project"]);
        // a view that is not one: dropped, the project loads
        for junk in [serde_json::json!(7), serde_json::json!({"open_sequences": {"a": 1}}), serde_json::json!({"sequences": [1]}), Value::Null] {
            doc["view"] = junk;
            let l = decode(&serde_json::to_vec(&doc).unwrap()).unwrap();
            assert_eq!((l.view, &l.project), (None, &p));
        }
        // unknown ids are not this crate's business (the editor checks them against the project)
        doc["view"] = serde_json::json!({"open_sequences": [424242]});
        assert_eq!(decode(&serde_json::to_vec(&doc).unwrap()).unwrap().view.unwrap().open_sequences, [ItemId(424242)]);
    }

    /// A clip with unresolved "auto" points (NaN) is written with `null` coordinates. Such a file
    /// used to be refused as damaged; it opens, and the points are still "auto".
    #[test]
    fn auto_points_written_as_null_open_again() {
        use filmcraft_project::{ItemKind, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, Tick, TimeRange};
        let mut p = Project::new("Imported");
        let layer = p.add_item(
            "Layer",
            filmcraft_project::Label::Iris,
            ItemKind::AdjustmentLayer { width: 3840, height: 2160, rate: FrameRate::FPS_24, duration: FrameRate::FPS_24.tick_of(48) },
            None,
        );
        let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 1, 1, None);
        let ti = p.make_track_item(layer, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(48)), FrameRate::FPS_24).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        for pretty in [true, false] {
            let bytes = encode(&p, pretty);
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert!(text.replace([' ', '\n'], "").contains(r#"{"Vec2":{"x":null,"y":null}}"#), "the file holds NaN points as null");
            let l = decode(&bytes).unwrap_or_else(|e| panic!("a file with `null` points is refused: {e}"));
            let motion = l.project.sequence(seq).unwrap().video_tracks[0].items[0].effect("motion").unwrap().clone();
            let anchor = motion.params["anchor"].value.as_vec2().unwrap();
            assert!(anchor.x.is_nan() && anchor.y.is_nan(), "still auto");
            assert_eq!(motion.params["scale"].value.as_f64(), Some(100.0));
            assert_eq!(encode(&l.project, pretty), bytes, "and the file is written back the same");
            // a `null` where no "auto" exists is still a damaged file
            let broken = text.replacen("3840", "null", 1);
            assert!(matches!(decode(broken.as_bytes()), Err(FormatError::Corrupt(_))));
        }
    }

    #[test]
    fn envelope_has_explicit_schema_version() {
        let v: Value = serde_json::from_slice(&encode(&Project::new("x"), true)).unwrap();
        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        assert_eq!(v["format"], FORMAT_ID);
        assert!(v["project"]["items"].is_object());
        assert!(v["project"].get("version").is_none());
    }

    #[test]
    fn refuses_newer() {
        let mut v: Value = serde_json::from_slice(&encode(&Project::new("x"), false)).unwrap();
        v["schema_version"] = Value::from(SCHEMA_VERSION + 1);
        let e = decode(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { .. }));
        assert!(e.to_string().contains("newer version of FilmCraft"), "{e}");
    }

    #[test]
    fn rejects_foreign_and_damaged() {
        assert!(matches!(decode(br#"{"hello": 1}"#), Err(FormatError::NotAProject(_))));
        assert!(matches!(decode(br#"{"format":"other","schema_version":1}"#), Err(FormatError::NotAProject(_))));
        let b = encode(&Project::new("x"), false);
        assert!(matches!(decode(&b[..b.len() / 2]), Err(FormatError::Corrupt(_))));
    }

    #[test]
    fn chain_runs_each_step_in_order() {
        fn a(mut d: Value) -> Result<Value, String> {
            d["trail"] = Value::from(format!("{}a", d["trail"].as_str().unwrap_or("")));
            Ok(d)
        }
        fn b(mut d: Value) -> Result<Value, String> {
            d["trail"] = Value::from(format!("{}b", d["trail"].as_str().unwrap_or("")));
            Ok(d)
        }
        fn fail(_: Value) -> Result<Value, String> {
            Err("boom".into())
        }
        let table: &[Migration] = &[a, b, a];
        let out = migrate_with(table, serde_json::json!({}), 1).unwrap();
        assert_eq!(out["trail"], "aba");
        assert_eq!(out["schema_version"], 4);
        let out = migrate_with(table, serde_json::json!({}), 3).unwrap();
        assert_eq!(out["trail"], "a");
        let out = migrate_with(table, serde_json::json!({"x": 1}), 4).unwrap();
        assert_eq!(out, serde_json::json!({"x": 1}));
        assert!(matches!(migrate_with(table, serde_json::json!({}), 5), Err(FormatError::TooNew { found: 5, supported: 4 })));
        let e = migrate_with(&[a, fail], serde_json::json!({}), 1).unwrap_err();
        assert!(matches!(e, FormatError::Migration { from: 2, .. }), "{e}");
    }
}

//! Combined-import regressions: use generated PCM and synthetic catalogs, never private fixtures.
use super::*;
use filmcraft_project::{BinEntry, ItemId};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Input {
    report: Vec<u8>,
    media: BTreeMap<String, Vec<u8>>,
    reads: AtomicUsize,
}
impl Services for Input {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        if path == "catalog.json" {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.report.clone())
        } else {
            self.media.get(path).cloned().ok_or_else(|| std::io::Error::other("unexpected read"))
        }
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        if path == "catalog.json" {
            Ok(self.report.len() as u64)
        } else {
            self.media.get(path).map(|v| v.len() as u64).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
        }
    }
    fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("import must not write source files"))
    }
    fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
        Some(self.read_file(path).map(|bytes| Arc::new(filmcraft_media::reader::MemReader(bytes.into())) as filmcraft_media::SharedReader))
    }
}
fn fixture() -> (Value, BTreeMap<String, Vec<u8>>) {
    let mut v = crate::avid_catalog_tests::fixture();
    let mut media = BTreeMap::new();
    let mut tracks = Vec::new();
    for i in 1..=2 {
        let ids = filmcraft_mxf::PackageIds::from_seed(&format!("combined-{i}"), "test");
        let raw: String = ids.file.0.iter().map(|b| format!("{b:02x}")).collect();
        let bytes = filmcraft_mxf::write_opatom_pcm(
            &filmcraft_mxf::OpAtomPcm { sample_rate: 48000, bits: 16, channels: 1, edit_rate: None, ids, timecode: None },
            &vec![if i == 1 { 4000 } else { -8000 }; 4800],
        )
        .unwrap();
        let filename = format!("A{i}.mxf");
        media.insert(format!("/combined/{filename}"), bytes);
        tracks.push(json!({"index":i,"status":"present","component":{"class":"SCLP","media_kind":2,"source_id":{"raw":raw,"byte_order":"big"},"source_track":i,"start":0,"length":4800,"rate":{"numerator":48000,"denominator":1}},"references":[{
            "status":"present","source_id":{"raw":raw,"byte_order":"big"},"source_track":i,"source_start":0,"length":4800,
            "rate":{"numerator":48000,"denominator":1},"locations":[{"folder":"/combined","filename":filename}]}]}));
    }
    let master = &mut v["bins"][0]["catalog"]["clips"][0];
    master["length"] = json!(4800);
    master["rate"] = json!({"numerator":48000,"denominator":1});
    master["workflow_evidence"] = json!("material_identity_in_managed_pmr");
    master["availability"] = json!("present");
    master["tracks"] = json!(tracks);
    let rate = json!({"numerator":24,"denominator":1});
    let clip = json!({"class":"SCLP","media_kind":2,"length":1,"start":1,"rate":rate,"source_id":master["id"],"source_track":2});
    let sequence = json!({"kind":"CompositionMob","name":"Trimmed","user_placed":true,"live":true,"id":{"raw":"02".repeat(32),"byte_order":"little"},
        "length":2,"rate":rate,"attributes":{"SEQUERNCE_FORMAT_STRING":"HD 1080p/24"},"tracks":[
        {"index":1,"component":{"class":"TCCP","media_kind":3,"length":2,"rate":rate,"start":86400,"fps":24,"flags":0}},
        {"index":2,"component":{"class":"SEQU","media_kind":2,"length":2,"rate":rate,"components":[
            {"class":"FILL","media_kind":2,"length":1,"rate":rate},clip]}}]});
    v["bins"][0]["mobs"] = json!([master.clone(), sequence]);
    (v, media)
}
fn session(v: &Value, media: BTreeMap<String, Vec<u8>>) -> (Session, Arc<Input>) {
    let input = Arc::new(Input { report: serde_json::to_vec(v).unwrap(), media, reads: AtomicUsize::new(0) });
    (Session::new(input.clone()), input)
}
fn params(mode: &str) -> Value {
    json!({"path":"catalog.json","mode":mode})
}

#[test]
fn an_imported_subclip_is_not_offered_to_media_binding() {
    // A subclip has no media of its own: it binds through the master it restricts. Offering one
    // to binding fails the item, and in strict mode would fail the whole import.
    let (mut v, media) = fixture();
    let master = v["bins"][0]["catalog"]["clips"][0].clone();
    let sub = json!({"kind":"CompositionMob","name":"Trimmed.sub.01","live":true,"user_placed":true,
        "id":{"raw":"05".repeat(32),"byte_order":"little"},"length":4800,"rate":master["rate"],"usage_code":2,
        "attributes":{"_START":1200,"_END":2400},
        "tracks":[{"index":1,"component":{"class":"SCLP","media_kind":2,"length":4800,"rate":master["rate"],
            "source_id":master["id"],"source_track":1,"start":0}}]});
    v["bins"][0]["mobs"].as_array_mut().unwrap().push(sub);
    let (s, _) = session(&v, media);
    let prepared = crate::avid_catalog::prepare_document(
        &s.project,
        &*s.services,
        &serde_json::to_vec(&v).unwrap(),
        crate::avid_catalog::ImportOptions { mode: filmcraft_interchange::avid_catalog::TransferMode::Sequences, allow_missing: true, best_effort: false },
        None,
        &|| Ok(()),
    )
    .expect("a subclip must not fail a strict import");
    assert_eq!(prepared.result["bins"][0]["subclips"], json!(1));
    assert!(!prepared.result["errors"].to_string().contains("not a media source"), "{}", prepared.result["errors"]);
    // It carries the recorded range, and its master still bound.
    let (parent, range) = prepared
        .project
        .items
        .values()
        .find_map(|i| match &i.kind {
            filmcraft_project::ItemKind::Subclip { parent, range, .. } => Some((*parent, *range)),
            _ => None,
        })
        .expect("a subclip item");
    let master_item = prepared.project.item(parent).unwrap().as_media().unwrap();
    assert!(range.duration > Tick::ZERO && range.end() <= master_item.info.duration);
    assert!(!master_item.offline, "the master it restricts still bound its media");
}

#[test]
fn an_unbindable_referenced_source_never_fails_a_strict_import() {
    // A cut bin's referenced masters are not its contents. One whose media cannot be bound costs
    // its own sequence clip a placeholder; it must not fail an import the bin's own contents
    // satisfy, including in strict mode, where nothing previously reached binding at all.
    let (mut v, media) = fixture();
    let mut orphan = v["bins"][0]["catalog"]["clips"][0].clone();
    orphan["name"] = json!("Referenced without descriptors");
    orphan["id"]["raw"] = json!("07".repeat(32));
    orphan["user_placed"] = json!(false);
    orphan.as_object_mut().unwrap().remove("workflow_evidence");
    v["bins"][0]["catalog"]["clips"].as_array_mut().unwrap().push(orphan.clone());
    v["bins"][0]["catalog"]["summary"]["master_count"] = json!(2);
    v["bins"][0]["mobs"].as_array_mut().unwrap().push(orphan);
    let (s, _) = session(&v, media);
    let prepared = crate::avid_catalog::prepare_document(
        &s.project,
        &*s.services,
        &serde_json::to_vec(&v).unwrap(),
        crate::avid_catalog::ImportOptions { mode: filmcraft_interchange::avid_catalog::TransferMode::Sequences, allow_missing: true, best_effort: false },
        None,
        &|| Ok(()),
    )
    .expect("an unbindable referenced source must not fail the import");
    // The bin's own content still bound and its sequence still converted.
    assert_eq!(prepared.result["bins"][0]["clips"], json!(1));
    assert_eq!(prepared.result["bins"][0]["referenced"], json!(1));
    assert_eq!(prepared.result["sequences"].as_array().unwrap().len(), 1);
    // The source that could not be bound is reported by name and stays offline.
    assert!(prepared.result["errors"].to_string().contains("Referenced without descriptors"), "{}", prepared.result["errors"]);
    assert_eq!(prepared.result["mediaBinding"], "offline_components");
}

#[test]
fn subclip_routing_and_hidden_dependencies_survive_import_undo_and_reopen() {
    for placed in [true, false] {
        let (mut v, media) = fixture();
        let master = v["bins"][0]["catalog"]["clips"][0].clone();
        let sub = json!({"kind":"CompositionMob","name":"A2 only","live":true,"user_placed":placed,
            "id":{"raw":"05".repeat(32),"byte_order":"little"},"length":4800,"rate":master["rate"],"usage_code":2,
            "attributes":{"_START":0,"_END":4800},
            "tracks":[{"index":1,"component":{"class":"SCLP","media_kind":2,"length":4800,"rate":master["rate"],
                "source_id":master["id"],"source_track":2,"start":0}}]});
        let cut = &mut v["bins"][0]["mobs"][1]["tracks"][1]["component"]["components"][1];
        cut["source_id"] = sub["id"].clone();
        cut["source_track"] = json!(1);
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(sub);
        let (mut s, _) = session(&v, media);
        let before = s.project.clone();
        let out = s.execute("file.importAvidCatalog", params("sequences")).unwrap();
        assert_eq!(out["bins"][0]["subclips"], json!(usize::from(placed)));
        assert_eq!(out["bins"][0]["referenced_subclips"], json!(usize::from(!placed)));
        assert!(out["errors"].as_array().unwrap().is_empty(), "{out}");
        let seq = s.project.sequence(ItemId(out["sequences"][0]["item"].as_u64().unwrap())).unwrap();
        let clip = &seq.audio_tracks[1].items[0];
        assert_eq!(clip.source_channels, vec![1]);
        assert!(matches!(s.project.item(clip.item).unwrap().kind, filmcraft_project::ItemKind::Subclip { .. }));
        let source = s.media.full_res_source(&s.project, clip.item, &*s.services).unwrap();
        let audio = source.audio(2000, 8, 48000).unwrap();
        assert_eq!(audio.channels[usize::from(clip.source_channels[0])], vec![-8000.0 / 32768.0; 8]);
        let after = s.project.clone();
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
}

#[test]
fn avid_import_combined_preview_commit_undo_and_reopen() {
    let (v, media) = fixture();
    let (mut s, input) = session(&v, media);
    let before = s.project.clone();
    let state = serde_json::to_value(&s.state).unwrap();
    let revision = s.revision;
    let preview = s.execute("file.previewAvidCatalog", params("sequences")).unwrap();
    assert_eq!(s.project, before);
    assert_eq!(serde_json::to_value(&s.state).unwrap(), state);
    assert_eq!(s.revision, revision);
    assert!(s.history.undo.is_empty());
    assert_eq!(input.reads.load(Ordering::Relaxed), 1, "one snapshot per operation");
    assert_eq!(preview["bound"].as_array().unwrap().len(), 1);
    assert_eq!(preview["sequences"].as_array().unwrap().len(), 1);
    assert!(!preview["warnings"].to_string().contains("was not imported"));
    assert!(!preview["warnings"].to_string().contains("Metadata-only"));
    let out = s.execute("file.importAvidCatalog", params("sequences")).unwrap();
    assert_eq!(out, preview);
    assert_eq!(input.reads.load(Ordering::Relaxed), 2);
    assert_eq!(s.history.undo.len(), 1);
    let id = ItemId(out["sequences"][0]["item"].as_u64().unwrap());
    assert_eq!(s.state.active_sequence, Some(id));
    assert!(s.state.open_sequences.contains(&id));
    let seq = s.project.sequence(id).unwrap();
    assert_eq!(seq.duration(), Tick::from_units(2, 24));
    let clip = &seq.audio_tracks[1].items[0];
    assert_eq!((clip.start, clip.source_in), (Tick::from_units(1, 24), Tick::from_units(1, 24)));
    assert_eq!(clip.source_channels, vec![1]);
    let source = s.media.full_res_source(&s.project, clip.item, &*s.services).unwrap();
    assert_eq!(source.audio(2000, 8, 48000).unwrap().channels[1], vec![-8000.0 / 32768.0; 8]);
    let first_master = clip.item;
    let after = s.project.clone();
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
    let out = s.execute("file.importAvidCatalog", params("sequences")).unwrap();
    assert_eq!(out["bins"][0]["name"], "Sound Effects_2");
    let seq2 = s.project.sequence(ItemId(out["sequences"][0]["item"].as_u64().unwrap())).unwrap();
    assert_ne!(seq2.audio_tracks[1].items[0].item, first_master, "new sequence must use new bin's master");
}

#[test]
fn avid_import_combined_failures_leave_no_bins_media_or_history() {
    let (good, media) = fixture();
    for stage in ["second-bin-binding", "sequence"] {
        let mut v = good.clone();
        if stage == "sequence" {
            v["bins"][0]["mobs"][1]["tracks"][1]["component"]["components"][1]["class"] = json!("UNSUPPORTED");
        } else {
            let mut b = v["bins"][0].clone();
            b["summary"]["bin_uid"] = json!("second");
            b["catalog"]["clips"][0]["tracks"][1]["references"][0]["source_id"]["raw"] = json!("ff".repeat(32));
            b["catalog"]["clips"][0]["tracks"][1]["component"]["source_id"]["raw"] = json!("ff".repeat(32));
            b["mobs"][0] = b["catalog"]["clips"][0].clone();
            v["bins"].as_array_mut().unwrap().push(b);
        }
        let (mut s, _) = session(&v, media.clone());
        let before = s.project.clone();
        let state = serde_json::to_value(&s.state).unwrap();
        let revision = s.revision;
        for cmd in ["file.previewAvidCatalog", "file.importAvidCatalog"] {
            let error = s.execute(cmd, params("sequences")).unwrap_err().to_string();
            assert!(error.contains(if stage == "sequence" { "UNSUPPORTED" } else { "identity or single-track profile" }), "{stage}: {error}");
            assert_eq!(s.project, before);
            assert_eq!(serde_json::to_value(&s.state).unwrap(), state);
            assert_eq!(s.revision, revision);
            assert!(s.history.undo.is_empty());
            assert!(s.history.redo.is_empty());
        }
    }
    for mode in [json!(null), json!(true), json!(0), json!([]), json!("all"), json!({})] {
        let (mut s, _) = session(&good, media.clone());
        let before = s.project.clone();
        for cmd in ["file.previewAvidCatalog", "file.importAvidCatalog"] {
            assert!(s.execute(cmd, json!({"path":"catalog.json","mode":mode})).is_err());
            assert_eq!(s.project, before);
            assert!(s.history.undo.is_empty());
        }
    }
}

#[test]
fn avid_import_combined_modes_partial_channels_and_empty_bins() {
    let (mut v, media) = fixture();
    v["bins"][0]["catalog"]["clips"][0]["tracks"][0]["references"][0]["status"] = json!("absent");
    v["bins"][0]["mobs"][0] = v["bins"][0]["catalog"]["clips"][0].clone();
    let mut empty = v["bins"][0].clone();
    empty["summary"]["bin_uid"] = json!("empty");
    empty["mobs"] = json!([]);
    empty["catalog"]["clips"] = json!([]);
    empty["catalog"]["summary"]["master_count"] = json!(0);
    v["bins"].as_array_mut().unwrap().push(empty);
    for mode in ["metadata", "media", "sequences"] {
        let (mut s, _) = session(&v, media.clone());
        let out = s.execute("file.importAvidCatalog", params(mode)).unwrap();
        assert_eq!(out["bins"][1]["name"], "Sound Effects_2");
        assert_eq!(out["sequences"].as_array().unwrap().len(), usize::from(mode == "sequences"));
        let m = s.project.items.values().find_map(|i| i.as_media()).unwrap();
        if mode == "metadata" {
            assert!(m.offline);
            assert!(out["bound"].as_array().unwrap().is_empty());
        } else {
            let filmcraft_project::MediaRef::Components { channels, .. } = &m.media else { panic!("expected bound channels") };
            assert!(channels[0].path.is_none());
            assert!(channels[1].path.is_some());
            assert_eq!(out["bound"][0]["channels"], 2);
            assert!(!out["bound"][0]["warnings"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn avid_import_combined_batches_more_than_64_masters_in_one_edit() {
    let (mut v, media) = fixture();
    let master = v["bins"][0]["catalog"]["clips"][0].clone();
    let clips: Vec<_> = (1..=65)
        .map(|i| {
            let mut c = master.clone();
            c["id"]["raw"] = json!(format!("{i:064x}"));
            c
        })
        .collect();
    v["bins"][0]["catalog"]["clips"] = json!(clips);
    v["bins"][0]["catalog"]["summary"]["master_count"] = json!(65);
    v["bins"][0]["mobs"] = json!(clips);
    let (mut s, _) = session(&v, media);
    let out = s.execute("file.importAvidCatalog", params("sequences")).unwrap();
    assert_eq!(out["bound"].as_array().unwrap().len(), 65);
    assert_eq!(s.history.undo.len(), 1);
    assert_eq!(s.project.items.len(), 65);
    assert!(matches!(&s.project.root.children[0], BinEntry::Bin(b) if b.children.len() == 65));
}

#[test]
#[ignore = "private reports supplied through environment; reads existing media only"]
fn avid_import_combined_local_matches_staged_conversion() {
    for key in ["FILMCRAFT_AVID_PHASE9_REPORT", "FILMCRAFT_AVID_PHASE10_REPORT", "FILMCRAFT_AVID_PHASE11_REPORT", "FILMCRAFT_AVID_PHASE12_REPORT"] {
        let path = std::env::var(key).expect("provide local report");
        let mut combined = Session::default();
        let out = combined.execute("file.importAvidCatalog", json!({"path":path,"mode":"sequences"})).unwrap();
        assert_eq!(combined.history.undo.len(), 1);
        let mut staged = Session::default();
        let bins = staged.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
        let ids: Vec<_> = staged.project.items.keys().copied().collect();
        staged.execute("media.bindAvidMedia", json!({"items":ids})).unwrap();
        staged.execute("file.importAvidSequences", json!({"path":path,"bin":bins["bins"][0]["bin"]})).unwrap();
        assert_eq!(combined.project, staged.project, "{key}: must equal already-validated separate commands");
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&combined.project, false)).unwrap().project, *combined.project);
        eprintln!(
            "{key}: {} masters, {} sequence(s), identical native project, one undo step",
            out["bound"].as_array().unwrap().len(),
            out["sequences"].as_array().unwrap().len()
        );
    }
}

#[test]
fn avid_audit_binding_rejects_inconsistent_reference_evidence() {
    let (good, media) = fixture();
    for (field, value) in [
        ("source_id", json!({"raw":"ff".repeat(32),"byte_order":"big"})),
        ("source_track", json!(2)),
        ("start", json!(1)),
        ("length", json!(4799)),
        ("rate", json!({"numerator":96000,"denominator":1})),
    ] {
        let mut v = good.clone();
        v["bins"][0]["catalog"]["clips"][0]["tracks"][0]["component"][field] = value;
        v["bins"][0]["mobs"][0] = v["bins"][0]["catalog"]["clips"][0].clone();
        let (mut s, _) = session(&v, media.clone());
        let before = s.project.clone();
        assert!(s.execute("file.importAvidCatalog", params("media")).is_err(), "accepted inconsistent {field}");
        assert_eq!(s.project, before);
        assert!(s.history.undo.is_empty());
    }
}
#[test]
fn avid_audit_sequence_rejects_incomplete_composition_and_timecode() {
    let (good, media) = fixture();
    for target in ["composition", "timecode", "track"] {
        let mut v = good.clone();
        let c = &mut v["bins"][0]["mobs"][1];
        match target {
            "composition" => c["truncated"] = json!(true),
            "timecode" => c["tracks"][0]["component"]["truncated"] = json!(true),
            _ => c["tracks"][0]["truncated"] = json!(true),
        }
        let (mut s, _) = session(&v, media.clone());
        let before = s.project.clone();
        assert!(s.execute("file.importAvidCatalog", params("sequences")).is_err(), "accepted incomplete {target}");
        assert_eq!(s.project, before);
        assert!(s.history.undo.is_empty());
    }
}

#[test]
fn avid_audit_catalog_rejects_disagreement_with_decoded_master() {
    let (good, media) = fixture();
    for field in ["name", "user", "length", "attributes", "tracks"] {
        let mut v = good.clone();
        let c = &mut v["bins"][0]["catalog"]["clips"][0];
        match field {
            "name" => c["name"] = json!("unverified rename"),
            "user" => c["user"]["Scene"] = json!("unverified scene"),
            "length" => c["length"] = json!(4700),
            "attributes" => c["attributes"] = json!({"_CHANNEL_GROUP_LIST":"ST:A1A2,"}),
            _ => c["tracks"][0]["component"]["source_id"] = c["tracks"][1]["component"]["source_id"].clone(),
        }
        let (mut s, _) = session(&v, media.clone());
        let before = s.project.clone();
        assert!(s.execute("file.importAvidCatalog", params("metadata")).is_err(), "accepted conflicting decoded {field}");
        assert_eq!(s.project, before);
    }
}

fn with_audio_descriptors(mut v: Value) -> Value {
    let tracks = v["bins"][0]["catalog"]["clips"][0]["tracks"].as_array().unwrap().clone();
    for t in tracks {
        let id = t["references"][0]["source_id"].clone();
        let d = json!({"class":"PCMA","mob_kind":1,"is_omfi":0,"channels":1,"coding_format":1,
            "sample_rate":48000.0,"edit_rate":48000.0,"quantization_bits":16,"block_align":2,"average_bps":96000,"length":4800,
            "locator":{"class":"MSML","properties":{"mob_id":{"id_raw":id["raw"],"byte_order":id["byte_order"]}}}});
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(json!({"kind":"SourceMob","live":true,"id":id,"descriptor":d}));
    }
    v
}
#[test]
fn offline_audio_import_retains_slots_timing_and_relinks_atomically() {
    let (v, media) = fixture();
    let v = with_audio_descriptors(v);
    let (mut s, _) = session(&v, BTreeMap::new());
    let before = s.project.clone();
    let preview = s.execute("file.previewAvidCatalog", params("sequences")).unwrap();
    assert_eq!(s.project, before);
    assert_eq!(preview["mediaBinding"], "offline_components");
    assert_eq!(preview["bound"][0]["offlineComponents"], 2);
    let imported = s.execute("file.importAvidCatalog", params("sequences")).unwrap();
    assert_eq!(imported, preview);
    let offline = s.project.clone();
    let item = ItemId(imported["bound"][0]["item"].as_u64().unwrap());
    assert_eq!(crate::relink::scan(&s.project, &*s.services).len(), 2);
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&offline, false)).unwrap().project, *offline);
    let sequence = ItemId(imported["sequences"][0]["item"].as_u64().unwrap());
    let timeline = s.project.sequence(sequence).unwrap().clone();
    s.services = session(&v, media).1;
    s.execute(
        "media.relinkComponents",
        json!({"bindings":[
            {"item":item,"component":{"kind":"audio","channel":0},"path":"/combined/A1.mxf"},
            {"item":item,"component":{"kind":"audio","channel":1},"path":"/combined/A2.mxf"}
        ]}),
    )
    .unwrap();
    assert_eq!(s.project.sequence(sequence).unwrap(), &timeline);
    assert!(crate::relink::scan(&s.project, &*s.services).is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, offline);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
}
#[test]
fn offline_import_requires_validated_descriptors_and_explicit_policy() {
    let (v, _) = fixture();
    let good = with_audio_descriptors(v);
    for case in ["strict", "invalid_policy", "no_descriptor", "duplicate", "inactive", "locator", "short", "rate", "duration"] {
        let mut v = good.clone();
        let mut p = params("sequences");
        match case {
            "strict" => p["missingMedia"] = json!("error"),
            "invalid_policy" => p["missingMedia"] = json!(true),
            "no_descriptor" => {
                v["bins"][0]["mobs"].as_array_mut().unwrap().truncate(2);
            }
            "duplicate" => {
                let duplicate = v["bins"][0]["mobs"][2].clone();
                v["bins"][0]["mobs"].as_array_mut().unwrap().push(duplicate);
            }
            "inactive" => v["bins"][0]["mobs"][2]["live"] = json!(false),
            "locator" => v["bins"][0]["mobs"][2]["descriptor"]["locator"]["properties"]["mob_id"]["id_raw"] = json!("aa".repeat(32)),
            "short" => v["bins"][0]["mobs"][2]["id"]["raw"] = json!("aa".repeat(12)),
            "rate" => v["bins"][0]["mobs"][2]["descriptor"]["sample_rate"] = json!(44100.0),
            _ => v["bins"][0]["mobs"][2]["descriptor"]["length"] = json!(1),
        }
        let (mut s, _) = session(&v, BTreeMap::new());
        let before = s.project.clone();
        assert!(s.execute("file.importAvidCatalog", p).is_err(), "accepted {case}");
        assert_eq!(s.project, before);
        assert!(s.history.undo.is_empty());
    }
    // Inactive history does not make the unique live descriptor ambiguous.
    let mut v = good;
    let mut old = v["bins"][0]["mobs"][2].clone();
    old["live"] = json!(false);
    v["bins"][0]["mobs"].as_array_mut().unwrap().push(old);
    session(&v, BTreeMap::new()).0.execute("file.importAvidCatalog", params("sequences")).unwrap();
}
#[test]
fn offline_fallback_does_not_hide_existing_corrupt_media() {
    let (v, mut media) = fixture();
    let v = with_audio_descriptors(v);
    media.insert("/combined/A1.mxf".into(), vec![0; 128]);
    let (mut s, _) = session(&v, media);
    let before = s.project.clone();
    assert!(s.execute("file.importAvidCatalog", params("sequences")).is_err());
    assert_eq!(s.project, before);
}

#[test]
#[ignore = "private Phase 12 report supplied via environment; never modifies originals"]
fn offline_phase12_import_and_relink_preserve_exact_timeline() {
    struct Offline;
    impl Services for Offline {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            FsServices.read_file(path)
        }
        fn file_size(&self, path: &str) -> std::io::Result<u64> {
            if path.to_ascii_lowercase().ends_with(".mxf") {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "test volume unavailable"))
            } else {
                FsServices.file_size(path)
            }
        }
        fn reader(&self, _: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
            Some(Err(std::io::Error::new(std::io::ErrorKind::NotFound, "test volume unavailable")))
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("read only"))
        }
    }
    let path = std::env::var("FILMCRAFT_AVID_PHASE12_REPORT").unwrap();
    let p = json!({"path":path,"mode":"sequences"});
    let mut online = Session::default();
    let online_out = online.execute("file.importAvidCatalog", p.clone()).unwrap();
    let mut s = Session::new(Arc::new(Offline));
    let out = s.execute("file.importAvidCatalog", p).unwrap();
    assert_eq!(out["mediaBinding"], "offline_components");
    assert_eq!(crate::relink::scan(&s.project, &*s.services).len(), 6);
    let sequence = ItemId(out["sequences"][0]["item"].as_u64().unwrap());
    let online_sequence = ItemId(online_out["sequences"][0]["item"].as_u64().unwrap());
    assert_eq!(s.project.sequence(sequence), online.project.sequence(online_sequence));
    assert_eq!(s.project.sequence(sequence).unwrap().duration(), filmcraft_time::Tick(filmcraft_time::FrameRate::FPS_24.frame_duration().0 * 152));
    let offline = s.project.clone();
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&offline, false)).unwrap().project, *offline);
    // Isolated user-test project; originals and their media are never moved.
    let mut disposable = (*offline).clone();
    for item in disposable.items.values_mut() {
        if let Some(m) = item.as_media_mut()
            && let filmcraft_project::MediaRef::Components { channels, picture } = &mut m.media
        {
            for path in channels.iter_mut().map(|c| &mut c.path).chain(picture.iter_mut().map(|c| &mut c.path)) {
                if let Some(old) = path.take() {
                    *path = Some(format!("/unmounted{old}"));
                }
            }
        }
    }
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/Avid Offline Import Test.fcproj"),
        filmcraft_format::encode(&disposable, false),
    )
    .unwrap();
    let mut bindings = Vec::new();
    for (id, item) in &s.project.items {
        if let Some(m) = item.as_media()
            && let filmcraft_project::MediaRef::Components { channels, picture } = &m.media
        {
            for (i, c) in channels.iter().enumerate() {
                bindings.push(json!({"item":id,"component":{"kind":"audio","channel":i},"path":c.path}));
            }
            if let Some(c) = picture {
                assert!(c.format_pending);
                bindings.push(json!({"item":id,"component":{"kind":"picture"},"path":c.path}));
            }
        }
    }
    s.services = Arc::new(FsServices);
    // Existing paths must not bypass pending picture format verification.
    assert_eq!(crate::relink::scan(&s.project, &*s.services).len(), 2);
    let picture_ids: Vec<_> = bindings.iter().filter(|b| b["component"]["kind"] == "picture").map(|b| b["item"].clone()).collect();
    for id in &picture_ids {
        let status = s.execute("media.status", json!({"item":id})).unwrap();
        assert_ne!(status[0]["status"], "online", "pending picture must agree with playback's offline slate");
        let picture = status[0]["components"].as_array().unwrap().iter().find(|c| c["kind"] == "picture").unwrap();
        assert_eq!(picture["present"], false);
        assert_eq!(picture["formatPending"], true);
    }
    for wrong in ["rate", "aspect", "width"] {
        let mut invalid = (*offline).clone();
        let m = invalid.items.values_mut().filter_map(|i| i.as_media_mut()).find(|m| m.info.video.is_some()).unwrap();
        let video = m.info.video.as_mut().unwrap();
        match wrong {
            "rate" => video.frame_rate = filmcraft_time::FrameRate::new(25, 1),
            "aspect" => video.par = (2, 1),
            _ => video.width = 1280,
        }
        let mut rejected = Session { project: Arc::new(invalid), ..Default::default() };
        let before = rejected.project.clone();
        assert!(rejected.execute("media.relinkComponents", json!({"bindings":bindings})).is_err(), "accepted incompatible {wrong}");
        assert_eq!(rejected.project, before);
        assert!(rejected.history.undo.is_empty());
    }
    s.execute("media.relinkComponents", json!({"bindings":bindings})).unwrap();
    for id in &picture_ids {
        assert_eq!(s.execute("media.status", json!({"item":id})).unwrap()[0]["status"], "online");
    }
    for (id, item) in &s.project.items {
        assert_eq!(item.kind, online.project.item(*id).unwrap().kind, "relinked contents differ for {}", item.name);
    }
    assert!(crate::relink::scan(&s.project, &*s.services).is_empty());
    assert_eq!(s.project.sequence(sequence), offline.sequence(sequence));
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project, *s.project);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, offline);
    s.execute("edit.redo", json!({})).unwrap();
    assert!(crate::relink::scan(&s.project, &*s.services).is_empty());
    eprintln!("Phase 12: six offline components, identical 152-frame timeline, hydrated media identical to online import, undo/redo/save/reopen pass");
}

#[test]
fn offline_import_reports_permission_failure_instead_of_absence() {
    struct Denied(Arc<Input>);
    impl Services for Denied {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            self.0.read_file(path)
        }
        fn file_size(&self, path: &str) -> std::io::Result<u64> {
            if path.ends_with(".mxf") { Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied")) } else { self.0.file_size(path) }
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            panic!("import must not write")
        }
    }
    let (v, media) = fixture();
    let v = with_audio_descriptors(v);
    let (_, input) = session(&v, media);
    let mut s = Session::new(Arc::new(Denied(input)));
    let before = s.project.clone();
    let error = s.execute("file.importAvidCatalog", params("media")).unwrap_err().to_string();
    assert!(error.contains("access denied"), "{error}");
    assert_eq!(s.project, before);
    assert!(s.history.undo.is_empty());
}

#[test]
fn best_effort_sequence_bypasses_effects_and_marks_failed_segments() {
    let (mut v, media) = fixture();
    let source = v["bins"][0]["mobs"][1]["tracks"][1]["component"]["components"][1].clone();
    let mut second = v["bins"][0]["mobs"][1].clone();
    second["name"] = json!("Needs placeholder");
    second["id"]["raw"] = json!("03".repeat(32));
    second["tracks"][1]["component"]["components"][1]["class"] = json!("UNKNOWN");
    v["bins"][0]["mobs"].as_array_mut().unwrap().push(second);
    let mut wrapper = source.clone();
    wrapper["class"] = json!("TKFX");
    wrapper["unsupported"] = json!("component_semantics");
    wrapper["properties"] = json!({"effect_id":"UNKNOWN_EFFECT"});
    wrapper["input_tracks"] = json!([{"index":1,"component":source}]);
    v["bins"][0]["mobs"][1]["tracks"][1]["component"]["components"][1] = wrapper;
    let mut unreadable = v["bins"][0]["mobs"][1].clone();
    unreadable["name"] = json!("Uninterpretable tracks");
    unreadable["id"]["raw"] = json!("04".repeat(32));
    unreadable["tracks"][1]["index"] = json!(0);
    v["bins"][0]["mobs"].as_array_mut().unwrap().push(unreadable);
    let (s, _) = session(&v, media);
    let prepared = crate::avid_catalog::prepare_document(
        &s.project,
        &*s.services,
        &serde_json::to_vec(&v).unwrap(),
        crate::avid_catalog::ImportOptions { mode: filmcraft_interchange::avid_catalog::TransferMode::Sequences, allow_missing: true, best_effort: true },
        None,
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(prepared.result["sequences"].as_array().unwrap().len(), 3);
    assert!(prepared.result["errors"].to_string().contains("Entire sequence represented"));
    assert!(prepared.result["warnings"].to_string().contains("effect bypassed"));
    assert!(prepared.result["errors"].to_string().contains("offline placeholder"));
    let a = prepared.project.sequence(ItemId(prepared.result["sequences"][0]["item"].as_u64().unwrap())).unwrap();
    let b = prepared.project.sequence(ItemId(prepared.result["sequences"][1]["item"].as_u64().unwrap())).unwrap();
    assert_eq!(a.duration(), Tick::from_units(2, 24));
    assert_eq!(a.duration(), b.duration());
    assert_eq!(a.audio_tracks[1].items[0].start, b.audio_tracks[1].items[0].start);
    let missing = prepared.project.item(b.audio_tracks[1].items[0].item).unwrap();
    assert!(missing.as_media().unwrap().offline);
    assert!(missing.metadata.contains_key("filmcraft:avid-import-error"));
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&prepared.project, false)).unwrap().project, prepared.project);
    assert!(s.history.undo.is_empty());
}

#[test]
fn best_effort_picture_placeholder_has_finite_saved_motion_points() {
    let (mut v, media) = fixture();
    let c = &mut v["bins"][0]["mobs"][1]["tracks"][1]["component"];
    c["media_kind"] = json!(1);
    for child in c["components"].as_array_mut().unwrap() {
        child["media_kind"] = json!(1);
    }
    c["components"][1]["class"] = json!("UNKNOWN_PICTURE");
    let (s, _) = session(&v, media);
    let p = crate::avid_catalog::prepare_document(
        &s.project,
        &*s.services,
        &serde_json::to_vec(&v).unwrap(),
        crate::avid_catalog::ImportOptions { mode: filmcraft_interchange::avid_catalog::TransferMode::Sequences, allow_missing: true, best_effort: true },
        None,
        &|| Ok(()),
    )
    .unwrap()
    .project;
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&p, false)).unwrap().project, p);
}

#[test]
fn best_effort_sequences_isolate_bad_entries_without_reviving_history() {
    let (mut v, media) = fixture();
    let good = v["bins"][0]["mobs"][1].clone();
    let mut inactive = good.clone();
    inactive["name"] = json!("Inactive history");
    inactive["live"] = json!(false);
    // Historical duplicates must not make the current identity ambiguous.
    let mut malformed = good.clone();
    malformed["name"] = json!("Invalid identity");
    malformed["id"]["raw"] = json!("bad");
    let mut unnamed = good.clone();
    unnamed["name"] = Value::Null;
    unnamed["id"]["raw"] = json!("04".repeat(32));
    let mut duplicate = good.clone();
    duplicate["name"] = json!("Ambiguous sequence");
    duplicate["id"]["raw"] = json!("05".repeat(32));
    v["bins"][0]["mobs"].as_array_mut().unwrap().extend([inactive, malformed, unnamed, duplicate.clone(), duplicate]);
    let (s, _) = session(&v, media);
    let prepared = crate::avid_catalog::prepare_document(
        &s.project,
        &*s.services,
        &serde_json::to_vec(&v).unwrap(),
        crate::avid_catalog::ImportOptions { mode: filmcraft_interchange::avid_catalog::TransferMode::Sequences, allow_missing: true, best_effort: true },
        None,
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(prepared.result["sequences"].as_array().unwrap().len(), 1, "only the valid live sequence should import: {}", prepared.result);
    assert_eq!(prepared.result["sequences"][0]["name"], "Trimmed");
    let errors = prepared.result["errors"].to_string();
    for reason in ["inactive composition", "Invalid identity", "invalid name", "duplicate composition identity"] {
        assert!(errors.contains(reason), "missing {reason}: {errors}");
    }
    assert!(!prepared.project.items.values().any(|item| item.name == "Unconverted Avid segment"));
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&prepared.project, false)).unwrap().project, prepared.project);
}

#[test]
fn indexed_mxf_corroborates_full_audio_clock_before_binding_and_retains_raw_evidence() {
    use crate::avid_catalog::{ImportOptions, prepare_document_with_index};
    use filmcraft_interchange::avid_catalog::TransferMode;
    let root = std::env::temp_dir().join(format!(
        "filmcraft-audio-clock-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    for (sr, frames, multiplier) in [(48000u32, 99i64, 4i64), (44100, 48, 1)] {
        let folder = root.join(sr.to_string());
        std::fs::create_dir_all(&folder).unwrap();
        let (v, _) = fixture();
        let mut v = with_audio_descriptors(v);
        let samples = (i128::from(frames) * 1001 * i128::from(sr) / 24000) as usize;
        let mut media = BTreeMap::new();
        let mut identities = std::collections::BTreeSet::new();
        let clip = &mut v["bins"][0]["catalog"]["clips"][0];
        clip["length"] = json!(frames);
        clip["rate"] = json!({"numerator":23976,"denominator":1000});
        clip["workflow_evidence"] = json!("managed_avb_source_descriptors");
        for (i, t) in clip["tracks"].as_array_mut().unwrap().iter_mut().enumerate() {
            let ids = filmcraft_mxf::PackageIds::from_seed(&format!("clock-{sr}-{i}"), "test");
            let id = json!({"raw":ids.file.0.iter().map(|b|format!("{b:02x}")).collect::<String>(),"byte_order":"big"});
            identities.insert(ids.file.0);
            let path = folder.join(format!("channel-{i}.mxf"));
            let bytes = filmcraft_mxf::write_opatom_pcm(
                &filmcraft_mxf::OpAtomPcm { sample_rate: sr, bits: 16, channels: 1, edit_rate: None, ids, timecode: None },
                &vec![if i == 0 { 4000 } else { -8000 }; samples],
            )
            .unwrap();
            std::fs::write(&path, &bytes).unwrap();
            media.insert(path.to_str().unwrap().to_owned(), bytes);
            t["component"]["source_id"] = id.clone();
            t["component"]["length"] = json!(frames * multiplier);
            t["component"]["rate"] = json!({"numerator":23976 * multiplier,"denominator":1000});
            let r = &mut t["references"][0];
            r["source_id"] = id;
            r["length"] = json!(frames * multiplier);
            r["rate"] = json!({"numerator":23976 * multiplier,"denominator":1000});
            r["status"] = json!("unresolved");
            r["managed_descriptor_verified"] = json!(true);
            r["location_scope"] = json!("no_media_locations_scanned");
            r["locations"] = json!([]);
        }
        v["bins"][0]["mobs"][0] = v["bins"][0]["catalog"]["clips"][0].clone();
        for i in 0..2 {
            let id = v["bins"][0]["mobs"][0]["tracks"][i]["references"][0]["source_id"].clone();
            let d = &mut v["bins"][0]["mobs"][i + 2];
            d["id"] = id.clone();
            d["descriptor"]["locator"]["properties"]["mob_id"] = json!({"id_raw":id["raw"],"byte_order":"big"});
            d["descriptor"]["sample_rate"] = json!(sr);
            d["descriptor"]["edit_rate"] = json!(sr);
            d["descriptor"]["average_bps"] = json!(sr * 2);
            // Descriptor padding must not be used as the playable range.
            d["descriptor"]["length"] = json!(samples + 132);
        }
        let (s, services) = session(&v, media.clone());
        let before = s.project.clone();
        let index = crate::component_relink::index_identities(identities.clone(), vec![folder.to_str().unwrap().into()], &*services, Arc::default()).unwrap();
        let prepare = |v: &Value, index| {
            prepare_document_with_index(
                &before,
                &*services,
                &serde_json::to_vec(v).unwrap(),
                ImportOptions { mode: TransferMode::Media, allow_missing: true, best_effort: true },
                None,
                &|| Ok(()),
                index,
            )
            .unwrap()
        };
        let prepared = prepare(&v, Some(&index));
        assert!(prepared.result["errors"].as_array().unwrap().is_empty(), "{}", prepared.result);
        assert_eq!(prepared.result["bound"][0]["audioTimingReconciled"], true);
        assert_eq!(prepared.result["bound"][0]["offlineComponents"], 0);
        assert!(prepared.result["warnings"].to_string().contains("declared whole samples"));
        let id = ItemId(prepared.result["bound"][0]["item"].as_u64().unwrap());
        let item = prepared.project.item(id).unwrap();
        let clip = item.as_media().unwrap();
        assert_eq!(clip.info.duration, Tick::from_units(frames * 1001, 24000));
        let filmcraft_project::MediaRef::Components { channels, picture } = &clip.media else { panic!() };
        assert!(picture.is_none());
        assert_eq!(channels[0].length, Tick::from_units(samples as i64, i64::from(sr)));
        let (source, failures) = crate::component_audio::open(clip.info.clone(), channels, &*services).unwrap();
        assert!(failures.is_empty());
        let audio = source.audio(samples as i64 - 1, 2, sr).unwrap();
        assert_eq!(audio.channels[0], vec![4000.0 / 32768.0, 0.0]);
        assert_eq!(audio.channels[1], vec![-8000.0 / 32768.0, 0.0]);
        let evidence: Value = serde_json::from_str(&item.metadata[filmcraft_interchange::avid_catalog::SOURCE_KEY]).unwrap();
        assert_eq!(evidence["clip"]["rate"]["numerator"], 23976);
        assert_eq!(evidence["audio_rate_reconciliation"]["master_rate"], json!({"numerator":24000,"denominator":1001}));
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&prepared.project, false)).unwrap().project, prepared.project);
        assert_eq!(s.project, before);
        assert_eq!(prepare(&v, None).result["bound"].as_array().unwrap().len(), 0, "nominal decimal alone must not authorize a clock");
        for case in ["offset", "wrong_clock", "wrong_length", "incomplete"] {
            let mut bad = v.clone();
            let t = &mut bad["bins"][0]["catalog"]["clips"][0]["tracks"][0];
            match case {
                "offset" => {
                    t["component"]["start"] = json!(1);
                    t["references"][0]["source_start"] = json!(1);
                }
                "wrong_clock" => {
                    t["component"]["rate"]["numerator"] = json!(24000 * multiplier);
                    t["references"][0]["rate"]["numerator"] = json!(24000 * multiplier);
                }
                "wrong_length" => {
                    t["component"]["length"] = json!(frames * multiplier + 1);
                    t["references"][0]["length"] = json!(frames * multiplier + 1);
                }
                _ => {
                    t["component"]["truncated"] = json!(true);
                }
            }
            bad["bins"][0]["mobs"][0] = bad["bins"][0]["catalog"]["clips"][0].clone();
            let result = prepare(&bad, Some(&index));
            assert_eq!(result.result["bound"].as_array().unwrap().len(), 0, "{case}");
        }
        // A structurally valid MXF can declare one more sample than it stores. That must
        // not corroborate the candidate clock, even if the declared endpoint matches.
        let mut short_media = BTreeMap::new();
        for (i, path) in media.keys().enumerate() {
            let ids = filmcraft_mxf::PackageIds::from_seed(&format!("clock-{sr}-{i}"), "test");
            let mut bytes = filmcraft_mxf::write_opatom_pcm(
                &filmcraft_mxf::OpAtomPcm { sample_rate: sr, bits: 16, channels: 1, edit_rate: None, ids, timecode: None },
                &vec![4000; samples - 1],
            )
            .unwrap();
            let mut tag = vec![0x02, 0x02, 0, 8];
            tag.extend_from_slice(&((samples - 1) as i64).to_be_bytes());
            let positions: Vec<_> = bytes.windows(tag.len()).enumerate().filter(|(_, v)| *v == tag).map(|(i, _)| i).collect();
            assert!(!positions.is_empty());
            for at in positions {
                bytes[at + 4..at + 12].copy_from_slice(&(samples as i64).to_be_bytes());
            }
            let mxf = filmcraft_mxf::open(bytes.as_slice()).unwrap();
            assert_eq!(mxf.tracks[0].duration, Some(samples as i64));
            assert_eq!(mxf.tracks[0].stored_sample_frames(), samples as u64 - 1);
            short_media.insert(path.clone(), bytes);
        }
        let (_, short_services) = session(&v, short_media);
        let short_index =
            crate::component_relink::index_identities(identities.clone(), vec![folder.to_str().unwrap().into()], &*short_services, Arc::default()).unwrap();
        let short = prepare_document_with_index(
            &before,
            &*short_services,
            &serde_json::to_vec(&v).unwrap(),
            ImportOptions { mode: TransferMode::Media, allow_missing: true, best_effort: true },
            None,
            &|| Ok(()),
            Some(&short_index),
        )
        .unwrap();
        assert!(short.result["bound"].as_array().unwrap().is_empty());
        let incomplete = crate::component_relink::index_identities(
            identities.clone(),
            vec![folder.to_str().unwrap().into(), root.join("missing").to_str().unwrap().into()],
            &*services,
            Arc::default(),
        )
        .unwrap();
        assert_eq!(prepare(&v, Some(&incomplete)).result["bound"].as_array().unwrap().len(), 0);
        let duplicate = folder.join("duplicate.mxf");
        let bytes = media.values().next().unwrap().clone();
        std::fs::write(&duplicate, &bytes).unwrap();
        let mut duplicate_media = media.clone();
        duplicate_media.insert(duplicate.to_str().unwrap().into(), bytes);
        let (_, duplicate_services) = session(&v, duplicate_media);
        let duplicate_index =
            crate::component_relink::index_identities(identities, vec![folder.to_str().unwrap().into()], &*duplicate_services, Arc::default()).unwrap();
        assert_eq!(prepare(&v, Some(&duplicate_index)).result["bound"].as_array().unwrap().len(), 0);
        for (path, bytes) in media {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

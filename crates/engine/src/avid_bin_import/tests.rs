use super::*;
use crate::Services;
use std::sync::atomic::{AtomicUsize, Ordering};
fn text(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}
fn identity(b: &mut Vec<u8>) {
    b.push(0x41);
    b.extend_from_slice(&12u32.to_le_bytes());
    b.extend_from_slice(&[6, 10, 43, 52, 1, 1, 1, 5, 1, 1, 15, 16]);
    for n in [19, 0, 0, 0] {
        b.extend_from_slice(&[0x44, n]);
    }
    for (tag, n) in [(0x48, 4), (0x46, 2), (0x46, 2)] {
        b.push(tag);
        b.extend(std::iter::repeat_n(1, n));
    }
    b.push(0x41);
    b.extend_from_slice(&8u32.to_le_bytes());
    b.extend_from_slice(&[1; 8]);
}
fn common(name: Option<&str>, attrs: u32) -> Vec<u8> {
    let mut b = vec![2, 3];
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&24u32.to_le_bytes());
    b.extend_from_slice(&[0; 2]);
    if let Some(name) = name {
        text(&mut b, name);
    } else {
        b.extend_from_slice(&[255; 2]);
    }
    b.extend_from_slice(&[255; 2]);
    b.extend_from_slice(&attrs.to_le_bytes());
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&[1, 1, 0x48, 0, 0, 0, 0]);
    b
}
fn fixture() -> Vec<u8> {
    let mut user = vec![2, 1];
    user.extend_from_slice(&1u32.to_le_bytes());
    user.extend_from_slice(&2u32.to_le_bytes());
    text(&mut user, "Scene");
    text(&mut user, "Native test");
    user.push(3);
    let mut attrs = vec![2, 1];
    attrs.extend_from_slice(&1u32.to_le_bytes());
    attrs.extend_from_slice(&3u32.to_le_bytes());
    text(&mut attrs, "_USER");
    attrs.extend_from_slice(&1u32.to_le_bytes());
    attrs.push(3);
    let mut clip = common(None, 0);
    clip.extend_from_slice(&[2, 1]);
    clip.extend_from_slice(&120u32.to_le_bytes());
    clip.extend_from_slice(&[2, 3]);
    clip.extend_from_slice(&[0; 8]);
    clip.extend_from_slice(&1u16.to_le_bytes());
    clip.extend_from_slice(&0u32.to_le_bytes());
    clip.extend_from_slice(&[1, 1]);
    identity(&mut clip);
    clip.push(3);
    let mut mob = common(Some("Audio"), 2);
    mob.extend_from_slice(&[2, 8, 0]);
    mob.extend_from_slice(&120u32.to_le_bytes());
    mob.extend_from_slice(&[0; 4]);
    mob.extend_from_slice(&1u32.to_le_bytes());
    mob.extend_from_slice(&[5, 0, 1, 0]);
    mob.extend_from_slice(&3u32.to_le_bytes());
    mob.extend_from_slice(&[1, 1, 0x45, 0, 0, 2, 2]);
    mob.extend_from_slice(&[1; 8]);
    mob.extend_from_slice(&[0; 4]);
    mob.push(2);
    mob.extend_from_slice(&[0; 8]);
    mob.extend_from_slice(&[1, 2]);
    identity(&mut mob);
    mob.push(3);
    let mut root = vec![2, 14];
    root.extend_from_slice(&[0; 12]);
    root.extend_from_slice(&1u16.to_le_bytes());
    root.extend_from_slice(&4u32.to_le_bytes());
    root.extend_from_slice(&[0; 8]);
    root.push(1);
    root.extend_from_slice(&[0; 103]);
    let mut out = b"\x06\0DomainDJBO\x07\0AObjDoc\x04\0\0".to_vec();
    out.extend_from_slice(&5u32.to_le_bytes());
    out.extend_from_slice(&5u32.to_le_bytes());
    out.extend_from_slice(&[0; 38]);
    for (tag, b) in [(*b"ATTR", user), (*b"ATTR", attrs), (*b"SCLP", clip), (*b"CMPO", mob), (*b"ABIN", root)] {
        out.extend(tag.into_iter().rev());
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(&b);
    }
    out
}
struct Input {
    bytes: Vec<u8>,
    reads: AtomicUsize,
    changing: bool,
}
impl Services for Input {
    fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
        Ok(self.bytes.clone())
    }
    fn file_size(&self, _: &str) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }
    fn read_range(&self, _: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        let mut bytes = self.bytes[offset as usize..(offset as usize + len).min(self.bytes.len())].to_vec();
        if self.changing && self.reads.fetch_add(1, Ordering::Relaxed) > 0 {
            bytes[0] ^= 1;
        }
        Ok(bytes)
    }
    fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        panic!("native import must not write sources");
    }
}
fn session(changing: bool) -> Session {
    Session::new(Arc::new(Input { bytes: fixture(), reads: AtomicUsize::new(0), changing }))
}
fn prepare(s: &mut Session) -> Value {
    s.execute("file.prepareAvidBin", json!({"path":"Sound Effects.avb","mode":"metadata","mediaFolders":[],"wait":true})).unwrap()
}
#[test]
fn native_preview_commit_suffix_undo_and_saved_metadata() {
    let mut s = session(false);
    let before = s.project.clone();
    let state = serde_json::to_value(&s.state).unwrap();
    let job = prepare(&mut s);
    let preview = s.execute("file.avidBinPreparation", job.clone()).unwrap();
    assert_eq!(preview["report"]["bins"][0]["clips"], 1);
    assert_eq!(s.project, before);
    assert_eq!(serde_json::to_value(&s.state).unwrap(), state);
    assert!(s.history.undo.is_empty());
    let out = s.execute("file.importAvidBin", job.clone()).unwrap();
    assert_eq!(out["bins"][0]["name"], "Sound Effects");
    let item = s.project.items.values().next().unwrap();
    assert_eq!(item.name, "Audio");
    assert_eq!(item.metadata["Scene"], "Native test");
    assert!(item.as_media().unwrap().offline);
    let after = s.project.clone();
    assert_eq!(s.history.undo.len(), 1);
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
    assert!(s.execute("file.importAvidBin", job).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
    let job = prepare(&mut s);
    let out = s.execute("file.importAvidBin", job).unwrap();
    assert_eq!(out["bins"][0]["name"], "Sound Effects_2");
}
#[test]
fn cancellation_stale_projects_and_same_size_source_changes_cannot_commit() {
    let mut s = session(false);
    let job = prepare(&mut s);
    s.execute("jobs.cancel", job.clone()).unwrap();
    assert!(s.execute("file.importAvidBin", job).is_err());
    assert!(s.history.undo.is_empty());
    let job = prepare(&mut s);
    s.execute("file.newBin", json!({"name":"Changed"})).unwrap();
    let before = s.project.clone();
    assert!(s.execute("file.importAvidBin", job).is_err());
    assert_eq!(s.project, before);
    let mut changing = session(true);
    let job = prepare(&mut changing);
    assert!(changing.execute("file.avidBinPreparation", job.clone()).unwrap_err().to_string().contains("changed"));
    assert!(changing.execute("file.importAvidBin", job).is_err());
    assert!(changing.history.undo.is_empty());
}
#[test]
fn hostile_inputs_and_malformed_bins_never_edit() {
    for p in [
        Value::Null,
        json!({}),
        json!({"path":12}),
        json!({"path":"\0"}),
        json!({"path":"x","parent":-1}),
        json!({"path":"x","wait":"yes"}),
        json!({"path":"x","mode":false}),
        json!({"path":"x","mediaFolder":12}),
        json!({"path":"x","mediaFolders":"folder"}),
        json!({"path":"x","mediaFolders":[null]}),
        json!({"path":"x","mediaFolders":["relative"]}),
        json!({"path":"x","mediaFolders":vec!["/";33]}),
        json!({"path":"x","mediaFolder":"/","mediaFolders":[]}),
    ] {
        let mut s = session(false);
        assert!(s.execute("file.prepareAvidBin", p).is_err());
        assert!(s.history.undo.is_empty());
        assert!(s.jobs.is_empty());
    }
    let mut s = Session::new(Arc::new(Input { bytes: vec![0; 30], reads: AtomicUsize::new(0), changing: false }));
    let job = prepare(&mut s);
    assert!(s.execute("file.importAvidBin", job).is_err());
    assert!(s.history.undo.is_empty());
}

#[test]
#[ignore = "explicit private AVB manifest; never discovers additional folders"]
fn native_graph_imports_tested_managed_sequences_offline() {
    let manifest: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_CORPUS_MANIFEST").unwrap()).unwrap()).unwrap();
    let mut count = 0;
    for entry in manifest.as_array().unwrap() {
        let path = entry["path"].as_str().unwrap();
        if !(9..=12).any(|phase| path.contains(&format!("/Phase {phase}/"))) {
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let document = avid_catalog::native_document(&bytes, path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let bytes = serde_json::to_vec(&document).unwrap();
        let prepared = crate::avid_catalog::prepare_document(
            &Project::default(),
            &crate::FsServices,
            &bytes,
            crate::avid_catalog::ImportOptions { mode: avid_catalog::TransferMode::Sequences, allow_missing: true, best_effort: false },
            None,
            &|| Ok(()),
        )
        .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(prepared.result["sequences"].as_array().unwrap().len(), 1, "{path}");
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&prepared.project, false)).unwrap().project, prepared.project);
        eprintln!("native {path}: {}", prepared.result["sequences"]);
        count += 1;
    }
    assert_eq!(count, 4);
}

#[test]
#[ignore = "explicit private Phase 14 bins and Phase 12 media folder; originals remain read-only"]
fn native_phase14_formats_timecode_and_mixed_rate_edits() {
    use filmcraft_time::{FrameRate, Tick};
    let folder = std::path::PathBuf::from(std::env::var("FILMCRAFT_AVID_PHASE14_FOLDER").unwrap());
    let media = std::env::var("FILMCRAFT_AVID_PHASE12_MEDIA_FOLDER").unwrap();
    for (name, size, rate, frames, start, drop, v1, a1) in [
        ("1080 2398 Bin.avb", (1920, 1080), FrameRate::FPS_23_976, 686, 86400, false, [(0, 197), (197, 118)], [(197, 118), (315, 371)]),
        ("2997 drop frame.avb", (1920, 1080), FrameRate::FPS_29_97, 1299, 107892, true, [(0, 302), (302, 334)], [(0, 302), (636, 663)]),
        ("UHD 25 Bin.avb", (3840, 2160), FrameRate::FPS_25, 1083, 90000, false, [(0, 252), (252, 278)], [(0, 252), (530, 553)]),
    ] {
        let path = folder.join(name);
        let original = std::fs::read(&path).unwrap();
        let mut s = Session::default();
        let before = s.project.clone();
        let job = s.execute("file.prepareAvidBin", json!({"path":path,"mediaFolders":[media],"wait":true})).unwrap();
        let preview = s.execute("file.avidBinPreparation", job.clone()).unwrap();
        assert_eq!(s.project, before);
        assert!(s.history.undo.is_empty());
        let result = s.execute("file.importAvidBin", job).unwrap();
        assert_eq!(result, preview["report"]);
        assert_eq!(result["errors"], json!([]), "{name}: {result}");
        assert_eq!(result["mediaSearch"]["linkedComponents"], 6);
        assert_eq!(result["mediaSearch"]["offlineComponents"], 0);
        assert_eq!(result["sequences"].as_array().unwrap().len(), 1);
        let id = filmcraft_project::ItemId(result["sequences"][0]["item"].as_u64().unwrap());
        let q = s.project.sequence(id).unwrap();
        assert_eq!((q.settings.width, q.settings.height), size);
        assert_eq!(q.settings.frame_rate, rate);
        assert_eq!(q.settings.drop_frame, drop);
        assert_eq!(q.start_timecode, start);
        assert_eq!(q.duration(), rate.tick_of(frames));
        assert_eq!(q.video_tracks.len(), 1);
        assert_eq!(q.audio_tracks.len(), 2);
        for (track, cuts) in [(&q.video_tracks[0], v1), (&q.audio_tracks[0], a1), (&q.audio_tracks[1], a1)] {
            assert_eq!(track.items.len(), 2);
            for (clip, (position, length)) in track.items.iter().zip(cuts) {
                assert_eq!((clip.start, clip.duration), (rate.tick_of(position), rate.tick_of(length)));
                assert_eq!(clip.speed, 1.0);
                assert!(!s.project.item(clip.item).unwrap().as_media().unwrap().offline);
            }
        }
        for (channel, track) in q.audio_tracks.iter().enumerate() {
            for clip in &track.items {
                assert_eq!(clip.source_channels, vec![channel as u16]);
            }
        }
        if rate == FrameRate::FPS_23_976 {
            assert_eq!(q.video_tracks[0].items[0].source_in, FrameRate::FPS_29_97.tick_of(87));
            assert_eq!(q.video_tracks[0].items[1].source_in, FrameRate::FPS_24.tick_of(109));
            for track in &q.audio_tracks {
                assert_eq!(track.items[0].source_in, FrameRate::FPS_24.tick_of(109));
                assert_eq!(track.items[1].source_in, FrameRate::FPS_24.tick_of(55));
            }
            assert!(result["warnings"].to_string().contains("phase offset 109 is not translated"));
        } else {
            for track in q.video_tracks.iter().chain(&q.audio_tracks) {
                assert!(track.items.iter().all(|c| c.source_in == Tick::ZERO));
            }
        }
        let after = s.project.clone();
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        eprintln!("{name}: {frames} frames, {size:?}, {rate:?}, start {start}, DF={drop}; six linked components and six timeline edits");
    }
}

#[test]
#[ignore = "explicit private Phase 12 report; native AVB import and media scan"]
fn native_phase12_matches_catalog_and_links_in_one_transaction() {
    let report_path = std::env::var("FILMCRAFT_AVID_PHASE12_REPORT").unwrap();
    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    let path = report["bins"][0]["path"].as_str().unwrap();
    let clips = report["bins"][0]["catalog"]["clips"].as_array().unwrap();
    let folders: std::collections::BTreeSet<_> = clips
        .iter()
        .flat_map(|c| c["tracks"].as_array().unwrap())
        .flat_map(|t| t["references"].as_array().unwrap())
        .flat_map(|r| r["locations"].as_array().unwrap())
        .map(|l| l["folder"].as_str().unwrap())
        .collect();
    assert_eq!(folders.len(), 1);
    let folder = *folders.first().unwrap();
    let mut native = Session::default();
    let before = native.project.clone();
    let job = native.execute("file.prepareAvidBin", json!({"path":path,"mode":"sequences","mediaFolders":[folder,folder],"wait":true})).unwrap();
    let preview = native.execute("file.avidBinPreparation", job.clone()).unwrap();
    assert_eq!(native.project, before);
    assert!(native.history.undo.is_empty());
    assert_eq!(preview["report"]["mediaSearch"]["linkedComponents"], 6, "{preview}");
    assert_eq!(preview["report"]["mediaSearch"]["offlineComponents"], 0);
    let result = native.execute("file.importAvidBin", job).unwrap();
    assert_eq!(result, preview["report"]);
    let mut reference = Session::default();
    reference.execute("file.importAvidCatalog", json!({"path":report_path,"mode":"sequences"})).unwrap();
    assert_eq!(native.project.items.len(), reference.project.items.len());
    for item in native.project.items.values() {
        let expected = reference.project.items.values().find(|i| i.name == item.name).unwrap();
        assert_eq!(item.kind, expected.kind, "{}", item.name);
    }
    assert!(crate::relink::scan(&native.project, &*native.services).is_empty());
    assert_eq!(native.history.undo.len(), 1);
    let after = native.project.clone();
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
    native.execute("edit.undo", json!({})).unwrap();
    assert_eq!(native.project, before);
    native.execute("edit.redo", json!({})).unwrap();
    assert_eq!(native.project, after);
    std::fs::write(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/Avid Native Phase 12.fcproj"), filmcraft_format::encode(&after, false))
        .unwrap();
}

#[test]
#[ignore = "explicit private manifest; native large-bin metadata benchmark"]
fn native_large_bin_metadata_roundtrip() {
    let manifest: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_CORPUS_MANIFEST").unwrap()).unwrap()).unwrap();
    let path = manifest.as_array().unwrap().iter().filter_map(|e| e["path"].as_str()).find(|p| p.contains("/Phase 6/")).unwrap();
    let mut s = Session::default();
    let start = web_time::Instant::now();
    let job = s.execute("file.prepareAvidBin", json!({"path":path,"mode":"metadata","wait":true})).unwrap();
    let out = s.execute("file.importAvidBin", job).unwrap();
    assert_eq!(out["bins"][0]["clips"], 1953);
    let import_ms = start.elapsed().as_millis();
    let bytes = filmcraft_format::encode(&s.project, false);
    assert_eq!(filmcraft_format::decode(&bytes).unwrap().project, *s.project);
    assert_eq!(s.history.undo.len(), 1);
    eprintln!("native Phase 6: 1953 masters, import {import_ms} ms, saved {} bytes", bytes.len());
}

#[test]
#[ignore = "private Phase 12 report; mutate only an in-memory AVB copy"]
fn native_unknown_effect_imports_partial_sequence_with_diagnostics() {
    let report: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVID_PHASE12_REPORT").unwrap()).unwrap()).unwrap();
    let path = report["bins"][0]["path"].as_str().unwrap();
    let mut bytes = std::fs::read(path).unwrap();
    let needle = b"EFF_TIMEWARP.MOTION_CTL";
    let offset = bytes.windows(needle.len()).position(|v| v == needle).unwrap();
    bytes[offset..offset + 3].copy_from_slice(b"BAD");
    let mut s = Session::new(Arc::new(Input { bytes, reads: AtomicUsize::new(0), changing: false }));
    let before = s.project.clone();
    let job = s.execute("file.prepareAvidBin", json!({"path":"Debug.avb","wait":true})).unwrap();
    let preview = s.execute("file.avidBinPreparation", job.clone()).unwrap();
    assert_eq!(preview["report"]["sequences"].as_array().unwrap().len(), 1);
    assert!(preview["report"]["errors"].to_string().contains("placeholder"), "{preview}");
    assert_eq!(s.project, before);
    let out = s.execute("file.importAvidBin", job).unwrap();
    let sequence = s.project.sequence(filmcraft_project::ItemId(out["sequences"][0]["item"].as_u64().unwrap())).unwrap();
    assert_eq!(sequence.duration(), filmcraft_time::Tick::from_units(152, 24));
    assert_eq!(sequence.video_tracks[0].items.len(), 2);
    assert_eq!(sequence.video_tracks[0].items[1].start, filmcraft_time::Tick::from_units(106, 24));
    let placeholder = s.project.item(sequence.video_tracks[0].items[1].item).unwrap();
    assert!(placeholder.as_media().unwrap().offline);
    assert!(placeholder.metadata.contains_key("filmcraft:avid-import-error"));
    let after = s.project.clone();
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
}

#[test]
#[ignore = "explicit private Phase 6 manifest; all-content native import"]
fn native_large_bin_default_attempts_every_master() {
    let manifest: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_CORPUS_MANIFEST").unwrap()).unwrap()).unwrap();
    let path = manifest.as_array().unwrap().iter().filter_map(|e| e["path"].as_str()).find(|p| p.contains("/Phase 6/")).unwrap();
    let mut s = Session::default();
    let started = web_time::Instant::now();
    let job = s.execute("file.prepareAvidBin", json!({"path":path,"wait":true})).unwrap();
    let preview = s.execute("file.avidBinPreparation", job.clone()).unwrap();
    assert_eq!(preview["report"]["bins"][0]["clips"], 1953);
    assert!(s.history.undo.is_empty());
    let result = s.execute("file.importAvidBin", job).unwrap();
    assert_eq!(s.project.items.len(), 1953);
    let mut reasons = std::collections::BTreeMap::new();
    for error in result["errors"].as_array().unwrap().iter().filter_map(Value::as_str) {
        let reason = error.split_once(": media remains offline: ").map_or(error, |(_, reason)| reason);
        *reasons.entry(reason).or_insert(0usize) += 1;
    }
    eprintln!("large-bin offline binding reasons: {reasons:?}");
    assert_eq!(s.history.undo.len(), 1);
    assert_eq!(result["bound"].as_array().unwrap().len() + result["errors"].as_array().unwrap().len(), 1953);
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project, *s.project);
    eprintln!(
        "native Phase 6 all-content: {} ms; {} bound offline; {} diagnosed unbound",
        started.elapsed().as_millis(),
        result["bound"].as_array().unwrap().len(),
        result["errors"].as_array().unwrap().len()
    );
}

#[test]
#[ignore = "explicit private Phase 6 AVB manifest and FILMCRAFT_AVID_PHASE6_MEDIA_FOLDER"]
fn native_large_bin_indexes_before_binding_and_verifies_audio_ranges() {
    let manifest: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_CORPUS_MANIFEST").unwrap()).unwrap()).unwrap();
    let path = manifest.as_array().unwrap().iter().filter_map(|e| e["path"].as_str()).find(|p| p.contains("/Phase 6/")).unwrap();
    let folder = std::env::var("FILMCRAFT_AVID_PHASE6_MEDIA_FOLDER").unwrap();
    let original = std::fs::read(path).unwrap();
    let mut s = Session::default();
    let before = s.project.clone();
    let job = s.execute("file.prepareAvidBin", json!({"path":path,"mediaFolders":[folder],"wait":true})).unwrap();
    let preview = s.execute("file.avidBinPreparation", job.clone()).unwrap();
    assert_eq!(s.project, before);
    assert!(s.history.undo.is_empty());
    let report = s.execute("file.importAvidBin", job).unwrap();
    assert_eq!(report, preview["report"]);
    assert_eq!(report["bins"][0]["clips"], 1953);
    assert_eq!(report["bound"].as_array().unwrap().len(), 1280);
    assert_eq!(report["errors"].as_array().unwrap().len(), 673);
    assert_eq!(report["mediaSearch"]["linkedComponents"], 2163);
    assert_eq!(report["mediaSearch"]["offlineComponents"], 0);
    assert_eq!(report["mediaSearch"]["search"]["complete"], true);
    let mut channel_count = 0;
    let mut sampled = 0;
    for bound in report["bound"].as_array().unwrap() {
        let id = filmcraft_project::ItemId(bound["item"].as_u64().unwrap());
        let item = s.project.item(id).unwrap();
        let media = item.as_media().unwrap();
        let filmcraft_project::MediaRef::Components { channels, picture } = &media.media else { panic!() };
        assert!(picture.is_none());
        let evidence: Value = serde_json::from_str(&item.metadata[avid_catalog::SOURCE_KEY]).unwrap();
        assert_eq!(evidence["audio_rate_reconciliation"]["master_rate"], json!({"numerator":24000,"denominator":1001}));
        for (slot, channel) in channels.iter().enumerate() {
            assert_eq!(channel.track as usize, slot + 1);
            assert_eq!(channel.source_start, filmcraft_time::Tick::ZERO);
            assert!(channel.path.is_some());
            assert!(channel.length <= media.info.duration);
            // Reopen every binding through playback's identity and range checks.
            crate::component_audio::open_component(channel, &*s.services).unwrap();
            channel_count += 1;
        }
        if sampled < 12 {
            let source = s.media.full_res_source(&s.project, id, &*s.services).unwrap();
            let rate = media.info.audio().unwrap().sample_rate;
            let count = channels[0].length.to_units_floor(i64::from(rate));
            let head = source.audio(0, 8, rate).unwrap();
            let tail = source.audio(count - 4, 8, rate).unwrap();
            assert!(head.channels.iter().flatten().all(|v| v.is_finite()));
            assert!(tail.channels.iter().all(|c| c[4..] == [0.0; 4]));
            if let Ok(ffmpeg) = std::env::var("FILMCRAFT_AVID_FFMPEG") {
                for (slot, channel) in channels.iter().enumerate() {
                    let output = std::process::Command::new(&ffmpeg)
                        .args([
                            "-v",
                            "error",
                            "-i",
                            channel.path.as_deref().unwrap(),
                            "-map",
                            "0:a:0",
                            "-af",
                            "atrim=start_sample=0:end_sample=8",
                            "-f",
                            "f32le",
                            "-acodec",
                            "pcm_f32le",
                            "pipe:1",
                        ])
                        .output()
                        .unwrap();
                    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
                    assert_eq!(output.stdout.len(), 8 * 4);
                    let expected: Vec<_> = output.stdout.chunks_exact(4).map(|v| f32::from_le_bytes(v.try_into().unwrap())).collect();
                    assert_eq!(head.channels[slot], expected, "PCM oracle mismatch for slot {slot}");
                }
            }
            sampled += 1;
        }
    }
    assert_eq!(channel_count, 2163);
    let after = s.project.clone();
    assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
    assert_eq!(s.history.undo.len(), 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
    assert_eq!(std::fs::read(path).unwrap(), original);
    eprintln!("Phase 6: 1280 linked masters / 2163 verified components; 673 diagnosed unbound; sample/range, undo/redo and roundtrip checks pass");
}

#[test]
fn native_shared_snapshot_matches_external_catalog_and_cancels_during_decode() {
    let bytes = fixture();
    let path = "Shared.avb";
    let compact = avid_catalog::read_native(&bytes, path).unwrap();
    assert!(compact.bins()[0].get("catalog").is_none());
    let expanded = avid_catalog::native_document(&bytes, path).unwrap();
    assert!(expanded["bins"][0]["mobs"][0].get("availability").is_none());
    assert!(expanded["bins"][0]["mobs"][0]["tracks"][0].get("references").is_none());
    let serialized = serde_json::to_vec(&expanded).unwrap();
    let external = avid_catalog::read_document(&serialized).unwrap();
    let project = Project::default();
    for mode in [TransferMode::Metadata, TransferMode::Media, TransferMode::Sequences] {
        let options = crate::avid_catalog::ImportOptions { mode, allow_missing: true, best_effort: true };
        let a = crate::avid_catalog::prepare_parsed_with_index(&project, &crate::FsServices, &compact, options, None, &|| Ok(()), None).unwrap();
        let b = crate::avid_catalog::prepare_parsed_with_index(&project, &crate::FsServices, &external, options, None, &|| Ok(()), None).unwrap();
        assert_eq!(a.project, b.project);
        assert_eq!(a.result, b.result);
    }
    let calls = std::cell::Cell::new(0);
    let total = std::cell::Cell::new(0);
    avid_catalog::read_native_with_checkpoint(&bytes, path, &|| {
        total.set(total.get() + 1);
        Ok(())
    })
    .unwrap();
    // Fail once at every checkpoint, including track recovery. A later successful check must
    // never turn that cancellation into a partially decoded bin.
    for stop in 1..=total.get() {
        calls.set(0);
        let result = avid_catalog::read_native_with_checkpoint(&bytes, path, &|| {
            calls.set(calls.get() + 1);
            if calls.get() == stop { Err(filmcraft_interchange::Error::Other("cancelled test".into())) } else { Ok(()) }
        });
        assert!(result.err().unwrap().to_string().contains("cancelled test"));
    }
}

#[test]
fn oversized_native_bin_is_rejected_before_reading_or_editing() {
    struct Oversized;
    impl crate::Services for Oversized {
        fn file_size(&self, _: &str) -> std::io::Result<u64> {
            Ok(filmcraft_interchange::avid_bin::MAX_BYTES as u64 + 1)
        }
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            panic!("must not read oversized file")
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            panic!("must not write source")
        }
    }
    let mut s = Session::new(Arc::new(Oversized));
    let before = s.project.clone();
    let job = prepare(&mut s);
    assert!(s.execute("file.avidBinPreparation", job.clone()).unwrap_err().to_string().contains("512 MiB"));
    assert!(s.execute("file.importAvidBin", job).is_err());
    assert_eq!(s.project, before);
    assert!(s.history.undo.is_empty());
}

#[test]
#[ignore = "explicit private large-bin manifest; no corpus discovery or media writes"]
fn native_large_bins_prepare_import_and_roundtrip() {
    let path = std::env::var("FILMCRAFT_AVID_LARGE_BIN_MANIFEST").unwrap();
    let manifest: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let entries = manifest.as_array().unwrap();
    assert!(!entries.is_empty());
    for entry in entries {
        let mut s = Session::default();
        let before = s.project.clone();
        let job = s.execute("file.prepareAvidBin", json!({"path":entry["path"],"wait":true})).unwrap();
        let preview = s.execute("file.avidBinPreparation", job.clone()).unwrap();
        assert_eq!(s.project, before);
        assert!(s.history.undo.is_empty());
        let result = s.execute("file.importAvidBin", job).unwrap();
        assert_eq!(result, preview["report"]);
        assert_eq!(result["bins"][0]["clips"], entry["clips"]);
        assert_eq!(result["bins"][0]["referenced"], entry["referenced"]);
        assert_eq!(result["sequences"].as_array().unwrap().len() as u64, entry["sequences"].as_u64().unwrap());
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project, *s.project);
        let after = s.project.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
        eprintln!(
            "large bin: {} clips, {} referenced, {} sequences; {} reported import errors",
            entry["clips"],
            entry["referenced"],
            entry["sequences"],
            result["errors"].as_array().unwrap().len()
        );
    }
}

use super::*;
use filmcraft_interchange::avid_catalog::SOURCE_KEY;
use filmcraft_project::{BinId, ItemKind};
use serde_json::json;

pub(super) fn fixture() -> Value {
    let mut v = json!({"schema_version":2,"workflow":"avid_managed_mxf","bins":[{
        "path":"/source/Sound Effects.avb","status":"parsed","snapshot_stable":true,"diagnostics":[],
        "summary":{"bin_uid":"synthetic"},"mobs":[],"catalog":{"summary":{"master_count":1},"clips":[{
            "kind":"MasterMob","name":"音声 🎵","user_placed":true,"id":{"raw":"01".repeat(32),"byte_order":"little"},
            "length":120,"rate":{"numerator":24,"denominator":1},
            "user":{"Scene":"SCENE_TEST_001","FC_Test":"FC_CUSTOM_001"},"availability":"partial",
            "tracks":[{"index":1,"status":"absent","component":{"media_kind":2,"length":120},"references":[]},
                      {"index":2,"status":"present","component":{"media_kind":2,"length":120},"references":[{"locations":[{"folder":"/media/002","filename":"A2.mxf"}]}]}]
        }]}
    }]});
    v["bins"][0]["mobs"] = json!([v["bins"][0]["catalog"]["clips"][0].clone()]);
    v
}
struct MemoryReport(Vec<u8>);
impl Services for MemoryReport {
    fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
        Ok(self.0.clone())
    }
    fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("unexpected write"))
    }
}
fn session(v: &Value) -> Session {
    Session::new(Arc::new(MemoryReport(serde_json::to_vec(v).unwrap())))
}

#[test]
fn avid_catalog_transfer_preview_undo_and_roundtrip() {
    let input = fixture();
    let mut s = session(&input);
    let original = s.project.clone();
    let state = serde_json::to_value(&s.state).unwrap();
    let preview = s.execute("file.previewAvidCatalog", json!({"path":"catalog.json"})).unwrap();
    assert_eq!(preview["bins"][0]["name"], "Sound Effects");
    assert_eq!(s.project, original);
    assert!(s.history.undo.is_empty());
    let result = s.execute("file.importAvidCatalog", json!({"path":"catalog.json"})).unwrap();
    assert_eq!(result["bins"][0]["clips"], 1);
    assert_eq!(s.history.undo.len(), 1);
    assert_eq!(serde_json::to_value(&s.state).unwrap(), state);
    let item = s.project.items.values().next().unwrap();
    assert_eq!(item.name, "音声 🎵");
    assert_eq!(item.metadata["Scene"], "SCENE_TEST_001");
    assert_eq!(item.metadata["FC_Test"], "FC_CUSTOM_001");
    let provenance: Value = serde_json::from_str(&item.metadata[SOURCE_KEY]).unwrap();
    assert_eq!(provenance["clip"], input["bins"][0]["catalog"]["clips"][0]);
    let ItemKind::Media(media) = &item.kind else {
        panic!("expected offline media");
    };
    assert!(media.offline);
    assert_eq!(media.info.duration, Tick(5 * filmcraft_time::TICKS_PER_SECOND));
    assert!(media.info.audio_streams.is_empty());
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, original);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
    // The document decoder has no services/report/AVB access.
    let saved = filmcraft_format::encode(&s.project, false);
    let loaded = filmcraft_format::decode(&saved).unwrap();
    assert_eq!(loaded.project, *after);
    s.execute("file.importAvidCatalog", json!({"path":"catalog.json"})).unwrap();
    assert!(s.project.root.children.iter().any(|b| matches!(b,filmcraft_project::BinEntry::Bin(b) if b.name=="Sound Effects_2")));
}

#[test]
fn avid_catalog_hostile_parameters_and_invalid_input_leave_state_unchanged() {
    for params in [
        json!(null),
        json!({}),
        json!({"path":3}),
        json!({"path":""}),
        json!({"path":"a\u{0000}"}),
        json!({"path":"a","parent":-1}),
        json!({"path":"a","parent":"1"}),
        json!({"path":"a","parent":999}),
    ] {
        let mut s = session(&fixture());
        let before = s.project.clone();
        let revision = s.revision;
        assert!(s.execute("file.importAvidCatalog", params).is_err());
        assert_eq!(s.project, before);
        assert_eq!(s.revision, revision);
        assert!(s.history.undo.is_empty());
    }
    let mut v = fixture();
    v["bins"][0]["catalog"]["clips"][0]["rate"]["denominator"] = json!(0);
    let mut s = session(&v);
    let before = s.project.clone();
    assert!(s.execute("file.importAvidCatalog", json!({"path":"a"})).is_err());
    assert_eq!(s.project, before);
}

#[test]
fn avid_catalog_parent_scope_and_name_holes() {
    let mut s = session(&fixture());
    let p = Arc::make_mut(&mut s.project);
    p.add_bin("Sound Effects", None);
    p.add_bin("Sound Effects_3", None);
    let parent = p.add_bin("Parent", None);
    let result = s.execute("file.importAvidCatalog", json!({"path":"a"})).unwrap();
    assert_eq!(result["bins"][0]["name"], "Sound Effects_2");
    let result = s.execute("file.importAvidCatalog", json!({"path":"a","parent":parent})).unwrap();
    assert_eq!(result["bins"][0]["name"], "Sound Effects");
    assert_eq!(s.project.root.find_bin(BinId(parent.0)).unwrap().children.len(), 1);
}

/// Opt-in local corpus: no proprietary samples or identifying metadata enter Git.
#[test]
#[ignore = "set FILMCRAFT_AVID_REPORTS to the existing acceptance-report directory"]
fn avid_catalog_local_corpus() {
    let root = std::path::PathBuf::from(std::env::var("FILMCRAFT_AVID_REPORTS").unwrap());
    for (name, expected) in [("phase4-04", 2), ("phase5-02", 2), ("phase6-full", 1953)] {
        let path = root.join(format!("{name}.json"));
        let bytes = std::fs::read(&path).unwrap();
        let input: Value = serde_json::from_slice(&bytes).unwrap();
        let mut s = Session::default();
        let output = s.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
        assert_eq!(output["bins"][0]["clips"], expected);
        assert_eq!(s.project.items.len(), expected);
        let mut all = 0;
        let mut partial = 0;
        for item in s.project.items.values() {
            let evidence: Value = serde_json::from_str(&item.metadata[SOURCE_KEY]).unwrap();
            let c = &evidence["clip"];
            let source = input["bins"][0]["catalog"]["clips"].as_array().unwrap().iter().find(|v| v["id"] == c["id"]).unwrap();
            assert_eq!(c, source, "all decoded clip evidence survives");
            match c["availability"].as_str() {
                Some("all") => all += 1,
                Some("partial") => {
                    partial += 1;
                    if name == "phase6-full" {
                        assert_eq!(c["tracks"][0]["index"], 1);
                        assert_eq!(c["tracks"][0]["status"], "absent");
                        assert_eq!(c["tracks"][1]["index"], 2);
                        assert_eq!(c["tracks"][1]["status"], "present");
                    }
                }
                _ => {}
            }
        }
        if name == "phase6-full" {
            assert_eq!((all, partial), (1761, 192));
        } else {
            let renamed = s.project.items.values().find(|i| i.name == "A_RENAME_TEST").unwrap();
            assert_eq!(renamed.metadata["Scene"], "SCENE_TEST_001");
            assert_eq!(renamed.metadata["FC_Test"], "FC_CUSTOM_001");
        }
        let saved = filmcraft_format::encode(&s.project, false);
        let reopened = filmcraft_format::decode(&saved).unwrap();
        assert_eq!(reopened.project, *s.project);
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "source report unchanged");
        eprintln!("{name}: {expected} masters transferred and saved/reopened; all={all}, partial={partial}; {} saved bytes", saved.len());
    }
}

#[test]
fn unrelated_inventory_sections_do_not_change_bin_transfer() {
    let input = fixture();
    let mut full = input.clone();
    full["folders"] = json!((0..5000).map(|i| json!({"mdb":{"objects":[{"id":i,"name":"unused","metadata":"x".repeat(100)}]}})).collect::<Vec<_>>());
    let mut small = session(&input);
    let mut large = session(&full);
    let expected = small.execute("file.importAvidCatalog", json!({"path":"catalog.json"})).unwrap();
    assert_eq!(large.execute("file.importAvidCatalog", json!({"path":"catalog.json"})).unwrap(), expected);
    assert_eq!(small.project, large.project);
    // Skipped inventory values still have to be syntactically valid JSON.
    let mut invalid = serde_json::to_vec(&input).unwrap();
    invalid.pop();
    invalid.extend_from_slice(b",\"folders\":[invalid]}");
    let mut malformed = Session::new(Arc::new(MemoryReport(invalid)));
    let before = malformed.project.clone();
    assert!(malformed.execute("file.importAvidCatalog", json!({"path":"catalog.json"})).is_err());
    assert_eq!(malformed.project, before);
    assert!(malformed.history.undo.is_empty());
}

#[test]
#[ignore = "set FILMCRAFT_AVID_LARGE_REPORT to the Phase 6 full catalog; run executable under an external RSS monitor"]
fn avid_catalog_large_bin_measurement() {
    let path = std::env::var("FILMCRAFT_AVID_LARGE_REPORT").unwrap();
    let report_bytes = std::fs::metadata(&path).unwrap().len();
    let mut s = Session::default();
    let started = std::time::Instant::now();
    let out = s.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
    let import_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(out["bins"][0]["clips"], 1953);
    assert_eq!(s.project.items.len(), 1953);
    assert_eq!(s.history.undo.len(), 1);
    let (mut all, mut partial) = (0, 0);
    for item in s.project.items.values() {
        let evidence: Value = serde_json::from_str(&item.metadata[SOURCE_KEY]).unwrap();
        match evidence["clip"]["availability"].as_str() {
            Some("all") => all += 1,
            Some("partial") => {
                partial += 1;
                assert_eq!(evidence["clip"]["tracks"][0]["status"], "absent");
                assert_eq!(evidence["clip"]["tracks"][1]["status"], "present");
            }
            _ => panic!("unexpected Phase 6 availability"),
        }
    }
    assert_eq!((all, partial), (1761, 192));
    let started = std::time::Instant::now();
    let saved = filmcraft_format::encode(&s.project, false);
    let save_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = std::time::Instant::now();
    let reopened = filmcraft_format::decode(&saved).unwrap();
    let reopen_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(reopened.project, *s.project);
    eprintln!(
        "{}",
        json!({"masters":1953,"all":all,"partial":partial,"report_bytes":report_bytes,"project_bytes":saved.len(),
        "import_ms":import_ms,"save_ms":save_ms,"reopen_ms":reopen_ms,"scope":"Rust catalog-to-project metadata transfer; excludes Python and media decode"})
    );
}

#[test]
fn avid_catalog_size_cap_precedes_read_and_rejects_changing_size() {
    struct Oversize;
    impl Services for Oversize {
        fn file_size(&self, _: &str) -> std::io::Result<u64> {
            Ok(filmcraft_interchange::avid_catalog::MAX_BYTES as u64 + 1)
        }
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            panic!("must reject before read")
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            panic!("no writes")
        }
    }
    let mut s = Session::new(Arc::new(Oversize));
    let before = s.project.clone();
    assert!(s.execute("file.importAvidCatalog", json!({"path":"huge"})).unwrap_err().to_string().contains("256 MiB"));
    assert_eq!(s.project, before);
    struct Truncated;
    impl Services for Truncated {
        fn file_size(&self, _: &str) -> std::io::Result<u64> {
            Ok(10)
        }
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Ok(vec![0; 2])
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            panic!("no writes")
        }
    }
    let mut s = Session::new(Arc::new(Truncated));
    assert!(s.execute("file.importAvidCatalog", json!({"path":"changing"})).unwrap_err().to_string().contains("changed"));
    assert!(s.history.undo.is_empty());
}

#[test]
#[ignore = "set FILMCRAFT_AVID_PHASE8_REPORT, FILMCRAFT_AVID_PHASE9_REPORT and FILMCRAFT_AVID_PHASE10_REPORT"]
fn avid_catalog_local_sequences_report_omissions() {
    for (key, expected) in [("FILMCRAFT_AVID_PHASE8_REPORT", 4), ("FILMCRAFT_AVID_PHASE9_REPORT", 2), ("FILMCRAFT_AVID_PHASE10_REPORT", 2)] {
        let path = std::env::var(key).unwrap();
        let mut s = Session::default();
        let result = s.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
        assert_eq!(result["bins"][0]["clips"], expected);
        assert_eq!(s.project.items.len(), expected);
        assert!(result["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("CompositionMob")));
        if key == "FILMCRAFT_AVID_PHASE10_REPORT" {
            let stereo = s.project.items.values().find(|i| i.name == "B_stereo.wav").unwrap();
            let evidence: Value = serde_json::from_str(&stereo.metadata[SOURCE_KEY]).unwrap();
            assert_eq!(evidence["clip"]["attributes"]["_CHANNEL_GROUP_LIST"], "ST:A1A2,");
            assert_eq!(evidence["clip"]["tracks"][0]["index"], 1);
            assert_eq!(evidence["clip"]["tracks"][1]["index"], 2);
            assert!(evidence["clip"]["tracks"][0].get("properties").is_some());
        }
        let saved = filmcraft_format::encode(&s.project, false);
        assert_eq!(filmcraft_format::decode(&saved).unwrap().project, *s.project);
        eprintln!("{key}: {expected} master placeholders saved/reopened, explicit unsupported-sequence warning");
    }
}

#[test]
fn avid_catalog_saved_project_opens_after_removing_catalog() {
    let dir = std::env::temp_dir().join(format!("filmcraft-avid-catalog-roundtrip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("catalog.json");
    let destination = dir.join("Transferred.fcproj");
    std::fs::write(&source, serde_json::to_vec(&fixture()).unwrap()).unwrap();
    let mut s = Session::default();
    s.execute("file.importAvidCatalog", json!({"path":source})).unwrap();
    s.execute("file.saveAs", json!({"path":destination})).unwrap();
    let expected = s.project.clone();
    std::fs::remove_file(&source).unwrap();
    let mut reopened = Session::default();
    reopened.execute("file.open", json!({"path":destination})).unwrap();
    assert_eq!(reopened.project, expected);
    assert_eq!(reopened.project.items.values().next().unwrap().metadata["Scene"], "SCENE_TEST_001");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "set FILMCRAFT_AVID_AUDIO_ROOT to the Phase 9 MXF folder; ffmpeg is an external test oracle"]
fn avid_audio_local_mxf_decoding_matches_oracle() {
    let root = std::path::PathBuf::from(std::env::var("FILMCRAFT_AVID_AUDIO_ROOT").unwrap());
    let s = Session::default();
    let mut count = 0;
    for e in std::fs::read_dir(root).unwrap() {
        let path = e.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mxf") {
            continue;
        }
        let src = s.media.open_file(path.to_str().unwrap(), &*s.services).unwrap();
        assert_eq!(src.info().container, "MXF OP-Atom");
        assert_eq!(src.info().audio().unwrap().channels, 1);
        assert_eq!(src.info().audio().unwrap().sample_rate, 48000);
        let oracle = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
            .output()
            .unwrap();
        assert!(oracle.status.success());
        let expected: Vec<f32> = oracle.stdout.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        assert_eq!(expected.len(), 240000);
        for start in [0, 48000, 192000, 239000] {
            let actual = src.audio(start, 1000, 48000).unwrap();
            assert_eq!(actual.channels.len(), 1);
            for (a, b) in actual.channels[0].iter().zip(&expected[start as usize..start as usize + 1000]) {
                assert!((a - b).abs() < 1e-6, "{} sample mismatch at {start}: {a} vs {b}", path.display());
            }
        }
        eprintln!("{}: mono PCM matches FFmpeg at four seek positions", path.file_name().unwrap().to_string_lossy());
        count += 1;
    }
    assert_eq!(count, 3);
}

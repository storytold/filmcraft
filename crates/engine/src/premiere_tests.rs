//! Synthetic native import and persistence acceptance tests. MIT OR Apache-2.0.
use crate::{Session, premiere::hash_file};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "premiere_corpus_tests.rs"]
mod corpus;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path =
            filmcraft_testkit::workspace_root().join("target/premiere-tests").join(format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        image::RgbaImage::from_pixel(16, 16, image::Rgba([30, 120, 220, 255])).save(path.join("source.png")).unwrap();
        let source = path.join("source.png");
        let xml = format!(
            r#"<PremiereData><Sequence ObjectUID="s"><Name>Sequence 01</Name><TrackGroups><TrackGroup><Second ObjectRef="1"/></TrackGroup></TrackGroups></Sequence><VideoTrackGroup ObjectID="1"><TrackGroup><FrameRate>8467191533</FrameRate><Tracks><Track ObjectURef="t"/></Tracks></TrackGroup></VideoTrackGroup><VideoClipTrack ObjectUID="t"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="2"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack><VideoClipTrackItem ObjectID="2"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="3"/></ClipTrackItem></VideoClipTrackItem><SubClip ObjectID="3"><Name>source.png</Name></SubClip><Media><Title>source.png</Title><FilePath>{}</FilePath></Media></PremiereData>"#,
            source.display()
        );
        std::fs::write(path.join("source.prproj"), xml).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn premiere_non_media_references_are_never_hashed_or_copied() {
    struct ReadCounter(AtomicU64);
    impl crate::Services for ReadCounter {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            std::fs::read(path)
        }
        fn write_file(&self, path: &str, bytes: &[u8]) -> std::io::Result<()> {
            std::fs::write(path, bytes)
        }
    }
    for name in ["secret.txt", "disguised.png"] {
        let fixture = Fixture::new();
        let source = fixture.0.join(name);
        std::fs::write(&source, b"private non-media contents").unwrap();
        let before = hash_file(&source).unwrap();
        let doc = fixture.0.join("source.prproj");
        let xml = std::fs::read_to_string(&doc).unwrap().replace("source.png", name).replace("FilePath>", "ActualMediaFilePath>");
        std::fs::write(&doc, xml).unwrap();
        for mode in ["link", "copy"] {
            let mut s = Session::default();
            let reads = std::sync::Arc::new(ReadCounter(AtomicU64::new(0)));
            s.services = reads.clone();
            let dest = fixture.0.join(mode);
            let result = s.execute("premiere.import", json!({"path":doc,"destination":dest,"mediaMode":mode})).unwrap();
            assert_eq!(result["offlineMedia"], json!([name]));
            assert_eq!(reads.0.load(Ordering::Relaxed), u64::from(name == "disguised.png"), "only recognized extensions may be probed");
            let workflow = s.project.workflow.as_ref().unwrap();
            let media = &workflow.imports[0].media[0];
            assert!(media.sha256.is_none() && media.bytes.is_none(), "rejected reference must not be hashed: {name}");
            assert_eq!(media.original_path, source.to_str().unwrap());
            assert_eq!(media.imported_path, media.original_path);
            assert!(result["report"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("offline placeholder")));
            if mode == "copy" {
                assert_eq!(std::fs::read_dir(dest.join("media")).unwrap().count(), 0);
            }
            let provenance = std::fs::read_to_string(dest.join("provenance.json")).unwrap();
            assert!(!provenance.contains(&before.0));
            assert_eq!(hash_file(&source).unwrap(), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn premiere_media_symlink_cannot_bypass_target_extension_check() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.png");
    let target = fixture.0.join("secret.txt");
    // Valid image bytes under a disallowed name prove that a successful probe alone is insufficient.
    std::fs::rename(&source, &target).unwrap();
    std::os::unix::fs::symlink(&target, &source).unwrap();
    let before = hash_file(&target).unwrap();
    for mode in ["link", "copy"] {
        let mut s = Session::default();
        let dest = fixture.0.join(mode);
        let result = s.execute("premiere.import", json!({"path":fixture.0.join("source.prproj"),"destination":dest,"mediaMode":mode})).unwrap();
        assert_eq!(result["offlineMedia"], json!(["source.png"]));
        assert!(s.project.workflow.as_ref().unwrap().imports[0].media[0].sha256.is_none());
        if mode == "copy" {
            assert_eq!(std::fs::read_dir(dest.join("media")).unwrap().count(), 0);
        }
        assert_eq!(hash_file(&target).unwrap(), before);
    }
}

#[test]
fn premiere_default_sequence_prefers_content_over_english_name() {
    let fixture = Fixture::new();
    let doc = fixture.0.join("source.prproj");
    let original = std::fs::read_to_string(&doc).unwrap().replace("Sequence 01", "Edición");
    for (index, other_group) in ["", "<TrackGroups><TrackGroup><Second ObjectRef=\"1\"/></TrackGroup></TrackGroups>"].iter().enumerate() {
        let xml =
            original.replace("</PremiereData>", &format!("<Sequence ObjectUID=\"empty\"><Name>Sequence 01</Name>{other_group}</Sequence></PremiereData>"));
        std::fs::write(&doc, xml).unwrap();
        let mut s = Session::default();
        s.execute("premiere.import", json!({"path":doc,"destination":fixture.0.join(index.to_string())})).unwrap();
        let id = s.state.active_sequence.unwrap();
        assert_eq!(s.project.item(id).unwrap().name, if index == 0 { "Edición" } else { "Sequence 01" });
        assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 1);
    }
}

#[test]
fn premiere_document_network_paths_are_refused_before_filesystem_access() {
    let fixture = Fixture::new();
    let doc = fixture.0.join("source.prproj");
    let xml = std::fs::read_to_string(&doc).unwrap().replace(fixture.0.join("source.png").to_str().unwrap(), "file://192.0.2.1/share/source.png");
    std::fs::write(&doc, xml).unwrap();
    let mut s = Session::default();
    let result = s.execute("premiere.import", json!({"path":doc,"destination":fixture.0.join("refused")})).unwrap();
    assert_eq!(result["offlineMedia"], json!(["source.png"]));
    assert!(result["report"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("document path refused")));
    assert!(s.project.workflow.as_ref().unwrap().imports[0].media[0].sha256.is_none());
}

#[test]
fn premiere_duplicate_sequence_names_import_and_select_by_uid() {
    let fixture = Fixture::new();
    let doc = fixture.0.join("source.prproj");
    let xml =
        std::fs::read_to_string(&doc).unwrap().replace("</PremiereData>", "<Sequence ObjectUID=\"other\"><Name>Sequence 01</Name></Sequence></PremiereData>");
    std::fs::write(&doc, xml).unwrap();
    let before = hash_file(&doc).unwrap();
    for selector in [None, Some("s"), Some("other"), Some("Sequence 01")] {
        let mut s = Session::default();
        let destination = fixture.0.join(selector.unwrap_or("default"));
        let mut params = json!({"path":doc,"destination":destination,"mediaMode":"link"});
        if let Some(selector) = selector {
            params["sequence"] = json!(selector);
        }
        let result = s.execute("premiere.import", params);
        if selector == Some("Sequence 01") {
            assert!(result.unwrap_err().to_string().contains("ObjectUID"));
            assert!(!destination.exists());
        } else {
            assert_eq!(result.unwrap()["sequences"].as_array().unwrap().len(), 2);
            let active = s.active_sequence().unwrap();
            assert_eq!(active.video_tracks.is_empty(), selector == Some("other"));
        }
        assert_eq!(hash_file(&doc).unwrap(), before);
    }
}

#[test]
fn premiere_link_copy_save_reopen_and_undo_preserve_input() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.png");
    let doc = fixture.0.join("source.prproj");
    let source_hash = hash_file(&source).unwrap();
    let doc_hash = hash_file(&doc).unwrap();
    for mode in ["link", "copy"] {
        let mut session = Session::default();
        let dest = fixture.0.join(mode);
        let r = session.execute("premiere.import", json!({"path":doc,"destination":dest,"mediaMode":mode,"frameRate":{"num":30,"den":1}})).unwrap();
        assert_eq!(r["offlineMedia"], json!([]));
        assert_eq!(r["sourceSha256"], doc_hash.0);
        let provenance = session.project.workflow.clone().unwrap();
        let media = &provenance.imports[0].media[0];
        let imported = PathBuf::from(&media.imported_path);
        assert_eq!(hash_file(&imported).unwrap(), source_hash);
        // Stored interchange paths deliberately omit Windows verbatim prefixes.
        assert_eq!(imported.canonicalize().unwrap() == source.canonicalize().unwrap(), mode == "link");
        #[cfg(target_os = "windows")]
        assert!(!media.imported_path.starts_with(r"\\?\"));
        let saved = fixture.0.join(format!("{mode}.fcproj"));
        session.execute("file.saveAs", json!({"path":saved})).unwrap();
        let bytes = std::fs::read(&saved).unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(envelope["format"], filmcraft_format::WORKFLOW_FORMAT_ID);
        let mut reopened = Session::default();
        reopened.execute("file.open", json!({"path":saved})).unwrap();
        assert_eq!(reopened.project.workflow.as_ref(), Some(&provenance));
        reopened.execute("project.setNotes", json!({"text":"edited"})).unwrap();
        reopened.execute("file.save", json!({})).unwrap();
        assert_eq!(filmcraft_format::decode(&std::fs::read(&saved).unwrap()).unwrap().project.workflow, Some(provenance));
        session.execute("edit.undo", json!({})).unwrap();
        assert!(session.project.workflow.is_none());
        assert!(dest.join("source.prproj").exists());
        session.execute("edit.redo", json!({})).unwrap();
        assert!(session.project.workflow.is_some());
        assert_eq!(hash_file(&source).unwrap(), source_hash);
        assert_eq!(hash_file(&doc).unwrap(), doc_hash);
    }
}
#[test]
fn premiere_missing_media_and_standard_import_route() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.0.join("source.png")).unwrap();
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths":[fixture.0.join("source.prproj")],"destination":fixture.0.join("offline")})).unwrap();
    assert_eq!(r["sequences"].as_array().unwrap().len(), 1);
    assert!(s.project.workflow.is_some());
    assert!(s.project.items.values().any(|i| matches!(&i.kind,filmcraft_project::ItemKind::Media(m) if m.offline)));
}
#[cfg(unix)]
#[test]
fn premiere_unreadable_media_stays_offline_in_link_and_copy_modes() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let source = fixture.0.join("source.png");
    let doc = fixture.0.join("source.prproj");
    let source_hash = hash_file(&source).unwrap();
    let doc_hash = hash_file(&doc).unwrap();
    // Change permissions only on this test's generated image, never on reference footage.
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&source).is_ok() {
        eprintln!("permission regression requires an unprivileged user; root bypasses mode bits");
        return;
    }
    let read_error = std::fs::File::open(&source).unwrap_err().to_string();
    for mode in ["link", "copy"] {
        let mut s = Session::default();
        let dest = fixture.0.join(mode);
        let r = s.execute("premiere.import", json!({"path":doc,"destination":dest,"mediaMode":mode})).unwrap();
        assert_eq!(r["offlineMedia"], json!(["source.png"]));
        assert!(r["report"].as_array().unwrap().iter().any(|w| {
            let w = w.as_str().unwrap();
            w.contains(source.to_str().unwrap()) && w.contains(&read_error) && w.contains("offline placeholder")
        }));
        let workflow = s.project.workflow.as_ref().unwrap();
        let media = &workflow.imports[0].media[0];
        assert_eq!(media.original_path, source.to_str().unwrap());
        assert!(media.sha256.is_none() && media.bytes.is_none());
        assert_eq!(hash_file(&dest.join("source.prproj")).unwrap(), doc_hash);
        if mode == "copy" {
            assert_eq!(std::fs::read_dir(dest.join("media")).unwrap().count(), 0);
        }
        let saved = fixture.0.join(format!("{mode}.fcproj"));
        s.execute("file.saveAs", json!({"path":saved})).unwrap();
        let mut reopened = Session::default();
        reopened.execute("file.open", json!({"path":saved})).unwrap();
        assert!(reopened.project.items.values().any(|i| matches!(&i.kind, filmcraft_project::ItemKind::Media(m) if m.offline)));
        assert_eq!(reopened.project.workflow, s.project.workflow);
    }
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(hash_file(&source).unwrap(), source_hash);
    assert_eq!(hash_file(&doc).unwrap(), doc_hash);
}
#[test]
fn premiere_invalid_commands_do_not_mutate_project_or_existing_destination() {
    let fixture = Fixture::new();
    let mut s = Session::default();
    let before = (*s.project).clone();
    for params in [
        json!(null),
        json!({"path":5}),
        json!({"path":fixture.0.join("source.prproj"),"mediaMode":"move"}),
        json!({"path":fixture.0.join("source.prproj"),"destination":fixture.0}),
        json!({"path":fixture.0.join("source.prproj"),"frameRate":{"num":0,"den":0}}),
    ] {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("premiere.import", params)));
        assert!(r.is_ok());
        assert!(r.unwrap().is_err());
        assert_eq!(*s.project, before);
    }
    assert!(fixture.0.join("source.png").is_file());
}

#[test]
fn premiere_workflow_versions_fail_closed_and_plain_projects_stay_upstream_compatible() {
    let plain = filmcraft_project::Project::default();
    let encoded = filmcraft_format::encode(&plain, false);
    let doc: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(doc["format"], filmcraft_format::FORMAT_ID);
    let mut modified = plain;
    modified.workflow = Some(Default::default());
    let mut doc: serde_json::Value = serde_json::from_slice(&filmcraft_format::encode(&modified, false)).unwrap();
    assert_eq!(doc["format"], filmcraft_format::WORKFLOW_FORMAT_ID);
    doc["project"]["workflow"]["version"] = json!(999);
    assert!(filmcraft_format::decode(&serde_json::to_vec(&doc).unwrap()).unwrap_err().to_string().contains("unsupported workflow"));
}

#[test]
fn premiere_link_defers_hashes_and_explicit_inspection_does_not_rewrite_provenance() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.png");
    let original = hash_file(&source).unwrap();
    let doc = fixture.0.join("source.prproj");
    let dest = fixture.0.join("deferred");
    let mut s = Session::default();
    let result = s.execute("premiere.import", json!({"path":doc,"destination":dest})).unwrap();
    assert_eq!(result["offlineMedia"], json!([]));
    let provenance = s.project.workflow.clone().unwrap();
    assert_eq!(provenance.imports[0].media[0].sha256, None, "linked import must not compute a full media hash");
    assert_eq!(provenance.imports[0].media[0].bytes, Some(original.1));
    let identity = crate::relink::identity_of(&*s.services, source.to_str().unwrap()).unwrap();
    assert_eq!(provenance.imports[0].media[0].identity, Some(identity));
    // Older provenance without the new optional fingerprint remains readable.
    let mut old = serde_json::to_value(&provenance.imports[0].media[0]).unwrap();
    old.as_object_mut().unwrap().remove("identity");
    assert!(serde_json::from_value::<filmcraft_project::workflow::SourceMedia>(old).unwrap().identity.is_none());
    let disk_record = std::fs::read(dest.join("provenance.json")).unwrap();
    let project = filmcraft_format::encode(&s.project, false);
    let observed = s.execute("premiere.hashMedia", json!({"import":0})).unwrap();
    assert_eq!(observed["media"][0]["sha256"], original.0);
    assert_eq!(observed["media"][0]["bytes"], original.1);
    assert_eq!(observed["media"][0]["matchesImport"], serde_json::Value::Null);
    // A later observation is not proof of what existed at import time.
    image::RgbaImage::from_pixel(16, 16, image::Rgba([220, 40, 30, 255])).save(&source).unwrap();
    let changed = hash_file(&source).unwrap();
    assert_ne!(changed.0, original.0);
    let observed = s.execute("premiere.hashMedia", json!({"import":0})).unwrap();
    assert_eq!(observed["media"][0]["sha256"], changed.0);
    assert_eq!(observed["media"][0]["matchesImport"], serde_json::Value::Null);
    assert_eq!(s.project.workflow.as_ref(), Some(&provenance));
    assert_eq!(std::fs::read(dest.join("provenance.json")).unwrap(), disk_record);
    assert_eq!(filmcraft_format::encode(&s.project, false), project);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.workflow.is_none(), "hash inspection must not add an undo step");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project.workflow.as_ref(), Some(&provenance));
}

#[test]
fn premiere_hash_inspection_rechecks_media_policy_and_copy_identity() {
    let fixture = Fixture::new();
    let mut s = Session::default();
    let dest = fixture.0.join("copy");
    s.execute("premiere.import", json!({"path":fixture.0.join("source.prproj"),"destination":dest,"mediaMode":"copy"})).unwrap();
    let media = s.project.workflow.as_ref().unwrap().imports[0].media[0].clone();
    let observed = s.execute("premiere.hashMedia", json!({"import":0})).unwrap();
    assert_eq!(observed["media"][0]["sha256"], json!(media.sha256));
    assert_eq!(observed["media"][0]["matchesImport"], true);
    image::RgbaImage::from_pixel(16, 16, image::Rgba([1, 2, 3, 255])).save(&media.imported_path).unwrap();
    let observed = s.execute("premiere.hashMedia", json!({"import":0})).unwrap();
    assert_eq!(observed["media"][0]["matchesImport"], false);
    std::fs::write(&media.imported_path, b"not media anymore").unwrap();
    let observed = s.execute("premiere.hashMedia", json!({"import":0})).unwrap();
    assert!(observed["media"][0]["sha256"].is_null());
    assert!(observed["media"][0]["error"].as_str().unwrap().contains("probe failed"));
    // A hand-edited project record must not bypass the document path / extension gate.
    let mut project = (*s.project).clone();
    let workflow = std::sync::Arc::make_mut(project.workflow.as_mut().unwrap());
    let import = std::sync::Arc::make_mut(&mut workflow.imports[0]);
    import.media[0].imported_path = "file://192.0.2.1/share/clip.mov".into();
    s.project = std::sync::Arc::new(project);
    let observed = s.execute("premiere.hashMedia", json!({"import":0})).unwrap();
    assert!(observed["media"][0]["error"].as_str().unwrap().contains("document path refused"));
    for params in [json!(null), json!([]), json!({}), json!({"import":-1}), json!({"import":0.5}), json!({"import":"0"}), json!({"import":u64::MAX})] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("premiere.hashMedia", params)));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}

#[test]
fn premiere_link_never_calls_media_hasher_but_copy_still_does() {
    let fixture = Fixture::new();
    let doc = fixture.0.join("source.prproj").canonicalize().unwrap();
    let calls = std::cell::Cell::new(0);
    let hasher = |path: &std::path::Path| {
        if path != doc {
            calls.set(calls.get() + 1);
            return Err(crate::EngineError::Other("media hasher unavailable".into()));
        }
        hash_file(path)
    };
    for mode in ["link", "copy"] {
        let mut s = Session::default();
        let result = crate::premiere::import_with_hasher(&mut s, &json!({"path":doc,"destination":fixture.0.join(mode),"mediaMode":mode}), &hasher).unwrap();
        if mode == "link" {
            assert_eq!(calls.get(), 0);
            assert_eq!(result["offlineMedia"], json!([]));
        } else {
            assert_eq!(calls.get(), 1);
            assert_eq!(result["offlineMedia"], json!(["source.png"]));
        }
    }
}

#[test]
fn premiere_plain_xml_larger_than_64_mib_reaches_parser_and_snapshot() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.prproj");
    let xml = std::fs::read_to_string(&source).unwrap();
    let mut padded = String::from("<!--");
    padded.extend(std::iter::repeat_n(' ', 65 * 1024 * 1024));
    padded.push_str("-->");
    padded.push_str(&xml);
    std::fs::write(&source, padded.as_bytes()).unwrap();
    let before = hash_file(&source).unwrap();
    let destination = fixture.0.join("large-plain-import");
    let mut session = Session::default();
    session.execute("premiere.import", json!({"path":source,"destination":destination,"mediaMode":"link"})).unwrap();
    assert_eq!(hash_file(&source).unwrap(), before);
    let imports = &session.project.workflow.as_ref().unwrap().imports;
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].source_sha256, before.0);
    assert_eq!(hash_file(std::path::Path::new(&imports[0].snapshot_path)).unwrap(), before);
}

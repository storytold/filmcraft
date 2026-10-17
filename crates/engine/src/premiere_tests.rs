//! Synthetic native import and persistence acceptance tests. MIT OR Apache-2.0.
use crate::{Session, premiere::hash_file};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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
            r#"<PremiereData><Sequence ObjectUID="s"><Name>Sequence 01</Name><TrackGroups><TrackGroup><Second ObjectRef="1"/></TrackGroup></TrackGroups></Sequence><VideoTrackGroup ObjectID="1"><TrackGroup><Tracks><Track ObjectURef="t"/></Tracks></TrackGroup></VideoTrackGroup><VideoClipTrack ObjectUID="t"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="2"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack><VideoClipTrackItem ObjectID="2"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="3"/></ClipTrackItem></VideoClipTrackItem><SubClip ObjectID="3"><Name>source.png</Name></SubClip><Media><Title>source.png</Title><FilePath>{}</FilePath></Media></PremiereData>"#,
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
        assert_eq!(imported == source, mode == "link");
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

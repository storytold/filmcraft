//! Interchange documents (EDL, FCP7 XML, AAF, …) imported while a bin is open in the Project
//! panel land in that bin, as media files do, not at the project root (#2036).

use filmcraft_project::{BinId, ItemId};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{make_movie, session_with, tmp_dir};

const RATE: &str = "<rate><timebase>24</timebase><ntsc>FALSE</ntsc></rate>";

/// A one-clip FCP7 XML sequence of `shot.mov`.
fn fcp7_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xmeml>
<xmeml version="4">
  <sequence id="sequence-1">
    <name>Cut</name>
    <duration>24</duration>
    {RATE}
    <media>
      <video>
        <format><samplecharacteristics>{RATE}<width>1920</width><height>1080</height></samplecharacteristics></format>
        <track>
          <clipitem id="clipitem-1">
            <name>shot</name>
            {RATE}
            <start>0</start><end>24</end><in>0</in><out>24</out>
            <file id="file-1">
              <name>shot.mov</name>
              <pathurl>shot.mov</pathurl>
              {RATE}
              <duration>24</duration>
            </file>
          </clipitem>
        </track>
      </video>
    </media>
  </sequence>
</xmeml>
"#
    )
}

const EDL: &str = "TITLE: Cut\nFCM: NON-DROP FRAME\n\n001  AX       V     C        00:00:00:00 00:00:01:00 01:00:00:00 01:00:01:00\n* FROM CLIP NAME: shot.mov\n* SOURCE FILE: shot.mov\n";

/// Bins "Avis" ▸ "Kordex", as in the report.
fn nested_bins(s: &mut Session) -> (BinId, BinId) {
    let outer = s.execute("file.newBin", json!({"name": "Avis"})).unwrap()["bin"].as_u64().unwrap();
    let inner = s.execute("file.newBin", json!({"name": "Kordex", "parent": outer})).unwrap()["bin"].as_u64().unwrap();
    (BinId(outer), BinId(inner))
}

/// The items an import added (every one of them: the sequences and the media they use).
fn added(s: &Session, before: &[ItemId]) -> Vec<ItemId> {
    s.project.items.keys().copied().filter(|i| !before.contains(i)).collect()
}

/// Whether `id` is in `bin` or one of its sub-bins (a document's own bins come along inside it).
fn within(s: &Session, bin: BinId, id: ItemId) -> bool {
    let mut all = Vec::new();
    if let Some(b) = s.project.root.find_bin(bin) {
        b.all_items(&mut all);
    }
    all.contains(&id)
}

fn sequences_of(r: &Value) -> Vec<ItemId> {
    let docs = r["documents"].as_array().cloned().unwrap_or_default();
    let mut seqs: Vec<ItemId> = r["sequences"].as_array().cloned().unwrap_or_default().iter().filter_map(Value::as_u64).map(ItemId).collect();
    if seqs.is_empty() {
        seqs = docs.iter().flat_map(|d| d["sequences"].as_array().cloned().unwrap_or_default()).filter_map(|v| v.as_u64()).map(ItemId).collect();
    }
    seqs
}

#[test]
fn an_interchange_document_lands_in_the_bin_it_was_imported_into() {
    let d = tmp_dir("ix-bin");
    make_movie(&d.join("shot.mov"), filmcraft_media::DemoScene::OceanSunset, 64, 36, 24);
    std::fs::write(d.join("cut.xml"), fcp7_xml()).unwrap();
    std::fs::write(d.join("cut.edl"), EDL).unwrap();
    for doc in ["cut.xml", "cut.edl"] {
        let mut s = Session::default();
        let (_, kordex) = nested_bins(&mut s);
        let before_items: Vec<ItemId> = s.project.items.keys().copied().collect();
        let before = s.project.clone();
        let path = d.join(doc).to_string_lossy().into_owned();
        let r = s.execute("file.import", json!({"paths": [path], "bin": kordex.0})).unwrap();
        let seqs = sequences_of(&r);
        assert_eq!(seqs.len(), 1, "{doc}: {r}");
        let new = added(&s, &before_items);
        assert!(new.len() >= 2, "{doc}: a sequence and its media: {new:?}");
        for id in &new {
            assert!(within(&s, kordex, *id), "{doc}: item {id:?} is in the bin the import was aimed at");
        }
        // the import (and Link Media) undo back to the project as it was
        while *s.project != *before {
            s.execute("edit.undo", json!({})).unwrap();
        }
        assert!(s.project.root.find_bin(kordex).is_some_and(|b| b.children.is_empty()), "{doc}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_interchange_document_aimed_at_an_unknown_bin_lands_at_the_root() {
    let d = tmp_dir("ix-bin-unknown");
    std::fs::write(d.join("cut.xml"), fcp7_xml()).unwrap();
    let mut s = Session::default();
    let (avis, _) = nested_bins(&mut s);
    let before: Vec<ItemId> = s.project.items.keys().copied().collect();
    let path = d.join("cut.xml").to_string_lossy().into_owned();
    s.execute("file.import", json!({"paths": [path], "bin": 987_654_321u64})).unwrap();
    let new = added(&s, &before);
    assert!(!new.is_empty());
    for id in new {
        assert!(!within(&s, avis, id), "item {id:?} is not in a bin it wasn't aimed at");
        assert!(within(&s, s.project.root.id, id), "item {id:?} is in the project");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// AAF goes through the same import, from `file.import` and from `file.importAaf`.
#[test]
fn an_aaf_lands_in_the_bin_from_file_import_and_file_import_aaf() {
    let d = tmp_dir("ix-bin-aaf");
    let a = d.join("a.mov");
    make_movie(&a, filmcraft_media::DemoScene::Aurora, 64, 36, 24);
    let (mut s, _, seq) = session_with(&[&a]);
    let aaf = d.join("cut.aaf").to_string_lossy().into_owned();
    s.execute("file.exportAaf", json!({"path": aaf, "audio": "embedded", "sequence": seq.0})).unwrap();
    for (cmd, params) in [("file.import", json!({"paths": [aaf]})), ("file.importAaf", json!({"path": aaf}))] {
        let mut t = Session::default();
        let (_, kordex) = nested_bins(&mut t);
        let before: Vec<ItemId> = t.project.items.keys().copied().collect();
        let mut params = params;
        params["bin"] = json!(kordex.0);
        let r = t.execute(cmd, params).unwrap();
        assert_eq!(sequences_of(&r).len(), 1, "{cmd}: {r}");
        let new = added(&t, &before);
        assert!(new.len() >= 2, "{cmd}: {new:?}");
        for id in new {
            assert!(within(&t, kordex, id), "{cmd}: item {id:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

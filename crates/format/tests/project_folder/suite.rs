#[path = "../../examples/project_folder/converter.rs"]
mod converter;

use converter::{ROOT, pack, unpack};
use filmcraft_format::atomic_write_new;
use serde_json::Value;
use serde_json::json;
use std::{fs, io, path::Path};
use tempfile::tempdir;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

#[test]
fn legacy_roundtrips_match_ordinary_migration() {
    let tmp = tempdir().unwrap();
    for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")).unwrap() {
        let src = entry.unwrap().path();
        if src.extension().is_none_or(|ext| ext != "fcproj") {
            continue;
        }
        let folder = tmp.path().join(src.file_stem().unwrap());
        let out = tmp.path().join(src.file_name().unwrap());
        let bytes = fs::read(&src).unwrap();
        pack(&src, &folder, atomic_write_new).unwrap();
        unpack(&folder, &out).unwrap();
        let result = fs::read(out).unwrap();
        let schema = craft_fcproj_format::detect_bytes(&bytes).unwrap();
        let expected = craft_fcproj_format::migrate_with(craft_fcproj_format::MIGRATIONS, serde_json::from_slice(&bytes).unwrap(), schema).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&result).unwrap(), expected);
        assert_eq!(filmcraft_format::decode(&result).unwrap().project, filmcraft_format::decode(&bytes).unwrap().project);
    }
}

fn current() -> Value {
    let loaded = filmcraft_format::decode(&fixture("v1-edit.fcproj")).unwrap();
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&loaded.project, false)).unwrap();
    doc["extra"] = json!({"unknown":[null,{}]});
    doc["view"] = json!({"open_sequences":[21,73],"active_sequence":73});
    let p = &mut doc["project"];
    let mut seq = p["items"]["21"].clone();
    seq["id"] = json!(73);
    seq["kind"]["Sequence"]["video_tracks"][0]["items"][0]["item"] = json!(21);
    p["items"]["73"] = seq;
    let mut unfiled = p["items"]["5"].clone();
    unfiled["id"] = json!(74);
    p["items"]["74"] = unfiled;
    p["root"]["children"][0]["Bin"]["children"].as_array_mut().unwrap().push(json!({"Bin":{"id":88,"name":"Footage","children":[{"Item":73}]}}));
    p["next_id"] = json!(900000);
    doc
}

#[test]
fn offline_sources_and_proxies_follow_the_document_and_views_and_opaque_fields_survive() {
    let tmp = tempdir().unwrap();
    let base = tmp.path();
    let src = base.join("edit/cut.fcproj");
    let folder = base.join("archive/day1/cut");
    let out = base.join("delivery/restored.fcproj");
    for parent in [&src, &folder, &out] {
        fs::create_dir_all(parent.parent().unwrap()).unwrap();
    }
    let mut expected = current();
    let orig = "/project/items/5/kind/Media/media";
    let proxy = "/project/items/5/kind/Media/proxy";
    expected.pointer_mut("/project/items/5/kind/Media").unwrap()["proxy"] = json!({"File":{"path":"../proxies/p.mov"}});
    *expected.pointer_mut(orig).unwrap() = json!({"File":{"path":"media/a.mov"}});
    let bytes = serde_json::to_vec(&expected).unwrap();
    fs::write(&src, &bytes).unwrap();

    pack(&src, &folder, atomic_write_new).unwrap();
    unpack(&folder, &out).unwrap();
    assert_eq!(fs::read(&src).unwrap(), bytes);
    let result = fs::read(&out).unwrap();
    let mut actual: Value = serde_json::from_slice(&result).unwrap();
    let view = filmcraft_format::decode(&result).unwrap().view.unwrap();
    assert_eq!(view.active_sequence, Some(filmcraft_project::ItemId(73)));
    assert_eq!(view, filmcraft_format::decode(&bytes).unwrap().view.unwrap());
    for ptr in [orig, proxy] {
        let value = actual.pointer_mut(ptr).unwrap();
        assert_eq!(
            craft_docpath::resolve_absolute(value["File"]["path"].as_str().unwrap(), Some(&out)).unwrap(),
            craft_docpath::resolve_absolute(expected.pointer(ptr).unwrap()["File"]["path"].as_str().unwrap(), Some(&src)).unwrap()
        );
        *value = expected.pointer(ptr).unwrap().clone();
    }
    assert_eq!(actual, expected);
}

#[test]
fn occupied_destinations_and_write_failures_preserve_existing_data() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("input.fcproj");
    let bytes = fixture("v1-edit.fcproj");
    fs::write(&src, &bytes).unwrap();
    let mut perms = fs::metadata(&src).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&src, perms).unwrap();
    let folder = tmp.path().join("folder");
    fs::create_dir(&folder).unwrap();
    assert_eq!(pack(&src, &folder, atomic_write_new).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    for fail_at in [1, 3] {
        let dest = tmp.path().join(format!("failed-{fail_at}"));
        let mut count = 0;
        let err = pack(&src, &dest, |p, bytes| {
            count += 1;
            if count == fail_at {
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            atomic_write_new(p, bytes)
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
        assert!(!dest.join(ROOT).exists());
        assert!(unpack(&dest, &tmp.path().join("should-not-exist")).is_err());
    }
    let dest = tmp.path().join("complete");
    pack(&src, &dest, atomic_write_new).unwrap();
    assert_eq!(unpack(&dest, &src).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&src).unwrap(), bytes);
}

#[test]
fn damaged_missing_and_unknown_records_refuse_export() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("input");
    fs::write(&src, fixture("v1-edit.fcproj")).unwrap();
    let dir = tmp.path().join("dir");
    pack(&src, &dir, atomic_write_new).unwrap();
    let root = dir.join(ROOT);
    let mut m: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    let file = dir.join(m["items"][0][1].as_str().unwrap());
    let out = tmp.path().join("export");
    fs::write(&file, b"{}").unwrap();
    for _ in 0..2 {
        let error = unpack(&dir, &out).unwrap_err().to_string();
        assert!(error.contains(&format!("item {}", m["items"][0][0])), "{error}");
        assert!(!out.exists());
        if file.exists() {
            fs::remove_file(&file).unwrap();
        }
    }
    m["layout"] = json!(2);
    fs::write(root, serde_json::to_vec(&m).unwrap()).unwrap();
    assert!(unpack(&dir, &out).unwrap_err().to_string().contains("unsupported"));
}

#[test]
fn verification_precedes_commit_and_interrupted_publication_is_unreadable() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::write(&src, fixture("v1-edit.fcproj")).unwrap();
    for corrupt in [true, false] {
        let dir = tmp.path().join(format!("dest-{corrupt}"));
        let result = pack(&src, &dir, |p, bytes| {
            if corrupt {
                atomic_write_new(p, b"corrupted")
            } else if p.file_name().unwrap() == ROOT {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                atomic_write_new(p, bytes)
            }
        });
        assert!(result.is_err());
        assert!(!dir.join(ROOT).exists());
    }
}

#[cfg(unix)]
#[test]
fn foreign_absolute_paths_are_preserved_in_both_directions() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let folder = tmp.path().join("folder");
    let out = tmp.path().join("out");
    let mut doc = current();
    let media = doc.pointer_mut("/project/items/5/kind/Media").unwrap();
    media["media"] = json!({"File":{"path":r"C:\Camera\A.mov"}});
    media["proxy"] = json!({"File":{"path":r"\\server\media\A.mov"}});
    fs::write(&src, serde_json::to_vec(&doc).unwrap()).unwrap();
    pack(&src, &folder, atomic_write_new).unwrap();
    unpack(&folder, &out).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(out).unwrap()).unwrap(), doc);
}

#[test]
fn invalid_inputs_never_publish_a_destination() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input.fcproj");
    let folder = tmp.path().join("folder");
    let mut project = current();
    project["schema_version"] = json!(filmcraft_format::SCHEMA_VERSION + 1);
    fs::write(&input, serde_json::to_vec(&project).unwrap()).unwrap();
    assert!(pack(&input, &folder, atomic_write_new).is_err());
    assert!(!folder.exists());

    fs::write(&input, fixture("v1-edit.fcproj")).unwrap();
    pack(&input, &folder, atomic_write_new).unwrap();
    let root = folder.join(ROOT);
    let original: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    let mut repeated = original.clone();
    repeated["items"].as_array_mut().unwrap().push(original["items"][0].clone());
    let mut wrong_id = original.clone();
    wrong_id["items"][0][0] = json!(900000);
    let mut invalid_digest = original;
    invalid_digest["envelope"] = json!("../input.fcproj");
    let out = tmp.path().join("output.fcproj");
    for manifest in [repeated, wrong_id, invalid_digest] {
        fs::write(&root, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(unpack(&folder, &out).is_err());
        assert!(!out.exists());
    }
}

#[test]
fn editing_one_sequence_changes_only_its_record() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.fcproj");
    let mut project = current();
    let write_snapshot = |name: &str, doc: &Value| {
        fs::write(&source, serde_json::to_vec(doc).unwrap()).unwrap();
        let dir = tmp.path().join(name);
        pack(&source, &dir, atomic_write_new).unwrap();
        serde_json::from_slice::<Value>(&fs::read(dir.join(ROOT)).unwrap()).unwrap()
    };
    let first = write_snapshot("first", &project);
    project["project"]["items"]["73"]["name"] = json!("Revised cut");
    let second = write_snapshot("second", &project);
    assert_eq!(first["bins"], second["bins"]);
    assert_eq!(first["envelope"], second["envelope"]);
    let items = |v: &Value| {
        let entries: Vec<(u64, String)> = serde_json::from_value(v["items"].clone()).unwrap();
        entries.into_iter().collect::<std::collections::BTreeMap<_, _>>()
    };
    let mut old = items(&first);
    let mut new = items(&second);
    let old_cut = old.remove(&73).unwrap();
    let new_cut = new.remove(&73).unwrap();
    assert_ne!(old_cut, new_cut);
    assert_eq!(old, new);
    let cut: Value = serde_json::from_slice(&fs::read(tmp.path().join("second").join(new_cut)).unwrap()).unwrap();
    assert_eq!(cut, project["project"]["items"]["73"]);
}

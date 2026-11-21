//! Tests for importing several media files in one `file.import` command (#1324).

use filmcraft_media::DemoScene;
use serde_json::json;

use crate::media_test_util::{make_movie, tmp_dir};
use crate::{ItemId, Session};

#[test]
fn import_command_imports_every_selected_mp4() {
    let dir = tmp_dir("multi-import");
    let scenes = [DemoScene::Aurora, DemoScene::Dunes, DemoScene::Forest];
    let paths: Vec<_> = scenes.iter().map(|scene| dir.join(format!("{scene:?}.mp4"))).collect();
    for (path, scene) in paths.iter().zip(scenes) {
        make_movie(path, scene, 64, 36, 4);
    }
    let strings: Vec<String> = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect();
    let mut session = Session::default();
    let r = session.execute("file.import", json!({"paths": strings})).unwrap_or_else(|e| panic!("multi-path import failed: {e}"));
    let item_ids = r["items"].as_array().unwrap_or_else(|| panic!("no items in {r}"));
    assert_eq!(item_ids.len(), paths.len(), "{r}");
    let imported = item_ids
        .iter()
        .map(|id| ItemId(id.as_u64().unwrap_or_else(|| panic!("bad item id {id}"))))
        .filter_map(|item| session.project.item(item))
        .map(|item| item.name.clone())
        .collect::<Vec<_>>();
    for path in paths {
        let name = path.file_name().unwrap_or_else(|| panic!("no filename in {path:?}")).to_string_lossy().into_owned();
        assert!(imported.contains(&name), "missing {name}; imported {imported:?}");
    }
}

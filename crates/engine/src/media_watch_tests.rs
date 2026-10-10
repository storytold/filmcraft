//! Media files changing on disk while the project is open (Settings ▸ Media ▸ Automatically
//! refresh growing files, `media_watch`): a re-rendered movie is read again (new length, new
//! picture, new identity) without an undo step, a file still being written is tried again, and
//! the background look does the same.

use filmcraft_media::DemoScene;
use serde_json::json;

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};

const W: u32 = 160;
const H: u32 = 90;

/// Write a movie over `path` so its modification time moves even on coarse file systems.
fn rerender(path: &std::path::Path, scene: DemoScene, frames: i64) {
    std::thread::sleep(std::time::Duration::from_millis(30));
    make_movie(path, scene, W, H, frames);
}

fn refresh(s: &mut Session) -> serde_json::Value {
    s.execute("media.refreshChanged", json!({})).unwrap()
}

/// What frame 3 of a fresh import of `scene` looks like.
fn reference(name: &str, scene: DemoScene, frames: i64) -> Vec<u8> {
    let root = tmp_dir(name);
    let f = root.join("ref.mov");
    make_movie(&f, scene, W, H, frames);
    let (mut r, _, _) = session_with(&[&f]);
    frame_rgba(&mut r, 3, 1.0).2
}

#[test]
fn a_rerendered_movie_is_read_again_without_an_undo_step() {
    let root = tmp_dir("watch-rerender");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let (mut s, items, _) = session_with(&[&a]);
    let item = items[0];
    let before = frame_rgba(&mut s, 3, 1.0).2;
    let length = s.project.item(item).unwrap().duration();
    s.saved_revision = s.revision;
    let undo = s.history.undo.len();
    // First look: remembered, nothing to read.
    assert_eq!(refresh(&mut s), json!({"refreshed": []}));
    rerender(&a, DemoScene::CityNight, 24);
    assert_eq!(refresh(&mut s), json!({"refreshed": [item.0]}));
    let m = s.project.item(item).unwrap().as_media().unwrap().clone();
    assert_eq!(m.info.duration.0, length.0 * 2, "the new length");
    assert_eq!(m.identity, crate::relink::identity_of(&*s.services, &a.to_string_lossy()).ok(), "relinking compares with the new file");
    let after = frame_rgba(&mut s, 3, 1.0).2;
    assert!(psnr(&after, &reference("watch-rerender-ref", DemoScene::CityNight, 24)) > 40.0, "the new picture");
    assert!(psnr(&after, &before) < 30.0, "not the old one");
    assert_eq!(s.history.undo.len(), undo, "the file changed, not the edit");
    assert!(!s.is_dirty());
    assert_eq!(refresh(&mut s), json!({"refreshed": []}), "acted on once");
}

#[test]
fn a_file_still_being_written_is_tried_again() {
    let root = tmp_dir("watch-partial");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let (mut s, items, _) = session_with(&[&a]);
    refresh(&mut s);
    // The other app has only written the start of the file so far.
    let full = root.join("full.mov");
    make_movie(&full, DemoScene::CityNight, W, H, 24);
    let bytes = std::fs::read(&full).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&a, &bytes[..64]).unwrap();
    assert_eq!(refresh(&mut s), json!({"refreshed": []}));
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&a, &bytes).unwrap();
    assert_eq!(refresh(&mut s), json!({"refreshed": [items[0].0]}));
}

#[test]
fn a_background_look_reads_changed_media_again() {
    let root = tmp_dir("watch-scan");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let (mut s, items, _) = session_with(&[&a]);
    let look = |s: &mut Session| {
        s.start_media_scan();
        let t0 = std::time::Instant::now();
        loop {
            if let Some(r) = s.poll_media_scan() {
                return r.unwrap();
            }
            assert!(t0.elapsed().as_secs() < 10, "the look never finished");
            std::thread::yield_now();
        }
    };
    assert_eq!(look(&mut s), json!({"refreshed": []}));
    rerender(&a, DemoScene::CityNight, 24);
    assert_eq!(look(&mut s), json!({"refreshed": [items[0].0]}));
    assert!(s.media_scan.is_none());
}

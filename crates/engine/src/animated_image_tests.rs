//! Animated GIF and APNG files import as movie clips that play every frame with its transparency
//! (#402), and reopen that way from a saved project.

use filmcraft_media::{FrameRequest, MediaKind};
use filmcraft_project::ItemId;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

/// A 16×8 animated GIF: frame `i` is red `40 * i` with its left half transparent, 100 ms each.
fn write_gif(path: &std::path::Path, frames: u8) {
    let mut out = Vec::new();
    {
        let mut e = image::codecs::gif::GifEncoder::new(&mut out);
        let frames = (0..frames).map(|i| {
            let img = image::RgbaImage::from_fn(16, 8, |x, _| image::Rgba([40 * i, 0, 0, if x < 8 { 0 } else { 255 }]));
            image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(100, 1))
        });
        e.encode_frames(frames).unwrap();
    }
    std::fs::write(path, out).unwrap();
}

fn item_of(r: &Value) -> ItemId {
    ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")))
}

/// The pixel at (`x`, 0) at media time `ms`.
fn pixel_at(s: &Session, item: ItemId, ms: i64, x: usize) -> [u8; 4] {
    let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let f = src.video_frame(FrameRequest::full(Tick(ms * TICKS_PER_SECOND / 1000))).unwrap();
    match &f.data {
        filmcraft_frame::PixelData::Rgba8(p) => [p[4 * x], p[4 * x + 1], p[4 * x + 2], p[4 * x + 3]],
        _ => panic!("rgba frames"),
    }
}

#[test]
fn animated_gif_imports_as_a_clip_and_reopens() {
    let dir = tmp_dir("animated-gif");
    let gif = dir.join("loop.gif");
    write_gif(&gif, 4);
    let mut s = Session::default();
    let id = item_of(&s.execute("file.import", json!({"paths": [gif.to_string_lossy()]})).unwrap());
    let m = s.project.item(id).unwrap().as_media().unwrap();
    assert_eq!(m.info.kind, MediaKind::Movie, "not a still showing the first frame");
    assert_eq!(m.info.duration, Tick(4 * TICKS_PER_SECOND / 10));
    assert!(m.info.video.as_ref().unwrap().has_alpha);
    assert_eq!(pixel_at(&s, id, 0, 12), [0, 0, 0, 255]);
    assert_eq!(pixel_at(&s, id, 250, 12), [80, 0, 0, 255], "the third frame plays");
    assert_eq!(pixel_at(&s, id, 250, 2)[3], 0, "transparent pixels stay transparent, not black");

    let proj = dir.join("anim.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": proj.to_string_lossy()})).unwrap();
    assert_eq!(t.project.item(id).unwrap().as_media().unwrap().info.kind, MediaKind::Movie);
    assert_eq!(pixel_at(&t, id, 350, 12), [120, 0, 0, 255], "frames reopen through the media pool");
    let _ = std::fs::remove_dir_all(&dir);
}

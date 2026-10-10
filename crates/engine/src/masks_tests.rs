use serde_json::json;

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;

fn demo() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks[0].items[0].clone();
    // middle of the first V1 clip (past its fade from black)
    s.set_playhead(it.start + Tick(it.duration.0 / 2));
    s.state.selection = vec![it.id];
    (s, it.id)
}

fn px(img: &filmcraft_render::Image, fx: f32, fy: f32) -> [f32; 4] {
    img.get(((img.w as f32 * fx) as usize).min(img.w - 1), ((img.h as f32 * fy) as usize).min(img.h - 1))
}

fn dist(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max)
}

#[test]
fn masked_effect_applies_only_inside_the_mask() {
    let (mut s, clip) = demo();
    let plain = s.render_program(0.25).unwrap();
    s.execute("effects.apply", json!({"effect": "invert"})).unwrap();
    let inverted = s.render_program(0.25).unwrap();
    // ellipse mask on Invert, in the left half of the frame
    let r = s.execute("masks.add", json!({"effect": "invert", "shape": "ellipse", "center": [480, 540], "size": [600, 600]})).unwrap();
    assert_eq!(r["name"], "Mask (1)");
    assert_eq!(r["mask"], 0);
    let masked = s.render_program(0.25).unwrap();
    // inside: inverted; outside: untouched
    assert!(dist(px(&masked, 0.25, 0.5), px(&inverted, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&masked, 0.8, 0.5), px(&plain, 0.8, 0.5)) < 1e-4);
    assert!(dist(px(&plain, 0.25, 0.5), px(&inverted, 0.25, 0.5)) > 0.05, "invert changes the picture");
    // inverted mask swaps the regions
    s.execute("masks.set", json!({"clip": clip.0, "effect": "invert", "mask": 0, "inverted": true})).unwrap();
    let inv = s.render_program(0.25).unwrap();
    assert!(dist(px(&inv, 0.25, 0.5), px(&plain, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&inv, 0.8, 0.5), px(&inverted, 0.8, 0.5)) < 1e-4);
    // undo ×2 → no mask
    s.undo();
    s.undo();
    let back = s.render_program(0.25).unwrap();
    assert_eq!(back.px, inverted.px);
    assert!(s.state.selected_mask.is_none(), "selection follows undo");
}

#[test]
fn opacity_mask_cuts_out_the_clip() {
    let (mut s, clip) = demo();
    // V1 only: an opacity mask leaves black outside
    s.execute("masks.add", json!({"clip": clip.0, "shape": "polygon", "center": [960, 540], "size": [960, 540]})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let img = s.render_program(0.25).unwrap();
    assert!(px(&img, 0.5, 0.5)[3] > 0.99, "inside opaque");
    let corner = px(&img, 0.05, 0.05);
    assert!(corner[0] + corner[1] + corner[2] < 1e-4, "outside cut: {corner:?}");
    let list = s.execute("masks.list", json!({})).unwrap();
    let m = &list["masks"][0];
    assert_eq!(m["effectId"], "opacity");
    assert_eq!(m["path"]["vertices"].as_array().unwrap().len(), 4);
    assert_eq!(m["selected"], true);
}

#[test]
fn keyframed_mask_path_interpolates() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    let t1 = it.start + rate.tick_of(10);
    s.set_playhead(t1);
    s.execute("masks.translate", json!({"delta": [100, -50]})).unwrap();
    // feather keyframes too, through the generic keyframe command
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather", "value": 30.0})).unwrap();
    let mid = it.start + rate.tick_of(5);
    let l = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    let v0 = &l["masks"][0]["path"]["vertices"][0]["p"];
    assert!((v0[0].as_f64().unwrap() - 350.0).abs() < 1e-6, "{v0}");
    assert!((v0[1].as_f64().unwrap() - 275.0).abs() < 1e-6, "{v0}");
    assert_eq!(l["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    let end = s.execute("masks.list", json!({"time": t1.0})).unwrap();
    assert_eq!(end["masks"][0]["feather"], 30.0);
    // vertex edit at a keyframe replaces that keyframe; handles mirror unless broken
    s.execute("masks.moveVertex", json!({"vertex": 1, "handle": "out", "delta": [0, 40]})).unwrap();
    let e = s.execute("masks.list", json!({})).unwrap();
    let v1 = &e["masks"][0]["path"]["vertices"][1];
    assert_eq!(v1["out"], json!([0.0, 40.0]));
    assert_eq!(v1["in"], json!([-0.0, -40.0]));
    assert_eq!(e["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    // add / remove vertices keep all keyframes interpolable
    s.execute("masks.addVertex", json!({"after": 0, "at": [500, 220]})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);
    s.execute("masks.removeVertex", json!({"vertex": 1})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 4);
}

#[test]
fn add_vertex_on_animated_path_clamps_oversized_index_and_rejects_empty_path() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"delta": [5, 0]})).unwrap();
    // an index past the last vertex appends instead of panicking
    s.execute("masks.addVertex", json!({"after": 100, "at": [1, 1]})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);

    // an empty animated path is a parameter error and leaves the mask alone
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "path": {"vertices": [], "closed": false}})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 1, "param": "path"})).unwrap();
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"mask": 1, "delta": [5, 0]})).unwrap();
    let e = s.execute("masks.addVertex", json!({"mask": 1, "after": 0, "at": [1, 1]})).unwrap_err().to_string();
    assert!(e.contains("no vertices"), "{e}");
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][1]["path"]["vertices"].as_array().unwrap().len(), 0);

    // a static empty path still takes its first vertex
    s.execute("masks.add", json!({"effect": "opacity", "path": {"vertices": [], "closed": false}})).unwrap();
    s.execute("masks.addVertex", json!({"mask": 2, "after": 0, "at": [1, 1]})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][2]["path"]["vertices"].as_array().unwrap().len(), 1);
}

#[test]
fn merged_drag_is_one_undo_step_and_project_round_trips() {
    let (mut s, _) = demo();
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    s.execute("masks.add", json!({"effect": "gaussian_blur", "shape": "ellipse"})).unwrap();
    let n = s.history.undo.len();
    for _ in 0..5 {
        s.execute("masks.translate", json!({"delta": [3, 1], "merge": "drag-1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1);
    s.execute("masks.set", json!({"mode": "subtract", "trackMethod": "position", "expansion": -4})).unwrap();
    let bytes = filmcraft_format::encode(&s.project, false);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(&back, &*s.project);
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["mode"], "Subtract");
    assert_eq!(l["masks"][0]["trackMethod"], "Position");
    assert!(s.execute("masks.add", json!({"effect": "motion"})).is_err(), "Motion has no masks");
}

// ---------------------------------------------------------------- tracking

/// A 320×240 24 fps clip of a textured disc moving under a known similarity motion over a static
/// textured background.
struct Moving {
    info: filmcraft_media::MediaInfo,
}

fn texture(u: f64, v: f64) -> f64 {
    let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
    let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
    let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
    x ^= x >> 13;
    x = x.wrapping_mul(0x5bd1_e995);
    let h = (x >> 40) as f64 / (1u64 << 24) as f64;
    (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0)
}

/// Ground-truth pose of the disc at frame `k`: translation, 1.5°/frame rotation, +1 %/frame scale.
fn truth(k: f64) -> filmcraft_geom::Affine {
    filmcraft_geom::Affine::motion(
        filmcraft_geom::Vec2::new(120.0 + 3.0 * k, 110.0 + 1.2 * k),
        filmcraft_geom::Vec2::new(1.0 + 0.01 * k, 1.0 + 0.01 * k),
        1.5 * k,
        filmcraft_geom::Vec2::ZERO,
    )
}

impl filmcraft_media::MediaSource for Moving {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let k = rate.frame_at(req.time) as f64;
        let inv = truth(k).inverse().unwrap();
        let (w, h) = (320usize, 240usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = filmcraft_geom::Vec2::new(x as f64 + ox, y as f64 + oy);
                    let o = inv.apply(p);
                    acc += if o.length() < 70.0 { texture(o.x + 200.0, o.y + 300.0) } else { 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5) };
                }
                let v = (acc / 4.0 * 255.0).round() as u8;
                px[(y * w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn tracking_session() -> (Session, ClipId) {
    use filmcraft_media::MediaSource;
    let rate = filmcraft_time::FrameRate::FPS_24;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        320,
        240,
        rate,
        rate.tick_of(48),
    );
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("track");
    let item = p.add_item(
        "moving",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let seq = p.new_sequence("s", filmcraft_project::SequenceSettings { width: 320, height: 240, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let ti =
        p.make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: std::sync::Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.state.selection = vec![clip];
    s.media.insert(item, std::sync::Arc::new(Moving { info }));
    (s, clip)
}

#[test]
fn tracking_follows_known_motion_forward_and_backward() {
    let (mut s, _) = tracking_session();
    let rate = filmcraft_time::FrameRate::FPS_24;
    s.set_playhead(rate.tick_of(10));
    // a circle of radius 45 inside the disc at frame 10
    let c = truth(10.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let path0 = s.execute("masks.list", json!({})).unwrap()["masks"][0]["path"].clone();
    let p0 = crate::masks::path_from_json(&path0).unwrap();
    // object coordinates of the mask vertices (they ride on the disc)
    let inv10 = truth(10.0).inverse().unwrap();
    let obj: Vec<filmcraft_geom::Vec2> = p0.vertices.iter().map(|v| inv10.apply(v.p)).collect();
    let r = s.execute("masks.track", json!({"direction": "forward", "frames": 12, "wait": true})).unwrap();
    assert_eq!(r["frames"], 12, "{r}");
    assert!(s.mask_jobs.is_empty());
    let r = s.execute("masks.track", json!({"direction": "backward", "frames": 8, "wait": true, "method": "positionScaleRotation"})).unwrap();
    assert_eq!(r["frames"], 8);
    let l = s.execute("masks.list", json!({})).unwrap();
    let keys: Vec<i64> = l["masks"][0]["pathKeyframes"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(keys.len(), 21, "frames 2..=22: {keys:?}");
    let mut worst = 0.0f64;
    for f in [2i64, 6, 10, 14, 18, 22] {
        let t = rate.tick_of(f);
        let m = s.execute("masks.list", json!({"time": t.0})).unwrap();
        let path = crate::masks::path_from_json(&m["masks"][0]["path"]).unwrap();
        for (v, o) in path.vertices.iter().zip(&obj) {
            worst = worst.max((v.p - truth(f as f64).apply(*o)).length());
        }
    }
    eprintln!("mask tracking: max vertex error {worst:.3} px over -8..+12 frames");
    assert!(worst < 1.5, "max vertex error {worst} px");
    // the two tracking runs are two undo steps
    let h = s.execute("history.list", json!({})).unwrap();
    let undo: Vec<&str> = h["undo"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(&undo[undo.len() - 2..], ["Track Mask", "Track Mask"], "{undo:?}");
}

#[test]
fn tracking_runs_in_the_background_and_can_be_cancelled() {
    let (mut s, _) = tracking_session();
    let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let r = s.execute("masks.track", json!({"direction": "forward"})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert_eq!(r["frames"], 47);
    assert!(s.execute("masks.track", json!({})).is_err(), "one job per mask");
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.mask_jobs.is_empty() && t0.elapsed().as_secs() < 60 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.mask_jobs.is_empty());
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).unwrap().clone();
    assert_eq!(j["finished"], true);
    let n = s.execute("masks.list", json!({})).unwrap()["masks"][0]["pathKeyframes"].as_array().unwrap().len();
    assert!(n < 48, "stopped early ({n} keyframes)");
    assert!(s.execute("masks.track", json!({"direction": "backward"})).is_err(), "nothing before the first frame");
}

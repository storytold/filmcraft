//! Proxies end to end: create (background job) + attach + toggle gives the same composite modulo
//! resolution (scale-aware effects and Motion), attach checks, export ignores proxies, toggling
//! doesn't dirty the project, ingest copies on import.

use filmcraft_media::DemoScene;
use filmcraft_project::{ParamValue, find_effect};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};

const W: u32 = 320;
const H: u32 = 180;

/// Give V1's clip a Motion move and a blur, so proxies must render effects scale-aware.
fn add_effects(s: &mut Session) {
    let seq = s.state.active_sequence.unwrap();
    let mut p = (*s.project).clone();
    let q = p.sequence_mut(seq).unwrap();
    let it = &mut q.video_tracks[0].items[0];
    let m = it.effect_mut("motion").unwrap();
    m.params.get_mut("scale").unwrap().value = ParamValue::Float(70.0);
    m.params.get_mut("rotation").unwrap().value = ParamValue::Float(8.0);
    let mut blur = find_effect("gaussian_blur").unwrap().instance();
    if let Some(b) = blur.params.get_mut("blurriness") {
        b.value = ParamValue::Float(6.0);
    }
    it.effects.push(blur);
    s.project = std::sync::Arc::new(p);
}

#[test]
fn create_attach_toggle_renders_the_same_picture() {
    let root = tmp_dir("proxy-create");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::OceanSunset, W, H, 12);
    let (mut s, items, _) = session_with(&[&a]);
    add_effects(&mut s);
    let full = frame_rgba(&mut s, 5, 1.0);
    let full_half = frame_rgba(&mut s, 5, 0.5);
    let r = s.execute("media.createProxies", json!({"items": [items[0].0], "preset": "prores_proxy_half", "wait": true})).unwrap();
    let out = r["outputs"][0]["path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&out).ends_with(std::path::Path::new("Proxies").join("a_Proxy.mov")), "{out}");
    let job = s.execute("jobs.list", json!({})).unwrap();
    assert_eq!(job[0]["finished"], json!(true), "{job}");
    let m = s.project.item(items[0]).unwrap().as_media().unwrap().clone();
    assert_eq!(m.proxy, Some(filmcraft_project::MediaRef::File { path: out.clone() }), "attached when the job finished");
    // proxies off: unchanged
    assert_eq!(frame_rgba(&mut s, 5, 1.0), full);
    let rev = s.revision;
    let dirty = s.is_dirty();
    s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    assert!(s.revision > rev, "frame caches refresh");
    assert_eq!(s.is_dirty(), dirty, "toggling proxies is not an edit");
    let px = frame_rgba(&mut s, 5, 1.0);
    assert_eq!((px.0, px.1), (full.0, full.1));
    assert_ne!(px.2, full.2, "the proxy is really used");
    let q = psnr(&px.2, &full.2);
    let q_half = psnr(&frame_rgba(&mut s, 5, 0.5).2, &full_half.2);
    eprintln!("proxy vs full: {q:.1} dB at full scale, {q_half:.1} dB at ½");
    assert!(q > 30.0, "proxy composite matches full res modulo resolution: {q:.1} dB");
    assert!(q_half > 33.0, "at ½ playback resolution the proxy is near-identical: {q_half:.1} dB");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn attach_checks_duration_and_export_ignores_proxies() {
    let root = tmp_dir("proxy-attach");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let short = root.join("short.mov");
    make_movie(&short, DemoScene::Aurora, W / 2, H / 2, 6);
    // a "proxy" with other pictures but the right duration and rate
    let other = root.join("other.mp4");
    make_movie(&other, DemoScene::Plasma, W / 4, H / 4, 12);
    let (mut s, items, _) = session_with(&[&a]);
    let e = s.execute("media.attachProxies", json!({"item": items[0].0, "path": short.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("long"), "{e}");
    s.execute("media.attachProxies", json!({"item": items[0].0, "path": other.to_string_lossy()})).unwrap();
    let full = frame_rgba(&mut s, 4, 1.0);
    s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    assert!(psnr(&frame_rgba(&mut s, 4, 1.0).2, &full.2) < 20.0, "monitors show the (different) proxy");
    let png = root.join("out.png").to_string_lossy().into_owned();
    s.execute("file.exportMedia", json!({"path": png, "format": "png", "wait": true})).unwrap();
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    assert!(jobs.as_array().unwrap().iter().all(|j| j["result"].get("error").is_none()), "{jobs}");
    let f = image::open(root.join("out004.png")).unwrap().to_rgba8();
    assert_eq!(psnr(f.as_raw(), &full.2), f64::INFINITY, "export renders full-resolution media");
    s.execute("media.detachProxies", json!({"items": [items[0].0]})).unwrap();
    assert_eq!(frame_rgba(&mut s, 4, 1.0), full);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ingest_copies_and_creates_proxies_on_import() {
    let root = tmp_dir("proxy-ingest");
    let cards = root.join("Card");
    std::fs::create_dir_all(&cards).unwrap();
    let a = cards.join("A001.mov");
    make_movie(&a, DemoScene::Forest, W, H, 8);
    let dest = root.join("Ingest");
    let mut s = Session::default();
    s.execute(
        "project.ingestSettings",
        json!({"enabled": true, "action": "copyAndCreateProxies", "destination": dest.to_string_lossy(), "preset": "h264_quarter"}),
    )
    .unwrap();
    let r = s.execute("file.import", json!({"paths": [a.to_string_lossy()]})).unwrap();
    let item = filmcraft_project::ItemId(r["items"][0].as_u64().unwrap());
    let job = r["ingest"]["job"]["job"].as_u64().unwrap_or_else(|| panic!("{r}"));
    // wait for the background proxy job, then let the session apply it
    for _ in 0..600 {
        if s.jobs.iter().any(|j| j.id == job && j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    s.poll_persistence();
    let m = s.project.item(item).unwrap().as_media().unwrap();
    let copy = dest.join("A001.mov").to_string_lossy().into_owned();
    assert_eq!(m.media, filmcraft_project::MediaRef::File { path: copy.clone() }, "the clip uses the verified copy");
    assert_eq!(m.identity, Some(crate::relink::identity_of(&crate::FsServices, &a.to_string_lossy()).unwrap()));
    assert_eq!(m.proxy, Some(filmcraft_project::MediaRef::File { path: dest.join("A001_Proxy.mp4").to_string_lossy().into_owned() }));
    let _ = std::fs::remove_dir_all(&root);
}

/// Measure: Program-monitor playback cost of 4K H.264 with and without proxies (sequential frames
/// at ½ resolution). Run with `RAYON_NUM_THREADS=1` so wall time ≈ CPU time:
/// `cargo test --release -p filmcraft-engine proxy_playback_perf_4k -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark: 4K fixture via ffmpeg"]
fn proxy_playback_perf_4k() {
    let Some(ffmpeg) = filmcraft_testkit::oracle::ffmpeg_or_skip("proxy_playback_perf_4k") else { return };
    let out = filmcraft_testkit::fixtures_dir("proxies").join("uhd_h264.mp4");
    let src = filmcraft_testkit::fixtures::generate(&out, |tmp| {
        std::process::Command::new(&ffmpeg)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=3840x2160:rate=24:duration=2",
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "48",
            ])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
    .expect("fixture");
    let dir = tmp_dir("proxy-perf");
    let media = dir.join("uhd.mp4");
    std::fs::copy(&src, &media).unwrap();
    let (mut s, items, _) = session_with(&[&media]);
    let n = 24;
    // (decode ms/frame as the GPU playback path pays it, CPU composite ms/frame at ½ and ¼)
    let measure = |s: &mut Session| {
        let rate = s.sequence_rate();
        let src = s.source(items[0]).unwrap();
        let _ = src.video_frame(filmcraft_media::FrameRequest { time: rate.tick_of(0), scale: 0.5 });
        let t0 = std::time::Instant::now();
        for f in 1..n {
            let _ = src.video_frame(filmcraft_media::FrameRequest { time: rate.tick_of(f), scale: 0.5 }).unwrap();
        }
        let decode = t0.elapsed().as_secs_f64() * 1000.0 / (n - 1) as f64;
        let mut comp = [0.0; 2];
        for (k, scale) in [0.5f32, 0.25].into_iter().enumerate() {
            s.media.clear();
            frame_rgba(s, 0, scale);
            let t0 = std::time::Instant::now();
            for f in 1..n {
                frame_rgba(s, f, scale);
            }
            comp[k] = t0.elapsed().as_secs_f64() * 1000.0 / (n - 1) as f64;
        }
        (decode, comp[0], comp[1])
    };
    let full = measure(&mut s);
    eprintln!("4K H.264, full res: decode {:.1} ms/frame; CPU composite {:.1} ms/frame at 1/2, {:.1} at 1/4", full.0, full.1, full.2);
    for preset in ["prores_proxy_quarter", "h264_quarter"] {
        let t0 = std::time::Instant::now();
        s.execute("media.createProxies", json!({"items": [items[0].0], "preset": preset, "wait": true})).unwrap();
        let make = t0.elapsed().as_secs_f64();
        s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
        let px = measure(&mut s);
        s.execute("media.toggleProxies", json!({"enabled": false})).unwrap();
        eprintln!(
            "{preset} proxy (made in {make:.1}s): decode {:.1} ms/frame ({:.1}x less); CPU composite {:.1} ms/frame at 1/2 ({:.1}x less), {:.1} at 1/4 ({:.1}x less)",
            px.0,
            full.0 / px.0,
            px.1,
            full.1 / px.1,
            px.2,
            full.2 / px.2
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Render previews are cached by content and reused whether proxies are on or off, so they are
/// always rendered from full-resolution media (like export).
#[test]
fn render_previews_ignore_proxies() {
    let root = tmp_dir("proxy-previews");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let other = root.join("other.mp4");
    make_movie(&other, DemoScene::Plasma, W / 4, H / 4, 12);
    let (mut s, items, _) = session_with(&[&a]);
    add_effects(&mut s);
    let full = frame_rgba(&mut s, 4, 1.0);
    s.execute("media.attachProxies", json!({"item": items[0].0, "path": other.to_string_lossy()})).unwrap();
    s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let f = s.previews.frame(&s.media, &s.project, seq, 4, 1.0).expect("frame 4 has a preview");
    let q = psnr(&f.to_rgba8(), &full.2);
    assert!(q > 30.0, "preview rendered from full-resolution media, not the (different) proxy: {q:.1} dB");
    let _ = std::fs::remove_dir_all(&root);
}

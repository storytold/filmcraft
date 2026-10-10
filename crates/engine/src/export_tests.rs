//! M6.5 export parity through engine commands: every built-in preset exports the demo sequence
//! and ffprobe (an external test oracle only) confirms codec / size / rate / bitrate; the user
//! preset library; the export queue (order, cancel, retry, several sequences and ranges); Quick
//! Export; ranges and caption sidecars.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;

#[test]
fn hostile_export_ranges_are_rejected_without_panicking() {
    let mut session = demo();
    for params in [
        json!({"range":"custom", "startTime":i64::MIN, "endTime":i64::MAX}),
        json!({"range":"custom", "startTime":-1, "endTime":100}),
        json!({"range":"custom", "startTime":100, "endTime":100}),
    ] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.execute("export.resolve", params)));
        assert!(result.is_ok(), "a hostile export range must not panic");
        assert!(result.unwrap().is_err());
    }
}

/// A scratch directory under the workspace `target/`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let d = filmcraft_testkit::workspace_root().join("target").join("export-tests").join(format!("engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

#[test]
fn export_integer_parameters_cannot_wrap_or_overflow() {
    let mut s = demo();
    for name in ["bitrateKbps", "maxBitrateKbps", "keyframeDistance"] {
        for value in [json!(-1), json!(1.5), json!(u64::from(u32::MAX) + 1)] {
            let mut params = json!({});
            params[name] = value;
            assert!(s.execute("export.resolve", params).is_err(), "{name}");
        }
    }
    assert!(s.execute("export.resolve", json!({"bitrateKbps":u32::MAX})).is_ok());
    assert!(s.execute("export.resolve", json!({"bitrateKbps":8000.0, "keyframeDistance":48.0})).is_ok(), "integer-valued floats are integers");
}

#[test]
fn wav_resolve_honours_explicit_sample_rates() {
    let mut s = demo();
    for rate in [4000, 192_000, 192_001, 200_000, 352_800, 384_000] {
        let result = s.execute("export.resolve", json!({"format":"wav", "settings":{"audio":{"sample_rate":rate}}})).unwrap();
        assert_eq!(result["settings"]["audio"]["sample_rate"], rate);
        assert_eq!(result["output"]["sampleRate"], rate);
    }
    let result = s.execute("export.resolve", json!({"format":"wav", "settings":{"audio":{"sample_rate":null}}})).unwrap();
    assert_eq!(result["output"]["sampleRate"], 48_000);
    for rate in [0, 384_001] {
        assert!(s.execute("export.resolve", json!({"format":"wav", "settings":{"audio":{"sample_rate":rate}}})).is_err());
    }
}

#[test]
fn nested_camel_case_settings_are_honoured() {
    let mut s = demo();
    let camel = json!({"audio":{"sampleRate":96000}, "effects":{"loudness":{"enabled":true, "targetLufs":-16}}});
    let snake = json!({"audio":{"sample_rate":96000}, "effects":{"loudness":{"enabled":true, "target_lufs":-16}}});
    let camel = s.execute("export.resolve", json!({"format":"wav", "settings":camel})).unwrap();
    let snake = s.execute("export.resolve", json!({"format":"wav", "settings":snake})).unwrap();
    assert_eq!(camel["output"]["sampleRate"], 96000);
    assert_eq!(camel["settings"]["audio"]["sample_rate"], 96000);
    assert_eq!(camel["settings"]["effects"]["loudness"]["target_lufs"], -16.0);
    assert_eq!(camel["settings"], snake["settings"]);
}

fn probe(path: &str) -> Option<Value> {
    let ffprobe = filmcraft_testkit::ffprobe_or_skip("export presets")?;
    let out = std::process::Command::new(ffprobe).args(["-v", "error", "-of", "json", "-show_format", "-show_streams", path]).output().unwrap();
    assert!(out.status.success(), "{path}: {}", String::from_utf8_lossy(&out.stderr));
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

fn stream<'a>(j: &'a Value, kind: &str) -> Option<&'a Value> {
    j["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == kind)
}

fn num(v: &Value) -> f64 {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()).unwrap_or(f64::NAN)
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

/// Every built-in preset: a quarter second of the demo sequence (seven frames at 23.976) exported
/// with `file.exportMedia {preset}`; ffprobe checks container, codec, profile, frame size, frame
/// rate and audio format, and the data rate where the codec fixes it.
#[test]
fn every_builtin_preset_exports_and_ffprobe_confirms() {
    let mut s = demo();
    let presets = s.execute("export.presets.list", json!({})).unwrap()["presets"].as_array().unwrap().clone();
    assert!(presets.len() >= 24);
    let dir = Scratch::new("presets");
    for pr in &presets {
        let name = pr["name"].as_str().unwrap();
        let sub = dir.0.join(format!("p{}", filmcraft_export::presets::preset_key(name)));
        std::fs::create_dir_all(&sub).unwrap();
        let path = sub.join("Demo").to_string_lossy().to_string();
        let params = json!({"preset": name, "path": path, "range": "custom", "startSeconds": 1.0, "endSeconds": 1.25, "wait": true});
        let frames = s.execute("export.resolve", params.clone()).unwrap()["frames"].as_u64().unwrap();
        let r = s.execute("file.exportMedia", params).unwrap_or_else(|e| panic!("{name}: {e}"));
        let out = r["path"].as_str().unwrap().to_string();
        let settings = s.execute("export.presets.get", json!({"name": name})).unwrap()["settings"].clone();
        let fmt = settings["format"].as_str().unwrap().to_string();
        let (w, h) = match settings["frameSize"].as_array() {
            Some(a) => (a[0].as_u64().unwrap(), a[1].as_u64().unwrap()),
            None => (1920, 1080),
        };
        if matches!(fmt.as_str(), "png" | "tiff" | "bmp") {
            let files = files_in(&sub);
            let ext = Path::new(&out).extension().unwrap().to_string_lossy().to_string();
            assert_eq!(frames, 7, "frames 23..=29 at 23.976 fps");
            let want: Vec<String> = (0..frames).map(|i| format!("Demo{i:03}.{ext}")).collect();
            assert_eq!(files, want, "{name}");
            if let Some(j) = probe(&sub.join(&want[0]).to_string_lossy()) {
                let v = stream(&j, "video").unwrap();
                let codec = match fmt.as_str() {
                    "png" => "png",
                    "tiff" => "tiff",
                    _ => "bmp",
                };
                assert_eq!(v["codec_name"], codec, "{name}");
                assert_eq!((v["width"].as_u64().unwrap(), v["height"].as_u64().unwrap()), (w, h), "{name}");
            }
            continue;
        }
        assert!(Path::new(&out).exists(), "{name}: {out}");
        let Some(j) = probe(&out) else { continue };
        let fmt_name = j["format"]["format_name"].as_str().unwrap();
        match fmt.as_str() {
            "wav" | "aiff" => {
                let a = stream(&j, "audio").unwrap();
                let bits = settings["audio"]["bits"].as_u64().unwrap();
                let codec = match (fmt.as_str(), bits) {
                    ("wav", 24) => "pcm_s24le",
                    ("wav", _) => "pcm_s16le",
                    (_, 24) => "pcm_s24be",
                    _ => "pcm_s16be",
                };
                assert_eq!(a["codec_name"], codec, "{name}");
                assert_eq!(a["sample_rate"], "48000", "{name}");
                assert_eq!(a["channels"], 2, "{name}");
                assert!((num(&j["format"]["duration"]) - 0.25).abs() < 0.01, "{name}: {}", j["format"]["duration"]);
                continue;
            }
            "gif" => {
                let v = stream(&j, "video").unwrap();
                assert_eq!(v["codec_name"], "gif");
                assert_eq!((v["width"].as_u64().unwrap(), v["height"].as_u64().unwrap()), (640, 360));
                continue;
            }
            "mxf-opatom" => {
                // Avid style: picture only in Demo.mxf, one mono PCM file per channel
                assert_eq!(fmt_name, "mxf", "{name}");
                assert_eq!(j["streams"].as_array().unwrap().len(), 1, "{name}");
                assert_eq!(stream(&j, "video").unwrap()["codec_name"], "dnxhd", "{name}");
                for k in 1..=2 {
                    let a = probe(&format!("{}_A{k}.mxf", out.trim_end_matches(".mxf"))).unwrap();
                    let a = stream(&a, "audio").unwrap();
                    assert_eq!((a["codec_name"].as_str(), a["channels"].as_u64()), (Some("pcm_s24le"), Some(1)), "{name} A{k}");
                }
                continue;
            }
            _ => {}
        }
        let v = stream(&j, "video").unwrap_or_else(|| panic!("{name}: no video"));
        assert_eq!((v["width"].as_u64().unwrap(), v["height"].as_u64().unwrap()), (w, h), "{name}");
        assert_eq!(v["r_frame_rate"], "24000/1001", "{name}");
        let a = stream(&j, "audio").unwrap_or_else(|| panic!("{name}: no audio"));
        assert_eq!(a["sample_rate"], "48000", "{name}");
        assert_eq!(a["channels"], 2, "{name}");
        let vbr = num(&v["bit_rate"]) / 1e6;
        let px = (w * h) as f64 / (1920.0 * 1080.0) * (24000.0 / 1001.0) / 29.97;
        match fmt.as_str() {
            "h264" => {
                assert!(fmt_name.contains("mp4"), "{name}: {fmt_name}");
                assert_eq!(v["codec_name"], "h264", "{name}");
                assert_eq!(v["profile"], "High", "{name}");
                assert_eq!(a["codec_name"], "aac", "{name}");
                assert!((num(&a["bit_rate"]) / 1000.0 - 320.0).abs() < 80.0, "{name}: AAC {}", a["bit_rate"]);
            }
            "prores" => {
                assert!(fmt_name.contains("mov"), "{name}: {fmt_name}");
                assert_eq!(v["codec_name"], "prores", "{name}");
                let (profile, mbps) = match settings["proresProfile"].as_str().unwrap() {
                    "proxy" => ("Proxy", 45.0),
                    "lt" => ("LT", 102.0),
                    "standard" => ("Standard", 147.0),
                    "4444" => ("4444", 330.0),
                    "4444xq" => ("XQ", 500.0),
                    _ => ("HQ", 220.0),
                };
                assert_eq!(v["profile"], profile, "{name}");
                if matches!(profile, "4444" | "XQ") {
                    let pix = v["pix_fmt"].as_str().unwrap_or_default();
                    let alpha = settings["alpha"].as_bool().unwrap_or(false);
                    assert!(pix.contains("444") && pix.starts_with("yuva") == alpha, "{name}: {pix}");
                }
                assert!(vbr <= mbps * px * 1.5, "{name}: {vbr:.1} Mb/s vs nominal {:.1}", mbps * px);
                assert_eq!(a["codec_name"], "pcm_s24le", "{name}");
            }
            "dnxhr" => {
                assert_eq!(v["codec_name"], "dnxhd", "{name}");
                let p = settings["dnxProfile"].as_str().unwrap().to_ascii_uppercase();
                assert_eq!(v["profile"], format!("DNXHR {p}"), "{name}");
                let mbps = match p.as_str() {
                    "LB" => 45.0,
                    "SQ" => 145.0,
                    _ => 220.0,
                };
                assert!((vbr / (mbps * px) - 1.0).abs() < 0.25, "{name}: {vbr:.1} Mb/s vs nominal {:.1}", mbps * px);
                assert_eq!(a["codec_name"], "pcm_s24le", "{name}");
            }
            "apv" => {
                assert!(fmt_name.contains("mov"), "{name}: {fmt_name}");
                assert_eq!(v["codec_name"], "apv", "{name}");
                let p = settings["apvProfile"].as_str().unwrap();
                let idc = match p {
                    "422-10" => "33",
                    "422-12" => "44",
                    "444-10" => "55",
                    "444-12" => "66",
                    "4444-10" => "77",
                    "4444-12" => "88",
                    "400-10" => "99",
                    other => other,
                };
                let got_profile = v["profile"].as_str().unwrap_or("");
                assert!(got_profile == p || got_profile == idc, "{name}: profile {got_profile} != {p} ({idc})");
                assert_eq!(a["codec_name"], "pcm_s24le", "{name}");
            }
            "mxf-op1a" => {
                assert_eq!(fmt_name, "mxf", "{name}");
                let codec = match settings["mxfVideoCodec"].as_str().unwrap() {
                    "proRes" => "prores",
                    "h264" => "h264",
                    _ => "dnxhd",
                };
                assert_eq!(v["codec_name"], codec, "{name}");
                assert_eq!(a["codec_name"], "pcm_s24le", "{name}");
            }
            other => panic!("{name}: unexpected format {other}"),
        }
    }
}

/// H.264 bitrate control over three seconds of the demo: the adaptive Match Source preset
/// targets width × height × fps × bpp, the 1080p delivery presets their fixed targets; ffprobe's
/// measured video bitrate stays within the VBR maximum and close to the target.
#[test]
fn h264_presets_hit_their_bitrates() {
    let mut s = demo();
    let dir = Scratch::new("bitrate");
    for (preset, target_kbps, max_kbps) in [
        ("Match Source – Adaptive High Bitrate", 1920.0 * 1080.0 * (24000.0 / 1001.0) * 0.2 / 1000.0, 1.5),
        ("YouTube 1080p Full HD", 16_000.0, 20.0 / 16.0),
        ("Vimeo 1080p Full HD", 18_000.0, 24.0 / 18.0),
    ] {
        let path = dir.path(&format!("{}.mp4", filmcraft_export::presets::preset_key(preset)));
        s.execute(
            "file.exportMedia",
            json!({"preset": preset, "path": path, "range": "custom", "startSeconds": 0.0, "endSeconds": 3.0, "audio": false, "wait": true}),
        )
        .unwrap();
        let Some(j) = probe(&path) else { return };
        let v = stream(&j, "video").unwrap();
        let kbps = num(&v["bit_rate"]) / 1000.0;
        eprintln!("{preset}: {kbps:.0} kbps (target {target_kbps:.0})");
        assert!(kbps <= target_kbps * max_kbps * 1.1, "{preset}: {kbps:.0} kbps over the maximum");
        assert!(kbps >= target_kbps * 0.5, "{preset}: {kbps:.0} kbps far under the target {target_kbps:.0}");
    }
}

#[test]
fn user_presets_persist_with_favourites_import_and_export() {
    let dir = Scratch::new("library");
    let mut s = demo();
    s.export_presets.set_dir(&dir.0);
    // a custom preset from a built-in plus overrides
    s.execute(
        "export.presets.save",
        json!({"name": "Review 720p", "from": "YouTube 1080p Full HD", "width": 1280, "height": 720, "bitrateKbps": 5000, "description": "client review"}),
    )
    .unwrap();
    let got = s.execute("export.presets.get", json!({"name": "review 720P"})).unwrap();
    assert_eq!(got["builtin"], false);
    assert_eq!(got["settings"]["frameSize"], json!([1280, 720]));
    assert_eq!(got["settings"]["bitrateKbps"], 5000);
    assert!(s.execute("export.presets.save", json!({"name": "YouTube 1080p Full HD"})).is_err(), "built-in names are reserved");
    assert!(s.execute("export.presets.delete", json!({"name": "Apple ProRes 422 HQ"})).is_err(), "built-ins cannot be deleted");
    // favourites
    s.execute("export.presets.favorite", json!({"name": "Apple ProRes 422 HQ"})).unwrap();
    s.execute("export.presets.favorite", json!({"name": "Review 720p", "favorite": true})).unwrap();
    let favs = s.execute("export.presets.list", json!({"favorites": true})).unwrap()["presets"].as_array().unwrap().len();
    assert_eq!(favs, 2);
    let hits = s.execute("export.presets.list", json!({"query": "prores"})).unwrap()["presets"].as_array().unwrap().len();
    assert_eq!(hits, 8, "seven QuickTime ProRes presets and MXF OP1a ProRes 422 HQ");
    // persisted: a fresh session with the same data directory sees both
    let mut s2 = demo();
    s2.export_presets.set_dir(&dir.0);
    assert!(s2.export_presets.find("Review 720p").is_some());
    assert!(s2.export_presets.is_favorite("Apple ProRes 422 HQ"));
    // export → delete → import
    let file = dir.path("mine.json");
    s2.execute("export.presets.export", json!({"path": file, "names": ["Review 720p"]})).unwrap();
    s2.execute("export.presets.delete", json!({"name": "Review 720p"})).unwrap();
    assert!(s2.export_presets.find("Review 720p").is_none());
    let imp = s2.execute("export.presets.import", json!({"path": file})).unwrap();
    assert_eq!(imp["imported"], json!(["Review 720p"]));
    // and it exports
    let out = dir.path("review.mp4");
    s2.execute("file.exportMedia", json!({"preset": "Review 720p", "path": out, "range": "custom", "startSeconds": 0, "endSeconds": 0.25, "wait": true}))
        .unwrap();
    if let Some(j) = probe(&out) {
        assert_eq!(stream(&j, "video").unwrap()["width"], 1280);
    }
    assert!(s2.execute("export.presets.import", json!({"path": dir.path("missing.json")})).is_err());
}

fn queue(s: &mut Session) -> Vec<Value> {
    s.execute("export.queue.list", json!({})).unwrap()["items"].as_array().unwrap().clone()
}

#[test]
fn queue_orders_cancels_and_retries() {
    let mut s = demo();
    let dir = Scratch::new("queue");
    let add = |s: &mut Session, name: &str, preset: &str| -> u64 {
        let r =
            s.execute("export.queue.add", json!({"preset": preset, "path": dir.path(name), "range": "custom", "startSeconds": 0, "endSeconds": 0.25})).unwrap();
        r["added"][0].as_u64().unwrap()
    };
    let a = add(&mut s, "a.mp4", "Match Source – Adaptive Low Bitrate");
    let b = add(&mut s, "b.mov", "Apple ProRes 422 Proxy");
    let c = add(&mut s, "c.wav", "Waveform Audio 48 kHz 16-bit");
    let d = add(&mut s, "d.mp4", "Match Source – Adaptive Low Bitrate");
    assert!(queue(&mut s).iter().all(|i| i["status"] == "ready"));
    // reorder: c to the front, a one down
    s.execute("export.queue.move", json!({"id": c, "to": 0})).unwrap();
    s.execute("export.queue.move", json!({"id": a, "by": 1})).unwrap();
    let order: Vec<u64> = queue(&mut s).iter().map(|i| i["id"].as_u64().unwrap()).collect();
    assert_eq!(order, [c, b, a, d]);
    // cancel one before it starts, then run the queue to completion
    s.execute("export.queue.cancel", json!({"id": d})).unwrap();
    s.execute("export.queue.start", json!({"wait": true})).unwrap();
    let items = queue(&mut s);
    let status = |id: u64| items.iter().find(|i| i["id"] == id).unwrap()["status"].clone();
    assert_eq!([status(c), status(b), status(a), status(d)], [json!("done"), json!("done"), json!("done"), json!("cancelled")]);
    // jobs ran in queue order
    let job = |id: u64| items.iter().find(|i| i["id"] == id).unwrap()["job"].as_u64().unwrap();
    assert!(job(c) < job(b) && job(b) < job(a));
    for f in ["a.mp4", "b.mov", "c.wav"] {
        assert!(Path::new(&dir.path(f)).exists(), "{f}");
    }
    assert!(!Path::new(&dir.path("d.mp4")).exists());
    // retry the cancelled one
    s.execute("export.queue.retry", json!({"id": d, "start": true, "wait": true})).unwrap();
    assert_eq!(queue(&mut s).iter().find(|i| i["id"] == d).unwrap()["status"], "done");
    assert!(Path::new(&dir.path("d.mp4")).exists());
    assert!(s.execute("export.queue.retry", json!({"id": 999})).is_err());
    // clear finished
    s.execute("export.queue.clear", json!({})).unwrap();
    assert!(queue(&mut s).is_empty());
}

#[test]
fn queue_move_by_extreme_offsets_clamps_instead_of_overflowing() {
    let mut s = demo();
    let ids: Vec<u64> = (0..3)
        .map(|_| {
            let r = s.execute("export.queue.add", json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": "queued-output/"})).unwrap();
            r["added"][0].as_u64().unwrap()
        })
        .collect();
    let order = |s: &mut Session| -> Vec<u64> { queue(s).iter().map(|i| i["id"].as_u64().unwrap()).collect() };
    s.execute("export.queue.move", json!({"id": ids[1], "by": i64::MAX})).unwrap();
    assert_eq!(order(&mut s), [ids[0], ids[2], ids[1]]);
    s.execute("export.queue.move", json!({"id": ids[1], "by": i64::MIN})).unwrap();
    assert_eq!(order(&mut s), [ids[1], ids[0], ids[2]]);
}

#[test]
fn queue_cancels_a_running_export_and_retries_a_failed_one() {
    let mut s = demo();
    let dir = Scratch::new("queue-cancel");
    // a long encode, cancelled while it runs
    let long = s.execute("export.queue.add", json!({"preset": "Apple ProRes 422 HQ", "path": dir.path("long.mov"), "range": "entire", "start": true})).unwrap()
        ["added"][0]
        .as_u64()
        .unwrap();
    assert_eq!(queue(&mut s)[0]["status"], "encoding");
    s.execute("export.queue.cancel", json!({"id": long, "wait": true})).unwrap();
    assert_eq!(queue(&mut s)[0]["status"], "cancelled");
    // a failure (the output directory cannot be created: a file is in the way) and a retry
    std::fs::write(dir.path("blocker"), b"x").unwrap();
    let bad = s
        .execute(
            "export.queue.add",
            json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": dir.path("blocker/x.wav"), "range": "custom", "startSeconds": 0, "endSeconds": 0.1}),
        )
        .unwrap()["added"][0]
        .as_u64()
        .unwrap();
    s.execute("export.queue.start", json!({"wait": true})).unwrap();
    let it = queue(&mut s).into_iter().find(|i| i["id"] == bad).unwrap();
    assert_eq!(it["status"], "failed");
    assert!(it["error"].as_str().unwrap().contains("I/O"), "{it}");
    s.execute("export.queue.retry", json!({"id": bad})).unwrap();
    assert_eq!(queue(&mut s).into_iter().find(|i| i["id"] == bad).unwrap()["status"], "ready");
    s.execute("export.queue.remove", json!({"id": bad})).unwrap();
    assert!(queue(&mut s).iter().all(|i| i["id"] != bad));
}

#[test]
fn jobs_and_queue_items_report_the_time_they_have_left() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use crate::export_tools::QueueStatus;
    let mut s = demo();
    let dir = Scratch::new("eta");
    let job = crate::Job { id: 77, label: "Exporting".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(1000, Ordering::Relaxed);
    assert!(job.to_json()["etaSeconds"].is_null(), "no speed measured yet");
    // 50 units a second for 5 s, read from the near future (the JSON reads the real clock, which is then behind them)
    let t0 = web_time::Instant::now() + Duration::from_secs(60);
    for i in 0..=50u64 {
        job.progress.done.store(i * 5, Ordering::Relaxed);
        job.progress.eta_at(t0 + Duration::from_millis(i * 100));
    }
    let eta = job.to_json()["etaSeconds"].as_f64().expect("a number once the job has a speed");
    assert!((eta - 15.0).abs() < 0.5, "750 units left at 50 a second: {eta}");
    s.jobs.push(job);

    // a queue item encoding that job shows it; one that waits shows nothing
    for name in ["a.wav", "b.wav"] {
        s.execute(
            "export.queue.add",
            json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": dir.path(name), "range": "custom", "startSeconds": 0, "endSeconds": 0.25}),
        )
        .unwrap();
    }
    s.export_queue.items[0].status = QueueStatus::Encoding;
    s.export_queue.items[0].job = Some(77);
    let items = queue(&mut s);
    assert!((items[0]["etaSeconds"].as_f64().expect("encoding") - 15.0).abs() < 0.5, "{}", items[0]);
    assert_eq!(items[0]["status"], "encoding");
    assert!(items[1]["etaSeconds"].is_null(), "{}", items[1]);
    // and `jobs.list` carries it too
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let listed = jobs["jobs"].as_array().or(jobs.as_array()).expect("a list of jobs").iter().find(|j| j["id"] == 77).cloned().expect("the job is listed");
    assert!(listed["etaSeconds"].as_f64().is_some(), "{listed}");
}

#[test]
fn hardware_encoding_is_off_unless_asked_for() {
    use filmcraft_export::HardwareEncoding::{Auto, Off};
    let s = demo();
    let setting = |p: Value| crate::export_tools::settings_from_params(&s, &p, "file.exportMedia").map(|(_, st)| st.hardware_encoding);
    assert_eq!(setting(json!({"path": "x.mp4"})).unwrap(), Off);
    assert_eq!(setting(json!({"path": "x.mp4", "hardwareEncoding": "auto"})).unwrap(), Auto);
    assert_eq!(setting(json!({"path": "x.mp4", "hardwareEncoding": "off"})).unwrap(), Off);
    // a boolean is understood too: true is auto
    assert_eq!(setting(json!({"path": "x.mp4", "hardwareEncoding": true})).unwrap(), Auto);
    assert_eq!(setting(json!({"path": "x.mp4", "hardwareEncoding": false})).unwrap(), Off);
    // anything else names the choices
    let e = setting(json!({"path": "x.mp4", "hardwareEncoding": "gpu"})).unwrap_err().to_string();
    assert!(e.contains("off | auto"), "{e}");
    // and it travels in the settings object, with the default filling in when it is missing
    assert_eq!(setting(json!({"path": "x.mp4", "settings": {"hardwareEncoding": "auto"}})).unwrap(), Auto);
    let old: filmcraft_export::ExportSettings = serde_json::from_value(json!({"format": "h264"})).unwrap();
    assert_eq!(old.hardware_encoding, Off);
}

#[test]
fn crf_param_picks_constant_quality() {
    use filmcraft_export::BitrateMode::{Cbr, Crf, Vbr1Pass};
    let s = demo();
    let setting = |p: Value| crate::export_tools::settings_from_params(&s, &p, "file.exportMedia").map(|(_, st)| (st.bitrate_mode, st.crf));
    assert_eq!(setting(json!({"path": "x.mp4"})).unwrap(), (Vbr1Pass, 23.0));
    // a factor alone means CRF mode; the mode can also be named, or another one kept explicitly
    assert_eq!(setting(json!({"path": "x.mp4", "crf": 18})).unwrap(), (Crf, 18.0));
    assert_eq!(setting(json!({"path": "x.mp4", "bitrateMode": "crf"})).unwrap(), (Crf, 23.0));
    assert_eq!(setting(json!({"path": "x.mp4", "bitrateMode": "cbr", "crf": 18})).unwrap(), (Cbr, 18.0));
    let e = setting(json!({"path": "x.mp4", "bitrateMode": "abr"})).unwrap_err().to_string();
    assert!(e.contains("crf"), "{e}");
}

#[test]
fn queue_exports_several_sequences_and_ranges() {
    let mut s = demo();
    let dir = Scratch::new("queue-many");
    let first = s.state.active_sequence.unwrap();
    let second = s.execute("file.newSequence", json!({"name": "Second Cut", "width": 640, "height": 360, "fps": 25})).unwrap()["sequence"].as_u64().unwrap();
    let r = s
        .execute(
            "export.queue.add",
            json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": format!("{}/", dir.0.display()), "sequences": [first.0, second], "range": "custom", "startSeconds": 0, "endSeconds": 0.5}),
        )
        .unwrap();
    assert_eq!(r["added"].as_array().unwrap().len(), 2);
    let r = s
        .execute(
            "export.queue.add",
            json!({"preset": "Waveform Audio 48 kHz 16-bit", "sequence": first.0, "path": dir.path("parts.wav"),
                   "ranges": [{"startSeconds": 0, "endSeconds": 0.5}, {"startSeconds": 1, "endSeconds": 1.25}]}),
        )
        .unwrap();
    assert_eq!(r["added"].as_array().unwrap().len(), 2);
    s.execute("export.queue.start", json!({"wait": true})).unwrap();
    assert!(queue(&mut s).iter().all(|i| i["status"] == "done"), "{:?}", queue(&mut s));
    let names = files_in(&dir.0);
    let first_name = s.project.item(first).unwrap().name.clone();
    assert!(names.contains(&format!("{first_name}.wav")), "{names:?}");
    assert!(names.contains(&"Second Cut.wav".to_string()), "{names:?}");
    assert!(names.contains(&"parts.wav".to_string()) && names.contains(&"parts_2.wav".to_string()), "{names:?}");
    let len = |f: &str| std::fs::metadata(dir.0.join(f)).unwrap().len();
    assert_eq!(len("parts_2.wav"), 44 + 12_000 * 4, "0.25 s of 16-bit stereo");
}

#[test]
fn queue_folder_paths_expand_home_and_take_either_separator() {
    let mut s = demo();
    let first = s.state.active_sequence.unwrap();
    let second = s.execute("file.newSequence", json!({"name": "Second Cut", "width": 640, "height": 360, "fps": 25})).unwrap()["sequence"].as_u64().unwrap();
    let preset = "Waveform Audio 48 kHz 16-bit";
    // a folder that does not exist yet, named by a trailing `\`: each sequence keeps its own name
    s.execute("export.queue.add", json!({"preset": preset, "path": "not-yet-made\\", "sequences": [first.0, second]})).unwrap();
    let paths: Vec<String> = queue(&mut s).iter().map(|i| i["path"].as_str().unwrap().to_string()).collect();
    assert!(paths.iter().any(|p| p.ends_with("Second Cut.wav")), "{paths:?}");
    if let Some(home) = crate::media_browser::std_home_dir() {
        s.execute("export.queue.add", json!({"preset": preset, "path": "~/Exports/", "sequence": first.0})).unwrap();
        let last = queue(&mut s).last().unwrap()["path"].as_str().unwrap().to_string();
        assert!(last.starts_with(&home), "{last}");
    }
}

#[test]
fn quick_export_uses_and_remembers_a_preset() {
    let mut s = demo();
    let dir = Scratch::new("quick");
    let r = s.execute("export.quick", json!({"path": dir.path("quick"), "range": "custom", "startSeconds": 0, "endSeconds": 0.25, "wait": true})).unwrap();
    assert_eq!(r["preset"], filmcraft_export::presets::DEFAULT_PRESET);
    assert_eq!(r["path"], dir.path("quick.mp4"));
    assert!(Path::new(&dir.path("quick.mp4")).exists());
    s.execute(
        "export.quick",
        json!({"preset": "Apple ProRes 422 LT", "path": dir.path("q2"), "range": "custom", "startSeconds": 0, "endSeconds": 0.1, "wait": true}),
    )
    .unwrap();
    let r = s.execute("export.quick", json!({"path": dir.path("q3"), "range": "custom", "startSeconds": 0, "endSeconds": 0.1, "wait": true})).unwrap();
    assert_eq!(r["preset"], "Apple ProRes 422 LT", "the last preset is remembered");
    assert!(Path::new(&dir.path("q3.mov")).exists());
}

#[test]
fn ranges_resolve_and_sidecar_captions_are_written() {
    let mut s = demo();
    let dir = Scratch::new("ranges");
    let fps = 24000.0 / 1001.0;
    let frames = |s: &mut Session, p: Value| s.execute("export.resolve", p).unwrap()["frames"].as_i64().unwrap();
    let whole = frames(&mut s, json!({"preset": "PNG Sequence", "range": "entire"}));
    let dur = s.active_sequence().unwrap().duration();
    assert_eq!(whole, (dur.seconds() * fps).ceil() as i64);
    s.execute("markers.markIn", json!({"seconds": 1.0})).unwrap();
    s.execute("markers.markOut", json!({"seconds": 2.0})).unwrap();
    let io = frames(&mut s, json!({"preset": "PNG Sequence", "range": "inOut"}));
    assert!((24..=26).contains(&io), "{io}");
    assert_eq!(frames(&mut s, json!({"preset": "PNG Sequence"})), io, "In/Out is the default");
    assert!(s.execute("export.resolve", json!({"range": "workArea"})).is_err(), "no work area");
    assert_eq!(frames(&mut s, json!({"preset": "Animated GIF 640×360", "range": "custom", "startSeconds": 0, "endSeconds": 2})), 30, "15 fps");
    let bare = frames(&mut s, json!({"preset": "Animated GIF 640×360", "startSeconds": 0, "endSeconds": 2}));
    assert_eq!(bare, 30, "start/end times without `range` are a custom range, not the In/Out or whole sequence");
    assert!(s.execute("export.resolve", json!({"startSeconds": 1})).is_err(), "a start without an end is an error");
    let res = s.execute("export.resolve", json!({"preset": "YouTube 2160p 4K Ultra HD"})).unwrap();
    assert_eq!(res["output"]["width"], 3840);
    assert!(res["summary"]["video"].as_str().unwrap().contains("3840x2160"));
    assert!(res["summary"]["estimated_bytes"].as_u64().unwrap() > 1_000_000);
    // caption sidecar next to the output
    s.execute("captions.add", json!({"seconds": 1.5, "durationSeconds": 1.0, "text": "Hello sidecar"})).unwrap();
    let out = dir.path("cap.wav");
    s.execute("file.exportMedia", json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": out, "captionSidecar": "srt", "wait": true})).unwrap();
    let srt = std::fs::read_to_string(dir.path("cap.srt")).unwrap();
    // timed from the start of the export range (In at 1 s; the caption starts on frame 35)
    assert!(srt.contains("Hello sidecar") && srt.contains("00:00:00,460 --> 00:00:01,461"), "{srt}");
    s.execute("file.exportMedia", json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": out, "captionSidecar": "vtt", "wait": true})).unwrap();
    assert!(std::fs::read_to_string(dir.path("cap.vtt")).unwrap().starts_with("WEBVTT"));
    // a bad preset name is a clean error
    assert!(s.execute("file.exportMedia", json!({"preset": "Nope", "path": out})).is_err());
}

/// Export mode asks for a default folder as soon as it opens. With no saved project and no
/// HOME (the web build) this used to reach `std::env::temp_dir()`, which panics on wasm32 and
/// killed the web app whenever the Export tab was clicked.
#[test]
fn default_export_dir_never_needs_a_real_filesystem() {
    let s = Session::default();
    let d = crate::export_tools::default_export_dir(&s);
    assert!(!d.as_os_str().is_empty());
    // the shared temp-dir helper is what every former `std::env::temp_dir()` call goes through
    assert!(!crate::temp_dir().as_os_str().is_empty());
    // a saved project exports next to itself
    let s = Session { path: Some("/projects/show/edit.fcproj".into()), ..Default::default() };
    assert_eq!(crate::export_tools::default_export_dir(&s), std::path::PathBuf::from("/projects/show"));
}

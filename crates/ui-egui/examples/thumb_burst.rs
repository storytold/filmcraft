//! Memory of the thumbnail burst that follows an import (issue: 6+ GB while importing 42 4K H.264
//! clips with hardware decoding off).
//!
//! Imports every `*.mp4` of a directory one by one, as the Project panel does, and asks for each
//! clip's poster thumbnail (priority 50, 160 px) through the real [`FrameServer`] right after its
//! import. Prints the wall time until every thumbnail is ready and the peak number of live
//! decoders. Measure the process' private bytes from outside (`stress.py`'s sampler, or any
//! process monitor).
//!
//! ```sh
//! cargo run --release -p filmcraft-ui-egui --example thumb_burst -- <clips dir> [--hw off|auto] [--limit N]
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_project::ItemId;
use filmcraft_ui_egui::frames::{FrameKey, FrameServer, Target};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let Some(dir) = args.first().filter(|a| !a.starts_with("--")).map(PathBuf::from) else {
        eprintln!("usage: thumb_burst <clips dir> [--hw off|auto] [--limit N]");
        return;
    };
    filmcraft_codecs::hw::set_hardware_decoding(value("--hw").as_deref() != Some("off"));
    let limit = value("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let mut clips: Vec<PathBuf> = std::fs::read_dir(&dir).map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    clips.retain(|p| p.extension().is_some_and(|e| e == "mp4"));
    clips.sort();
    clips.truncate(limit);

    let mut s = Session::default();
    let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), FrameServer::default_workers());

    let peak = std::sync::Arc::new(AtomicUsize::new(0));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(filmcraft_codecs::live_decoders(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let t0 = Instant::now();
    let mut keys = Vec::new();
    for path in &clips {
        let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).expect("import");
        let id = ItemId(r["items"][0].as_u64().expect("item"));
        let (rate, width) = match s.project.item(id).map(|i| (i.frame_rate(), &i.kind)) {
            Some((rate, filmcraft_project::ItemKind::Media(m))) => (rate, m.info.video.as_ref().map_or(1920, |v| v.width)),
            _ => continue,
        };
        let rev = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            if let Some(filmcraft_project::ItemKind::Media(m)) = s.project.item(id).map(|i| &i.kind) {
                filmcraft_engine::media_pool::media_key(m).hash(&mut h);
                false.hash(&mut h);
            }
            h.finish()
        };
        let key = FrameKey { target: Target::Item(id), frame: 0, size: 160, revision: rev, draft: false };
        server.request(key, rate.tick_of(0), 160.0 / width.max(1) as f32, &s.project.clone(), 50);
        keys.push(key);
    }
    let imported = t0.elapsed();
    while keys.iter().any(|k| !server.is_ready(k)) && t0.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();
    println!(
        "clips {} imported in {:.2}s, all thumbnails ready after {:.2}s, peak live decoders {}, gop frames cached {:.0} MB",
        keys.len(),
        imported.as_secs_f64(),
        t0.elapsed().as_secs_f64(),
        peak.load(Ordering::Relaxed),
        filmcraft_codecs::cached_bytes() as f64 / 1048576.0
    );
}

//! `perf.stats` in the app: the engine's counters (decoder / GOP cache, jobs) plus playback
//! (shown / dropped frames), the frame workers (decode and render ms per job, request hit rate,
//! cache use), UI frame rate and process CPU time. Readable over the control channel
//! (`perf.stats`, or `engine.execute {"command": "perf.stats"}`) and MCP `command_run`.

use serde_json::{Value, json};

use crate::FilmcraftApp;

pub fn stats(app: &FilmcraftApp) -> Value {
    let mut v = filmcraft_engine::perf::stats(&app.session);
    let (shown, dropped) = app.playback.meter.counts();
    let (images, image_bytes, plans, plan_bytes) = app.frames.cache_entries();
    v["playback"] = json!({
        "playing": app.playback.playing,
        "speed": app.playback.speed,
        "shown": shown,
        "dropped": dropped,
        "dropRate": if shown + dropped == 0 { 0.0 } else { dropped as f64 / (shown + dropped) as f64 },
        "resolution": app.ui.program.res.label(),
        "draftDecode": app.session.prefs.playback.draft_decode,
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        let sr = app.audio.as_ref().map_or(0, |a| a.sample_rate());
        v["playback"]["audio"] = app.playback.audio_stats.to_json(sr);
    }
    let mut frames = app.frames.stats().to_json();
    frames["workers"] = json!(app.frames.workers());
    frames["queued"] = json!(app.frames.queue_len());
    frames["renderCostMs"] = json!(app.frames.render_cost() * 1e3);
    frames["cache"] = json!({"images": images, "imageMB": image_bytes as f64 / 1e6, "plans": plans, "planMB": plan_bytes as f64 / 1e6});
    v["frames"] = frames;
    v["gpu"] = app.gpu.as_ref().map_or(Value::Null, |g| {
        json!({
            "uploadedBytes": g.compositor.uploaded_bytes,
            "transitions": g.compositor.gpu_transitions,
            "cpuTransitions": g.compositor.cpu_transitions,
            "submitMs": g.last_ms,
        })
    });
    // the Program monitor's picture against what is due: how far a drag or a scrub is ahead of it
    v["monitor"] = json!({
        "frame": app.program_picture().map(|k| k.frame),
        "revision": app.program_picture().map(|k| k.revision),
        "projectRevision": app.session.revision,
        "playheadFrame": app.session.active_sequence().map(|q| q.settings.frame_rate.frame_at(app.session.playhead())),
    });
    v["ui"] = json!({"fps": app.fps, "frameMs": if app.fps > 0.0 { 1000.0 / app.fps as f64 } else { 0.0 }});
    v["process"] = json!({"cpuS": crate::frames::process_cpu_time().map(|d| d.as_secs_f64())});
    v
}

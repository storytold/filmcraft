//! `perf.stats`: performance counters an agent can read (headless engine part). The desktop UI
//! extends the same query with playback, frame-worker and UI timings (`ui-egui` `perf.rs`).

use serde_json::{Value, json};

use crate::Session;

/// Decoder / GOP-cache counters since the process started (they only grow: diff two readings to
/// measure an interval).
pub fn decode_json() -> Value {
    let g = filmcraft_codecs::gop_stats();
    let hw = filmcraft_codecs::hw::hw_stats();
    json!({
        "requests": g.hits + g.misses,
        "cacheHits": g.hits,
        "cacheMisses": g.misses,
        "cacheHitRate": g.hit_rate(),
        "seeks": g.seeks,
        "samplesDecoded": g.decoded,
        "samplesSkipped": g.skipped,
        "draftFrames": g.draft,
        "h264Threads": filmcraft_codecs::video::h264_threads(),
        "evicted": g.evicted,
        // sources that hold a decoder right now (idle ones give theirs up beyond a cap)
        "liveDecoders": filmcraft_codecs::live_decoders(),
        // decoded frames all sources hold, and the budget idle sources are trimmed to
        "cacheMB": filmcraft_codecs::cached_bytes() as f64 / 1e6,
        "cacheBudgetMB": filmcraft_codecs::FRAME_BUDGET as f64 / 1e6,
        // plane buffers of evicted frames waiting for the next decoded pictures, and how many
        // planes were decoded into a recycled buffer
        "planePoolMB": filmcraft_frame::pool::stats().idle_bytes as f64 / 1e6,
        "planesReused": filmcraft_frame::pool::stats().reused,
        "decodeMs": g.decode_ns as f64 / 1e6,
        "decodeMsPerSample": g.decode_ms_per_sample(),
        "framesDecoded": g.frames,
        // Settings ▸ Playback ▸ Hardware decoding: pictures from OS hardware decoders vs ours,
        // hardware decoders created, streams handed to software up front, mid-stream fallbacks.
        "hardware": {
            "enabled": filmcraft_codecs::hw::hardware_decoding(),
            // the OS decoder backend registered at startup (null where there is none)
            "backend": filmcraft_codecs::hw::hw_backend(),
            "frames": hw.frames,
            "softwareFrames": g.frames.saturating_sub(hw.frames),
            "sessions": hw.sessions,
            "declined": hw.declined,
            "fallbacks": hw.fallbacks,
            // of `frames`: pictures the compositor samples straight from the decoder's GPU memory
            "zeroCopyFrames": hw.zero_copy_frames,
        },
    })
}

/// Export counters: pictures encoded by hardware encoders, sessions, declined requests; and the wall
/// time of each export stage in milliseconds (`stages`: setup, loudness, render, encode with its
/// `convert` part, audio, mux, finish, wait), summed over the exports of this process (diff two readings).
/// Render and encode overlap (the next batch renders while this one is encoded), so their sum can exceed
/// the export's wall time; `waitMs` is the encoding side's idle wait for the render.
pub fn export_json() -> Value {
    let hw = filmcraft_export::hw_encode_stats();
    let stages: serde_json::Map<String, Value> =
        filmcraft_export::stage_times().into_iter().map(|(s, ns)| (format!("{}Ms", s.name()), json!(ns as f64 / 1e6))).collect();
    json!({"hardware": {"frames": hw.frames, "sessions": hw.sessions, "declined": hw.declined}, "stages": stages})
}

/// The engine's `perf.stats`.
pub fn stats(s: &Session) -> Value {
    let running = s.jobs.iter().filter(|j| j.result.lock().map(|r| r.is_none()).unwrap_or(false)).count();
    json!({
        "decode": decode_json(),
        "export": export_json(),
        "media": {"openSources": s.media.open_sources()},
        "jobs": {"total": s.jobs.len(), "running": running},
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn perf_stats_query_reports_decode_counters() {
        let mut s = Session::default();
        assert!(s.execute("perf.stats", json!({})).is_ok(), "available without a project");
        s.execute("file.openDemoProject", json!({})).unwrap();
        let undo = s.history.undo.len();
        let v = s.execute("perf.stats", json!({})).unwrap();
        for k in [
            "requests",
            "cacheHitRate",
            "seeks",
            "samplesDecoded",
            "samplesSkipped",
            "draftFrames",
            "h264Threads",
            "decodeMs",
            "decodeMsPerSample",
            "framesDecoded",
            "liveDecoders",
            "cacheMB",
            "cacheBudgetMB",
            "planePoolMB",
            "planesReused",
        ] {
            assert!(v["decode"][k].is_number(), "decode.{k} in {v}");
        }
        for k in ["frames", "softwareFrames", "sessions", "declined", "fallbacks", "zeroCopyFrames"] {
            assert!(v["decode"]["hardware"][k].is_number(), "decode.hardware.{k} in {v}");
        }
        assert!(v["decode"]["hardware"]["enabled"].is_boolean());
        assert!(v["decode"]["hardware"]["backend"].is_null() || v["decode"]["hardware"]["backend"].is_string());
        for k in ["frames", "sessions", "declined"] {
            assert!(v["export"]["hardware"][k].is_number(), "export.hardware.{k} in {v}");
        }
        for k in ["setupMs", "loudnessMs", "renderMs", "encodeMs", "convertMs", "audioMs", "muxMs", "finishMs", "waitMs"] {
            assert!(v["export"]["stages"][k].is_number(), "export.stages.{k} in {v}");
        }
        assert!(v["media"]["openSources"].is_number());
        assert_eq!(v["jobs"]["running"], json!(0));
        assert_eq!(s.history.undo.len(), undo, "a query adds no undo step");
    }
}

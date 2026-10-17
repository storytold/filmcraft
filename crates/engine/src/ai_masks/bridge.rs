//! Opt-in loopback transport to the local reference worker. The editor decodes all input frames
//! with its own media sources; no external codec, Python embedding, or runtime dependency is added.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use filmcraft_project::raster_mask::RasterMask;
use serde_json::{Value, json};

use super::{Backend, Input};

const BODY_LIMIT: u64 = 64 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);

struct Local {
    address: SocketAddr,
}

pub(super) fn from_env() -> Option<Arc<dyn Backend>> {
    let address = std::env::var("FILMCRAFT_AI_ROTO_ENDPOINT").ok()?.parse::<SocketAddr>().ok()?;
    (address.ip().is_loopback() && address.port() > 0).then(|| Arc::new(Local { address }) as Arc<dyn Backend>)
}

impl Local {
    fn request(&self, method: &str, path: &str, body: &Value) -> Result<Value, String> {
        let data = if method == "POST" { serde_json::to_vec(body).map_err(|e| e.to_string())? } else { Vec::new() };
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(3))
            .map_err(|e| format!("Local AI worker unavailable at {}: {e}. Start run-editor.sh.", self.address))?;
        stream.set_read_timeout(Some(Duration::from_secs(15))).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
        let header = format!(
            "{method} {path} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.address,
            data.len()
        );
        stream.write_all(header.as_bytes()).and_then(|_| stream.write_all(&data)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        stream.take(BODY_LIMIT + 1).read_to_end(&mut bytes).map_err(|e| format!("AI worker response: {e}"))?;
        if bytes.len() as u64 > BODY_LIMIT {
            return Err("AI result exceeds the 64 MiB response budget. Reduce Frames.".into());
        }
        let end = bytes.windows(4).position(|p| p == b"\r\n\r\n").ok_or("invalid AI worker response")?;
        let status = std::str::from_utf8(bytes.get(..end).ok_or("invalid response header")?).map_err(|e| e.to_string())?;
        let response: Value = serde_json::from_slice(bytes.get(end + 4..).ok_or("invalid response body")?).map_err(|e| e.to_string())?;
        if !status.starts_with("HTTP/1.0 200") && !status.starts_with("HTTP/1.1 200") {
            return Err(response.get("error").and_then(Value::as_str).unwrap_or("AI worker rejected the request").to_string());
        }
        Ok(response)
    }
}

impl Backend for Local {
    fn infer(&self, input: &Input, progress: &filmcraft_export::Progress) -> Result<RasterMask, String> {
        if input.base_alpha.is_some() {
            return Err("Propagated-frame refinement requires the native Rust backend".into());
        }
        let dir = crate::temp_dir().join(format!("filmcraft-ai-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&dir).map_err(|e| format!("AI input cache: {e}"))?;
        let compute = || {
            for (i, frame) in input.frames.iter().enumerate() {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err("Cancelled: nothing was changed".into());
                }
                let mut file = std::fs::File::create(dir.join(format!("{i:05}.ppm"))).map_err(|e| e.to_string())?;
                write!(file, "P6\n{} {}\n255\n", input.width, input.height).map_err(|e| e.to_string())?;
                let rgb: Vec<u8> = frame.rgba.chunks_exact(4).flat_map(|p| p.iter().take(3).copied()).collect();
                file.write_all(&rgb).map_err(|e| e.to_string())?;
            }
            let request = json!({"version": 1, "directory": dir, "width": input.width, "height": input.height,
                "times": input.frames.iter().map(|f| f.time.0).collect::<Vec<_>>(), "end": input.end.0,
                "fps": input.fps, "points": input.points, "quality": input.quality, "band": input.band});
            let response = self.request("POST", "/jobs", &request)?;
            let id =
                response.get("id").and_then(Value::as_str).filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("invalid AI job id")?;
            let path = format!("/jobs/{id}");
            let started = web_time::Instant::now();
            let poll_interval = Duration::from_millis(if input.frames.len() == 1 { 20 } else { 150 });
            loop {
                if started.elapsed() > Duration::from_secs(15 * 60) {
                    let _ = self.request("POST", &format!("{path}/cancel"), &json!({}));
                    return Err("AI worker exceeded the 15-minute job limit. Reduce Frames and retry.".into());
                }
                if progress.cancel.load(Ordering::Relaxed) {
                    let _ = self.request("POST", &format!("{path}/cancel"), &json!({}));
                    return Err("Cancelled: nothing was changed".into());
                }
                let state = self.request("GET", &path, &Value::Null)?;
                if let Some(error) = state.get("error").and_then(Value::as_str) {
                    return Err(error.to_string());
                }
                if let Some(mask) = state.get("mask") {
                    return serde_json::from_value::<RasterMask>(mask.clone()).map_err(|e| format!("invalid AI alpha: {e}"));
                }
                if let Some(done) = state.get("done").and_then(Value::as_u64) {
                    progress.done.store((input.frames.len() as u64).saturating_add(done).min((input.frames.len() * 3) as u64), Ordering::Relaxed);
                }
                if let Some(status) = state.get("status").and_then(Value::as_str) {
                    *progress.status.lock().unwrap_or_else(|e| e.into_inner()) = status.chars().take(256).collect();
                }
                std::thread::sleep(poll_interval);
            }
        };
        let result = compute();
        // Only this request's private, temporary decoded frames are removed. Alpha lives in the
        // document; source footage and the reference worker's model cache are never touched.
        let _ = std::fs::remove_dir_all(&dir);
        result
    }
}

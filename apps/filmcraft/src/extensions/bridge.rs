//! Versioned, permissioned editor boundary. No raw command names or filesystem paths cross it.
use super::package::Manifest;
use filmcraft_engine::Session;
use filmcraft_ui_egui::theme::Tokens;
use serde::Deserialize;
use serde_json::{Value, json};

pub const MAX_MESSAGE: usize = 256 * 1024;
const MAX_MARKERS: usize = 128;
const JS_MAX_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u32,
    method: String,
    #[serde(default)]
    params: Value,
}

pub fn theme(t: &Tokens) -> Value {
    let hex = |c: egui::Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
    json!({"bg": hex(t.panel_bg), "text": hex(t.text), "muted": hex(t.text_dim), "accent": hex(t.accent), "border": hex(t.separator)})
}

fn duration_frames(rate: filmcraft_time::FrameRate, duration: filmcraft_time::Tick) -> i64 {
    filmcraft_export::frame_span(rate, filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, duration)).1.clamp(0, JS_MAX_INTEGER)
}

fn short(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

pub fn snapshot(s: &Session) -> Value {
    let sequence = s.state.active_sequence.and_then(|id| {
        s.active_sequence().map(|q| {
            let rate = q.settings.frame_rate;
            let mut marker_bytes = 0usize;
            let markers: Vec<_> = q
                .markers
                .iter()
                .take(MAX_MARKERS)
                .map(|m| {
                    json!({
                        "id": m.id.0.to_string(), "frame": rate.frame_at(m.start).clamp(0, JS_MAX_INTEGER),
                        "name": short(&m.name, 128), "comment": short(&m.comment, 256), "color": m.color.name(),
                    })
                })
                .take_while(|marker| {
                    marker_bytes = marker_bytes.saturating_add(marker.to_string().len());
                    marker_bytes <= 192 * 1024
                })
                .collect();
            let truncated = q.markers.len() > markers.len();
            json!({"id":id.0.to_string(), "name":s.project.item(id).map(|i| short(&i.name,128)),
                "durationFrames":duration_frames(rate, q.duration()),
                "playheadFrame":rate.frame_at(s.playhead()).clamp(0, JS_MAX_INTEGER), "frameRate":{"num":rate.num,"den":rate.den},
                "markers":markers,"totalMarkers":q.markers.len(),"markersTruncated":truncated})
        })
    });
    json!({"documentId":s.document_generation.to_string(),"revision":s.revision.to_string(),"name":short(&s.project.name,128),"sequence":sequence})
}

fn error(code: &str, message: impl ToString) -> Value {
    json!({"code":code,"message":message.to_string()})
}
fn require(manifest: &Manifest, permission: &str) -> Result<(), Value> {
    if manifest.has_permission(permission) { Ok(()) } else { Err(error("PERMISSION_DENIED", format!("This extension has not been granted {permission}"))) }
}
fn params_only(p: &Value, keys: &[&str]) -> Result<(), Value> {
    let object = p.as_object().ok_or_else(|| error("INVALID_PARAMS", "Expected an object"))?;
    if object.keys().any(|k| !keys.contains(&k.as_str())) {
        return Err(error("INVALID_PARAMS", "Unknown parameter"));
    }
    Ok(())
}
fn target(s: &Session, p: &Value) -> Result<i64, Value> {
    if p.get("documentId").and_then(Value::as_str) != Some(s.document_generation.to_string().as_str()) {
        return Err(error("STALE_DOCUMENT", "The active document changed; request a new snapshot"));
    }
    let id = s.state.active_sequence.ok_or_else(|| error("NO_SEQUENCE", "Open a sequence first"))?;
    if p.get("sequenceId").and_then(Value::as_str) != Some(id.0.to_string().as_str()) {
        return Err(error("STALE_SEQUENCE", "The active sequence changed; request a new snapshot"));
    }
    let q = s.active_sequence().ok_or_else(|| error("NO_SEQUENCE", "Open a sequence first"))?;
    let frame = p.get("frame").and_then(Value::as_i64).ok_or_else(|| error("INVALID_PARAMS", "frame must be an integer"))?;
    if !(0..=duration_frames(q.settings.frame_rate, q.duration())).contains(&frame) {
        return Err(error("INVALID_PARAMS", "frame must be inside the current sequence"));
    }
    Ok(frame)
}

fn call(s: &mut Session, t: &Tokens, m: &Manifest, method: &str, p: &Value) -> Result<Value, Value> {
    match method {
        "host.info" => {
            let mut capabilities = vec!["host.info"];
            for (perm, method) in [("project.read", "document.snapshot"), ("playhead.write", "playhead.seek"), ("markers.write", "markers.add")] {
                if m.has_permission(perm) {
                    capabilities.push(method);
                }
            }
            Ok(json!({"apiVersion":1,"appVersion":env!("CARGO_PKG_VERSION"),"capabilities":capabilities,"permissions":m.permissions,"theme":theme(t)}))
        }
        "document.snapshot" => {
            require(m, "project.read")?;
            Ok(snapshot(s))
        }
        "playhead.seek" => {
            require(m, "playhead.write")?;
            if s.mixrec.active() || s.mcrec.active() || s.voiceover.recording() {
                return Err(error("BUSY", "Finish recording before navigating from an extension"));
            }
            params_only(p, &["documentId", "sequenceId", "frame"])?;
            let frame = target(s, p)?;
            s.execute("playhead.set", json!({"frame":frame})).map_err(|e| error("EDITOR_ERROR", e))?;
            Ok(json!({"frame":s.sequence_rate().frame_at(s.playhead())}))
        }
        "markers.add" => {
            require(m, "markers.write")?;
            params_only(p, &["documentId", "sequenceId", "revision", "frame", "name", "comment"])?;
            let frame = target(s, p)?;
            if p.get("revision").and_then(Value::as_str) != Some(s.revision.to_string().as_str()) {
                return Err(error("STALE_REVISION", "The document changed; review a new snapshot before adding a marker"));
            }
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty() && v.len() <= 512)
                .ok_or_else(|| error("INVALID_PARAMS", "name must contain 1–512 bytes"))?;
            let comment = match p.get("comment") {
                None => "",
                Some(Value::String(v)) if v.len() <= 2048 => v,
                _ => return Err(error("INVALID_PARAMS", "comment must contain at most 2048 bytes")),
            };
            let result =
                s.execute("markers.add", json!({"target":"program","frame":frame,"name":name,"comment":comment})).map_err(|e| error("EDITOR_ERROR", e))?;
            Ok(json!({"markerId":result.get("marker").and_then(Value::as_u64).map(|v| v.to_string()),"revision":s.revision.to_string()}))
        }
        _ => Err(error("UNSUPPORTED_METHOD", "This method is not part of editor API v1")),
    }
}

pub fn dispatch(s: &mut Session, tokens: &Tokens, manifest: &Manifest, text: &str) -> Value {
    if text.len() >= MAX_MESSAGE {
        return json!({"id":null,"error":error("MESSAGE_TOO_LARGE","Request exceeds 256 KiB")});
    }
    let r: Request = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(_) => return json!({"id":null,"error":error("INVALID_REQUEST","Expected {id,method,params}")}),
    };
    if r.id == 0 || r.method.len() > 64 {
        return json!({"id":r.id,"error":error("INVALID_REQUEST","Invalid request id or method")});
    }
    match call(s, tokens, manifest, &r.method, &r.params) {
        Ok(result) => json!({"id":r.id,"result":result}),
        Err(error) => json!({"id":r.id,"error":error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(permissions: &[&str]) -> Manifest {
        serde_json::from_value(json!({"id":"test.notes","name":"Notes","version":"1.0.0","apiVersion":1,"entry":"index.html","permissions":permissions}))
            .unwrap()
    }
    fn request(s: &mut Session, m: &Manifest, method: &str, params: Value) -> Value {
        dispatch(s, &Tokens::for_kind(filmcraft_ui_egui::theme::ThemeKind::Dark), m, &json!({"id":1,"method":method,"params":params}).to_string())
    }
    fn session() -> Session {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s
    }
    fn target_params(s: &Session) -> Value {
        json!({"documentId":s.document_generation.to_string(),"sequenceId":s.state.active_sequence.unwrap().0.to_string(),"revision":s.revision.to_string(),"frame":1,"name":"Review"})
    }
    #[test]
    fn permissions_and_unknown_methods_do_not_mutate() {
        let mut s = session();
        let m = manifest(&[]);
        let before = s.revision;
        let p = target_params(&s);
        assert_eq!(request(&mut s, &m, "markers.add", p)["error"]["code"], "PERMISSION_DENIED");
        assert_eq!(request(&mut s, &m, "file.newProject", json!({}))["error"]["code"], "UNSUPPORTED_METHOD");
        assert_eq!(s.revision, before);
    }
    #[test]
    fn stale_revision_and_document_are_rejected_and_success_is_undoable() {
        let mut s = session();
        let m = manifest(&["markers.write"]);
        let p = target_params(&s);
        let count = s.active_sequence().unwrap().markers.len();
        assert!(request(&mut s, &m, "markers.add", p.clone()).get("result").is_some());
        assert_eq!(s.active_sequence().unwrap().markers.len(), count + 1);
        assert_eq!(request(&mut s, &m, "markers.add", p.clone())["error"]["code"], "STALE_REVISION");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active_sequence().unwrap().markers.len(), count);
        s.execute("file.openDemoProject", json!({})).unwrap();
        assert_eq!(request(&mut s, &m, "markers.add", p)["error"]["code"], "STALE_DOCUMENT");
    }
    #[test]
    fn seeking_rejects_other_sequences_negative_and_excessive_frames() {
        let mut s = session();
        let m = manifest(&["playhead.write"]);
        let mut p = target_params(&s);
        p.as_object_mut().unwrap().remove("revision");
        p.as_object_mut().unwrap().remove("name");
        p["frame"] = json!(-1);
        assert_eq!(request(&mut s, &m, "playhead.seek", p.clone())["error"]["code"], "INVALID_PARAMS");
        p["frame"] = json!(i64::MAX);
        assert_eq!(request(&mut s, &m, "playhead.seek", p.clone())["error"]["code"], "INVALID_PARAMS");
        p["frame"] = json!(1);
        p["sequenceId"] = json!("wrong");
        assert_eq!(request(&mut s, &m, "playhead.seek", p)["error"]["code"], "STALE_SEQUENCE");
    }
    #[test]
    fn snapshots_do_not_expose_file_paths() {
        let mut s = session();
        s.path = Some("/private/example.fcproj".into());
        let result = request(&mut s, &manifest(&["project.read"]), "document.snapshot", json!({}));
        assert!(!result.to_string().contains("/private/"));
        assert!(result["result"]["revision"].is_string());
    }
    #[test]
    fn escaped_snapshot_strings_stay_inside_the_transport_budget() {
        let mut s = session();
        for _ in 0..128 {
            s.execute("markers.add", json!({"target":"program","frame":0,"name":"\u{1}".repeat(128),"comment":"\u{2}".repeat(256)})).unwrap();
        }
        let snapshot = snapshot(&s);
        assert!(snapshot.to_string().len() < MAX_MESSAGE - 1024);
        assert_eq!(snapshot["sequence"]["markersTruncated"], true);
        assert!(snapshot["sequence"]["markers"].as_array().unwrap().len() < 128);
    }
    #[test]
    fn duration_includes_a_partial_last_frame() {
        let rate = filmcraft_time::FrameRate::FPS_24;
        assert_eq!(duration_frames(rate, filmcraft_time::Tick::ZERO), 0);
        assert_eq!(duration_frames(rate, rate.tick_of(2) + filmcraft_time::Tick(1)), 3);
    }
    #[test]
    fn marker_is_saved_and_old_context_is_invalid_after_reopening() {
        let mut s = session();
        let m = manifest(&["markers.write"]);
        let p = target_params(&s);
        assert!(request(&mut s, &m, "markers.add", p.clone()).get("result").is_some());
        let dir = std::env::temp_dir().join(format!("filmcraft-extension-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("markers.fcproj");
        s.execute("file.saveAs", json!({"path":path})).unwrap();
        s.execute("file.open", json!({"path":path})).unwrap();
        assert!(s.active_sequence().unwrap().markers.iter().any(|m| m.name == "Review"));
        assert_eq!(request(&mut s, &m, "markers.add", p)["error"]["code"], "STALE_DOCUMENT");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

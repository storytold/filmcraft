//! Sequence review notes as a spreadsheet-safe CSV, using the host's atomic file service.

use filmcraft_project::Label;
use filmcraft_time::{Tick, format_timecode_frames};
use serde_json::{Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};

const CMD: &str = "markers.exportCsv";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_MARKERS: usize = 100_000;

fn label_name(s: &Session, color: Label) -> &str {
    s.prefs.labels.colors.get(&crate::settings::label_id(color)).map(|c| c.name.trim()).filter(|n| !n.is_empty()).unwrap_or(color.name())
}

pub(crate) fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let path = p
        .get("path")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty() && !v.contains('\0'))
        .ok_or_else(|| bad(CMD, "need a non-empty `path` without NUL characters"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    seq.settings.validate().map_err(|e| bad(CMD, e))?;
    let name = s.state.active_sequence.and_then(|id| s.project.item(id)).map(|i| i.name.as_str()).unwrap_or("Sequence");
    if seq.markers.len() > MAX_MARKERS {
        return Err(bad(CMD, "too many markers (maximum 100000)"));
    }
    // Bound the worst-case quoted output before sorting or allocating it. 1024 covers numeric,
    // timecode and enum fields; arbitrary text may double in size when every character is a quote.
    let mut bound = 512usize;
    for m in &seq.markers {
        let row = name
            .len()
            .checked_add(m.name.len())
            .and_then(|n| n.checked_add(m.comment.len()))
            .and_then(|n| n.checked_add(label_name(s, m.color).len()))
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(1024));
        bound = row.and_then(|n| bound.checked_add(n)).filter(|n| *n <= MAX_BYTES).ok_or_else(|| bad(CMD, "marker report exceeds the 16 MiB limit"))?;
    }
    let rate = seq.settings.frame_rate;
    let mut markers: Vec<_> = seq.markers.iter().collect();
    markers.sort_by_key(|m| (m.start, m.id));
    let mut csv =
        String::from("Sequence,Marker ID,Name,Comment,Kind,Color,Start Timecode,End Timecode,Duration Frames,Start Ticks,Duration Ticks,Frame Rate\r\n");
    for m in markers {
        if m.start.0 < 0 || m.duration.0 < 0 {
            return Err(bad(CMD, format!("marker {} has a negative start or duration", m.id.0)));
        }
        let end = m.start.0.checked_add(m.duration.0).ok_or_else(|| bad(CMD, format!("marker {} end overflows", m.id.0)))?;
        let tc = |t: Tick| -> Result<String> {
            let frame = rate.frame_at(t).checked_add(seq.start_timecode).ok_or_else(|| bad(CMD, "sequence timecode overflows"))?;
            Ok(format_timecode_frames(frame, rate, seq.settings.drop_frame))
        };
        let fields = [
            name.to_string(),
            m.id.0.to_string(),
            m.name.clone(),
            m.comment.clone(),
            format!("{:?}", m.kind),
            label_name(s, m.color).to_string(),
            tc(m.start)?,
            tc(Tick(end))?,
            rate.frame_at(m.duration).to_string(),
            m.start.0.to_string(),
            m.duration.0.to_string(),
            format!("{}/{}", rate.num, rate.den),
        ];
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                csv.push(',');
            }
            csv.push('"');
            // Quoting alone does not stop spreadsheet formulas. Escape only arbitrary text,
            // including formulas preceded by whitespace; numeric/timecode fields stay numeric.
            if matches!(i, 0 | 2 | 3 | 5) && field.trim_start().starts_with(['=', '+', '-', '@']) {
                csv.push('\'');
            }
            for ch in field.chars() {
                if ch == '"' {
                    csv.push('"');
                }
                csv.push(ch);
            }
            csv.push('"');
        }
        csv.push_str("\r\n");
    }
    s.services.write_file(path, csv.as_bytes()).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "markers": seq.markers.len(), "bytes": csv.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use filmcraft_project::{Label, Marker, MarkerId, MarkerKind};
    use filmcraft_time::FrameRate;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Files {
        writes: Mutex<Vec<(String, Vec<u8>)>>,
        fail: bool,
    }
    impl Services for Files {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            if self.fail {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            self.writes.lock().unwrap().push((path.into(), data.to_vec()));
            Ok(())
        }
    }
    fn session(files: Arc<Files>) -> Session {
        let mut s = Session { services: files, ..Default::default() };
        s.execute("file.newSequence", json!({"name":"Review, 日本語", "fps":24})).unwrap();
        s
    }
    fn marker(id: u64, start: Tick, duration: Tick, name: &str) -> Marker {
        Marker {
            id: MarkerId(id),
            start,
            duration,
            name: name.into(),
            comment: "First line\r\nSecond, \"quoted\"".into(),
            kind: MarkerKind::Chapter,
            color: Label::Iris,
        }
    }
    fn set_markers(s: &mut Session, markers: Vec<Marker>) {
        s.edit_sequence("Review markers", |q, _, _| {
            q.markers = markers;
            Ok(())
        })
        .unwrap();
    }
    fn written(files: &Files) -> String {
        String::from_utf8(files.writes.lock().unwrap().last().unwrap().1.clone()).unwrap()
    }

    #[test]
    fn command_exports_sorted_unicode_review_notes_without_changing_the_edit() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        let rate = FrameRate::FPS_24;
        set_markers(&mut s, vec![marker(100, rate.tick_of(48), rate.tick_of(24), "Later"), marker(101, rate.tick_of(24), Tick::ZERO, "早い")]);
        s.execute("markers.markIn", json!({"frame":12})).unwrap();
        s.state.hidden_marker_colors.push(Label::Iris);
        let before = s.project.clone();
        let state = serde_json::to_value(&s.state).unwrap();
        let undo = s.history.undo.len();
        let result = s.execute(CMD, json!({"path":"review.csv"})).unwrap();
        let csv = written(&files);
        assert_eq!(result["markers"], 2);
        assert_eq!(result["bytes"], csv.len());
        assert!(csv.contains("\"Review, 日本語\",\"101\",\"早い\",\"First line\r\nSecond, \"\"quoted\"\"\""));
        assert!(csv.find("\"101\"").unwrap() < csv.find("\"100\"").unwrap());
        assert!(csv.contains("\"00:00:02:00\",\"00:00:03:00\",\"24\",\"508032000000\",\"254016000000\",\"24/1\"\r\n"));
        assert_eq!(*s.project, *before);
        assert_eq!(serde_json::to_value(&s.state).unwrap(), state);
        assert_eq!(s.history.undo.len(), undo);
        s.undo();
        s.redo();
        assert_eq!(*s.project, *before);
    }

    #[test]
    fn drop_frame_and_sequence_start_timecode_are_used_for_both_bounds() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        let rate = FrameRate::FPS_29_97;
        s.edit_sequence("Timing", |q, _, _| {
            q.settings.frame_rate = rate;
            q.settings.drop_frame = true;
            q.start_timecode = 107892;
            q.markers = vec![marker(100, rate.tick_of(1800), rate.tick_of(30), "DF")];
            Ok(())
        })
        .unwrap();
        s.execute(CMD, json!({"path":"df.csv"})).unwrap();
        assert!(written(&files).contains("\"01;01;00;02\",\"01;01;01;02\",\"30\""));
        assert!(written(&files).contains("\"30000/1001\""));
    }

    #[test]
    fn spreadsheet_formulas_are_escaped_in_all_user_text_columns() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        for text in ["=1+1", "+1+1", "-1+1", "@SUM(1)", "\t =1+1"] {
            let mut m = marker(100, Tick::ZERO, Tick::ZERO, text);
            m.comment = text.into();
            set_markers(&mut s, vec![m]);
            let id = s.state.active_sequence.unwrap();
            s.edit("Name", |p, _| {
                p.items.get_mut(&id).unwrap().name = text.into();
                Ok(())
            })
            .unwrap();
            s.execute(CMD, json!({"path":"safe.csv"})).unwrap();
            assert_eq!(written(&files).matches(&format!("\"'{text}\"")).count(), 3);
            s.prefs.labels.colors.get_mut("iris").unwrap().name = text.into();
            s.execute(CMD, json!({"path":"safe-label.csv"})).unwrap();
            assert!(written(&files).contains(&format!("\"Chapter\",\"'{}\"", text.trim())));
            s.prefs.labels.colors.get_mut("iris").unwrap().name = "Iris".into();
        }
    }

    #[test]
    fn empty_sequence_writes_a_header_and_no_sequence_is_disabled() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        let spec = crate::find_command(CMD).unwrap();
        assert_eq!(spec.menu, ["Markers"]);
        assert!(spec.params.contains("path"));
        assert!(spec.shortcut.is_none());
        assert_eq!(s.execute(CMD, json!({"path":"empty.csv"})).unwrap()["markers"], 0);
        assert_eq!(written(&files).lines().count(), 1);
        assert!((spec.enabled)(&Session::default()).is_err());
        assert!(matches!(Session::default().execute(CMD, json!({"path":"none.csv"})), Err(EngineError::Disabled(..))));
    }

    #[test]
    fn hostile_parameters_and_damaged_timing_fail_before_writing() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        for p in [Value::Null, json!({}), json!({"path":0}), json!({"path":" "}), json!({"path":"bad\0.csv"})] {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(CMD, p))).unwrap().is_err());
        }
        let id = s.state.active_sequence.unwrap();
        for (start, duration) in [(-1, 0), (0, -1), (i64::MAX, 1)] {
            Arc::make_mut(&mut s.project).sequence_mut(id).unwrap().markers = vec![marker(100, Tick(start), Tick(duration), "Bad")];
            assert!(s.execute(CMD, json!({"path":"bad.csv"})).is_err());
        }
        let q = Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
        q.markers = vec![marker(100, Tick::ZERO, Tick::ZERO, "Bad")];
        q.settings.frame_rate.num = i64::MAX;
        assert!(s.execute(CMD, json!({"path":"bad.csv"})).is_err());
        let q = Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
        q.settings.frame_rate = FrameRate::FPS_24;
        q.start_timecode = i64::MAX;
        q.markers[0].start = FrameRate::FPS_24.tick_of(1);
        assert!(s.execute(CMD, json!({"path":"bad.csv"})).is_err());
        q_reset(&mut s, id);
        assert!(files.writes.lock().unwrap().is_empty());
        s.execute(CMD, json!({"path":"recovered.csv"})).unwrap();
    }
    fn q_reset(s: &mut Session, id: filmcraft_project::ItemId) {
        let q = Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
        q.start_timecode = 0;
        q.markers.clear();
    }

    #[test]
    fn oversized_reports_and_write_errors_are_actionable() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        set_markers(&mut s, vec![marker(100, Tick::ZERO, Tick::ZERO, &"x".repeat(MAX_BYTES / 2))]);
        assert!(s.execute(CMD, json!({"path":"huge.csv"})).unwrap_err().to_string().contains("16 MiB"));
        assert!(files.writes.lock().unwrap().is_empty());
        s.services = Arc::new(Files { fail: true, ..Default::default() });
        set_markers(&mut s, vec![]);
        s.saved_revision = s.revision;
        assert!(!s.is_dirty());
        let before = s.project.clone();
        assert!(s.execute(CMD, json!({"path":"denied.csv"})).unwrap_err().to_string().contains("denied.csv"));
        assert_eq!(*s.project, *before);
        assert!(!s.is_dirty());
    }

    #[test]
    fn locked_tracks_custom_labels_and_unusual_paths_export_without_dirtying() {
        let files = Arc::new(Files::default());
        let mut s = session(files.clone());
        let rate = FrameRate::FPS_23_976;
        s.edit_sequence("Locked review", |q, _, _| {
            q.settings.frame_rate = rate;
            for t in q.video_tracks.iter_mut().chain(&mut q.audio_tracks) {
                t.locked = true;
            }
            q.markers =
                vec![marker(101, Tick(rate.tick_of(24).0 + 1), Tick(1), "A, \"B\"\n字幕"), marker(100, Tick(rate.tick_of(24).0 + 1), Tick::ZERO, "Point")];
            Ok(())
        })
        .unwrap();
        s.prefs.labels.colors.get_mut("iris").unwrap().name = "Review, \"urgent\"\n紫".into();
        let id = s.state.active_sequence.unwrap();
        s.edit("Review name", |p, _| {
            p.items.get_mut(&id).unwrap().name = "Cut, \"night\"\n日本語".into();
            Ok(())
        })
        .unwrap();
        s.saved_revision = s.revision;
        let path = "review, \"notes\"\n日本語.csv";
        s.execute(CMD, json!({"path":path})).unwrap();
        assert_eq!(files.writes.lock().unwrap().last().unwrap().0, path);
        let csv = written(&files);
        assert!(csv.contains("\"Cut, \"\"night\"\"\n日本語\""));
        assert!(csv.contains("\"Review, \"\"urgent\"\"\n紫\""));
        assert!(csv.contains("\"A, \"\"B\"\"\n字幕\""));
        assert!(csv.find("\"100\"").unwrap() < csv.find("\"101\"").unwrap());
        assert!(csv.contains(&format!("\"00:00:01:00\",\"00:00:01:00\",\"0\",\"{}\",\"1\",\"24000/1001\"", rate.tick_of(24).0 + 1)));
        assert!(!s.is_dirty());
    }
}

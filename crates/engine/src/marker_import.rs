//! Create many markers at once, from a JSON list or a CSV in the `markers.exportCsv` format, as one undo step.

use filmcraft_project::{Label, Marker, MarkerId, MarkerKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{bad, marker_color_p, str_p, time_p_at};
use crate::{EngineError, Result, Session};

const CMD: &str = "markers.import";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_MARKERS: usize = 100_000;
/// The export has 12 columns; a wider row is not a marker list.
const MAX_COLUMNS: usize = 64;
const KINDS: [(&str, MarkerKind); 5] = [
    ("Comment", MarkerKind::Comment),
    ("Chapter", MarkerKind::Chapter),
    ("Segmentation", MarkerKind::Segmentation),
    ("WebLink", MarkerKind::WebLink),
    ("FlashCue", MarkerKind::FlashCue),
];

/// One marker as asked for, before the sequence's marker list is touched.
struct Draft {
    start: Tick,
    duration: Tick,
    name: String,
    comment: String,
    kind: MarkerKind,
    color: Label,
}

fn kind_named(name: &str) -> Option<MarkerKind> {
    KINDS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|(_, k)| *k)
}

/// A marker color by the name `markers.exportCsv` writes for it (the name from the label preferences, else the default
/// one), then by its default name. A name shared by several labels is refused rather than guessed.
fn color_named(s: &Session, name: &str) -> Result<Label> {
    let name = name.trim();
    let effective = |l: Label| s.prefs.labels.colors.get(&crate::settings::label_id(l)).map(|c| c.name.trim()).filter(|n| !n.is_empty()).unwrap_or(l.name());
    let mut named = Label::ALL.iter().copied().filter(|l| effective(*l) == name);
    match (named.next(), named.next()) {
        (Some(_), Some(_)) => Err(bad(CMD, format!("the color name `{name}` belongs to more than one label"))),
        (Some(l), None) => Ok(l),
        (None, _) => Label::from_name(name).ok_or_else(|| bad(CMD, format!("unknown marker color `{name}`"))),
    }
}

/// Records of a CSV text (RFC 4180: quoted fields may hold commas, quotes and line breaks).
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => {
                if row.len() >= MAX_COLUMNS {
                    return Err(bad(CMD, "a CSV row has too many columns"));
                }
                row.push(std::mem::take(&mut field));
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                if rows.len() > MAX_MARKERS + 1 {
                    return Err(bad(CMD, "too many markers (maximum 100000)"));
                }
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err(bad(CMD, "the CSV ends inside a quoted field"));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// Undo the apostrophe `markers.exportCsv` puts before text a spreadsheet would run as a formula. The export cannot tell
/// that apostrophe from a literal one typed before the same text, so a literal one is read as the escape.
fn unescape(field: &str) -> String {
    match field.strip_prefix('\'') {
        Some(rest) if rest.trim_start().starts_with(['=', '+', '-', '@']) => rest.to_string(),
        _ => field.to_string(),
    }
}

fn from_csv(s: &Session, text: &str) -> Result<Vec<Draft>> {
    let rows = parse_csv(text)?;
    let mut rows = rows.into_iter();
    let header = rows.next().ok_or_else(|| bad(CMD, "the CSV is empty"))?;
    let column = |name: &str| header.iter().position(|h| h.trim().trim_start_matches('\u{feff}').eq_ignore_ascii_case(name));
    let missing = |name: &str| bad(CMD, format!("the CSV has no `{name}` column"));
    let start_col = column("Start Ticks").ok_or_else(|| missing("Start Ticks"))?;
    let (duration_col, name_col, comment_col, kind_col, color_col) =
        (column("Duration Ticks"), column("Name"), column("Comment"), column("Kind"), column("Color"));
    let mut out = Vec::new();
    for (i, row) in rows.enumerate() {
        if row.len() == 1 && row[0].is_empty() {
            continue;
        }
        let line = i + 2;
        let cell = |col: Option<usize>| col.and_then(|c| row.get(c)).map(String::as_str).unwrap_or("");
        let ticks = |col: usize, what: &str| -> Result<Tick> {
            let v = row.get(col).map(|v| v.trim()).unwrap_or("");
            let t = v.parse::<i64>().map_err(|_| bad(CMD, format!("row {line}: {what} `{v}` is not a whole number of ticks")))?;
            if t < 0 {
                return Err(bad(CMD, format!("row {line}: {what} must not be negative")));
            }
            Ok(Tick(t))
        };
        let kind_text = cell(kind_col);
        let color_text = unescape(cell(color_col));
        out.push(Draft {
            start: ticks(start_col, "Start Ticks")?,
            duration: match duration_col {
                Some(c) => ticks(c, "Duration Ticks")?,
                None => Tick::ZERO,
            },
            name: unescape(cell(name_col)),
            comment: unescape(cell(comment_col)),
            kind: if kind_text.trim().is_empty() {
                MarkerKind::Comment
            } else {
                kind_named(kind_text).ok_or_else(|| bad(CMD, format!("row {line}: unknown marker kind `{kind_text}`")))?
            },
            color: if color_text.trim().is_empty() { Label::Green } else { color_named(s, &color_text).map_err(|e| bad(CMD, format!("row {line}: {e}")))? },
        });
    }
    Ok(out)
}

fn from_list(s: &Session, list: &[Value]) -> Result<Vec<Draft>> {
    let rate = s.sequence_rate();
    let drop_frame = s.active_sequence().is_some_and(|q| q.settings.drop_frame);
    list.iter()
        .enumerate()
        .map(|(i, m)| {
            let at = |msg: &str| bad(CMD, format!("marker {i}: {msg}"));
            if !m.is_object() {
                return Err(at("must be an object"));
            }
            let start = time_p_at(m, "", rate, drop_frame, Tick::ZERO).ok_or_else(|| at("needs a `time` (ticks) or a `frame`, `seconds` or `timecode`"))?;
            if start < Tick::ZERO {
                return Err(at("time must not be negative"));
            }
            let duration = match m.get("durationFrames") {
                None | Some(Value::Null) => Tick::ZERO,
                Some(v) => {
                    let f = v.as_i64().filter(|f| *f >= 0).ok_or_else(|| at("`durationFrames` must be a whole number of frames, 0 or more"))?;
                    rate.tick_of(f)
                }
            };
            let text = |k: &str| match m.get(k) {
                None | Some(Value::Null) => Ok(String::new()),
                Some(Value::String(v)) => Ok(v.clone()),
                Some(_) => Err(at(&format!("`{k}` must be text"))),
            };
            let kind = match str_p(m, "kind") {
                None => MarkerKind::Comment,
                Some(k) => kind_named(k).ok_or_else(|| at(&format!("unknown marker kind `{k}`")))?,
            };
            Ok(Draft {
                start,
                duration,
                name: text("name")?,
                comment: text("comment")?,
                kind,
                color: marker_color_p(m, CMD).map_err(|e| at(&e.to_string()))?.unwrap_or(Label::Green),
            })
        })
        .collect()
}

pub(crate) fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let existing = s.active_sequence().ok_or(EngineError::NoSequence)?.markers.len();
    let drafts = match (p.get("markers"), str_p(p, "path")) {
        (Some(Value::Array(list)), None) => {
            if list.len() > MAX_MARKERS {
                return Err(bad(CMD, "too many markers (maximum 100000)"));
            }
            from_list(s, list)?
        }
        (None, Some(path)) if !path.trim().is_empty() && !path.contains('\0') => {
            // the size first: the whole file is not read into memory just to find out it is too large
            let size = s.services.file_size(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            if size > MAX_BYTES as u64 {
                return Err(bad(CMD, "the CSV exceeds the 16 MiB limit"));
            }
            let data = s.services.read_file(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            if data.len() > MAX_BYTES {
                return Err(bad(CMD, "the CSV exceeds the 16 MiB limit"));
            }
            let text = std::str::from_utf8(&data).map_err(|_| bad(CMD, "the CSV is not UTF-8 text"))?;
            from_csv(s, text)?
        }
        _ => return Err(bad(CMD, "need either a `markers` list or a CSV `path`, not both")),
    };
    if drafts.len().checked_add(existing).is_none_or(|n| n > MAX_MARKERS) {
        return Err(bad(CMD, "too many markers (maximum 100000)"));
    }
    if let Some(i) = drafts.iter().position(|d| d.start.0.checked_add(d.duration.0).is_none()) {
        return Err(bad(CMD, format!("marker {i}: its end overflows")));
    }
    let count = drafts.len();
    if count == 0 {
        // nothing to add: no undo step, and the redo history stays
        return Ok(json!({"imported": 0, "markers": []}));
    }
    let ids = s.edit_sequence("Import Markers", |q, ctx, _| {
        let mut ids = Vec::with_capacity(drafts.len());
        for d in drafts {
            let id = MarkerId(ctx.alloc());
            q.markers.push(Marker { id, start: d.start, duration: d.duration, name: d.name, comment: d.comment, kind: d.kind, color: d.color });
            ids.push(id.0);
        }
        q.markers.sort_by_key(|m| m.start);
        Ok(ids)
    })?;
    Ok(json!({"imported": count, "markers": ids}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use std::sync::{Arc, Mutex};

    /// An in-memory file service: `markers.exportCsv` writes into it and `markers.import` reads it back.
    #[derive(Default)]
    struct Files(Mutex<std::collections::HashMap<String, Vec<u8>>>);
    impl Services for Files {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            self.0.lock().unwrap().get(path).cloned().ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().insert(path.into(), data.to_vec());
            Ok(())
        }
    }

    fn session() -> (Session, Arc<Files>) {
        let files = Arc::new(Files::default());
        let mut s = Session { services: files.clone(), ..Default::default() };
        s.execute("file.newSequence", json!({"name": "Seq", "fps": 24})).unwrap();
        (s, files)
    }

    fn markers(s: &Session) -> Vec<(Tick, Tick, String, String, MarkerKind, Label)> {
        s.active_sequence().unwrap().markers.iter().map(|m| (m.start, m.duration, m.name.clone(), m.comment.clone(), m.kind, m.color)).collect()
    }

    #[test]
    fn a_list_creates_every_marker_as_one_undo_step() {
        let (mut s, _) = session();
        let undo = s.history.undo.len();
        let r = s
            .execute(
                CMD,
                json!({"markers": [
                    {"time": 254_016_000_000i64, "name": "A", "comment": "c", "color": "Rose", "durationFrames": 24},
                    {"frame": 12, "kind": "Chapter"},
                    {"seconds": 0.5, "name": "B"},
                ]}),
            )
            .unwrap();
        assert_eq!(r["imported"], 3);
        let m = markers(&s);
        assert_eq!(m.len(), 3);
        let half = s.sequence_rate().tick_of(12);
        assert_eq!((m[0].0, m[1].0, m[2].0), (half, half, Tick(254_016_000_000)));
        assert_eq!((m[2].2.as_str(), m[2].3.as_str(), m[2].5, m[2].1), ("A", "c", Label::Rose, s.sequence_rate().tick_of(24)));
        assert_eq!(m[0].4, MarkerKind::Chapter);
        assert_eq!(s.history.undo.len(), undo + 1);
        s.undo();
        assert!(markers(&s).is_empty());
        s.redo();
        assert_eq!(markers(&s).len(), 3);
    }

    #[test]
    fn export_then_import_into_a_fresh_sequence_reproduces_the_markers() {
        let (mut s, _) = session();
        s.prefs.labels.colors.get_mut("iris").unwrap().name = "Needs, \"work\"".into();
        s.execute(
            CMD,
            json!({"markers": [
                {"frame": 48, "name": "=SUM(1)", "comment": "line\r\nbreak, \"quoted\"", "color": "Iris", "durationFrames": 10, "kind": "Chapter"},
                {"frame": 24, "name": "早い", "color": "Rose"},
            ]}),
        )
        .unwrap();
        let before = markers(&s);
        s.execute("markers.exportCsv", json!({"path": "m.csv"})).unwrap();
        s.execute("file.newSequence", json!({"name": "Fresh", "fps": 24})).unwrap();
        assert!(markers(&s).is_empty());
        let r = s.execute(CMD, json!({"path": "m.csv"})).unwrap();
        assert_eq!(r["imported"], 2);
        assert_eq!(markers(&s), before);
        s.undo();
        assert!(markers(&s).is_empty());
    }

    #[test]
    fn custom_label_names_win_over_default_names_and_ambiguity_is_refused() {
        let (mut s, files) = session();
        // Iris is renamed to the default name of Rose: the exporter writes "Rose" for an Iris marker
        s.prefs.labels.colors.get_mut("iris").unwrap().name = "Mint".into();
        s.prefs.labels.colors.get_mut("rose").unwrap().name = "Iris".into();
        s.execute(CMD, json!({"markers": [{"time": 0, "color": "Iris"}, {"time": 1, "color": "Rose"}]})).unwrap();
        let before = markers(&s);
        s.execute("markers.exportCsv", json!({"path": "swap.csv"})).unwrap();
        s.execute("file.newSequence", json!({"name": "Fresh", "fps": 24})).unwrap();
        s.execute(CMD, json!({"path": "swap.csv"})).unwrap();
        assert_eq!(markers(&s), before);
        // two labels with the same effective name cannot be told apart
        s.prefs.labels.colors.get_mut("iris").unwrap().name = "Rose".into();
        s.prefs.labels.colors.get_mut("rose").unwrap().name = "".into();
        s.execute("markers.exportCsv", json!({"path": "same.csv"})).unwrap();
        assert!(s.execute(CMD, json!({"path": "same.csv"})).unwrap_err().to_string().contains("more than one label"));
        assert!(files.read_file("same.csv").is_ok());
    }

    #[test]
    fn hostile_input_is_rejected_without_changing_the_sequence() {
        let (mut s, files) = session();
        let undo = s.history.undo.len();
        let huge: Vec<Value> = (0..100_001).map(|i| json!({"time": i})).collect();
        let bad_lists = [
            json!({}),
            json!({"markers": [], "path": "x.csv"}),
            json!({"markers": "no"}),
            json!({"markers": [1]}),
            json!({"markers": [{"name": "no time"}]}),
            json!({"markers": [{"time": -1}]}),
            json!({"markers": [{"time": 0, "durationFrames": -1}]}),
            json!({"markers": [{"time": i64::MAX, "durationFrames": 5}]}),
            json!({"markers": [{"time": 0, "color": "Nope"}]}),
            json!({"markers": [{"time": 0, "kind": "Nope"}]}),
            json!({"markers": [{"time": 0, "name": 5}]}),
            json!({"markers": huge}),
            json!({"path": ""}),
            json!({"path": "bad\0.csv"}),
            json!({"path": "missing.csv"}),
        ];
        for p in bad_lists {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(CMD, p.clone())));
            assert!(r.unwrap().is_err(), "{p}");
        }
        let header = "Sequence,Marker ID,Name,Comment,Kind,Color,Start Timecode,End Timecode,Duration Frames,Start Ticks,Duration Ticks,Frame Rate\r\n";
        let csvs = [
            String::new(),
            "Name,Comment\r\n\"a\",\"b\"\r\n".to_string(),
            format!("{header}\"s\",\"1\",\"n\",\"c\",\"Comment\",\"Green\",\"\",\"\",\"\",\"-5\",\"0\",\"24/1\"\r\n"),
            format!("{header}\"s\",\"1\",\"n\",\"c\",\"Comment\",\"Green\",\"\",\"\",\"\",\"x\",\"0\",\"24/1\"\r\n"),
            format!("{header}\"s\",\"1\",\"n\",\"c\",\"Comment\",\"Green\",\"\",\"\",\"\",\"99999999999999999999\",\"0\",\"24/1\"\r\n"),
            format!("{header}\"s\",\"1\",\"n\",\"c\",\"Bogus\",\"Green\",\"\",\"\",\"\",\"1\",\"0\",\"24/1\"\r\n"),
            format!("{header}\"s\",\"1\",\"unterminated"),
            format!("{header}\"s\",\"1\",\"n\",\"c\",\"Comment\",\"Green\",\"\",\"\",\"\",\"{}\",\"1\",\"24/1\"\r\n", i64::MAX),
        ];
        for (i, csv) in csvs.iter().enumerate() {
            files.write_file("bad.csv", csv.as_bytes()).unwrap();
            assert!(s.execute(CMD, json!({"path": "bad.csv"})).is_err(), "csv {i}");
        }
        files.write_file("big.csv", &vec![b'a'; MAX_BYTES + 1]).unwrap();
        assert!(s.execute(CMD, json!({"path": "big.csv"})).unwrap_err().to_string().contains("16 MiB"));
        files.write_file("bin.csv", &[0xff, 0xfe, 0x00]).unwrap();
        assert!(s.execute(CMD, json!({"path": "bin.csv"})).is_err());
        assert!(markers(&s).is_empty());
        assert_eq!(s.history.undo.len(), undo);
    }

    #[test]
    fn an_oversized_file_is_refused_by_its_size_without_being_read() {
        struct Huge;
        impl Services for Huge {
            fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
                panic!("the file must not be read");
            }
            fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
                Ok(())
            }
            fn file_size(&self, _: &str) -> std::io::Result<u64> {
                Ok(u64::MAX)
            }
        }
        let (mut s, _) = session();
        s.services = Arc::new(Huge);
        assert!(s.execute(CMD, json!({"path": "huge.csv"})).unwrap_err().to_string().contains("16 MiB"));
    }

    #[test]
    fn an_empty_import_changes_nothing_and_keeps_the_redo_history() {
        let (mut s, files) = session();
        s.execute("markers.add", json!({"time": 0})).unwrap();
        s.undo();
        let undo = s.history.undo.len();
        assert_eq!(s.execute(CMD, json!({"markers": []})).unwrap()["imported"], 0);
        s.execute("markers.exportCsv", json!({"path": "empty.csv"})).unwrap();
        assert_eq!(s.execute(CMD, json!({"path": "empty.csv"})).unwrap()["imported"], 0);
        assert_eq!(s.history.undo.len(), undo);
        s.redo();
        assert_eq!(markers(&s).len(), 1);
        files.write_file("wide.csv", format!("Start Ticks{}\n", ",".repeat(1_000_000)).as_bytes()).unwrap();
        assert!(s.execute(CMD, json!({"path": "wide.csv"})).unwrap_err().to_string().contains("too many columns"));
    }

    #[test]
    fn the_total_marker_count_is_capped_with_existing_markers() {
        let (mut s, _) = session();
        let first: Vec<Value> = (0..60_000).map(|i| json!({"time": i})).collect();
        s.execute(CMD, json!({"markers": first})).unwrap();
        let more: Vec<Value> = (0..60_000).map(|i| json!({"time": i})).collect();
        assert!(s.execute(CMD, json!({"markers": more})).unwrap_err().to_string().contains("too many"));
        assert_eq!(markers(&s).len(), 60_000);
    }

    #[test]
    fn registered_in_the_markers_menu_and_needs_a_sequence() {
        let spec = crate::find_command(CMD).unwrap();
        assert_eq!(spec.menu, ["Markers"]);
        assert!(Session::default().execute(CMD, json!({"markers": []})).is_err());
    }
}

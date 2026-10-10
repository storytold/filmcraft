//! Engine side of the M12.6 panels (the scopes are in [`crate::scopes`]).
//!
//! | Id | What |
//! |---|---|
//! | `events.list` / `events.clear` | the Events panel: warnings and errors of commands, background jobs and the auto-save worker ([`EventLog`]) |
//! | `metadata.get` / `metadata.set` | the Metadata panel: an item's clip and file properties; editable log fields (undoable) |
//!
//! The Progress panel uses `jobs.list` / `jobs.cancel`; the Timecode panel and the Reference
//! Monitor are frontend state only.

use std::collections::{BTreeMap, VecDeque};

use filmcraft_project::{ItemId, ItemKind, Label, MediaRef, Project};
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, item_p, str_p, u64_p};
use crate::{Result, Session};

// ------------------------------------------------------------------------------------ events log

/// Severity of an Events panel entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "Info",
            Level::Warning => "Warning",
            Level::Error => "Error",
        }
    }
    pub fn from_name(s: &str) -> Option<Level> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Some(Level::Info),
            "warning" | "warn" => Some(Level::Warning),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// One Events panel entry. Repeats of the newest entry (same level, source and message) bump
/// `count` instead of adding a row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: u64,
    pub level: Level,
    /// Command id, `job`, `autosave` or `app`.
    pub source: String,
    pub message: String,
    pub count: u32,
    /// Wall-clock time of the last occurrence (ms since the Unix epoch).
    pub time_ms: u64,
}

/// The session's event log (newest last, at most [`EventLog::LIMIT`] entries).
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    pub entries: VecDeque<LogEntry>,
    next: u64,
    /// Jobs already reported: id → finished.
    jobs: BTreeMap<u64, bool>,
}

fn now_ms() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl EventLog {
    pub const LIMIT: usize = 1000;

    pub fn push(&mut self, level: Level, source: &str, message: impl Into<String>) {
        let message = message.into();
        let t = now_ms();
        if let Some(last) = self.entries.back_mut()
            && last.level == level
            && last.source == source
            && last.message == message
        {
            last.count += 1;
            last.time_ms = t;
            return;
        }
        self.next += 1;
        self.entries.push_back(LogEntry { id: self.next, level, source: source.to_string(), message, count: 1, time_ms: t });
        while self.entries.len() > Self::LIMIT {
            self.entries.pop_front();
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn count(&self, level: Level) -> usize {
        self.entries.iter().filter(|e| e.level == level).count()
    }
}

/// Report background jobs that started or finished since the last call.
pub fn log_jobs(s: &mut Session) {
    let mut out = Vec::new();
    for j in &s.jobs {
        let finished = j.result.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let known = s.log.jobs.get(&j.id).copied();
        if known.is_none() {
            out.push((Level::Info, format!("Started: {}", j.label)));
        }
        if known != Some(true)
            && let Some(r) = finished
        {
            let cancelled = j.progress.cancel.load(std::sync::atomic::Ordering::Relaxed);
            out.push(match r {
                Ok(_) => (Level::Info, format!("Finished: {}", j.label)),
                Err(_) if cancelled => (Level::Warning, format!("Cancelled: {}", j.label)),
                Err(e) => (Level::Error, format!("Failed: {}: {e}", j.label)),
            });
            s.log.jobs.insert(j.id, true);
        } else {
            s.log.jobs.entry(j.id).or_insert(false);
        }
    }
    for (l, m) in out {
        s.log.push(l, "job", m);
    }
}

fn events_list(s: &mut Session, p: &Value) -> Result<Value> {
    log_jobs(s);
    let min = str_p(p, "level").and_then(Level::from_name).unwrap_or(Level::Info);
    let since = u64_p(p, "since").unwrap_or(0);
    let entries: Vec<&LogEntry> = s.log.entries.iter().filter(|e| e.level >= min && e.id > since).collect();
    Ok(json!({
        "entries": entries,
        "counts": {"info": s.log.count(Level::Info), "warning": s.log.count(Level::Warning), "error": s.log.count(Level::Error)},
    }))
}

// ------------------------------------------------------------------------------------ metadata

/// The log fields the Metadata panel edits (stored in `ProjectItem::metadata`), in panel order.
pub const LOG_FIELDS: [&str; 8] = ["Description", "Scene", "Shot", "Log Note", "Comment", "Tape Name", "Client", "Camera Angle"];

/// One Metadata panel row.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    /// "Clip" or "File".
    pub section: &'static str,
    pub name: String,
    pub value: String,
    pub editable: bool,
}

/// The item the Metadata panel describes: `item`, else the item of timeline clip `clip`, else the
/// Project panel selection, else the timeline selection.
pub fn metadata_target(s: &Session, p: &Value) -> Option<ItemId> {
    if let Some(i) = item_p(p, "item") {
        return Some(i);
    }
    let clip = u64_p(p, "clip").map(filmcraft_project::ClipId);
    let seq = s.active_sequence();
    let clip_item = |c| seq.and_then(|q| q.find_item(c)).map(|(_, it)| it.item);
    if let Some(c) = clip {
        return clip_item(c);
    }
    s.state.project_selection.first().copied().or_else(|| s.state.selection.first().and_then(|c| clip_item(*c)))
}

fn tc(t: Tick, rate: filmcraft_time::FrameRate) -> String {
    format_time(t, rate, false, TimeDisplay::Timecode, 48000)
}

/// Every Metadata panel row of an item (Clip section, then File section).
pub fn metadata_fields(p: &Project, id: ItemId) -> Vec<Field> {
    let Some(it) = p.item(id) else { return Vec::new() };
    let media = match &it.kind {
        ItemKind::Media(m) => Some(m),
        ItemKind::Subclip { parent, .. } => p.item(*parent).and_then(|x| x.as_media()),
        _ => None,
    };
    let rate = match &it.kind {
        ItemKind::Subclip { parent, .. } => p.item(*parent).map(|x| x.frame_rate()).unwrap_or_default(),
        _ => it.frame_rate(),
    };
    let mut out = Vec::new();
    let mut add = |section, name: &str, value: String, editable| out.push(Field { section, name: name.to_string(), value, editable });
    add("Clip", "Name", it.name.clone(), true);
    add("Clip", "Label", it.label.name().to_string(), true);
    add("Clip", "Media Type", it.type_label().to_string(), false);
    let col = |c: &str| filmcraft_project::find::column_text(p, it, c);
    add("Clip", "Frame Rate", col("Frame Rate"), false);
    // media start / end / duration in the item's own timecode
    let start = media.and_then(|m| m.info.start_timecode).map(|f| rate.tick_of(f)).unwrap_or(Tick::ZERO);
    let (offset, dur) = match &it.kind {
        ItemKind::Subclip { range, .. } => (range.start, range.duration),
        _ => (Tick::ZERO, it.duration()),
    };
    let first = start + offset;
    add("Clip", "Media Start", tc(first, rate), false);
    let last = if dur > Tick::ZERO { first + dur - rate.frame_duration() } else { first };
    add("Clip", "Media End", tc(last, rate), false);
    add("Clip", "Media Duration", tc(dur, rate), false);
    let marks = match &it.kind {
        ItemKind::Media(m) => (m.mark_in, m.mark_out),
        ItemKind::Sequence(q) => (q.mark_in, q.mark_out),
        _ => (None, None),
    };
    if let (Some(i), Some(o)) = marks {
        add("Clip", "In Point", tc(start + i, rate), false);
        add("Clip", "Out Point", tc(start + o - rate.frame_duration(), rate), false);
    }
    if let Some(v) = media.and_then(|m| m.info.video.as_ref()) {
        add("Clip", "Video Info", format!("{} x {} ({:.4})", v.width, v.height, v.par.0 as f64 / v.par.1.max(1) as f64), false);
        add("Clip", "Video Codec", v.codec.clone(), false);
        let detected = filmcraft_color::ColorSpace::from_info(&v.color);
        let cs = media.and_then(|m| m.interpret.color_space).unwrap_or(detected);
        add("Clip", "Color Space", cs.label().to_string(), false);
    }
    if let Some(a) = media.and_then(|m| m.info.audio()) {
        add("Clip", "Audio Info", format!("{} Hz - {} ch", a.sample_rate, a.channels), false);
        add("Clip", "Audio Codec", a.codec.clone(), false);
    }
    if let ItemKind::Sequence(q) = &it.kind {
        add("Clip", "Video Info", format!("{} x {}", q.settings.width, q.settings.height), false);
        add("Clip", "Color Space", q.settings.color.working.label().to_string(), false);
    }
    for k in LOG_FIELDS {
        add("Clip", k, it.metadata.get(k).cloned().unwrap_or_default(), true);
    }
    // other stored fields (from imports, Edit Offline, agents…)
    for (k, v) in &it.metadata {
        if !LOG_FIELDS.contains(&k.as_str()) && !crate::project_panel::is_freeform_key(k) {
            add("Clip", k, v.clone(), true);
        }
    }
    if let Some(m) = media {
        let path = match &m.media {
            MediaRef::File { path } => path.clone(),
            other => format!("{other:?}"),
        };
        add("File", "File Path", path, false);
        add("File", "Format", m.info.container.clone(), false);
        if let Some(sz) = m.info.file_size {
            add("File", "File Size", format_bytes(sz), false);
        }
        if let Some(v) = &m.info.video
            && let Some(b) = v.bitrate
        {
            add("File", "Video Bitrate", format!("{:.2} Mbit/s", b as f64 / 1e6), false);
        }
        add("File", "Status", if m.offline { "Offline".into() } else { "Online".into() }, false);
    }
    out
}

fn format_bytes(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}

fn metadata_get(s: &mut Session, p: &Value) -> Result<Value> {
    let id = metadata_target(s, p).ok_or_else(|| bad("metadata.get", "no item: pass `item` or `clip`, or select one"))?;
    let it = s.project.item(id).ok_or_else(|| bad("metadata.get", "no such item"))?;
    let fields = metadata_fields(&s.project, id);
    Ok(json!({"item": id.0, "name": it.name, "fields": fields, "metadata": it.metadata}))
}

/// `metadata.set {item?, field, value}` or `{item?, fields: {name: value}}`: one undo step.
/// Name renames the item, Label sets its label, every other name is a log field (an empty value
/// removes it).
fn metadata_set(s: &mut Session, p: &Value) -> Result<Value> {
    let id = metadata_target(s, p).ok_or_else(|| bad("metadata.set", "no item: pass `item` or `clip`, or select one"))?;
    let mut changes: Vec<(String, String)> = Vec::new();
    if let Some(f) = str_p(p, "field") {
        let v = p.get("value").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default();
        changes.push((f.to_string(), v));
    }
    if let Some(m) = p.get("fields").and_then(Value::as_object) {
        for (k, v) in m {
            changes.push((k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())));
        }
    }
    if changes.is_empty() {
        return Err(bad("metadata.set", "need `field` + `value` or `fields`"));
    }
    for (k, v) in &changes {
        if k.trim().is_empty() {
            return Err(bad("metadata.set", "empty field name"));
        }
        if k.eq_ignore_ascii_case("label") && Label::from_name(v).is_none() {
            return Err(bad("metadata.set", format!("unknown label `{v}`")));
        }
        if k.eq_ignore_ascii_case("name") && v.trim().is_empty() {
            return Err(bad("metadata.set", "the name cannot be empty"));
        }
        if metadata_fields(&s.project, id).iter().any(|f| f.name.eq_ignore_ascii_case(k) && !f.editable) {
            return Err(bad("metadata.set", format!("`{k}` is read-only")));
        }
    }
    let unchanged = {
        let it = s.project.item(id).ok_or_else(|| bad("metadata.set", "no such item"))?;
        changes.iter().all(|(k, v)| match k.to_ascii_lowercase().as_str() {
            "name" => it.name == *v,
            "label" => Label::from_name(v) == Some(it.label),
            _ => it.metadata.get(k).map(String::as_str).unwrap_or("") == v.as_str(),
        })
    };
    if !unchanged {
        s.edit("Edit Metadata", |pr, _| {
            let it = pr.item_mut(id).ok_or_else(|| bad("metadata.set", "no such item"))?;
            for (k, v) in &changes {
                match k.to_ascii_lowercase().as_str() {
                    "name" => it.name = v.trim().to_string(),
                    "label" => it.label = Label::from_name(v).unwrap_or(it.label),
                    _ if v.is_empty() => {
                        it.metadata.remove(k);
                    }
                    _ => {
                        it.metadata.insert(k.clone(), v.clone());
                    }
                }
            }
            Ok(())
        })?;
    }
    let it = s.project.item(id).ok_or_else(|| bad("metadata.set", "no such item"))?;
    Ok(json!({"item": id.0, "name": it.name, "label": it.label.name(), "metadata": it.metadata, "changed": !unchanged}))
}

// ------------------------------------------------------------------------------------ registry

pub(crate) fn commands() -> Vec<CommandSpec> {
    let q = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false };
    vec![
        q("events.list", "List Events", r#"{"level":"info|warning|error"?,"since":id?}"#, events_list),
        CommandSpec {
            id: "events.clear",
            label: "Clear All Events",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |s, _| {
                let n = s.log.entries.len();
                s.log.clear();
                Ok(json!({"cleared": n}))
            },
            journal: true,
        },
        q("metadata.get", "Get Metadata", r#"{"item":id?,"clip":id?}"#, metadata_get),
        CommandSpec {
            id: "metadata.set",
            label: "Edit Metadata",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id?,"clip":id?,"field":"Name|Label|Description|Scene|Shot|Log Note|Comment|Tape Name|Client|Camera Angle|…","value":str} | {"item":id?,"fields":{name:str}}"#,
            enabled: always,
            run: metadata_set,
            journal: true,
        },
    ]
}

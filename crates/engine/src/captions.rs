//! Caption commands: caption tracks, caption editing, caption file import/export.
//!
//! Ids (`captions.*`) follow Premiere's Sequence ▸ Captions menu and the Text panel's Captions tab.
//! Captions are addressed by id (`caption`, `captions`); without one, commands use the caption
//! selection (`EditorState::caption_selection`) or the caption under the playhead. Caption tracks
//! are addressed by id or `"C1"`, `"C2"`… (top first); the default is the first caption track.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_captions::{Document, Format, WriteOptions};
use filmcraft_edit as edit;
use filmcraft_edit::Edge;
use filmcraft_project::{CaptionAlign, CaptionAnchor, CaptionFormat, CaptionStyle, CaptionTrack, ClipId, SequenceSettings, TrackId};
use filmcraft_time::{Tick, TimeDisplay, format_time};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

const STYLE_PRESET_FILE: &str = "caption-style-presets.json";
const STYLE_PRESET_FORMAT: &str = "filmcraft.caption-style-presets";
const MAX_STYLE_PRESETS: usize = 100;
const MAX_STYLE_PRESET_FILE_BYTES: u64 = 1024 * 1024;
const MAX_STYLE_PRESET_NAME_CHARS: usize = 100;
const MAX_STYLE_FONT_BYTES: usize = 1024;

/// A named, reusable caption track appearance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionStylePreset {
    pub name: String,
    pub style: CaptionStyle,
}

#[derive(Serialize, Deserialize)]
struct CaptionStylePresetFile {
    format: String,
    version: u32,
    presets: Vec<CaptionStylePreset>,
}

/// User-created caption track styles persisted in the application data directory.
#[derive(Default)]
pub struct CaptionStyleLibrary {
    user: Vec<CaptionStylePreset>,
    dir: Option<PathBuf>,
    load_error: Option<String>,
}

impl CaptionStyleLibrary {
    /// Load saved styles from the app data directory. Invalid files are left untouched and reported.
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.to_path_buf());
        self.user.clear();
        self.load_error = None;
        let path = data_dir.join(STYLE_PRESET_FILE);
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                log::warn!("{}: {e}", path.display());
                self.load_error = Some(e.to_string());
                return;
            }
        };
        let mut bytes = Vec::new();
        let read = file.take(MAX_STYLE_PRESET_FILE_BYTES.saturating_add(1)).read_to_end(&mut bytes);
        if let Err(e) = read {
            log::warn!("{}: {e}", path.display());
            self.load_error = Some(e.to_string());
            return;
        }
        match if bytes.len() as u64 > MAX_STYLE_PRESET_FILE_BYTES {
            Err("caption style preset file exceeds the 1 MiB limit".into())
        } else {
            parse_style_presets(&bytes)
        } {
            Ok(presets) => self.user = presets,
            Err(e) => {
                log::warn!("{}: {e}", path.display());
                self.load_error = Some(e);
            }
        }
    }

    /// Return all user-saved caption track style presets.
    pub fn all(&self) -> &[CaptionStylePreset] {
        &self.user
    }

    /// Find a saved preset by its exact display name.
    pub fn find(&self, name: &str) -> Option<CaptionStyle> {
        self.user.iter().find(|p| p.name == name).map(|p| p.style.clone())
    }

    /// Save or replace a preset, persisting it before changing the in-memory list.
    pub fn save(&mut self, name: &str, style: CaptionStyle) -> std::result::Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_STYLE_PRESET_NAME_CHARS || name.chars().any(char::is_control) {
            return Err(format!("preset name must be 1–{MAX_STYLE_PRESET_NAME_CHARS} printable characters"));
        }
        let style = normalize_style(style).ok_or_else(|| "caption style has an invalid font or non-finite value".to_string())?;
        let mut next = self.user.clone();
        let key = style_preset_name_key(name);
        if let Some(preset) = next.iter_mut().find(|p| style_preset_name_key(&p.name) == key) {
            preset.name = name.to_string();
            preset.style = style;
        } else {
            if next.len() >= MAX_STYLE_PRESETS {
                return Err(format!("caption style preset limit ({MAX_STYLE_PRESETS}) reached"));
            }
            next.push(CaptionStylePreset { name: name.to_string(), style });
        }
        self.persist(&next)?;
        self.user = next;
        Ok(())
    }

    /// Delete a preset by its exact display name, persisting the change before removing it in memory.
    pub fn delete(&mut self, name: &str) -> std::result::Result<bool, String> {
        let Some(index) = self.user.iter().position(|p| p.name == name) else { return Ok(false) };
        let mut next = self.user.clone();
        next.remove(index);
        self.persist(&next)?;
        self.user = next;
        Ok(true)
    }

    fn persist(&self, presets: &[CaptionStylePreset]) -> std::result::Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(format!("cannot save caption style presets because the existing file failed to load: {error}"));
        }
        let Some(dir) = &self.dir else { return Err("caption style preset persistence is not configured".into()) };
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create caption style preset directory: {e}"))?;
        let bytes = style_preset_file_bytes(presets)?;
        filmcraft_format::atomic_write(&dir.join(STYLE_PRESET_FILE), &bytes).map_err(|e| e.to_string())
    }
}

/// Normalize preset names with the same Unicode case-folding rule for saves and file validation.
fn style_preset_name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn normalize_style(mut style: CaptionStyle) -> Option<CaptionStyle> {
    if style.font.len() > MAX_STYLE_FONT_BYTES
        || !style.size.is_finite()
        || !style.margin.is_finite()
        || !style.line_spacing.is_finite()
        || !style.outline.is_finite()
    {
        return None;
    }
    style.size = style.size.clamp(4.0, 400.0);
    style.margin = style.margin.clamp(0.0, 0.45);
    style.line_spacing = style.line_spacing.clamp(0.8, 4.0);
    style.outline = style.outline.clamp(0.0, 40.0);
    Some(style)
}

fn style_preset_file_bytes(presets: &[CaptionStylePreset]) -> std::result::Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(&CaptionStylePresetFile { format: STYLE_PRESET_FORMAT.into(), version: 1, presets: presets.to_vec() })
        .map_err(|e| format!("could not serialize caption style presets: {e}"))
}

fn parse_style_presets(bytes: &[u8]) -> std::result::Result<Vec<CaptionStylePreset>, String> {
    if bytes.len() as u64 > MAX_STYLE_PRESET_FILE_BYTES {
        return Err("caption style preset file exceeds the 1 MiB limit".into());
    }
    let file: CaptionStylePresetFile = serde_json::from_slice(bytes).map_err(|e| format!("invalid caption style preset file: {e}"))?;
    if file.format != STYLE_PRESET_FORMAT {
        return Err(format!("not a caption style preset file (format `{}`)", file.format));
    }
    if file.version != 1 {
        return Err(format!("unsupported caption style preset version {}", file.version));
    }
    if file.presets.len() > MAX_STYLE_PRESETS {
        return Err(format!("caption style preset file exceeds the {MAX_STYLE_PRESETS} preset limit"));
    }
    let mut names = std::collections::HashSet::new();
    let mut presets = Vec::with_capacity(file.presets.len());
    for mut preset in file.presets {
        let name = preset.name.trim().to_string();
        if name.is_empty() || name.chars().count() > MAX_STYLE_PRESET_NAME_CHARS || name.chars().any(char::is_control) {
            return Err("caption style preset file contains an invalid name".into());
        }
        if !names.insert(style_preset_name_key(&name)) {
            return Err("caption style preset file contains duplicate names".into());
        }
        preset.name = name.clone();
        preset.style = normalize_style(preset.style).ok_or_else(|| format!("caption style preset `{name}` has an invalid style"))?;
        presets.push(preset);
    }
    Ok(presets)
}

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal: true }
}

fn has_caption_track(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.active_sequence().is_some_and(|q| !q.caption_tracks.is_empty()) { Ok(()) } else { Err("the sequence has no caption track".into()) }
}

fn has_captions(s: &Session) -> std::result::Result<(), String> {
    has_caption_track(s)?;
    if s.active_sequence().is_some_and(|q| q.caption_tracks.iter().any(|t| !t.captions.is_empty())) { Ok(()) } else { Err("there are no captions".into()) }
}

/// The caption track named by `track` (id or "C1"…), else the first one.
pub fn track_param(s: &Session, p: &Value) -> Option<TrackId> {
    let seq = s.active_sequence()?;
    match p.get("track") {
        Some(Value::Number(n)) => n.as_u64().map(TrackId).filter(|t| seq.caption_track(*t).is_some()),
        Some(Value::String(name)) => {
            let idx: usize = name.trim_start_matches(['C', 'c']).parse().ok()?;
            seq.caption_tracks.get(idx.checked_sub(1)?).map(|t| t.id)
        }
        _ => seq.caption_tracks.first().map(|t| t.id),
    }
}

fn caption_ids(s: &Session, p: &Value) -> Vec<ClipId> {
    if let Some(a) = p.get("captions").and_then(Value::as_array) {
        return a.iter().filter_map(Value::as_u64).map(ClipId).collect();
    }
    if let Some(c) = u64_p(p, "caption") {
        return vec![ClipId(c)];
    }
    if !s.state.caption_selection.is_empty() {
        return s.state.caption_selection.clone();
    }
    // the caption under the playhead on the first caption track that has one
    let t = s.playhead();
    s.active_sequence().and_then(|q| q.caption_tracks.iter().find_map(|tr| tr.caption_at(t).map(|c| c.id))).into_iter().collect()
}

fn one_caption(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    caption_ids(s, p).first().copied().ok_or_else(|| bad(cmd, "no caption (pass `caption` or select one)"))
}

fn parse_color(v: &Value) -> Option<[u8; 4]> {
    if let Some(a) = v.as_array() {
        let c: Vec<u8> = a.iter().filter_map(|x| x.as_u64().map(|n| n.min(255) as u8)).collect();
        return match c.len() {
            3 => Some([c[0], c[1], c[2], 255]),
            4 => Some([c[0], c[1], c[2], c[3]]),
            _ => None,
        };
    }
    let h = v.as_str()?;
    let h = h.strip_prefix('#').unwrap_or(h);
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some([b(0)?, b(2)?, b(4)?, 255]),
        8 => Some([b(0)?, b(2)?, b(4)?, b(6)?]),
        _ => None,
    }
}

fn hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
}

/// The caption format of a file, if it is one (extension `.srt` / `.vtt` / `.scc` / `.mcc` /
/// `.stl` / `.ttml` / `.dfxp` / `.xml`, confirmed by content).
pub fn detect(path: &str, bytes: &[u8]) -> Option<Format> {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("srt" | "vtt" | "scc" | "webvtt" | "mcc" | "stl" | "ttml" | "dfxp" | "xml")) {
        return None;
    }
    filmcraft_captions::detect(bytes, ext.as_deref())
}

/// Import a caption file as a new caption track in the active sequence (a new sequence is made
/// when none is open), snapped to the sequence's frames. One undo step.
pub fn import(s: &mut Session, path: &str, bytes: &[u8], format: Format, name: Option<&str>) -> Result<Value> {
    let mut doc = filmcraft_captions::parse(bytes, format).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let stem = std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Captions".into());
    let track_name = name.map(str::to_string).unwrap_or_else(|| stem.clone());
    let seq_id = match s.state.active_sequence.filter(|id| s.project.sequence(*id).is_some()) {
        Some(id) => id,
        None => {
            let id = s.edit("New Sequence", |p, _| Ok(p.new_sequence(&stem, SequenceSettings::default(), 3, 3, None)))?;
            s.state.active_sequence = Some(id);
            s.state.open_sequences.push(id);
            id
        }
    };
    let rate = s.project.sequence(seq_id).map(|q| q.settings.frame_rate).unwrap_or_default();
    doc.snap_to_frames(rate);
    let n = doc.cues.len();
    let warnings = doc.warnings.clone();
    let tid = s.edit(&format!("Import Captions {stem}"), |p, st| {
        let tid = TrackId(p.alloc_id());
        let mut next = p.next_id;
        let mut alloc = || {
            let v = next;
            next += 1;
            v
        };
        let mut track = filmcraft_captions::track_from_document(&doc, tid, &track_name, format.track_format(), &mut alloc);
        if format == Format::Scc {
            track.style.background_color = [0, 0, 0, 255];
        }
        p.next_id = next;
        let q = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        q.caption_tracks.insert(0, track);
        q.check().map_err(EngineError::Other)?;
        st.caption_selection.clear();
        Ok(tid)
    })?;
    Ok(json!({"format": format.name(), "sequence": seq_id.0, "track": tid.0, "captions": n, "warnings": warnings}))
}

/// Delete captions (Clear / Ripple Delete with captions selected).
pub fn delete(s: &mut Session, ids: &[ClipId], ripple: bool) -> Result<Value> {
    if ids.is_empty() {
        return Err(bad("captions.delete", "no captions"));
    }
    let ids = ids.to_vec();
    let n = s.edit_sequence(if ripple { "Ripple Delete Captions" } else { "Delete Captions" }, |q, _, st| {
        let n = edit::captions::delete_captions(q, &ids, ripple)?;
        st.caption_selection.retain(|c| !ids.contains(c));
        Ok(n)
    })?;
    Ok(json!({"deleted": n}))
}

fn caption_json(s: &Session, c: &filmcraft_project::Caption) -> Value {
    let q = s.active_sequence();
    let (rate, df) = q.map(|q| (q.settings.frame_rate, q.settings.drop_frame)).unwrap_or_default();
    let tc = |t: Tick| format_time(t, rate, df, TimeDisplay::Timecode, 48_000);
    json!({
        "id": c.id.0,
        "start": c.start.0,
        "end": c.end().0,
        "in": tc(c.start),
        "out": tc(c.end()),
        "duration": tc(c.duration),
        "text": c.text,
        "speaker": c.speaker,
        "settings": c.settings,
    })
}

fn track_json(s: &Session, t: &CaptionTrack, index: usize) -> Value {
    json!({
        "id": t.id.0,
        "label": format!("C{}", index + 1),
        "name": t.name,
        "format": t.format.label(),
        "language": t.language,
        "enabled": t.enabled,
        "locked": t.locked,
        "syncLock": t.sync_lock,
        "style": {
            "font": t.style.font, "size": t.style.size, "color": hex(t.style.color),
            "background": t.style.background, "backgroundColor": hex(t.style.background_color),
            "align": format!("{:?}", t.style.align).to_lowercase(), "anchor": format!("{:?}", t.style.anchor).to_lowercase(),
            "margin": t.style.margin, "lineSpacing": t.style.line_spacing, "outline": t.style.outline, "outlineColor": hex(t.style.outline_color),
        },
        "captions": t.captions.iter().map(|c| caption_json(s, c)).collect::<Vec<_>>(),
    })
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("captions.export", "need `path`"))?.to_string();
    let ext = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("");
    let format = str_p(p, "format").and_then(Format::from_name).or_else(|| Format::from_name(ext)).unwrap_or(Format::Srt);
    let tid = track_param(s, p).ok_or_else(|| bad("captions.export", "no caption track"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let track = seq.caption_track(tid).ok_or_else(|| bad("captions.export", "no such caption track"))?;
    let doc: Document = filmcraft_captions::document_from_track(track, Tick::ZERO);
    let df = bool_p(p, "dropFrame").unwrap_or(true);
    let bytes = filmcraft_captions::write(&doc, format, WriteOptions { drop_frame: df, rate: Some(seq.settings.frame_rate) });
    let n = doc.cues.len();
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({"path": path, "format": format.name(), "captions": n, "bytes": bytes.len()}))
}

fn delta_p(s: &Session, p: &Value) -> Option<Tick> {
    if let Some(d) = p.get("delta").and_then(Value::as_i64) {
        return Some(Tick(d));
    }
    p.get("deltaFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "captions.newTrack",
            "Add New Caption Track…",
            &["Sequence", "Captions"],
            Some("Cmd+Alt+A"),
            r#"{"format":"Subtitle|CEA-608|CEA-708|Teletext","name":str?,"language":str?}"#,
            has_seq,
            |s, p| {
                let format = str_p(p, "format").and_then(CaptionFormat::from_name).unwrap_or_default();
                let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format.label().to_string());
                let lang = str_p(p, "language").unwrap_or("en").to_string();
                let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
                let id = s.edit("Add Caption Track", |pr, _| {
                    let id = TrackId(pr.alloc_id());
                    let mut t = CaptionTrack::new(id, name, format);
                    t.language = lang;
                    if format == CaptionFormat::Cea608 {
                        t.style.background_color = [0, 0, 0, 255];
                    }
                    pr.sequence_mut(seq).ok_or(EngineError::NoSequence)?.caption_tracks.insert(0, t);
                    Ok(id)
                })?;
                Ok(json!({"track": id.0}))
            },
        ),
        spec("captions.deleteTrack", "Delete Caption Track", &[], None, r#"{"track":id|"C1"}"#, has_caption_track, |s, p| {
            let tid = track_param(s, p).ok_or_else(|| bad("captions.deleteTrack", "no such caption track"))?;
            s.edit_sequence("Delete Caption Track", |q, _, st| {
                q.caption_tracks.retain(|t| t.id != tid);
                st.caption_selection.retain(|c| q.find_caption(*c).is_some());
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        spec(
            "captions.setTrack",
            "Caption Track Settings",
            &[],
            None,
            r#"{"track":id|"C1","name":str?,"format":str?,"language":str?,"enabled":bool?,"locked":bool?,"syncLock":bool?}"#,
            has_caption_track,
            |s, p| {
                let tid = track_param(s, p).ok_or_else(|| bad("captions.setTrack", "no such caption track"))?;
                let p = p.clone();
                s.edit("Caption Track Settings", |pr, st| {
                    let seq = st.active_sequence.ok_or(EngineError::NoSequence)?;
                    let t = pr.sequence_mut(seq).and_then(|q| q.caption_track_mut(tid)).ok_or(EngineError::NoSequence)?;
                    if let Some(n) = str_p(&p, "name") {
                        t.name = n.to_string();
                    }
                    if let Some(f) = str_p(&p, "format") {
                        t.format = CaptionFormat::from_name(f).ok_or_else(|| bad("captions.setTrack", "unknown format"))?;
                    }
                    if let Some(l) = str_p(&p, "language") {
                        t.language = l.to_string();
                    }
                    if let Some(b) = bool_p(&p, "enabled") {
                        t.enabled = b;
                    }
                    if let Some(b) = bool_p(&p, "locked") {
                        t.locked = b;
                    }
                    if let Some(b) = bool_p(&p, "syncLock") {
                        t.sync_lock = b;
                    }
                    Ok(())
                })?;
                Ok(Value::Null)
            },
        ),
        spec(
            "captions.setStyle",
            "Caption Track Style",
            &[],
            None,
            r##"{"track":id|"C1","font":str?,"size":f32?,"color":"#rrggbb[aa]"?,"background":bool?,"backgroundColor":"#rrggbbaa"?,"align":"left|center|right"?,"anchor":"top|middle|bottom"?,"margin":0..0.45 (fraction of frame height)?,"lineSpacing":f32?,"outline":f32?,"outlineColor":str?,"reset":bool?}"##,
            has_caption_track,
            |s, p| {
                let tid = track_param(s, p).ok_or_else(|| {
                    bad(
                        "captions.setStyle",
                        format!(
                            "no such caption track ({}) — pass the track id from captions.list, or \"C1\"…",
                            p.get("track").map(|v| v.to_string()).unwrap_or_else(|| "no `track` given".into())
                        ),
                    )
                })?;
                let p = p.clone();
                s.edit("Caption Style", |pr, st| {
                    let seq = st.active_sequence.ok_or(EngineError::NoSequence)?;
                    let t = pr.sequence_mut(seq).and_then(|q| q.caption_track_mut(tid)).ok_or(EngineError::NoSequence)?;
                    let y = &mut t.style;
                    if bool_p(&p, "reset").unwrap_or(false) {
                        *y = CaptionStyle::default();
                    }
                    if let Some(v) = str_p(&p, "font") {
                        y.font = v.to_string();
                    }
                    if let Some(v) = f64_p(&p, "size") {
                        y.size = (v as f32).clamp(4.0, 400.0);
                    }
                    if let Some(c) = p.get("color").and_then(parse_color) {
                        y.color = c;
                    }
                    if let Some(b) = bool_p(&p, "background") {
                        y.background = b;
                    }
                    if let Some(c) = p.get("backgroundColor").and_then(parse_color) {
                        y.background_color = c;
                    }
                    if let Some(c) = p.get("outlineColor").and_then(parse_color) {
                        y.outline_color = c;
                    }
                    if let Some(v) = f64_p(&p, "outline") {
                        y.outline = (v as f32).clamp(0.0, 40.0);
                    }
                    if let Some(v) = f64_p(&p, "margin") {
                        y.margin = (v as f32).clamp(0.0, 0.45);
                    }
                    if let Some(v) = f64_p(&p, "lineSpacing") {
                        y.line_spacing = (v as f32).clamp(0.8, 4.0);
                    }
                    match str_p(&p, "align") {
                        Some("left") => y.align = CaptionAlign::Left,
                        Some("right") => y.align = CaptionAlign::Right,
                        Some("center") => y.align = CaptionAlign::Center,
                        Some(o) => return Err(bad("captions.setStyle", format!("unknown align `{o}`"))),
                        None => {}
                    }
                    match str_p(&p, "anchor") {
                        Some("top") => y.anchor = CaptionAnchor::Top,
                        Some("middle") => y.anchor = CaptionAnchor::Middle,
                        Some("bottom") => y.anchor = CaptionAnchor::Bottom,
                        Some(o) => return Err(bad("captions.setStyle", format!("unknown anchor `{o}`"))),
                        None => {}
                    }
                    Ok(())
                })?;
                Ok(Value::Null)
            },
        ),
        spec("captions.copyStyle", "Copy Caption Track Style", &[], None, r#"{"track":id|"C1"?}"#, has_caption_track, |s, p| {
            let tid = track_param(s, p).ok_or_else(|| bad("captions.copyStyle", "no such caption track"))?;
            let style = s.active_sequence().and_then(|q| q.caption_track(tid)).map(|t| t.style.clone()).ok_or(EngineError::NoSequence)?;
            s.caption_style_clipboard = Some(style);
            Ok(json!({"track": tid.0, "copied": true}))
        }),
        spec("captions.pasteStyle", "Paste Caption Track Style", &[], None, r#"{"track":id|"C1"?}"#, has_caption_track, |s, p| {
            let tid = track_param(s, p).ok_or_else(|| bad("captions.pasteStyle", "no such caption track"))?;
            let style = s.caption_style_clipboard.clone().ok_or_else(|| bad("captions.pasteStyle", "copy a caption track style first"))?;
            s.edit_sequence("Paste Caption Track Style", move |q, _, _| {
                let track = q.caption_track_mut(tid).ok_or_else(|| bad("captions.pasteStyle", "no such caption track"))?;
                track.style = style;
                Ok(())
            })?;
            Ok(json!({"track": tid.0, "pasted": true}))
        }),
        spec("captions.stylePreset.save", "Save Caption Style Preset", &[], None, r#"{"name":str,"track":id|"C1"?}"#, has_caption_track, |s, p| {
            let name = str_p(p, "name").ok_or_else(|| bad("captions.stylePreset.save", "need `name`"))?;
            let tid = track_param(s, p).ok_or_else(|| bad("captions.stylePreset.save", "no such caption track"))?;
            let style = s.active_sequence().and_then(|q| q.caption_track(tid)).map(|t| t.style.clone()).ok_or(EngineError::NoSequence)?;
            s.caption_style_presets.save(name, style).map_err(|e| bad("captions.stylePreset.save", e))?;
            Ok(json!({"name": name.trim()}))
        }),
        spec("captions.stylePreset.apply", "Apply Caption Style Preset", &[], None, r#"{"name":str,"track":id|"C1"?}"#, has_caption_track, |s, p| {
            let name = str_p(p, "name").ok_or_else(|| bad("captions.stylePreset.apply", "need `name`"))?;
            let tid = track_param(s, p).ok_or_else(|| bad("captions.stylePreset.apply", "no such caption track"))?;
            let style =
                s.caption_style_presets.find(name).ok_or_else(|| bad("captions.stylePreset.apply", format!("no caption style preset named `{name}`")))?;
            s.edit_sequence("Apply Caption Style Preset", move |q, _, _| {
                let track = q.caption_track_mut(tid).ok_or_else(|| bad("captions.stylePreset.apply", "no such caption track"))?;
                track.style = style;
                Ok(())
            })?;
            Ok(json!({"name": name, "track": tid.0}))
        }),
        spec("captions.stylePreset.delete", "Delete Caption Style Preset", &[], None, r#"{"name":str}"#, always, |s, p| {
            let name = str_p(p, "name").ok_or_else(|| bad("captions.stylePreset.delete", "need `name`"))?;
            let deleted = s.caption_style_presets.delete(name).map_err(|e| bad("captions.stylePreset.delete", e))?;
            if !deleted {
                return Err(bad("captions.stylePreset.delete", format!("no caption style preset named `{name}`")));
            }
            Ok(json!({"name": name, "deleted": true}))
        }),
        CommandSpec {
            id: "captions.stylePreset.list",
            label: "List Caption Style Presets",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |s, _| Ok(json!({"presets": s.caption_style_presets.all()})),
            journal: false,
        },
        spec(
            "captions.add",
            "Add Caption at Playhead",
            &["Sequence", "Captions"],
            Some("Cmd+Alt+C"),
            r#"{"track":id|"C1"?,"text":str?,"time":ticks?,"seconds":f64?,"durationSeconds":f64=3}"#,
            has_seq,
            |s, p| {
                // a caption track is created when the sequence has none
                if s.active_sequence().is_some_and(|q| q.caption_tracks.is_empty()) {
                    s.execute("captions.newTrack", json!({}))?;
                }
                let tid = track_param(s, p).ok_or_else(|| bad("captions.add", "no such caption track"))?;
                let t = time_p(s, p, "").unwrap_or(s.playhead());
                let rate = s.sequence_rate();
                let dur = Tick::from_seconds_f64(f64_p(p, "durationSeconds").unwrap_or(edit::captions::DEFAULT_SECONDS as f64));
                let dur = rate.snap_nearest(dur).max(rate.frame_duration());
                let text = str_p(p, "text").unwrap_or("New caption").to_string();
                let t = rate.snap(t);
                let id = s.edit_sequence("Add Caption", |q, ctx, st| {
                    let id = edit::captions::add_caption(q, tid, t, dur, &text, ctx)?;
                    st.caption_selection = vec![id];
                    st.selection.clear();
                    Ok(id)
                })?;
                Ok(json!({"caption": id.0}))
            },
        ),
        spec("captions.split", "Split Caption", &[], None, r#"{"caption":id?,"time":ticks?}"#, has_captions, |s, p| {
            let t = time_p(s, p, "").unwrap_or(s.playhead());
            let explicit = u64_p(p, "caption").map(ClipId);
            let ids = s.edit_sequence("Split Caption", |q, ctx, st| {
                let ids = match explicit {
                    Some(c) => vec![edit::captions::split_caption(q, c, t, ctx)?],
                    None => edit::captions::split_captions_at(q, &[], t, ctx),
                };
                if ids.is_empty() {
                    return Err(edit::EditError::Nothing.into());
                }
                st.caption_selection = ids.clone();
                Ok(ids)
            })?;
            Ok(json!({"captions": ids.iter().map(|c| c.0).collect::<Vec<_>>()}))
        }),
        spec("captions.merge", "Merge Captions", &[], None, r#"{"captions":[id]?}"#, has_captions, |s, p| {
            let ids = caption_ids(s, p);
            let id = s.edit_sequence("Merge Captions", |q, _, st| {
                let id = edit::captions::merge_captions(q, &ids)?;
                st.caption_selection = vec![id];
                Ok(id)
            })?;
            Ok(json!({"caption": id.0}))
        }),
        spec("captions.setText", "Edit Caption Text", &[], None, r#"{"caption":id?,"text":str?,"speaker":str|null?}"#, has_captions, |s, p| {
            let id = one_caption(s, p, "captions.setText")?;
            let text = str_p(p, "text").map(str::to_string);
            let speaker: Option<Option<String>> = match p.get("speaker") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(v) => Some(v.as_str().map(str::to_string)),
            };
            if text.is_none() && speaker.is_none() {
                return Err(bad("captions.setText", "need `text` or `speaker`"));
            }
            s.edit_sequence("Edit Caption", |q, _, _| Ok(edit::captions::set_caption(q, id, text.as_deref(), speaker.as_ref().map(|o| o.as_deref()))?))?;
            Ok(Value::Null)
        }),
        spec(
            "captions.setTimes",
            "Set Caption In/Out",
            &[],
            None,
            r#"{"caption":id?,"startTime|startFrame|startSeconds|startTimecode":…,"endTime|endFrame|endSeconds|endTimecode":…}"#,
            has_captions,
            |s, p| {
                let id = one_caption(s, p, "captions.setTimes")?;
                let cur = s
                    .active_sequence()
                    .and_then(|q| q.find_caption(id))
                    .map(|(_, c)| (c.start, c.end()))
                    .ok_or(EngineError::Edit(edit::EditError::NoItem(id)))?;
                let rate = s.sequence_rate();
                let start = time_p(s, p, "start").map(|t| rate.snap_nearest(t)).unwrap_or(cur.0);
                let end = time_p(s, p, "end").map(|t| rate.snap_nearest(t)).unwrap_or(cur.1);
                s.edit_sequence("Caption Timing", |q, ctx, _| Ok(edit::captions::set_caption_times(q, id, start, end, ctx)?))?;
                Ok(json!({"start": start.0, "end": end.0}))
            },
        ),
        spec("captions.trim", "Trim Caption", &[], None, r#"{"caption":id,"edge":"in|out","delta":ticks|"deltaFrames":i64}"#, has_captions, |s, p| {
            let id = one_caption(s, p, "captions.trim")?;
            let edge = if str_p(p, "edge") == Some("in") { Edge::In } else { Edge::Out };
            let d = delta_p(s, p).ok_or_else(|| bad("captions.trim", "need `delta` or `deltaFrames`"))?;
            let applied = s.edit_sequence("Trim Caption", |q, ctx, _| Ok(edit::captions::trim_caption(q, id, edge, d, ctx)?))?;
            Ok(json!({"applied": applied.0}))
        }),
        spec("captions.move", "Move Captions", &[], None, r#"{"captions":[id]?,"delta":ticks|"deltaFrames":i64}"#, has_captions, |s, p| {
            let ids = caption_ids(s, p);
            let d = delta_p(s, p).ok_or_else(|| bad("captions.move", "need `delta` or `deltaFrames`"))?;
            let applied = s.edit_sequence("Move Captions", |q, _, _| Ok(edit::captions::move_captions(q, &ids, d)?))?;
            Ok(json!({"applied": applied.0}))
        }),
        spec("captions.delete", "Delete Captions", &[], None, r#"{"captions":[id]?,"ripple":bool=false}"#, has_captions, |s, p| {
            let ids = caption_ids(s, p);
            delete(s, &ids, bool_p(p, "ripple").unwrap_or(false))
        }),
        spec("captions.select", "Select Captions", &[], None, r#"{"captions":[id],"add":bool?}"#, has_seq, |s, p| {
            let ids: Vec<ClipId> =
                p.get("captions").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ClipId).collect()).unwrap_or_default();
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let ids: Vec<ClipId> = ids.into_iter().filter(|c| seq.find_caption(*c).is_some()).collect();
            if bool_p(p, "add").unwrap_or(false) {
                for c in ids {
                    if !s.state.caption_selection.contains(&c) {
                        s.state.caption_selection.push(c);
                    }
                }
            } else {
                s.state.caption_selection = ids;
                s.state.selection.clear();
            }
            Ok(json!({"captions": s.state.caption_selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
        }),
        spec("captions.goTo", "Go to Caption", &[], None, r#"{"caption":id}"#, has_captions, |s, p| {
            let id = one_caption(s, p, "captions.goTo")?;
            let start = s.active_sequence().and_then(|q| q.find_caption(id)).map(|(_, c)| c.start).ok_or(EngineError::Edit(edit::EditError::NoItem(id)))?;
            s.set_playhead(start);
            s.state.caption_selection = vec![id];
            s.state.selection.clear();
            Ok(json!({"time": start.0}))
        }),
        spec("captions.next", "Go to Next Caption Segment", &["Sequence", "Captions"], Some("Cmd+Alt+Down"), "{}", has_captions, |s, _| step(s, true)),
        spec("captions.previous", "Go to Previous Caption Segment", &["Sequence", "Captions"], Some("Cmd+Alt+Up"), "{}", has_captions, |s, _| step(s, false)),
        spec("captions.showAll", "Show All Caption Tracks", &["Sequence", "Captions"], None, "{}", has_caption_track, |s, _| show_all(s, true)),
        spec("captions.hideAll", "Hide All Caption Tracks", &["Sequence", "Captions"], None, "{}", has_caption_track, |s, _| show_all(s, false)),
        spec("captions.import", "Import Captions…", &[], None, r#"{"path":str,"name":str?}"#, always, |s, p| {
            let path = str_p(p, "path").ok_or_else(|| bad("captions.import", "need `path`"))?.to_string();
            let bytes = s.services.read_file(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            let ext = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            let format = filmcraft_captions::detect(&bytes, ext.as_deref()).ok_or_else(|| EngineError::Other(format!("{path}: not a caption file")))?;
            import(s, &path, &bytes, format, str_p(p, "name"))
        }),
        spec(
            "captions.export",
            "Captions…",
            &["File", "Export"],
            None,
            r#"{"path":str,"format":"srt|vtt|scc|mcc|stl|ttml|dfxp"? (default: from the extension),"track":id|"C1"?,"dropFrame":bool=true}"#,
            has_caption_track,
            export,
        ),
        CommandSpec {
            id: "captions.list",
            label: "List Captions",
            menu: &[],
            shortcut: None,
            params: r#"{"track":id|"C1"?}"#,
            enabled: always,
            run: |s, p| {
                let Some(seq) = s.active_sequence() else { return Ok(json!({"tracks": []})) };
                // a `track` that names no caption track is an error, never every track
                let only = match p.get("track") {
                    Some(_) => Some(track_param(s, p).ok_or_else(|| bad("captions.list", "no such caption track"))?),
                    None => None,
                };
                let tracks: Vec<Value> =
                    seq.caption_tracks.iter().enumerate().filter(|(_, t)| only.is_none_or(|o| o == t.id)).map(|(i, t)| track_json(s, t, i)).collect();
                Ok(json!({"tracks": tracks, "selection": s.state.caption_selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
            },
            journal: false,
        },
    ]
}

fn step(s: &mut Session, forward: bool) -> Result<Value> {
    let t = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut starts: Vec<(Tick, ClipId)> = seq.caption_tracks.iter().filter(|t| t.enabled).flat_map(|t| t.captions.iter().map(|c| (c.start, c.id))).collect();
    starts.sort();
    let hit = if forward { starts.iter().find(|(st, _)| *st > t).copied() } else { starts.iter().rev().find(|(st, _)| *st < t).copied() };
    let (time, id) = hit.ok_or_else(|| EngineError::Other("no more captions".into()))?;
    s.set_playhead(time);
    s.state.caption_selection = vec![id];
    Ok(json!({"time": time.0, "caption": id.0}))
}

fn show_all(s: &mut Session, on: bool) -> Result<Value> {
    s.edit(if on { "Show All Caption Tracks" } else { "Hide All Caption Tracks" }, |p, st| {
        let seq = st.active_sequence.ok_or(EngineError::NoSequence)?;
        for t in &mut p.sequence_mut(seq).ok_or(EngineError::NoSequence)?.caption_tracks {
            t.enabled = on;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

#[cfg(test)]
#[path = "captions_tests.rs"]
mod tests;

#[cfg(test)]
mod preset_file_tests {
    use super::*;

    #[test]
    fn caption_style_preset_parser_survives_truncation_and_bit_flips() {
        let valid = style_preset_file_bytes(&[CaptionStylePreset { name: "Safe".into(), style: CaptionStyle::default() }]).expect("valid preset serializes");
        let mut candidates: Vec<Vec<u8>> = (0..valid.len()).map(|length| valid[..length].to_vec()).collect();
        candidates.push(valid.clone());
        for index in 0..valid.len() {
            for bit in 0..8 {
                let mut changed = valid.clone();
                changed[index] ^= 1 << bit;
                candidates.push(changed);
            }
        }
        candidates.push(vec![b' '; MAX_STYLE_PRESET_FILE_BYTES as usize + 1]);

        for candidate in candidates {
            let parsed = std::panic::catch_unwind(|| parse_style_presets(&candidate));
            assert!(parsed.is_ok(), "malformed preset data must return an error instead of panicking");
        }
    }
}

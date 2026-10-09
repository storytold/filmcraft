//! Media Browser (M12.7): directory listing, navigation history, Favorites, recent directories,
//! file-type filter, import and Open in Source Monitor. All file access goes through the host's
//! [`Services`] (`list_entries`, `volumes`, `home_dir`), so tests run against a fake filesystem
//! and the web build lists the files the user picked or dropped.
//!
//! | Id | What |
//! |---|---|
//! | `mediaBrowser.roots` | Favorites, Local Drives, Network, recent directories, home |
//! | `mediaBrowser.list` | entries of a directory (default: the current one), filtered by file type |
//! | `mediaBrowser.navigate` | go to a directory, or `back` / `forward` / `up` |
//! | `mediaBrowser.select` | the selected files (File ▸ Import from Media Browser uses them) |
//! | `mediaBrowser.favorite` | Add to / Remove from Favorites |
//! | `mediaBrowser.clearRecent` | Clear Recent Directories |
//! | `mediaBrowser.settings` | file types, view (list / thumbnails), columns (Edit Columns…), Import as Image Sequence, Hover Scrub, thumbnail size |
//! | `mediaBrowser.import` | Import (the selection or `paths`; Project ▸ Ingest settings apply) |
//! | `mediaBrowser.openInSource` | Open In Source Monitor (Shift+O) |
//! | `mediaBrowser.probe` | a file's media properties (frame rate, duration, video / audio info), cached |
//!
//! Navigation history and the selection are session state ([`BrowserState`]); Favorites, recent
//! directories and the view settings are preferences ([`MediaBrowserPrefs`]).

use std::collections::BTreeMap;

use filmcraft_media::MediaInfo;
use filmcraft_project::ItemId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, str_p, u64_p};
use crate::{EngineError, Result, Services, Session};

/// One directory entry as the host reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// Modification time (seconds since the Unix epoch).
    pub modified: Option<u64>,
}

/// Where a volume shows in the directory tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VolumeKind {
    #[default]
    Local,
    Network,
}

/// A drive or network location (Local Drives / Network).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub name: String,
    pub path: String,
    pub kind: VolumeKind,
}

/// File types shown (the "File Types Displayed" menu).
pub const FILE_TYPES: [(&str, &str); 6] = [
    ("all", "All Supported Files"),
    ("video", "Video Files"),
    ("audio", "Audio Files"),
    ("image", "Still Images"),
    ("project", "Projects and Interchange"),
    ("caption", "Captions"),
];

/// Columns the list view can show (Edit Columns…), in default order.
pub const ALL_COLUMNS: [&str; 10] =
    ["Name", "Media Type", "Size", "Date Modified", "Frame Rate", "Media Duration", "Video Info", "Audio Info", "Video Codec", "Audio Codec"];
pub const DEFAULT_COLUMNS: [&str; 5] = ["Name", "Frame Rate", "Media Duration", "Video Info", "Size"];

const PROJECT_EXTENSIONS: [&str; 7] = ["fcproj", "edl", "xml", "fcpxml", "otio", "ale", "prproj"];
const CAPTION_EXTENSIONS: [&str; 3] = ["srt", "vtt", "scc"];
const RECENT_MAX: usize = 10;

/// Preferences of the Media Browser (`Preferences::media_browser`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaBrowserPrefs {
    pub favorites: Vec<String>,
    /// Recent directories, newest first.
    pub recent: Vec<String>,
    /// Directory shown when the browser opens ("" = the home directory).
    pub last_dir: String,
    /// One of [`FILE_TYPES`], or a file extension (`mov`).
    pub file_types: String,
    /// `list` | `thumbnails`.
    pub view: String,
    pub columns: Vec<String>,
    pub import_as_image_sequence: bool,
    pub hover_scrub: bool,
    pub thumbnail_size: f32,
}

impl Default for MediaBrowserPrefs {
    fn default() -> Self {
        MediaBrowserPrefs {
            favorites: Vec::new(),
            recent: Vec::new(),
            last_dir: String::new(),
            file_types: "all".into(),
            view: "list".into(),
            columns: DEFAULT_COLUMNS.iter().map(|c| c.to_string()).collect(),
            import_as_image_sequence: false,
            hover_scrub: true,
            thumbnail_size: 120.0,
        }
    }
}

/// Session state of the Media Browser.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserState {
    /// The directory shown ("" = not chosen yet: the last or home directory).
    pub dir: String,
    pub back: Vec<String>,
    pub forward: Vec<String>,
    pub selection: Vec<String>,
    /// Probed media properties by path (None = not a readable media file).
    #[serde(skip)]
    pub probes: BTreeMap<String, Option<MediaInfo>>,
}

/// A listed entry with what the browser shows about it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// `folder` | `video` | `audio` | `image` | `project` | `caption`.
    pub kind: &'static str,
    pub size: Option<u64>,
    pub modified: Option<u64>,
    /// A numbered still (Import as Image Sequence applies).
    pub numbered: bool,
}

fn ext_of(name: &str) -> String {
    std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// The kind of file a name is, or None when the browser doesn't show it.
pub fn kind_of(name: &str) -> Option<&'static str> {
    let e = ext_of(name);
    if filmcraft_media::VIDEO_EXTENSIONS.contains(&e.as_str()) {
        Some("video")
    } else if filmcraft_media::AUDIO_EXTENSIONS.contains(&e.as_str()) {
        Some("audio")
    } else if filmcraft_media::STILL_EXTENSIONS.contains(&e.as_str()) {
        Some("image")
    } else if PROJECT_EXTENSIONS.contains(&e.as_str()) {
        Some("project")
    } else if CAPTION_EXTENSIONS.contains(&e.as_str()) {
        Some("caption")
    } else {
        None
    }
}

/// Does a file of `kind` and extension pass the file-type filter?
pub fn passes(filter: &str, kind: &str, name: &str) -> bool {
    match filter {
        "" | "all" => true,
        "video" | "audio" | "image" | "project" | "caption" => kind == filter,
        ext => ext_of(name) == ext.trim_start_matches('.').to_ascii_lowercase(),
    }
}

/// Every extension the browser lists (for the file-type menu).
pub fn extensions() -> Vec<&'static str> {
    filmcraft_media::VIDEO_EXTENSIONS
        .iter()
        .chain(filmcraft_media::AUDIO_EXTENSIONS)
        .chain(filmcraft_media::STILL_EXTENSIONS)
        .chain(&PROJECT_EXTENSIONS)
        .chain(&CAPTION_EXTENSIONS)
        .copied()
        .collect()
}

fn windows_style(p: &str) -> bool {
    p.contains('\\') && !p.contains('/')
}

/// `dir` + `name` with the directory's separator.
pub fn join(dir: &str, name: &str) -> String {
    let sep = if windows_style(dir) { '\\' } else { '/' };
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with(sep) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

/// The parent directory (None at a root).
pub fn parent(dir: &str) -> Option<String> {
    let sep = if windows_style(dir) { '\\' } else { '/' };
    let t = dir.trim_end_matches(sep);
    if t.is_empty() {
        return None;
    }
    let i = t.rfind(sep)?;
    let p = &t[..i];
    if p.is_empty() {
        Some(sep.to_string())
    } else if windows_style(dir) && p.ends_with(':') {
        Some(format!("{p}\\"))
    } else {
        Some(p.to_string())
    }
}

/// The last path component (a directory's name in the tree).
pub fn base_name(path: &str) -> String {
    let sep = if windows_style(path) { '\\' } else { '/' };
    let t = path.trim_end_matches(sep);
    t.rsplit(sep).next().filter(|n| !n.is_empty()).unwrap_or(path).to_string()
}

/// List a directory: sub-directories first, then files the browser shows (hidden files left out),
/// each group by name (case-insensitive).
pub fn list(services: &dyn Services, dir: &str, filter: &str) -> std::io::Result<Vec<Entry>> {
    let raw = services.list_entries(dir).unwrap_or_else(|| Err(std::io::Error::other("this host cannot list directories")))?;
    let mut out: Vec<Entry> = raw
        .into_iter()
        .filter(|e| !e.name.starts_with('.'))
        .filter_map(|e| {
            let path = join(dir, &e.name);
            if e.is_dir {
                return Some(Entry { numbered: false, name: e.name, path, is_dir: true, kind: "folder", size: None, modified: e.modified });
            }
            let kind = kind_of(&e.name)?;
            if !passes(filter, kind, &e.name) {
                return None;
            }
            let numbered = kind == "image" && filmcraft_media::sequence::Numbered::parse(&path).is_some();
            Some(Entry { numbered, name: e.name, path, is_dir: false, kind, size: e.size, modified: e.modified })
        })
        .collect();
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// The directory shown: the current one, else the last one, else home, else the first volume.
pub fn current_dir(s: &Session) -> String {
    if !s.browser.dir.is_empty() {
        return s.browser.dir.clone();
    }
    let last = &s.prefs.media_browser.last_dir;
    if !last.is_empty() {
        return last.clone();
    }
    s.services.home_dir().or_else(|| s.services.volumes().first().map(|v| v.path.clone())).unwrap_or_else(|| "/".into())
}

fn save_prefs(s: &mut Session, prefs: crate::autosave::Preferences) -> Result<()> {
    if prefs != s.prefs {
        s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(())
}

fn state_json(s: &Session) -> Value {
    json!({
        "dir": current_dir(s),
        "canBack": !s.browser.back.is_empty(),
        "canForward": !s.browser.forward.is_empty(),
        "canUp": parent(&current_dir(s)).is_some(),
        "selection": s.browser.selection,
        "favorite": s.prefs.media_browser.favorites.contains(&current_dir(s)),
    })
}

fn roots(s: &mut Session, _: &Value) -> Result<Value> {
    let vols = s.services.volumes();
    let (local, network): (Vec<Volume>, Vec<Volume>) = vols.into_iter().partition(|v| v.kind == VolumeKind::Local);
    let mb = &s.prefs.media_browser;
    let named = |v: &Vec<String>| v.iter().map(|p| json!({"name": base_name(p), "path": p})).collect::<Vec<_>>();
    Ok(json!({
        "favorites": named(&mb.favorites),
        "localDrives": local,
        "network": network,
        "recent": named(&mb.recent),
        "home": s.services.home_dir(),
    }))
}

fn list_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let dir = str_p(p, "path").map(str::to_string).unwrap_or_else(|| current_dir(s));
    let filter = str_p(p, "fileTypes").map(str::to_string).unwrap_or_else(|| s.prefs.media_browser.file_types.clone());
    let entries = list(&*s.services, &dir, &filter).map_err(|e| bad("mediaBrowser.list", format!("{dir}: {e}")))?;
    let mut out = state_json(s);
    out["dir"] = json!(dir);
    out["fileTypes"] = json!(filter);
    out["entries"] = serde_json::to_value(&entries).unwrap_or_default();
    Ok(out)
}

fn go(s: &mut Session, dir: String, history: bool) -> Result<()> {
    // the directory must be listable
    list(&*s.services, &dir, "all").map_err(|e| bad("mediaBrowser.navigate", format!("{dir}: {e}")))?;
    let cur = current_dir(s);
    if history && cur != dir {
        s.browser.back.push(cur);
        s.browser.forward.clear();
    }
    s.browser.dir = dir.clone();
    s.browser.selection.clear();
    let mut prefs = s.prefs.clone();
    let mb = &mut prefs.media_browser;
    mb.recent.retain(|r| *r != dir);
    mb.recent.insert(0, dir.clone());
    mb.recent.truncate(RECENT_MAX);
    mb.last_dir = dir;
    save_prefs(s, prefs)
}

fn navigate(s: &mut Session, p: &Value) -> Result<Value> {
    if bool_p(p, "back") == Some(true) {
        let to = s.browser.back.pop().ok_or_else(|| bad("mediaBrowser.navigate", "nothing to go back to"))?;
        let cur = current_dir(s);
        if let Err(e) = go(s, to.clone(), false) {
            s.browser.back.push(to);
            return Err(e);
        }
        s.browser.forward.push(cur);
    } else if bool_p(p, "forward") == Some(true) {
        let to = s.browser.forward.pop().ok_or_else(|| bad("mediaBrowser.navigate", "nothing to go forward to"))?;
        let cur = current_dir(s);
        if let Err(e) = go(s, to.clone(), false) {
            s.browser.forward.push(to);
            return Err(e);
        }
        s.browser.back.push(cur);
    } else if bool_p(p, "up") == Some(true) {
        let to = parent(&current_dir(s)).ok_or_else(|| bad("mediaBrowser.navigate", "already at the top"))?;
        go(s, to, true)?;
    } else {
        let path = str_p(p, "path").ok_or_else(|| bad("mediaBrowser.navigate", "need `path`, or `back` / `forward` / `up`"))?;
        go(s, path.to_string(), true)?;
    }
    list_cmd(s, &json!({}))
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    s.browser.selection = paths;
    Ok(state_json(s))
}

fn favorite(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").map(str::to_string).unwrap_or_else(|| current_dir(s));
    let remove = bool_p(p, "remove").unwrap_or(false);
    let mut prefs = s.prefs.clone();
    let fav = &mut prefs.media_browser.favorites;
    if remove {
        if !fav.contains(&path) {
            return Err(bad("mediaBrowser.favorite", format!("{path} is not a favorite")));
        }
        fav.retain(|f| *f != path);
    } else if !fav.contains(&path) {
        fav.push(path);
    }
    save_prefs(s, prefs)?;
    Ok(json!({"favorites": s.prefs.media_browser.favorites}))
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    let mut prefs = s.prefs.clone();
    let mb = &mut prefs.media_browser;
    if let Some(v) = str_p(p, "fileTypes") {
        let v = v.trim().trim_start_matches('.').to_ascii_lowercase();
        if !FILE_TYPES.iter().any(|f| f.0 == v) && !extensions().contains(&v.as_str()) {
            return Err(bad("mediaBrowser.settings", format!("unknown file type `{v}`")));
        }
        mb.file_types = v;
    }
    if let Some(v) = str_p(p, "view") {
        mb.view = match v.to_ascii_lowercase().as_str() {
            "list" => "list".into(),
            "thumbnails" | "thumbnail" | "icon" => "thumbnails".into(),
            _ => return Err(bad("mediaBrowser.settings", "view is `list` or `thumbnails`")),
        };
    }
    if let Some(a) = p.get("columns").and_then(Value::as_array) {
        let mut cols = vec!["Name".to_string()];
        for c in a.iter().filter_map(Value::as_str) {
            let c = ALL_COLUMNS.iter().find(|x| x.eq_ignore_ascii_case(c)).ok_or_else(|| bad("mediaBrowser.settings", format!("unknown column `{c}`")))?;
            if !cols.iter().any(|x| x == c) {
                cols.push(c.to_string());
            }
        }
        mb.columns = cols;
    }
    if let Some(v) = bool_p(p, "importAsImageSequence") {
        mb.import_as_image_sequence = v;
    }
    if let Some(v) = bool_p(p, "hoverScrub") {
        mb.hover_scrub = v;
    }
    if let Some(v) = f64_p(p, "thumbnailSize") {
        mb.thumbnail_size = (v as f32).clamp(60.0, 320.0);
    }
    save_prefs(s, prefs)?;
    Ok(serde_json::to_value(&s.prefs.media_browser).unwrap_or_default())
}

fn paths_p(s: &Session, p: &Value) -> Vec<String> {
    match p.get("paths").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        None => str_p(p, "path").map(|x| vec![x.to_string()]).unwrap_or_else(|| s.browser.selection.clone()),
    }
}

/// `mediaBrowser.import {paths?, bin?, imageSequence?}`: imports files (directories import their
/// supported files, like dragging a folder in Premiere).
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let paths = paths_p(s, p);
    if paths.is_empty() {
        return Err(bad("mediaBrowser.import", "select files in the Media Browser"));
    }
    let seq = bool_p(p, "imageSequence").unwrap_or(s.prefs.media_browser.import_as_image_sequence);
    let mut files = Vec::new();
    for path in paths {
        // a directory imports its media files (not recursive)
        let is_dir = matches!(s.services.list_entries(&path), Some(Ok(_))) && kind_of(&path).is_none();
        match list(&*s.services, &path, "all") {
            Ok(entries) if is_dir => files.extend(entries.into_iter().filter(|e| !e.is_dir).map(|e| e.path)),
            _ => files.push(path),
        }
    }
    // an image sequence imports from its first selected frame only
    if seq {
        files.truncate(1);
    }
    let mut q = json!({"paths": files, "imageSequence": seq});
    if let Some(b) = u64_p(p, "bin") {
        q["bin"] = json!(b);
    }
    s.execute("file.import", q)
}

/// The project item that already references a file.
pub fn item_for_path(s: &Session, path: &str) -> Option<ItemId> {
    s.project
        .items
        .values()
        .find(|i| i.as_media().is_some_and(|m| matches!(&m.media, filmcraft_project::MediaRef::File { path: p } if p == path)))
        .map(|i| i.id)
}

fn open_in_source(s: &mut Session, p: &Value) -> Result<Value> {
    let path = paths_p(s, p).into_iter().next().ok_or_else(|| bad("mediaBrowser.openInSource", "select a file in the Media Browser"))?;
    let item = match item_for_path(s, &path) {
        Some(i) => i,
        None => {
            let r = import(s, &json!({"paths": [path], "imageSequence": false}))?;
            r["items"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_u64)
                .map(ItemId)
                .ok_or_else(|| bad("mediaBrowser.openInSource", "the file could not be imported"))?
        }
    };
    s.execute("source.open", json!({"item": item.0}))?;
    Ok(json!({"item": item.0}))
}

/// Media properties of a file (cached per path). Only the container's index is read; a format
/// without a streaming reader is read whole only when small (see [`MediaPool::probe_file`]), so
/// browsing a folder of large AVI or WAV files doesn't read all of them (#157).
///
/// [`MediaPool::probe_file`]: crate::media_pool::MediaPool::probe_file
pub fn probe(s: &mut Session, path: &str) -> Option<MediaInfo> {
    if let Some(v) = s.browser.probes.get(path) {
        return v.clone();
    }
    let info = if kind_of(path).is_some_and(|k| matches!(k, "video" | "audio" | "image")) {
        s.media.probe_file(path, &*s.services).ok().map(|src| src.info().clone())
    } else {
        None
    };
    s.browser.probes.insert(path.to_string(), info.clone());
    info
}

/// The text of a list column for an entry (`probe`: its media properties, when known).
pub fn column_text(e: &Entry, column: &str, probe: Option<&MediaInfo>) -> String {
    match column {
        "Name" => e.name.clone(),
        "Media Type" => {
            if e.is_dir {
                "Folder".into()
            } else {
                format!("{}{}", e.kind[..1].to_uppercase(), &e.kind[1..])
            }
        }
        "Size" => e.size.map(format_size).unwrap_or_default(),
        "Date Modified" => e.modified.map(format_date).unwrap_or_default(),
        "Frame Rate" => probe.and_then(|i| i.video.as_ref()).filter(|_| e.kind == "video").map(|v| format!("{} fps", v.frame_rate.label())).unwrap_or_default(),
        "Media Duration" => probe
            .filter(|_| e.kind != "image")
            .map(|i| {
                let rate = i.video.as_ref().map(|v| v.frame_rate).unwrap_or_default();
                filmcraft_time::format_time(i.duration, rate, false, filmcraft_time::TimeDisplay::Timecode, 48000)
            })
            .unwrap_or_default(),
        "Video Info" => probe.and_then(|i| i.video.as_ref()).map(|v| format!("{} x {}", v.width, v.height)).unwrap_or_default(),
        "Audio Info" => probe.and_then(|i| i.audio()).map(|a| format!("{} Hz - {} ch", a.sample_rate, a.channels)).unwrap_or_default(),
        "Video Codec" => probe.and_then(|i| i.video.as_ref()).map(|v| v.codec.clone()).unwrap_or_default(),
        "Audio Codec" => probe.and_then(|i| i.audio()).map(|a| a.codec.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

fn format_size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// `YYYY-MM-DD HH:MM` (UTC) from Unix seconds.
pub fn format_date(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem / 60) % 60)
}

fn probe_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("mediaBrowser.probe", "need `path`"))?.to_string();
    let info = probe(s, &path);
    Ok(json!({"path": path, "media": info.is_some(), "info": info}))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    let c = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: true };
    let q = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false };
    vec![
        q("mediaBrowser.roots", "Media Browser Locations", "{}", roots),
        q("mediaBrowser.list", "List Directory", r#"{"path":str?,"fileTypes":str?}"#, list_cmd),
        c("mediaBrowser.navigate", "Go to Directory", r#"{"path":str} | {"back":true} | {"forward":true} | {"up":true}"#, navigate),
        c("mediaBrowser.select", "Select Files", r#"{"paths":[str]}"#, select),
        c("mediaBrowser.favorite", "Add to Favorites", r#"{"path":str?,"remove":bool?}"#, favorite),
        c("mediaBrowser.clearRecent", "Clear Recent Directories", "{}", |s, _| {
            let mut prefs = s.prefs.clone();
            prefs.media_browser.recent.clear();
            save_prefs(s, prefs)?;
            Ok(Value::Null)
        }),
        c(
            "mediaBrowser.settings",
            "Media Browser Settings",
            r#"{"fileTypes":"all|video|audio|image|project|caption|<ext>"?,"view":"list|thumbnails"?,"columns":[str]?,"importAsImageSequence":bool?,"hoverScrub":bool?,"thumbnailSize":f32?}"#,
            settings,
        ),
        c("mediaBrowser.import", "Import", r#"{"paths":[str]?,"bin":binId?,"imageSequence":bool?}"#, import),
        CommandSpec {
            id: "mediaBrowser.openInSource",
            label: "Open In Source Monitor",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str?}"#,
            enabled: always,
            run: open_in_source,
            journal: true,
        },
        q("mediaBrowser.probe", "Media File Properties", r#"{"path":str}"#, probe_cmd),
    ]
}

// ------------------------------------------------------------------------------------ native fs

/// Directory entries through `std::fs` (the desktop host).
pub fn std_list_entries(dir: &str) -> std::io::Result<Vec<DirEntry>> {
    let dir = if dir.is_empty() { "." } else { dir };
    let rd = std::fs::read_dir(dir)?;
    Ok(rd
        .flatten()
        .map(|e| {
            // follow symlinks (aliases to folders show as folders)
            let md = std::fs::metadata(e.path()).ok();
            let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
            DirEntry {
                name: e.file_name().to_string_lossy().to_string(),
                is_dir,
                size: md.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
                modified: md.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
            }
        })
        .collect())
}

/// Drives and network locations of the desktop host.
pub fn std_volumes() -> Vec<Volume> {
    let mut v = Vec::new();
    let dir_names = |d: &str| -> Vec<String> {
        std::fs::read_dir(d)
            .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default()
    };
    if cfg!(windows) {
        for l in 'A'..='Z' {
            let p = format!("{l}:\\");
            if std::path::Path::new(&p).exists() {
                v.push(Volume { name: format!("({l}:)"), path: p, kind: VolumeKind::Local });
            }
        }
        return v;
    }
    if cfg!(target_os = "macos") {
        let mut names = dir_names("/Volumes");
        names.sort();
        for n in names {
            v.push(Volume { path: format!("/Volumes/{n}"), name: n, kind: VolumeKind::Local });
        }
        if v.is_empty() {
            v.push(Volume { name: "/".into(), path: "/".into(), kind: VolumeKind::Local });
        }
        if std::path::Path::new("/Network").is_dir() {
            v.push(Volume { name: "Network".into(), path: "/Network".into(), kind: VolumeKind::Network });
        }
        return v;
    }
    v.push(Volume { name: "/".into(), path: "/".into(), kind: VolumeKind::Local });
    for base in ["/media", "/mnt", "/run/media"] {
        for n in dir_names(base) {
            let p = format!("{base}/{n}");
            // /media/<user>/<volume>
            let inner = dir_names(&p);
            if base != "/mnt" && !inner.is_empty() {
                for m in inner {
                    v.push(Volume { path: format!("{p}/{m}"), name: m, kind: VolumeKind::Local });
                }
            } else {
                v.push(Volume { name: n, path: p, kind: VolumeKind::Local });
            }
        }
    }
    v
}

/// The desktop user's home directory. Windows prefers `USERPROFILE`: shells like Git Bash set
/// `HOME` to a Unix-style path (`/c/Users/…`) that native file APIs cannot open.
pub fn std_home_dir() -> Option<String> {
    let vars = if cfg!(windows) { ["USERPROFILE", "HOME"] } else { ["HOME", "USERPROFILE"] };
    vars.iter().find_map(|v| std::env::var(v).ok().filter(|h| !h.is_empty()))
}

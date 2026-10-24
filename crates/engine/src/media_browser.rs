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
//! | `mediaBrowser.import` | Import (the selection or `paths`; a folder becomes a bin, its sub-folders bins inside it; Project ▸ Ingest settings apply) |
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
    /// A drive that has not answered yet (a network share that is slow or gone): listed, greyed,
    /// and asked again on the next scan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
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

/// Project and interchange files the browser can import. Avid Log Exchange (`.ale`) is left out:
/// FilmCraft writes it (File ▸ Export) but, like Premiere, does not read it (#1292).
const PROJECT_EXTENSIONS: [&str; 5] = ["fcproj", "edl", "xml", "fcpxml", "otio"];
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
    /// Volume scans and directory listings, read off the UI thread.
    #[serde(skip)]
    pub io: BrowserIo,
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

// ------------------------------------------------------------------------------------ off the UI thread

/// How long `mediaBrowser.list` waits for a directory before saying it is still being read.
const LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long `mediaBrowser.navigate` waits before going to a directory that has not answered.
const NAVIGATE_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// A directory listing read by [`BrowserIo`].
#[derive(Clone, Debug)]
pub enum Listing {
    Ready(std::sync::Arc<Vec<Entry>>),
    /// Not answered yet (a slow network share, a drive spinning up): ask again later.
    Pending,
    Failed(String),
}

/// The Media Browser's file system reads, each on a thread of its own and cached, so a slow or
/// dead share shows as pending instead of freezing the window. In a field report `mediaBrowser.roots`
/// looked at every drive letter on the UI thread, every frame the panel was open, and two SMB
/// shares froze the window for 13.8 s and 16.1 s. A read that is done within [`BrowserIo::WAIT`]
/// (a local disk, the tests' file system) is answered at once.
#[derive(Clone, Debug, Default)]
pub struct BrowserIo {
    inner: std::sync::Arc<std::sync::Mutex<IoState>>,
}

#[derive(Debug, Default)]
struct IoState {
    volumes: Option<(web_time::Instant, Vec<Volume>)>,
    scanning: bool,
    /// By directory, unfiltered: every file-type filter shares one read of a folder.
    lists: std::collections::HashMap<String, (web_time::Instant, Listing)>,
    reading: std::collections::HashSet<String>,
}

impl BrowserIo {
    /// How long a read is waited for before the caller gets the last result (or pending).
    pub const WAIT: std::time::Duration = std::time::Duration::from_millis(40);
    const VOLUMES_FRESH: std::time::Duration = std::time::Duration::from_secs(10);
    const LIST_FRESH: std::time::Duration = std::time::Duration::from_secs(3);
    /// Reads at once: a dead share holds its thread until the OS gives up, and no more are
    /// started past this many in all, nor past [`Self::MAX_READS_PER_ROOT`] on one drive or share.
    /// A drive or share with no read going may still start one up to [`Self::MAX_READS_HARD`], so
    /// dead shares never keep another folder (a local one) from being read.
    const MAX_READS: usize = 16;
    const MAX_READS_PER_ROOT: usize = 4;
    const MAX_READS_HARD: usize = 32;
    const MAX_LISTS: usize = 512;

    fn state(&self) -> std::sync::MutexGuard<'_, IoState> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The drives and network locations from the last scan, and whether a scan is going on.
    pub fn volumes(&self, services: &std::sync::Arc<dyn Services>) -> (Vec<Volume>, bool) {
        let start = {
            let mut st = self.state();
            let old = st.volumes.as_ref().is_none_or(|(t, _)| t.elapsed() >= Self::VOLUMES_FRESH);
            let start = old && !st.scanning;
            st.scanning |= start;
            start
        };
        if start {
            let inner = self.inner.clone();
            let started = run_off_thread("filmcraft-volumes", services.clone(), Self::WAIT, move |services| {
                let v = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| services.volumes())).unwrap_or_default();
                let mut st = inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                st.volumes = Some((web_time::Instant::now(), v));
                st.scanning = false;
            });
            if !started {
                // no scan is going on: the next call tries again
                self.state().scanning = false;
            }
        }
        let st = self.state();
        (st.volumes.as_ref().map(|(_, v)| v.clone()).unwrap_or_default(), st.scanning)
    }

    /// The entries of `dir` under `filter`: the cached listing (read again in the background
    /// once it is a few seconds old), or pending until the first read is done.
    pub fn listing(&self, services: &std::sync::Arc<dyn Services>, dir: &str, filter: &str) -> Listing {
        self.list(services, dir, filter, Self::WAIT, false)
    }

    /// [`Self::listing`] read again now (a command that asks for the directory as it is),
    /// waiting up to `wait` for it.
    pub fn read(&self, services: &std::sync::Arc<dyn Services>, dir: &str, filter: &str, wait: std::time::Duration) -> Listing {
        self.list(services, dir, filter, wait, true)
    }

    /// Whether `dir` is being read (again) now.
    pub fn is_reading(&self, dir: &str) -> bool {
        self.state().reading.contains(dir)
    }

    /// Mark every listing and the drives as old: they are read again when next asked for (the
    /// panel shows the old ones meanwhile).
    pub fn expire(&self) {
        let mut st = self.state();
        let long_ago = web_time::Instant::now().checked_sub(Self::VOLUMES_FRESH.saturating_mul(2));
        if let Some(t) = long_ago {
            for (at, _) in st.lists.values_mut() {
                *at = t;
            }
            if let Some((at, _)) = st.volumes.as_mut() {
                *at = t;
            }
        } else {
            st.lists.clear();
            st.volumes = None;
        }
    }

    fn list(&self, services: &std::sync::Arc<dyn Services>, dir: &str, filter: &str, wait: std::time::Duration, fresh: bool) -> Listing {
        let key = dir.to_string();
        let start = {
            let mut st = self.state();
            let old = fresh || st.lists.get(&key).is_none_or(|(t, _)| t.elapsed() >= Self::LIST_FRESH);
            let volumes = st.volumes.as_ref().map_or(&[][..], |(_, v)| v.as_slice());
            let root = read_root(dir, volumes);
            let on_root = st.reading.iter().filter(|d| read_root(d, volumes) == root).count();
            let room =
                if on_root == 0 { st.reading.len() < Self::MAX_READS_HARD } else { st.reading.len() < Self::MAX_READS && on_root < Self::MAX_READS_PER_ROOT };
            let start = old && !st.reading.contains(&key) && room;
            if start {
                st.reading.insert(key.clone());
            }
            start
        };
        if start {
            let (inner, k) = (self.inner.clone(), key.clone());
            let started = run_off_thread("filmcraft-media-browser", services.clone(), wait, move |services| {
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| list(&*services, &k, "all")));
                let listing = match read {
                    Ok(Ok(v)) => Listing::Ready(std::sync::Arc::new(v)),
                    Ok(Err(e)) => Listing::Failed(e.to_string()),
                    Err(_) => Listing::Failed("internal error while listing (see the crash log)".into()),
                };
                let mut st = inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if st.lists.len() >= Self::MAX_LISTS {
                    st.lists.retain(|_, (t, _)| t.elapsed() < Self::LIST_FRESH);
                }
                st.reading.remove(&k);
                st.lists.insert(k, (web_time::Instant::now(), listing));
            });
            if !started {
                // nothing is reading it: say so (read again once this is old) rather than pending forever
                let mut st = self.state();
                st.reading.remove(&key);
                st.lists.insert(key.clone(), (web_time::Instant::now(), Listing::Failed("could not start a thread to read the folder".into())));
            }
        }
        let st = self.state();
        let listing = match st.lists.get(&key) {
            // the last listing stands in while a fresh one is read, but a read asked for now
            // (`read`) is answered only by the read it started, once that is done
            Some((_, l)) if !fresh || (start && !st.reading.contains(&key)) => l.clone(),
            Some(_) | None => Listing::Pending,
        };
        match listing {
            Listing::Ready(v) if !matches!(filter, "" | "all") => {
                Listing::Ready(std::sync::Arc::new(v.iter().filter(|e| e.is_dir || passes(filter, e.kind, &e.name)).cloned().collect()))
            }
            l => l,
        }
    }
}

/// The drive or share `dir` is on, to share out the reads: the longest scanned volume that holds
/// it (the system root aside), else its first component (`X:`, `server\share`, `/home`).
fn read_root(dir: &str, volumes: &[Volume]) -> String {
    const SEPS: [char; 2] = ['/', '\\'];
    let holds = |v: &str| dir == v || dir.strip_prefix(v).is_some_and(|rest| v.ends_with(SEPS) || rest.starts_with(SEPS));
    let volume = volumes.iter().map(|v| v.path.as_str()).filter(|v| !v.trim_matches(SEPS).is_empty() && holds(v)).max_by_key(|v| v.len());
    if let Some(v) = volume {
        return v.to_string();
    }
    if windows_style(dir) {
        // a drive letter, or a UNC path's server and share
        let parts = if dir.starts_with(r"\\") { 2 } else { 1 };
        return dir.trim_start_matches('\\').split('\\').take(parts).collect::<Vec<_>>().join("\\");
    }
    dir.split('/').find(|c| !c.is_empty()).map_or_else(|| "/".to_string(), |c| format!("/{c}"))
}

/// Run `work` on a thread named `name` and wait up to `wait` for it (on the web, where there are
/// no threads, run it now). False when the thread could not be started (the OS refused one): then
/// `work` never runs, and the caller takes back what it marked as in progress.
fn run_off_thread(
    name: &str,
    services: std::sync::Arc<dyn Services>,
    wait: std::time::Duration,
    work: impl FnOnce(std::sync::Arc<dyn Services>) + Send + 'static,
) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (name, wait);
        work(services);
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        #[cfg(test)]
        if tests_hook::REFUSE_THREADS.with(std::cell::Cell::get) {
            log::warn!("{name}: thread not started: refused (test)");
            return false;
        }
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            work(services);
            let _ = tx.send(());
        });
        match spawned {
            Ok(_) => {
                let _ = rx.recv_timeout(wait);
                true
            }
            Err(e) => {
                log::warn!("{name}: thread not started: {e}");
                false
            }
        }
    }
}

/// Lets a test make [`run_off_thread`] fail as when the OS refuses a thread (on the test's own thread).
#[cfg(test)]
pub(crate) mod tests_hook {
    thread_local! {
        pub(crate) static REFUSE_THREADS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
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
    // the panel asks every frame: answered from the last scan, a new one started when it is old
    let (vols, scanning) = s.browser.io.volumes(&s.services);
    let (local, network): (Vec<Volume>, Vec<Volume>) = vols.into_iter().partition(|v| v.kind == VolumeKind::Local);
    let mb = &s.prefs.media_browser;
    let named = |v: &Vec<String>| v.iter().map(|p| json!({"name": base_name(p), "path": p})).collect::<Vec<_>>();
    Ok(json!({
        "favorites": named(&mb.favorites),
        "localDrives": local,
        "network": network,
        "recent": named(&mb.recent),
        "home": s.services.home_dir(),
        // the drives are still being looked at (the first scan, or a share that is slow)
        "scanning": scanning,
    }))
}

fn list_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let dir = str_p(p, "path").map(str::to_string).unwrap_or_else(|| current_dir(s));
    let filter = str_p(p, "fileTypes").map(str::to_string).unwrap_or_else(|| s.prefs.media_browser.file_types.clone());
    let entries = match s.browser.io.read(&s.services, &dir, &filter, LIST_WAIT) {
        Listing::Ready(v) => v,
        Listing::Pending => return Err(bad("mediaBrowser.list", format!("{dir}: still reading (the drive is not answering yet)"))),
        Listing::Failed(e) => return Err(bad("mediaBrowser.list", format!("{dir}: {e}"))),
    };
    let mut out = state_json(s);
    out["dir"] = json!(dir);
    out["fileTypes"] = json!(filter);
    out["entries"] = serde_json::to_value(&*entries).unwrap_or_default();
    Ok(out)
}

fn go(s: &mut Session, dir: String, history: bool) -> Result<()> {
    // The directory must be listable. A drive that does not answer within NAVIGATE_WAIT is gone
    // to anyway: the panel shows it being read, and its error if it fails.
    if let Listing::Failed(e) = s.browser.io.read(&s.services, &dir, "all", NAVIGATE_WAIT) {
        return Err(bad("mediaBrowser.navigate", format!("{dir}: {e}")));
    }
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
    // the directory as far as it has been read: a drive that has not answered yet is gone to
    // anyway (`pending`, no entries yet), the panel shows it being read
    let (dir, filter) = (current_dir(s), s.prefs.media_browser.file_types.clone());
    let listing = s.browser.io.listing(&s.services, &dir, &filter);
    let mut out = state_json(s);
    out["dir"] = json!(dir);
    out["fileTypes"] = json!(filter);
    match listing {
        Listing::Ready(entries) => {
            out["entries"] = serde_json::to_value(&*entries).unwrap_or_default();
            // the last listing while the folder is read again: say it may be out of date
            if s.browser.io.is_reading(&dir) {
                out["pending"] = json!(true);
            }
        }
        Listing::Pending => {
            out["entries"] = json!([]);
            out["pending"] = json!(true);
        }
        Listing::Failed(e) => return Err(bad("mediaBrowser.navigate", format!("{dir}: {e}"))),
    }
    Ok(out)
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

/// Most folders one import walks, and how deep it goes: a folder import mirrors the tree as bins,
/// and symlinked folders can loop.
const FOLDER_IMPORT_MAX_FOLDERS: usize = 4096;
const FOLDER_IMPORT_MAX_DEPTH: usize = 32;

/// A folder to import as a bin: its name, its files and its sub-folders that hold files.
struct Folder {
    name: String,
    files: Vec<String>,
    subs: Vec<Folder>,
}

/// The files the browser shows in `dir` and its sub-folders, as a tree; `None` when the tree
/// holds none (or `budget` folders were walked already).
fn walk_folder(services: &dyn Services, dir: &str, depth: usize, budget: &mut usize) -> Option<Folder> {
    *budget = budget.checked_sub(1)?;
    let entries = list(services, dir, "all").ok()?;
    let mut folder = Folder { name: base_name(dir), files: Vec::new(), subs: Vec::new() };
    for e in entries {
        if !e.is_dir {
            folder.files.push(e.path);
        } else if depth < FOLDER_IMPORT_MAX_DEPTH
            && let Some(sub) = walk_folder(services, &e.path, depth + 1, budget)
        {
            folder.subs.push(sub);
        }
    }
    (!folder.files.is_empty() || !folder.subs.is_empty()).then_some(folder)
}

/// Add a `file.import` result to the merged one: arrays add up, other values (the Ingest result)
/// are listed.
fn merge_import(out: &mut serde_json::Map<String, Value>, r: Value) {
    let Value::Object(r) = r else { return };
    for (k, v) in r {
        let slot = out.entry(k).or_insert_with(|| json!([]));
        let Value::Array(all) = slot else { continue };
        match v {
            Value::Null => {}
            Value::Array(a) => all.extend(a),
            other => all.push(other),
        }
    }
}

/// Run `file.import` and merge its result; a failure becomes an error of the merged result.
fn import_into(s: &mut Session, files: &[String], bin: Option<u64>, out: &mut serde_json::Map<String, Value>) {
    let mut q = json!({"paths": files, "imageSequence": false});
    if let Some(b) = bin {
        q["bin"] = json!(b);
    }
    match s.execute("file.import", q) {
        Ok(r) => merge_import(out, r),
        Err(e) => merge_import(out, json!({"errors": [e.to_string()]})),
    }
}

/// Import a folder as a bin named after it, in `parent`: its files go into the bin, each
/// sub-folder becomes a bin inside it (Premiere's Media Browser keeps the folder structure).
fn import_folder(s: &mut Session, folder: &Folder, parent: Option<u64>, out: &mut serde_json::Map<String, Value>) -> Result<()> {
    let name = folder.name.clone();
    let bin = s.edit("New Bin", |pr, _| Ok(pr.add_bin(&name, parent.map(filmcraft_project::BinId))))?;
    merge_import(out, json!({"bins": [bin.0]}));
    if !folder.files.is_empty() {
        import_into(s, &folder.files, Some(bin.0), out);
    }
    // recursion as deep as the walk went, which is bounded
    for sub in &folder.subs {
        import_folder(s, sub, Some(bin.0), out)?;
    }
    Ok(())
}

/// `mediaBrowser.import {paths?, bin?, imageSequence?}`: imports files. A folder becomes a bin
/// named after it (in `bin`) holding its files, and each sub-folder a bin inside that, as in
/// Premiere; the result then also lists the new `bins`. Import as Image Sequence takes the first
/// file instead.
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let paths = paths_p(s, p);
    if paths.is_empty() {
        return Err(bad("mediaBrowser.import", "select files in the Media Browser"));
    }
    let seq = bool_p(p, "imageSequence").unwrap_or(s.prefs.media_browser.import_as_image_sequence);
    let bin = u64_p(p, "bin");
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut empty = Vec::new();
    let mut budget = FOLDER_IMPORT_MAX_FOLDERS;
    for path in paths {
        let is_dir = matches!(s.services.list_entries(&path), Some(Ok(_))) && kind_of(&path).is_none();
        if !is_dir {
            files.push(path);
        } else if seq {
            // an image sequence: the folder's frames
            match list(&*s.services, &path, "all") {
                Ok(entries) => files.extend(entries.into_iter().filter(|e| !e.is_dir).map(|e| e.path)),
                Err(_) => files.push(path),
            }
        } else {
            match walk_folder(&*s.services, &path, 0, &mut budget) {
                Some(f) => folders.push(f),
                None => empty.push(path),
            }
        }
    }
    // an image sequence imports from its first selected frame only
    if seq {
        files.truncate(1);
    }
    if folders.is_empty() {
        if files.is_empty() && !empty.is_empty() {
            return Err(bad("mediaBrowser.import", format!("no media files in {}", empty.join(", "))));
        }
        let mut q = json!({"paths": files, "imageSequence": seq});
        if let Some(b) = bin {
            q["bin"] = json!(b);
        }
        return s.execute("file.import", q);
    }
    let mut out = serde_json::Map::new();
    merge_import(&mut out, json!({"items": [], "errors": empty.iter().map(|e| format!("{e}: no media files")).collect::<Vec<_>>(), "bins": []}));
    if !files.is_empty() {
        import_into(s, &files, bin, &mut out);
    }
    for f in &folders {
        import_folder(s, f, bin, &mut out)?;
    }
    Ok(Value::Object(out))
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

/// Unix seconds → (proleptic Gregorian year, the same wall-clock instant moved into chrono's range
/// by whole 400-year cycles). The Gregorian calendar repeats every 400 years (146_097 days), so
/// month, day and time of day come from chrono and only the year is carried, which keeps every
/// input exact instead of collapsing out-of-range years to the epoch. `secs` is `u64`, so `days`
/// (≤ `u64::MAX / 86_400`) always fits an `i64`.
fn utc_parts(secs: u64) -> (i64, chrono::NaiveDateTime) {
    use chrono::Datelike;
    let (days, sod) = ((secs / 86_400) as i64, (secs % 86_400) as i64);
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest + 719_163` (1970-01-01 is CE day 719_163) is always a valid CE day, and `sod` < 86_400.
    let date = i32::try_from(rest + 719_163).ok().and_then(chrono::NaiveDate::from_num_days_from_ce_opt);
    let time = u32::try_from(sod).ok().and_then(|s| chrono::NaiveTime::from_num_seconds_from_midnight_opt(s, 0));
    match (date, time) {
        (Some(d), Some(t)) => {
            let y = i128::from(d.year()) + i128::from(cycles) * 400;
            (i64::try_from(y).unwrap_or(i64::MAX), d.and_time(t))
        }
        _ => (1970, chrono::NaiveDateTime::default()), // unreachable
    }
}

/// `YYYY-MM-DD HH:MM` (UTC) from Unix seconds. Years far outside chrono's own range are preserved
/// (the old hand-rolled conversion kept them) rather than collapsing to the epoch.
pub fn format_date(secs: u64) -> String {
    let (y, t) = utc_parts(secs);
    // `{:04}` rather than `%Y`, which writes a `+` before years past 9999.
    format!("{y:04}-{}", t.format("%m-%d %H:%M"))
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

/// How long [`std_volumes`] waits for the drive letters to answer.
const DRIVE_WAIT: std::time::Duration = std::time::Duration::from_millis(400);

/// The drive letters that exist, each looked at on a thread of its own: a mapped network drive
/// that does not answer (`X:` on a share that is gone) takes the OS many seconds, and is listed
/// as pending after [`DRIVE_WAIT`] instead of holding up the others. A letter still being looked
/// at from an earlier scan is not asked again until it answers.
fn windows_drives() -> Vec<Volume> {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static PROBING: OnceLock<Mutex<std::collections::BTreeSet<char>>> = OnceLock::new();
    let probing = PROBING.get_or_init(Default::default);
    let (tx, rx) = std::sync::mpsc::channel::<(char, bool)>();
    let mut asked = Vec::new();
    let mut busy = Vec::new();
    for l in 'A'..='Z' {
        if !probing.lock().unwrap_or_else(PoisonError::into_inner).insert(l) {
            busy.push(l);
            continue;
        }
        let tx = tx.clone();
        let spawned = std::thread::Builder::new().name(format!("filmcraft-drive-{l}")).spawn(move || {
            let there = std::path::Path::new(&format!("{l}:\\")).exists();
            if let Some(p) = PROBING.get() {
                p.lock().unwrap_or_else(PoisonError::into_inner).remove(&l);
            }
            let _ = tx.send((l, there));
        });
        match spawned {
            Ok(_) => asked.push(l),
            Err(_) => {
                probing.lock().unwrap_or_else(PoisonError::into_inner).remove(&l);
            }
        }
    }
    drop(tx);
    let deadline = web_time::Instant::now() + DRIVE_WAIT;
    let mut answered = std::collections::BTreeMap::new();
    while answered.len() < asked.len() {
        let left = deadline.saturating_duration_since(web_time::Instant::now());
        match rx.recv_timeout(left) {
            Ok((l, there)) => {
                answered.insert(l, there);
            }
            Err(_) => break,
        }
    }
    let mut v = Vec::new();
    for l in 'A'..='Z' {
        let pending = busy.contains(&l) || (asked.contains(&l) && !answered.contains_key(&l));
        if pending || answered.get(&l) == Some(&true) {
            v.push(Volume { name: format!("({l}:)"), path: format!("{l}:\\"), kind: VolumeKind::Local, pending });
        }
    }
    v
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
        return windows_drives();
    }
    if cfg!(target_os = "macos") {
        let mut names = dir_names("/Volumes");
        names.sort();
        for n in names {
            v.push(Volume { path: format!("/Volumes/{n}"), name: n, kind: VolumeKind::Local, pending: false });
        }
        if v.is_empty() {
            v.push(Volume { name: "/".into(), path: "/".into(), kind: VolumeKind::Local, pending: false });
        }
        if std::path::Path::new("/Network").is_dir() {
            v.push(Volume { name: "Network".into(), path: "/Network".into(), kind: VolumeKind::Network, pending: false });
        }
        return v;
    }
    v.push(Volume { name: "/".into(), path: "/".into(), kind: VolumeKind::Local, pending: false });
    for base in ["/media", "/mnt", "/run/media"] {
        for n in dir_names(base) {
            let p = format!("{base}/{n}");
            // /media/<user>/<volume>
            let inner = dir_names(&p);
            if base != "/mnt" && !inner.is_empty() {
                for m in inner {
                    v.push(Volume { path: format!("{p}/{m}"), name: m, kind: VolumeKind::Local, pending: false });
                }
            } else {
                v.push(Volume { name: n, path: p, kind: VolumeKind::Local, pending: false });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_shared_out_by_drive_or_share() {
        let vol = |p: &str| Volume { name: String::new(), path: p.into(), kind: VolumeKind::Local, pending: false };
        let vols = [vol("/"), vol("/Volumes/Card"), vol("/Network/Studio"), vol("X:\\")];
        assert_eq!(read_root("/Volumes/Card/DCIM", &vols), "/Volumes/Card");
        assert_eq!(read_root("/Volumes/Cardigan/x", &vols), "/Volumes", "a volume holds only its own folders");
        assert_eq!(read_root("/Network/Studio", &vols), "/Network/Studio");
        assert_eq!(read_root("/home/me/Movies", &vols), "/home", "the system root does not count as one drive");
        assert_eq!(read_root("X:\\Footage\\Day1", &vols), "X:\\");
        assert_eq!(read_root("Y:\\Footage", &[]), "Y:");
        assert_eq!(read_root(r"\\nas\media\2026\day1", &[]), r"nas\media");
        assert_eq!(read_root("/", &[]), "/");
        assert_eq!(read_root("", &[]), "/");
    }

    #[test]
    fn format_date_is_utc_minutes() {
        // Pinned against the previous hand-rolled civil-from-days output.
        assert_eq!(format_date(0), "1970-01-01 00:00");
        assert_eq!(format_date(951_782_400), "2000-02-29 00:00");
        assert_eq!(format_date(1_709_164_800), "2024-02-29 00:00");
        assert_eq!(format_date(1_790_775_907), "2026-09-30 13:45");
        // Years past 9999 keep the plain zero-padded year (no `+`).
        assert_eq!(format_date(253_402_300_800), "10000-01-01 00:00");
        // Values far beyond chrono's own range are preserved (the old hand-rolled civil-from-days
        // kept them); they must not collapse to the epoch (reviewer FredApps).
        assert_eq!(format_date(10_000_000_000_000), "318857-05-20 17:46");
        assert_eq!(format_date(u64::MAX), "584554051223-11-09 07:00");
    }
}

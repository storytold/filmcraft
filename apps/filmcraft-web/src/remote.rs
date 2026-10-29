//! Remote storage in the browser: files on a same-origin WebDAV server appear under mounted
//! virtual paths (`./filmcraft-config.json`, see [`crate::remote_config`]) and are read with
//! `fetch` (PROPFIND listings, `Range` GETs). The pure parts live in [`crate::remote_config`] and
//! [`crate::remote_dav`]; this module is the browser side: config load, listing and stat caches,
//! ranged reads for [`crate::fs`], and opening a project together with the sizes of its media.
//!
//! The engine's `Services` are synchronous, so everything they need is fetched ahead:
//! a directory listing starts on the first look and answers `WouldBlock` until it arrives, and a
//! project's media sizes are fetched before `file.open` runs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;

use filmcraft_engine::media_browser::{DirEntry, Volume, VolumeKind};
use serde_json::{Value, json};

use crate::remote_config::{self, Config};
use crate::remote_dav::{self, Child};
pub(crate) use crate::remote_http::{fetch, fetch_within, header, read_body};
use crate::remote_lookup::{Lookups, Next, Outcome};

/// A failed listing is not retried for this long (seconds).
const ERROR_RETRY_S: f64 = 5.0;
/// Entries kept in the stat cache.
const STAT_MAX: usize = 200_000;
/// Largest config file read.
const CONFIG_MAX: usize = 64 * 1024;

/// What the server said about one path.
#[derive(Clone, Debug)]
pub struct Stat {
    pub size: u64,
    pub is_dir: bool,
    pub modified: Option<u64>,
    pub etag: Option<String>,
}

#[derive(Default)]
struct DirState {
    entries: Option<Vec<DirEntry>>,
    fetched: f64,
    /// Whether the listing was handed out since it was fetched (a TTL of 0 refetches after that).
    shown: bool,
    loading: bool,
    error: Option<(String, f64)>,
}

#[derive(Default)]
struct State {
    cfg: Option<Config>,
    origin: String,
    stats: HashMap<String, Stat>,
    dirs: HashMap<String, DirState>,
    lookups: Lookups,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static T0: web_time::Instant = web_time::Instant::now();
}

/// Seconds since the page started, for cache expiry.
fn now_s() -> f64 {
    T0.with(|t| t.elapsed().as_secs_f64())
}

/// Whether remote storage is configured.
pub fn enabled() -> bool {
    STATE.with(|s| s.borrow().cfg.is_some())
}

/// Whether `path` is on a remote mount.
pub fn is_remote(path: &str) -> bool {
    STATE.with(|s| s.borrow().cfg.as_ref().is_some_and(|c| c.mount_for(path).is_some()))
}

/// Each mount as a Network volume of the Media Browser.
pub fn volumes() -> Vec<Volume> {
    STATE.with(|s| {
        s.borrow().cfg.iter().flat_map(|c| c.mounts.iter()).map(|m| Volume { name: m.name.clone(), path: m.path.clone(), kind: VolumeKind::Network }).collect()
    })
}

/// The cached stat of a path (from a listing or a HEAD).
pub fn stat(path: &str) -> Option<Stat> {
    STATE.with(|s| s.borrow().stats.get(path.strip_suffix('/').unwrap_or(path)).cloned())
}

/// The remote config and the page origin, or why remote storage is unavailable.
pub(crate) fn cfg_and_origin() -> Result<(Config, String), String> {
    STATE.with(|s| {
        let s = s.borrow();
        s.cfg.clone().map(|c| (c, s.origin.clone())).ok_or_else(|| "remote storage is not configured".to_string())
    })
}

/// Chunk size (bytes) and chunks fetched at once for remote files: the configured values, the
/// defaults while remote storage is not configured.
pub(crate) fn transfer() -> (u64, u64) {
    STATE.with(|s| {
        let s = s.borrow();
        let (c, p) = s.cfg.as_ref().map_or((remote_config::DEFAULT_CHUNK_BYTES, remote_config::DEFAULT_PARALLEL), |c| (c.chunk_bytes, c.parallel));
        (c, p as u64)
    })
}

/// Read `./filmcraft-config.json`; remote storage stays off when it is missing or invalid.
pub async fn load_config() {
    let Some(loc) = web_sys::window().map(|w| w.location()) else { return };
    let (origin, path) = (loc.origin().unwrap_or_default(), loc.pathname().unwrap_or_default());
    let base_dir = path.get(..=path.rfind('/').unwrap_or(0)).unwrap_or("/").to_string();
    let bytes = match fetch("GET", "filmcraft-config.json", &[], None).await {
        Ok(r) if r.status() == 200 => read_body(&r, CONFIG_MAX).await,
        Ok(r) => Err(format!("status {}", r.status())),
        Err(e) => Err(e),
    };
    match bytes.and_then(|b| remote_config::parse(&b, &origin, &base_dir)) {
        Ok(cfg) => {
            log::info!("remote storage: {} mount(s)", cfg.mounts.len());
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.cfg = Some(cfg);
                s.origin = origin;
            });
        }
        Err(e) => log::info!("remote storage off: {e}"),
    }
}

/// PROPFIND `dir` (Depth 1) and map the answer to virtual paths; the request is aborted when
/// `deadline` passes.
async fn propfind(dir: &str, deadline: Option<web_time::Instant>) -> Result<Vec<Child>, String> {
    let (cfg, origin) = cfg_and_origin()?;
    let mount = cfg.mount_for(dir).ok_or_else(|| format!("{dir}: not on a remote mount"))?.clone();
    let url = cfg.url_for(&format!("{}/", dir.trim_end_matches('/')))?;
    let hdrs = [("Depth", "1"), ("Content-Type", "application/xml")];
    let r = fetch_within("PROPFIND", &url, &hdrs, Some(remote_dav::PROPFIND_BODY.as_bytes()), deadline).await?;
    if r.status() != 207 {
        return Err(format!("{dir}: the server answered {} to a listing", r.status()));
    }
    let entries = remote_dav::parse_multistatus(&read_body(&r, remote_dav::MAX_XML_BYTES).await?)?;
    Ok(remote_dav::children_of(&cfg, &mount, dir, &origin, entries))
}

/// Store the outcome of a folder listing in the cache and wake what waited for it.
fn record_dir(dir: &str, res: Result<Vec<Child>, String>) {
    let now = now_s();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.dirs.entry(dir.to_string()).or_default();
        st.loading = false;
        match res {
            Ok(children) => {
                st.error = None;
                st.fetched = now;
                st.shown = false;
                st.entries = Some(
                    children
                        .iter()
                        .map(|c| DirEntry { name: c.name.clone(), is_dir: c.entry.is_dir, size: c.entry.size, modified: c.entry.modified })
                        .collect(),
                );
                if s.stats.len() > STAT_MAX {
                    s.stats.clear();
                }
                for c in children {
                    let stat = Stat { size: c.entry.size.unwrap_or(0), is_dir: c.entry.is_dir, modified: c.entry.modified, etag: c.entry.etag };
                    s.stats.insert(c.path, stat);
                }
            }
            Err(e) => st.error = Some((e, now)),
        }
    });
}

/// Start the listing of `dir` unless one is already running.
fn start_listing(dir: String) {
    wasm_bindgen_futures::spawn_local(async move {
        let res = propfind(&dir, None).await;
        record_dir(&dir, res);
        crate::repaint();
    });
}

/// The Media Browser's listing of a directory under a mount; `None` for other paths. A listing
/// that is not here yet starts and answers `WouldBlock` ("Loading"); the browser retries on the
/// repaint that follows. An expired one is shown while it refreshes.
pub fn list(dir: &str) -> Option<io::Result<Vec<DirEntry>>> {
    let dir = dir.trim_end_matches('/').to_string();
    if !is_remote(&dir) {
        return None;
    }
    let now = now_s();
    let (result, start) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let ttl = s.cfg.as_ref().map_or(remote_config::DEFAULT_LIST_CACHE_S, |c| c.list_cache_s);
        let st = s.dirs.entry(dir.clone()).or_default();
        // A listing that was not shown yet is always fresh, so a TTL of 0 does not refetch in a loop.
        let fresh = st.entries.is_some() && (!st.shown || now - st.fetched < ttl);
        let blocked = st.loading || st.error.as_ref().is_some_and(|(_, at)| now - at < ERROR_RETRY_S);
        let start = !fresh && !blocked;
        if start {
            st.loading = true;
        }
        st.shown |= st.entries.is_some();
        let result = match (&st.entries, &st.error) {
            (Some(e), _) => Ok(e.clone()),
            // the error stays until a retry succeeds: no flicker between it and "Loading"
            (None, Some((msg, _))) => Err(io::Error::other(msg.clone())),
            _ => Err(io::Error::new(io::ErrorKind::WouldBlock, "Loading…")),
        };
        (result, start)
    });
    if start {
        start_listing(dir);
    }
    Some(result)
}

/// Why a stat could not be had.
enum StatError {
    /// The server has no such file.
    Absent(String),
    Other(String),
}

/// A file's size: cached, else from its parent's listing, else `HEAD`. A 404 is remembered, so a
/// file that is not there reads as missing and is not asked about again at once.
pub async fn ensure_stat(path: &str) -> Result<Stat, String> {
    ensure_stat_until(path, None).await
}

/// [`ensure_stat`] whose requests (listing, `HEAD`) are aborted when `deadline` passes, and that
/// stops waiting for another lookup's listing then.
pub async fn ensure_stat_until(path: &str, deadline: Option<web_time::Instant>) -> Result<Stat, String> {
    let r = fetch_stat(path, deadline).await;
    let outcome = match &r {
        Ok(_) => Outcome::Found,
        Err(StatError::Absent(_)) => Outcome::Absent,
        Err(StatError::Other(_)) => Outcome::Failed,
    };
    let now = now_s();
    STATE.with(|s| s.borrow_mut().lookups.done(path, outcome, now));
    r.map_err(|e| match e {
        StatError::Absent(m) | StatError::Other(m) => m,
    })
}

/// Whether `dir` has a cached listing still inside the cache time: asking the server again for it
/// would only repeat what is known (the file is then looked up with a `HEAD`).
fn listing_fresh(dir: &str) -> bool {
    let now = now_s();
    STATE.with(|s| {
        let s = s.borrow();
        let ttl = s.cfg.as_ref().map_or(remote_config::DEFAULT_LIST_CACHE_S, |c| c.list_cache_s);
        s.dirs.get(dir).is_some_and(|d| d.entries.is_some() && now - d.fetched < ttl)
    })
}

/// Size and kind of `path`: from the cache, else the parent folder's listing, else a `HEAD`;
/// every request is bound by `deadline`.
async fn fetch_stat(path: &str, deadline: Option<web_time::Instant>) -> Result<Stat, StatError> {
    let other = StatError::Other;
    if let Some(s) = stat(path) {
        return Ok(s);
    }
    if let Some(parent) = remote_config::parent(path) {
        // one listing per folder: a lookup that finds it running waits for it
        for _ in 0..500 {
            if !STATE.with(|s| s.borrow().dirs.get(parent).is_some_and(|d| d.loading)) || deadline.is_some_and(|d| web_time::Instant::now() >= d) {
                break;
            }
            crate::fs::sleep(10).await;
        }
        if stat(path).is_none() && !listing_fresh(parent) {
            STATE.with(|s| s.borrow_mut().dirs.entry(parent.to_string()).or_default().loading = true);
            let res = propfind(parent, deadline).await;
            if let Err(e) = &res {
                log::info!("listing {parent}: {e}");
            }
            record_dir(parent, res);
        }
        if let Some(s) = stat(path) {
            return Ok(s);
        }
    }
    let (cfg, _) = cfg_and_origin().map_err(other)?;
    let url = cfg.url_for(path).map_err(StatError::Other)?;
    let r = fetch_within("HEAD", &url, &[], None, deadline).await.map_err(StatError::Other)?;
    match r.status() {
        200 => {}
        404 | 410 => return Err(StatError::Absent(format!("{path}: not found on the server"))),
        n => return Err(StatError::Other(format!("{path}: the server answered {n}"))),
    }
    let size = header(&r, "content-length")
        .and_then(|h| remote_dav::parse_content_length(&h))
        .ok_or_else(|| StatError::Other(format!("{path}: the server gave no size")))?;
    let s = Stat { size, is_dir: false, modified: None, etag: header(&r, "etag") };
    STATE.with(|st| st.borrow_mut().stats.insert(path.to_string(), s.clone()));
    Ok(s)
}

/// What a synchronous look at a remote file found.
pub enum Lookup {
    Found(Stat),
    /// Being fetched: the caller answers "loading" and is asked again after the repaint.
    Loading,
    Missing,
}

/// The stat of a file on a remote mount for the synchronous `Services`; `None` for other paths.
/// A path the cache does not know starts one deduplicated fetch (listing of its folder, else
/// `HEAD`) and repaints when it ends; a path the server does not have is `Missing`, not `Loading`.
pub fn lookup(path: &str) -> Option<Lookup> {
    if !is_remote(path) {
        return None;
    }
    if let Some(s) = stat(path) {
        return Some(Lookup::Found(s));
    }
    let now = now_s();
    let next = STATE.with(|s| s.borrow_mut().lookups.miss(path, now));
    Some(match next {
        Next::Missing => Lookup::Missing,
        Next::Wait => Lookup::Loading,
        Next::Fetch => {
            let path = path.to_string();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = ensure_stat(&path).await {
                    log::info!("remote file {path}: {e}");
                }
                crate::repaint();
            });
            Lookup::Loading
        }
    })
}

/// Bytes `[start, start + len)` of a file of known `size`, as a validated `206` answer.
pub async fn fetch_range(path: &str, start: u64, len: u64, size: u64) -> Result<Vec<u8>, String> {
    let (cfg, _) = cfg_and_origin()?;
    let last = start.checked_add(len).and_then(|e| e.checked_sub(1)).ok_or("range overflows")?;
    let r = fetch("GET", &cfg.url_for(path)?, &[("Range", &format!("bytes={start}-{last}"))], None).await?;
    let expected = remote_dav::check_range_response(r.status(), header(&r, "content-range").as_deref(), start, len, size)?;
    if header(&r, "content-length").and_then(|h| remote_dav::parse_content_length(&h)).is_some_and(|n| n != expected) {
        return Err("Content-Length does not match the range".into());
    }
    let body = read_body(&r, usize::try_from(expected).map_err(|_| "range too large")?).await?;
    if body.len() as u64 != expected {
        return Err("short range body".into());
    }
    Ok(body)
}

/// Drop what is cached about `path` and its folder (after it was written): the next look asks again.
pub fn forget(path: &str) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.stats.remove(path);
        s.lookups.forget(path);
        if let Some(p) = remote_config::parent(path) {
            s.dirs.remove(p);
        }
    });
}

/// Open the project at a remote path: fetch it, fetch the sizes of its media, then `file.open`.
pub async fn open_project(path: &str) -> Result<Value, String> {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    crate::post(move |w, _| w.app.status(format!("Opening {name}…")));
    let st = ensure_stat(path).await?;
    if st.is_dir || st.size > remote_dav::MAX_PROJECT_BYTES {
        return Err(format!("{path}: not a project file this size can open"));
    }
    let (cfg, _) = cfg_and_origin()?;
    let r = fetch("GET", &cfg.url_for(path)?, &[], None).await?;
    if r.status() != 200 {
        return Err(format!("{path}: the server answered {}", r.status()));
    }
    let etag = header(&r, "etag");
    let bytes = read_body(&r, remote_dav::MAX_PROJECT_BYTES as usize).await?;
    crate::remote_import::prefetch_media(&bytes).await;
    crate::fs::put(path, bytes);
    let path = path.to_string();
    let opened_path = path.clone();
    let opened = crate::import::on_ui(move |w, _| w.app.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())).await?;
    // only a project that opened is tracked: a failed open must not arm later saves with this ETag
    crate::remote_upload::note_opened(&opened_path, etag);
    Ok(opened)
}

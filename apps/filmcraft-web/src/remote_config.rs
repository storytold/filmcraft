//! Remote storage configuration for the web build: which virtual paths are served by a WebDAV
//! server on the page's own origin, and how a virtual path maps to a request URL and back.
//!
//! The page fetches `./filmcraft-config.json` at start-up:
//!
//! ```json
//! {"remote":{"mounts":[{"path":"/data","url":"dav/","name":"Server","writable":false}],"listCacheSeconds":15}}
//! ```
//!
//! `listCacheSeconds` is optional (default 15, clamped to 0..=3600). `chunkBytes` (default 4 MiB,
//! 256 KiB..=16 MiB) is the size of one `Range` read of remote media and `parallel` (default 12,
//! 1..=32) how many chunks a sequential scan fetches at once.
//!
//! Pure (no browser types), so it runs natively in `tests/it`. Everything that comes from the
//! config file, from a virtual path or from a server `href` is untrusted: paths with `..`, empty
//! segments, backslashes or control characters never map to a URL, and an `href` that does not
//! lie under the mount's URL never maps to a path.

use serde_json::Value;

use crate::remote_roots::RESERVED_ROOTS;

/// Mounts accepted from one config file.
pub const MAX_MOUNTS: usize = 16;
/// Longest virtual path or URL path accepted (bytes).
pub const MAX_PATH: usize = 4096;
/// Most segments in one virtual path.
pub const MAX_SEGMENTS: usize = 128;
/// Longest display name of a mount (bytes).
const MAX_NAME: usize = 64;
/// Seconds a directory listing is reused before it is fetched again, unless the config says otherwise.
pub const DEFAULT_LIST_CACHE_S: f64 = 15.0;
/// Longest `remote.listCacheSeconds` accepted.
pub const MAX_LIST_CACHE_S: f64 = 3600.0;

/// Size of one `Range` read of remote media unless the config says otherwise (bytes).
pub const DEFAULT_CHUNK_BYTES: u64 = 4 << 20;
/// Smallest and largest `remote.chunkBytes` accepted.
pub const MIN_CHUNK_BYTES: u64 = 256 << 10;
pub const MAX_CHUNK_BYTES: u64 = 16 << 20;
/// Chunks a sequential scan of a remote file fetches at once unless the config says otherwise.
pub const DEFAULT_PARALLEL: usize = 12;
/// Smallest and largest `remote.parallel` accepted.
pub const MIN_PARALLEL: usize = 1;
pub const MAX_PARALLEL: usize = 32;

/// One server directory served under a virtual path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// Absolute virtual path without trailing slash (`/data`).
    pub path: String,
    /// Absolute URL path of the WebDAV collection, same origin, trailing slash (`/app/dav/`).
    pub url_path: String,
    /// Display name in the Media Browser.
    pub name: String,
    /// Whether the app may save to this mount; a read-only mount refuses every write before a request is made.
    pub writable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub mounts: Vec<Mount>,
    /// Seconds a directory listing is reused (`remote.listCacheSeconds`, 0 to 3600; 0 fetches it
    /// again each time the folder is refreshed).
    pub list_cache_s: f64,
    /// Bytes of one chunk of a remote file (`remote.chunkBytes`, 256 KiB to 16 MiB).
    pub chunk_bytes: u64,
    /// Chunks of a remote file fetched at once while it is scanned (`remote.parallel`, 1 to 32).
    pub parallel: usize,
}

impl Default for Config {
    /// The defaults: no mounts, the standard list cache time.
    fn default() -> Self {
        Self { mounts: Vec::new(), list_cache_s: DEFAULT_LIST_CACHE_S, chunk_bytes: DEFAULT_CHUNK_BYTES, parallel: DEFAULT_PARALLEL }
    }
}

/// A segment (or whole path) that cannot name a file safely.
fn bad_segment(s: &str) -> bool {
    s.is_empty() || s == "." || s == ".." || s.chars().any(|c| c.is_control() || c == '\\' || c == '/')
}

/// `/a/b/` or `/a/b` → `/a/b`; `None` for the root, relative paths and anything with a bad segment.
fn normalize_virtual(path: &str) -> Option<String> {
    if path.len() > MAX_PATH {
        return None;
    }
    let rest = path.strip_prefix('/')?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let segs: Vec<&str> = rest.split('/').collect();
    if segs.len() > MAX_SEGMENTS || segs.iter().any(|s| bad_segment(s)) {
        return None;
    }
    Some(format!("/{rest}"))
}

/// Percent-encode one path segment (everything except RFC 3986 unreserved characters).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(b"0123456789ABCDEF"[usize::from(b >> 4)]));
            out.push(char::from(b"0123456789ABCDEF"[usize::from(b & 15)]));
        }
    }
    out
}

/// Value of one ASCII hex digit.
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode; `None` for a malformed escape or bytes that are not UTF-8.
pub fn decode_segment(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hi = hex(*b.get(i.checked_add(1)?)?)?;
            let lo = hex(*b.get(i.checked_add(2)?)?)?;
            out.push(hi << 4 | lo);
            i = i.checked_add(3)?;
        } else {
            out.push(c);
            i = i.checked_add(1)?;
        }
    }
    String::from_utf8(out).ok()
}

/// Strip `origin` (`https://host:port`, compared case-insensitively) from an absolute URL, leaving
/// its path; a path stays as it is. `None` for any other origin, `//host/...` and non-HTTP schemes.
fn same_origin_path<'a>(url: &'a str, origin: &str) -> Option<&'a str> {
    if url.starts_with("//") {
        return None;
    }
    if url.starts_with('/') {
        return Some(url);
    }
    let head = url.get(..origin.len())?;
    if origin.is_empty() || !head.eq_ignore_ascii_case(origin) {
        return None;
    }
    let rest = url.get(origin.len()..)?;
    if rest.is_empty() { Some("/") } else { rest.starts_with('/').then_some(rest) }
}

/// Resolve a mount `url` to an absolute, normalised URL path ending in `/`. Relative URLs resolve
/// against `base_dir` (the page's directory, `/app/`). Other origins, query strings, fragments and
/// paths climbing above the root are refused.
fn resolve_url_path(url: &str, origin: &str, base_dir: &str) -> Option<String> {
    if url.is_empty() || url.len() > MAX_PATH || url.chars().any(|c| c.is_control() || c == '\\' || c == '?' || c == '#') {
        return None;
    }
    let absolute = if url.starts_with('/') || url.contains("://") {
        same_origin_path(url, origin)?.to_string()
    } else {
        // a relative reference with a scheme (`javascript:`, `data:`) is not a path
        if url.split('/').next().is_some_and(|first| first.contains(':')) {
            return None;
        }
        format!("{}{url}", if base_dir.ends_with('/') { base_dir.to_string() } else { format!("{base_dir}/") })
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in absolute.split('/').skip(1) {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            s => stack.push(s),
        }
    }
    if stack.len() > MAX_SEGMENTS {
        return None;
    }
    let mut out = String::from("/");
    for s in stack {
        out.push_str(s);
        out.push('/');
    }
    Some(out)
}

/// Parse `filmcraft-config.json`. `origin` is the page's origin (`https://host`), `base_dir` the
/// directory of the page's path (`/app/`). `Err` when the file is invalid or configures no mounts.
pub fn parse(bytes: &[u8], origin: &str, base_dir: &str) -> Result<Config, String> {
    if bytes.len() > 64 * 1024 {
        return Err("config too large".into());
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON: {e}"))?;
    let mounts = v.get("remote").and_then(|r| r.get("mounts")).and_then(Value::as_array).ok_or("no remote.mounts")?;
    if mounts.len() > MAX_MOUNTS {
        return Err("too many mounts".into());
    }
    let mut cfg = Config::default();
    // An optional number; any other type keeps the default, a number outside the range is clamped.
    if let Some(n) = v.get("remote").and_then(|r| r.get("listCacheSeconds")).and_then(Value::as_f64) {
        cfg.list_cache_s = n.clamp(0.0, MAX_LIST_CACHE_S);
    }
    // Optional numbers like the one above: another type keeps the default, a number outside the
    // range is clamped (and a fraction rounded down).
    if let Some(n) = v.get("remote").and_then(|r| r.get("chunkBytes")).and_then(Value::as_f64) {
        cfg.chunk_bytes = (n.clamp(MIN_CHUNK_BYTES as f64, MAX_CHUNK_BYTES as f64) as u64).clamp(MIN_CHUNK_BYTES, MAX_CHUNK_BYTES);
    }
    if let Some(n) = v.get("remote").and_then(|r| r.get("parallel")).and_then(Value::as_f64) {
        cfg.parallel = (n.clamp(MIN_PARALLEL as f64, MAX_PARALLEL as f64) as usize).clamp(MIN_PARALLEL, MAX_PARALLEL);
    }
    for m in mounts {
        let path = m
            .get("path")
            .and_then(Value::as_str)
            .and_then(normalize_virtual)
            .ok_or("mount.path must be an absolute path with no empty, `.` or `..` segments")?;
        if path.split('/').nth(1).is_some_and(|first| RESERVED_ROOTS.contains(&first)) {
            return Err(format!("mount path {path} shadows a built-in folder"));
        }
        if cfg.mounts.iter().any(|o| o.path == path || o.path.starts_with(&format!("{path}/")) || path.starts_with(&format!("{}/", o.path))) {
            return Err(format!("mount path {path} overlaps another mount"));
        }
        let url = m.get("url").and_then(Value::as_str).ok_or("mount.url missing")?;
        let url_path = resolve_url_path(url, origin, base_dir).ok_or_else(|| format!("mount url `{url}` must be on the page's own origin"))?;
        let default_name = path.rsplit('/').next().unwrap_or("Remote").to_string();
        let name = match m.get("name").and_then(Value::as_str) {
            Some(n) if !n.is_empty() && n.len() <= MAX_NAME && !n.chars().any(char::is_control) => n.to_string(),
            Some(_) => return Err("mount.name is empty, too long or has control characters".into()),
            None => default_name,
        };
        cfg.mounts.push(Mount { path, url_path, name, writable: m.get("writable").and_then(Value::as_bool).unwrap_or(false) });
    }
    if cfg.mounts.is_empty() {
        return Err("no mounts".into());
    }
    Ok(cfg)
}

impl Config {
    /// The mount serving `path`: the longest one equal to it or a parent of it.
    pub fn mount_for(&self, path: &str) -> Option<&Mount> {
        let p = path.strip_suffix('/').unwrap_or(path);
        self.mounts.iter().filter(|m| p == m.path || p.strip_prefix(&m.path).is_some_and(|r| r.starts_with('/'))).max_by_key(|m| m.path.len())
    }

    /// Request URL path for a virtual path, each segment percent-encoded. A directory keeps its
    /// trailing slash. `Err` for paths outside every mount or with a hostile segment.
    pub fn url_for(&self, path: &str) -> Result<String, String> {
        let m = self.mount_for(path).ok_or_else(|| format!("{path}: not on a remote mount"))?;
        let dir = path.ends_with('/');
        let norm = normalize_virtual(path).ok_or_else(|| format!("{path}: invalid path"))?;
        let rest = norm.strip_prefix(&m.path).unwrap_or("").trim_start_matches('/');
        let mut out = m.url_path.clone();
        if !rest.is_empty() {
            out.push_str(&rest.split('/').map(encode_segment).collect::<Vec<_>>().join("/"));
            if dir {
                out.push('/');
            }
        }
        Ok(out)
    }

    /// Virtual path for an `href` the server returned for `mount`: an absolute URL on `origin` or a
    /// URL path under the mount's URL. `None` for anything else (another origin, outside the
    /// mount, `..`, empty segments, or an encoded slash). The result has no trailing slash.
    pub fn path_for_href(&self, mount: &Mount, href: &str, origin: &str) -> Option<String> {
        if href.len() > MAX_PATH {
            return None;
        }
        let path = same_origin_path(href.trim(), origin)?;
        let path = path.split(['?', '#']).next()?;
        // Decode per segment: an encoded slash stays inside its segment, where it is refused.
        let decode_all = |p: &str| -> Option<Vec<String>> { p.split('/').map(decode_segment).collect() };
        let mut segs = decode_all(path)?;
        let mut base = decode_all(&mount.url_path)?;
        if segs.last().is_some_and(String::is_empty) {
            segs.pop();
        }
        base.pop();
        let rest = segs.strip_prefix(base.as_slice())?;
        if rest.len() > MAX_SEGMENTS || rest.iter().any(|s| bad_segment(s)) {
            return None;
        }
        let mut out = mount.path.clone();
        for s in rest {
            out.push('/');
            out.push_str(s);
        }
        Some(out)
    }
}

/// The parent of a virtual path (`/data/a/b.mp4` → `/data/a`); `None` for the root.
pub fn parent(path: &str) -> Option<&str> {
    let p = path.strip_suffix('/').unwrap_or(path);
    p.rfind('/').filter(|i| *i > 0).and_then(|i| p.get(..i))
}

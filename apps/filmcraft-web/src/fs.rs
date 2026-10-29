//! Browser-backed host services: a virtual file table and the engine's [`Services`].
//!
//! - Media the user opens (`<input type=file>`, drag-and-drop, File System Access pickers, files
//!   kept in OPFS) are registered as `File`/`Blob` entries under a virtual path (`/files/<name>`).
//!   They are never copied whole: [`BlobReader`] serves the demuxers' range reads from a cache of
//!   chunks (1 MiB for Blobs, `remote.chunkBytes` for remote files; LRU, [`CACHE_BUDGET`]), fetched asynchronously with `Blob.slice().arrayBuffer()`.
//!   A read whose chunks are not cached yet starts the fetch (plus read-ahead) and fails with
//!   `WouldBlock`, marking [`filmcraft_media::pending`]: the frame server and imports retry once
//!   the bytes have arrived (the repaint hook wakes the UI).
//! - Files the app writes (saved projects, exports, captions) are kept in memory under their
//!   path and offered as a browser download; paths under `/opfs/` are written to the Origin
//!   Private File System instead (auto-save / crash recovery).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::rc::Rc;
use std::sync::Arc;

use filmcraft_engine::Services;
use filmcraft_media::reader::{ByteReader, MemReader, SharedReader};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// Chunk size of Blob range reads.
pub const CHUNK: u64 = 1 << 20;
/// Fetches of a Blob kept in flight while its container index is prefetched.
const BLOB_PARALLEL: u64 = 8;
/// Bytes of cached chunks kept (least recently used evicted).
pub const CACHE_BUDGET: usize = 384 << 20;
/// Largest Blob `read_file` will load whole (captions, interchange documents, projects).
const WHOLE_FILE_MAX: u64 = 256 << 20;

enum Entry {
    Blob {
        id: u32,
        size: u64,
    },
    /// A file on a remote mount ([`crate::remote`]): read in chunks with `Range` requests.
    Remote {
        id: u32,
        size: u64,
    },
    Mem(Arc<[u8]>),
}

#[derive(Default)]
struct Fs {
    entries: HashMap<String, Entry>,
    blobs: HashMap<u32, web_sys::Blob>,
    /// Remote files by chunk-cache id: (virtual path, size).
    remote: HashMap<u32, (String, u64)>,
    /// Chunk size of remote files by chunk-cache id (a Blob has [`CHUNK`]).
    chunk_of: HashMap<u32, u64>,
    /// Misses of each file, to tell a sequential scan from random reads.
    scans: HashMap<u32, crate::readahead::Scan>,
    /// The files of the active sequence, fetched with priority while a project loads.
    focus: crate::focus::Focus,
    /// Resolvers of [`next_data`] waits, called when a chunk arrives or fails.
    waiters: Vec<js_sys::Function>,
    next_id: u32,
    chunks: HashMap<(u32, u64), (Arc<[u8]>, u64)>,
    bytes: usize,
    clock: u64,
    inflight: HashSet<(u32, u64)>,
    failed: HashMap<(u32, u64), String>,
    /// Called when fetched bytes arrive (repaint the UI so pending frames are retried).
    on_data: Option<Rc<dyn Fn()>>,
    /// Called after a file was written (path, bytes): the app offers it as a download.
    on_write: Option<Rc<dyn Fn(&str, &[u8])>>,
}

thread_local! {
    static FS: RefCell<Fs> = RefCell::new(Fs::default());
    static DOWNLOADS: RefCell<crate::download_pacing::Pacer> = RefCell::new(crate::download_pacing::Pacer::default());
}

/// Wake-up hook for arriving data.
pub fn set_on_data(f: impl Fn() + 'static) {
    FS.with(|fs| fs.borrow_mut().on_data = Some(Rc::new(f)));
}

/// Hook for written files (downloads).
pub fn set_on_write(f: impl Fn(&str, &[u8]) + 'static) {
    FS.with(|fs| fs.borrow_mut().on_write = Some(Rc::new(f)));
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Register a browser `File` (or named `Blob`) at an unused virtual path; collisions get numbered names.
/// Name + size do not identify content: replacing an occupied path changes every project item referencing it.
pub fn register_blob(name: &str, blob: web_sys::Blob) -> String {
    let size = blob.size() as u64;
    let clean: String = name.chars().map(|c| if c == '/' || c == '\\' { '_' } else { c }).collect();
    FS.with(|fs| {
        let mut fs = fs.borrow_mut();
        let (stem, ext) = match clean.rfind('.') {
            Some(i) if i > 0 => (clean[..i].to_string(), clean[i..].to_string()),
            _ => (clean.clone(), String::new()),
        };
        let mut n = 1;
        let path = loop {
            let p = if n == 1 { format!("/files/{clean}") } else { format!("/files/{stem} ({n}){ext}") };
            if !fs.entries.contains_key(&p) {
                break p;
            }
            n += 1;
        };
        let id = fs.next_id;
        fs.next_id += 1;
        fs.blobs.insert(id, blob);
        fs.entries.insert(path.clone(), Entry::Blob { id, size });
        fs.recount();
        path
    })
}

/// The chunk-cache id of the remote file at `path`; a new one when its size changed.
fn register_remote(path: &str, size: u64, chunk: u64) -> u32 {
    FS.with(|fs| {
        let mut fs = fs.borrow_mut();
        if let Some(Entry::Remote { id, size: s }) = fs.entries.get(path)
            && *s == size
        {
            return *id;
        }
        let id = fs.next_id;
        fs.next_id += 1;
        fs.remote.insert(id, (path.to_string(), size));
        fs.chunk_of.insert(id, chunk);
        fs.entries.insert(path.to_string(), Entry::Remote { id, size });
        id
    })
}

/// Make `files` (the active sequence's media) the focus; `loading` of them are being warmed up
/// ([`focus_done`] ends that). Other files fetch only a little while a focus file loads.
pub fn set_focus(files: &[String], loading: &[String]) {
    FS.with(|fs| fs.borrow_mut().focus.set(files, loading));
}

/// A focus file finished warming up.
pub fn focus_done(path: &str) {
    FS.with(|fs| fs.borrow_mut().focus.finish(path));
}

/// How the cache treats file `id` now: see [`crate::focus`]. A Blob is never throttled.
fn class_of(fs: &Fs, id: u32) -> crate::focus::Class {
    let Some((path, _)) = fs.remote.get(&id) else { return crate::focus::Class::Normal };
    let busy = fs.inflight.iter().any(|(i, _)| fs.remote.get(i).is_some_and(|(p, _)| fs.focus.contains(p)));
    fs.focus.class(path, busy)
}

/// Put bytes at a path (no download).
pub fn put(path: &str, data: Vec<u8>) {
    FS.with(|fs| fs.borrow_mut().entries.insert(path.to_string(), Entry::Mem(data.into())));
}

/// (path, size, kind) of every file.
pub fn list() -> Vec<(String, u64, &'static str)> {
    FS.with(|fs| {
        let fs = fs.borrow();
        let mut v: Vec<_> = fs
            .entries
            .iter()
            .map(|(p, e)| match e {
                Entry::Blob { size, .. } => (p.clone(), *size, "blob"),
                Entry::Remote { size, .. } => (p.clone(), *size, "remote"),
                Entry::Mem(b) => (p.clone(), b.len() as u64, "memory"),
            })
            .collect();
        v.sort();
        v
    })
}

/// The browser `Blob` behind a path (in-memory files are wrapped in a new Blob).
pub fn blob(path: &str) -> Option<web_sys::Blob> {
    FS.with(|fs| {
        let fs = fs.borrow();
        match fs.entries.get(path)? {
            Entry::Blob { id, .. } => fs.blobs.get(id).cloned(),
            Entry::Remote { .. } => None,
            Entry::Mem(b) => bytes_to_blob(b, "application/octet-stream").ok(),
        }
    })
}

pub fn bytes_to_blob(b: &[u8], mime: &str) -> Result<web_sys::Blob, wasm_bindgen::JsValue> {
    let arr = js_sys::Array::new();
    arr.push(&js_sys::Uint8Array::from(b));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    web_sys::Blob::new_with_u8_array_sequence_and_options(&arr, &opts)
}

/// Fetches still running.
pub fn inflight() -> usize {
    FS.with(|fs| fs.borrow().inflight.len())
}

/// Wait until no fetch is running.
pub async fn idle() {
    while inflight() > 0 {
        sleep(4).await;
    }
}

/// Resolve when a chunk next arrives (or fails), or after `ms` milliseconds.
pub async fn next_data(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        FS.with(|fs| fs.borrow_mut().waiters.push(resolve.clone()));
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// Resolve after `ms` milliseconds.
pub async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// Load `[a, b)` of a registered Blob or remote file into the chunk cache (import prewarm: container index).
pub async fn prefetch(path: &str, a: u64, b: u64) {
    let Some((id, size, chunk, limit)) = FS.with(|fs| {
        let fs = fs.borrow();
        match fs.entries.get(path) {
            Some(Entry::Blob { id, size }) => Some((*id, *size, CHUNK, BLOB_PARALLEL)),
            // a remote file: `remote.parallel` reads at once
            Some(Entry::Remote { id, size }) => Some((*id, *size, fs.chunk_of.get(id).copied().unwrap_or(CHUNK), crate::remote::transfer().1)),
            _ => None,
        }
    }) else {
        return;
    };
    let (c0, c1) = (a / chunk, b.min(size).div_ceil(chunk));
    for c in c0..c1 {
        // a throttled request (another file while a focus file loads) is retried
        while !request(id, c) {
            sleep(4).await;
        }
        // keep a bounded number of reads in flight
        while inflight() as u64 >= FS.with(|fs| class_of(&fs.borrow(), id)).parallel(limit) {
            sleep(2).await;
        }
    }
    // Wait for these chunks only: read-ahead of other reads (or other files) must not hold a
    // header hop back. A chunk that is neither cached nor on its way (failed, evicted) ends the wait.
    loop {
        let done = FS.with(|fs| {
            let fs = fs.borrow();
            (c0..c1).all(|c| fs.chunks.contains_key(&(id, c)) || !fs.inflight.contains(&(id, c)))
        });
        if done {
            return;
        }
        sleep(4).await;
    }
}

/// The bytes `[offset, offset + len)` of a registered file when every chunk covering them is
/// already cached, else `None`. Unlike a [`BlobReader`] read this starts no fetch and no
/// read-ahead, so container walks (atom headers) cost exactly the chunks they prefetched.
pub fn cached_range(path: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
    FS.with(|fs| {
        let fs = fs.borrow();
        let (id, size, chunk) = match fs.entries.get(path)? {
            Entry::Blob { id, size } => (*id, *size, CHUNK),
            Entry::Remote { id, size } => (*id, *size, fs.chunk_of.get(id).copied().unwrap_or(CHUNK)),
            Entry::Mem(b) => return b.get(usize::try_from(offset).ok()?..).map(|t| t[..len.min(t.len())].to_vec()),
        };
        let end = offset.checked_add(len as u64)?.min(size);
        let mut out = Vec::with_capacity(len.min(1 << 20));
        let mut pos = offset;
        while pos < end {
            let (d, _) = fs.chunks.get(&(id, pos / chunk))?;
            let a = usize::try_from(pos - pos / chunk * chunk).ok()?;
            let n = usize::try_from(end - pos).ok()?.min(d.len().checked_sub(a)?);
            if n == 0 {
                return None;
            }
            out.extend_from_slice(&d[a..a + n]);
            pos += n as u64;
        }
        Some(out)
    })
}

impl Fs {
    fn recount(&mut self) {
        self.bytes = self.chunks.values().map(|(d, _)| d.len()).sum();
    }

    fn evict(&mut self) {
        if self.bytes <= CACHE_BUDGET {
            return;
        }
        let mut all: Vec<((u32, u64), u64, usize)> = self.chunks.iter().map(|(k, (d, t))| (*k, *t, d.len())).collect();
        all.sort_unstable_by_key(|x| x.1);
        for (k, _, n) in all {
            if self.bytes <= CACHE_BUDGET * 9 / 10 {
                break;
            }
            self.chunks.remove(&k);
            self.bytes -= n;
        }
    }
}

/// Where a chunk comes from.
enum Source {
    Blob(web_sys::Blob),
    Remote(String, u64),
}

/// Start fetching chunk `c` of file `id` (a Blob or a remote file) unless it is cached or already
/// on its way. `false` when it was not started because other files may fetch only a little while a
/// focus file loads: the caller asks again later.
fn request(id: u32, c: u64) -> bool {
    let mut throttled = false;
    let source = FS.with(|fs| {
        let mut fs = fs.borrow_mut();
        if fs.chunks.contains_key(&(id, c)) || fs.inflight.contains(&(id, c)) {
            return None;
        }
        let others = fs.inflight.iter().filter(|(i, _)| *i != id && fs.remote.get(i).is_some_and(|(p, _)| !fs.focus.contains(p))).count();
        if !class_of(&fs, id).may_start(others) {
            throttled = true;
            return None;
        }
        let source = match (fs.blobs.get(&id), fs.remote.get(&id)) {
            (Some(b), _) => (Source::Blob(b.clone()), CHUNK),
            (None, Some((p, s))) => (Source::Remote(p.clone(), *s), fs.chunk_of.get(&id).copied().unwrap_or(CHUNK)),
            (None, None) => return None,
        };
        fs.failed.remove(&(id, c));
        fs.inflight.insert((id, c));
        Some(source)
    });
    let Some((source, chunk)) = source else { return !throttled };
    let size = match &source {
        Source::Blob(b) => b.size() as u64,
        Source::Remote(_, s) => *s,
    };
    let (a, b) = (c * chunk, ((c + 1) * chunk).min(size));
    wasm_bindgen_futures::spawn_local(async move {
        let r: Result<Vec<u8>, String> = match source {
            Source::Blob(blob) => match blob.slice_with_f64_and_f64(a as f64, b as f64) {
                Ok(s) => JsFuture::from(s.array_buffer()).await.map(|buf| js_sys::Uint8Array::new(&buf).to_vec()).map_err(|e| format!("{e:?}")),
                Err(e) => Err(format!("{e:?}")),
            },
            Source::Remote(path, size) => crate::remote::fetch_range(&path, a, b.saturating_sub(a), size).await,
        };
        let wake = FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            fs.inflight.remove(&(id, c));
            // a file replaced or dropped meanwhile: discard
            if fs.blobs.contains_key(&id) || fs.remote.contains_key(&id) {
                match r {
                    Ok(v) => {
                        fs.clock += 1;
                        let t = fs.clock;
                        fs.bytes += v.len();
                        fs.chunks.insert((id, c), (v.into(), t));
                        fs.evict();
                    }
                    Err(e) => {
                        fs.failed.insert((id, c), e);
                    }
                }
            }
            (fs.on_data.clone(), std::mem::take(&mut fs.waiters))
        });
        let (wake, waiters) = wake;
        // retries waiting for data run now, not after the slowest chunk or a fixed sleep
        for resolve in waiters {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        }
        if let Some(w) = wake {
            w();
        }
    });
    true
}

/// Random-access reads of a registered Blob through the chunk cache.
pub struct BlobReader {
    id: u32,
    size: u64,
    /// Chunk size of this file's cache entries.
    chunk: u64,
    /// Chunks a sequential scan of this file fetches at once.
    parallel: u64,
}

impl ByteReader for BlobReader {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|e| *e <= self.size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of file"))?;
        let (c0, c1) = (offset / self.chunk, (end - 1) / self.chunk);
        let last = (self.size.max(1) - 1) / self.chunk;
        let missing: Vec<u64> = FS.with(|fs| {
            let fs = fs.borrow();
            (c0..=c1).filter(|c| !fs.chunks.contains_key(&(self.id, *c))).collect()
        });
        if let Some(e) = FS.with(|fs| missing.iter().find_map(|c| fs.borrow().failed.get(&(self.id, *c)).cloned())) {
            return Err(io::Error::other(format!("file read failed: {e}")));
        }
        if !missing.is_empty() {
            // misses at a frontier that moves forward are a sequential scan (a demuxer opening the
            // file, retried from its start): fetch many chunks at once
            let ahead = FS.with(|fs| {
                let mut fs = fs.borrow_mut();
                let class = class_of(&fs, self.id);
                let scan = fs.scans.entry(self.id).or_default();
                scan.note_miss(missing[0], self.parallel);
                class.ahead(scan, self.parallel, self.chunk, CACHE_BUDGET)
            });
            for c in missing.iter().copied().chain(c1 + 1..=(c1 + ahead).min(last)) {
                request(self.id, c);
            }
            return Err(filmcraft_media::pending::would_block());
        }
        let complete = FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            fs.clock += 1;
            let t = fs.clock;
            let mut pos = 0usize;
            for c in c0..=c1 {
                // Present (checked above); evicted in between would be a bug, reported as an error.
                let Some((d, used)) = fs.chunks.get_mut(&(self.id, c)) else { return false };
                *used = t;
                let a = if c == c0 { (offset - c * self.chunk) as usize } else { 0 };
                let n = (d.len() - a).min(buf.len() - pos);
                buf[pos..pos + n].copy_from_slice(&d[a..a + n]);
                pos += n;
            }
            true
        });
        if !complete {
            return Err(filmcraft_media::pending::would_block());
        }
        // read-ahead for sequential access (as wide as the scan that was seen on this file)
        let ahead = FS.with(|fs| {
            let fs = fs.borrow();
            let class = class_of(&fs, self.id);
            fs.scans.get(&self.id).map_or(crate::readahead::BASE_AHEAD, |s| class.ahead(s, self.parallel, self.chunk, CACHE_BUDGET))
        });
        for c in c1 + 1..=(c1 + ahead).min(last) {
            request(self.id, c);
        }
        Ok(())
    }
}

fn not_found(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path}: not found (files from an earlier browser session must be imported again)"))
}

/// The engine's host services on the web.
pub struct WebServices;

impl WebServices {
    /// Open a reader over the in-memory entry at `path`, or the error the engine shows.
    fn entry_reader(path: &str) -> io::Result<SharedReader> {
        let local = FS.with(|fs| match fs.borrow().entries.get(path) {
            Some(Entry::Blob { id, size }) => {
                Some(Ok(Arc::new(BlobReader { id: *id, size: *size, chunk: CHUNK, parallel: crate::readahead::BASE_AHEAD }) as SharedReader))
            }
            Some(Entry::Mem(b)) => Some(Ok(Arc::new(MemReader(b.clone())) as SharedReader)),
            Some(Entry::Remote { .. }) | None => None,
        });
        if let Some(r) = local {
            return r;
        }
        // A remote file: its size comes from the listing/HEAD cache (a changed size is a new file).
        // A path the cache does not know yet starts a fetch and reads as "loading" (the caller
        // retries on the repaint); one the server does not have is not found.
        match crate::remote::lookup(path) {
            Some(crate::remote::Lookup::Found(st)) if st.is_dir => Err(io::Error::other(format!("{path}: is a directory"))),
            Some(crate::remote::Lookup::Found(st)) => {
                let (chunk, parallel) = crate::remote::transfer();
                Ok(Arc::new(BlobReader { id: register_remote(path, st.size, chunk), size: st.size, chunk, parallel }) as SharedReader)
            }
            Some(crate::remote::Lookup::Loading) => Err(filmcraft_media::pending::would_block()),
            Some(crate::remote::Lookup::Missing) | None => Err(not_found(path)),
        }
    }
}

impl Services for WebServices {
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        let r = Self::entry_reader(path)?;
        if r.len() > WHOLE_FILE_MAX {
            return Err(io::Error::other(format!("{path}: too large to read whole in the browser")));
        }
        filmcraft_media::reader::read_range(&*r, 0, r.len() as usize)
    }

    /// Write `data` at `path`: a remote mount uploads in the background, OPFS persists it, anything else stays in memory.
    fn write_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        // a path on a remote mount: uploaded to the server (writable mounts) or refused (read-only)
        if let Some(r) = crate::remote_upload::write(path, data) {
            return r;
        }
        if let Some(rel) = path.strip_prefix("/opfs/") {
            crate::opfs::write(rel.to_string(), data.to_vec());
        }
        let hook = FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            fs.entries.insert(path.to_string(), Entry::Mem(Arc::from(data)));
            fs.on_write.clone()
        });
        if !path.starts_with("/opfs/")
            && let Some(h) = hook
        {
            h(path, data);
        }
        Ok(())
    }

    /// Create `path` with `data` in memory; fails when it exists. Not available on remote mounts or OPFS.
    fn write_file_new(&self, path: &str, data: &[u8]) -> io::Result<()> {
        if crate::remote::is_remote(path) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "exclusive creation on remote storage is unavailable"));
        }
        if path.starts_with("/opfs/") {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "exclusive OPFS writes are unavailable"));
        }
        let hook = FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            if fs.entries.contains_key(path) {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs.entries.insert(path.to_owned(), Entry::Mem(Arc::from(data)));
            Ok(fs.on_write.clone())
        })?;
        if let Some(h) = hook {
            h(path, data);
        }
        Ok(())
    }

    fn file_size(&self, path: &str) -> io::Result<u64> {
        Self::entry_reader(path).map(|r| r.len())
    }

    fn read_range(&self, path: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        filmcraft_media::reader::read_range(&*Self::entry_reader(path)?, offset, len)
    }

    fn reader(&self, path: &str) -> Option<io::Result<SharedReader>> {
        Some(Self::entry_reader(path))
    }

    fn export_in_memory(&self) -> bool {
        true
    }

    /// The Media Browser lists the virtual file table: picked and dropped files under `/files`.
    fn list_entries(&self, dir: &str) -> Option<io::Result<Vec<filmcraft_engine::media_browser::DirEntry>>> {
        use filmcraft_engine::media_browser::DirEntry;
        if let Some(r) = crate::remote::list(dir) {
            return Some(r);
        }
        let prefix = if dir.ends_with('/') { dir.to_string() } else { format!("{dir}/") };
        let out = FS.with(|fs| {
            let fs = fs.borrow();
            let mut dirs: Vec<String> = Vec::new();
            let mut files = Vec::new();
            for (path, e) in &fs.entries {
                let Some(rest) = path.strip_prefix(&prefix) else { continue };
                match rest.split_once('/') {
                    Some((d, _)) if !d.is_empty() => {
                        if !dirs.iter().any(|x| x == d) {
                            dirs.push(d.to_string());
                        }
                    }
                    Some(_) => {}
                    None => {
                        let size = match e {
                            Entry::Blob { size, .. } | Entry::Remote { size, .. } => *size,
                            Entry::Mem(b) => b.len() as u64,
                        };
                        files.push(DirEntry { name: rest.to_string(), is_dir: false, size: Some(size), modified: None });
                    }
                }
            }
            let mut v: Vec<DirEntry> = dirs.into_iter().map(|name| DirEntry { name, is_dir: true, ..Default::default() }).collect();
            v.extend(files);
            v
        });
        Some(Ok(out))
    }

    /// Each remote mount is a Network location.
    fn volumes(&self) -> Vec<filmcraft_engine::media_browser::Volume> {
        crate::remote::volumes()
    }

    fn home_dir(&self) -> Option<String> {
        Some("/files".into())
    }
}

thread_local! {
    static DOWNLOAD_CLOCK: web_time::Instant = web_time::Instant::now();
}

/// Offer bytes as a browser download named `name`. Downloads are clicked at least
/// [`crate::download_pacing::GAP_MS`] apart: Chrome drops a burst of them (#378).
pub fn download(name: &str, data: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let blob = bytes_to_blob(data, "application/octet-stream")?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let a: web_sys::HtmlAnchorElement = doc.create_element("a")?.dyn_into()?;
    a.set_href(&url);
    a.set_download(file_name(name));
    a.style().set_property("display", "none")?;
    let now_ms = DOWNLOAD_CLOCK.with(|clock| clock.elapsed().as_secs_f64() * 1000.0);
    let delay = DOWNLOADS.with(|d| d.borrow_mut().reserve(now_ms));
    let name = name.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        if delay > 0.0 {
            sleep(delay as i32).await;
        }
        let clicked = match doc.body() {
            Some(body) => body.append_child(&a).map(|_| a.click()),
            None => Err("no body".into()),
        };
        a.remove();
        if let Err(e) = clicked {
            log::warn!("download {name}: {e:?}");
        }
        sleep(60_000).await;
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    Ok(())
}

#[cfg(test)]
mod exclusive_write_tests {
    use super::*;

    #[test]
    fn virtual_file_creation_is_exclusive_and_opfs_fails_closed() {
        let host = WebServices;
        let existing = "/files/collision-test.wav";
        let fresh = "/files/new-extraction-test.wav";
        FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            fs.on_write = None;
            fs.entries.insert(existing.into(), Entry::Mem(Arc::from(&b"original"[..])));
            fs.entries.remove(fresh);
        });
        assert_eq!(host.write_file_new(existing, b"replacement").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(host.read_file(existing).unwrap(), b"original");
        host.write_file_new(fresh, b"new").unwrap();
        assert_eq!(host.read_file(fresh).unwrap(), b"new");
        assert_eq!(host.write_file_new(fresh, b"again").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(host.write_file_new("/opfs/media/new.wav", b"new").unwrap_err().kind(), io::ErrorKind::Unsupported);
        FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            fs.entries.remove(existing);
            fs.entries.remove(fresh);
        });
    }
}

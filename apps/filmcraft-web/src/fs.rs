//! Browser-backed host services: a virtual file table and the engine's [`Services`].
//!
//! - Media the user opens (`<input type=file>`, drag-and-drop, File System Access pickers, files
//!   kept in OPFS) are registered as `File`/`Blob` entries under a virtual path (`/files/<name>`).
//!   They are never copied whole: [`BlobReader`] serves the demuxers' range reads from a cache of
//!   1 MiB chunks (LRU, [`CACHE_BUDGET`]), fetched asynchronously with `Blob.slice().arrayBuffer()`.
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
/// Chunks fetched past the end of a read (sequential playback, demuxer scans).
const READ_AHEAD: u64 = 3;
/// Bytes of cached chunks kept (least recently used evicted).
pub const CACHE_BUDGET: usize = 384 << 20;
/// Largest Blob `read_file` will load whole (captions, interchange documents, projects).
const WHOLE_FILE_MAX: u64 = 256 << 20;

enum Entry {
    Blob { id: u32, size: u64 },
    Mem(Arc<[u8]>),
}

#[derive(Default)]
struct Fs {
    entries: HashMap<String, Entry>,
    blobs: HashMap<u32, web_sys::Blob>,
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

/// Resolve after `ms` milliseconds.
pub async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// Load `[a, b)` of a registered Blob into the chunk cache (import prewarm: container index).
pub async fn prefetch(path: &str, a: u64, b: u64) {
    let Some((id, size)) = FS.with(|fs| match fs.borrow().entries.get(path) {
        Some(Entry::Blob { id, size }) => Some((*id, *size)),
        _ => None,
    }) else {
        return;
    };
    let (c0, c1) = (a / CHUNK, b.min(size).div_ceil(CHUNK));
    for c in c0..c1 {
        request(id, c);
        // keep a bounded number of reads in flight
        while inflight() >= 8 {
            sleep(2).await;
        }
    }
    idle().await;
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

/// Start fetching chunk `c` of blob `id` unless it is cached or already on its way.
fn request(id: u32, c: u64) {
    let blob = FS.with(|fs| {
        let mut fs = fs.borrow_mut();
        if fs.chunks.contains_key(&(id, c)) || fs.inflight.contains(&(id, c)) {
            return None;
        }
        let blob = fs.blobs.get(&id)?.clone();
        fs.failed.remove(&(id, c));
        fs.inflight.insert((id, c));
        Some(blob)
    });
    let Some(blob) = blob else { return };
    let size = blob.size() as u64;
    let (a, b) = (c * CHUNK, ((c + 1) * CHUNK).min(size));
    let fut = blob.slice_with_f64_and_f64(a as f64, b as f64).map(|s| JsFuture::from(s.array_buffer()));
    wasm_bindgen_futures::spawn_local(async move {
        let r = match fut {
            Ok(f) => f.await.map(|buf| js_sys::Uint8Array::new(&buf).to_vec()),
            Err(e) => Err(e),
        };
        let wake = FS.with(|fs| {
            let mut fs = fs.borrow_mut();
            fs.inflight.remove(&(id, c));
            // a blob replaced or dropped meanwhile: discard
            if fs.blobs.contains_key(&id) {
                match r {
                    Ok(v) => {
                        fs.clock += 1;
                        let t = fs.clock;
                        fs.bytes += v.len();
                        fs.chunks.insert((id, c), (v.into(), t));
                        fs.evict();
                    }
                    Err(e) => {
                        fs.failed.insert((id, c), format!("{e:?}"));
                    }
                }
            }
            fs.on_data.clone()
        });
        if let Some(w) = wake {
            w();
        }
    });
}

/// Random-access reads of a registered Blob through the chunk cache.
pub struct BlobReader {
    id: u32,
    size: u64,
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
        let (c0, c1) = (offset / CHUNK, (end - 1) / CHUNK);
        let last = (self.size.max(1) - 1) / CHUNK;
        let missing: Vec<u64> = FS.with(|fs| {
            let fs = fs.borrow();
            (c0..=c1).filter(|c| !fs.chunks.contains_key(&(self.id, *c))).collect()
        });
        if let Some(e) = FS.with(|fs| missing.iter().find_map(|c| fs.borrow().failed.get(&(self.id, *c)).cloned())) {
            return Err(io::Error::other(format!("file read failed: {e}")));
        }
        if !missing.is_empty() {
            for c in missing.iter().copied().chain(c1 + 1..=(c1 + READ_AHEAD).min(last)) {
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
                let a = if c == c0 { (offset - c * CHUNK) as usize } else { 0 };
                let n = (d.len() - a).min(buf.len() - pos);
                buf[pos..pos + n].copy_from_slice(&d[a..a + n]);
                pos += n;
            }
            true
        });
        if !complete {
            return Err(filmcraft_media::pending::would_block());
        }
        // read-ahead for sequential access
        for c in c1 + 1..=(c1 + READ_AHEAD).min(last) {
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
    fn entry_reader(path: &str) -> io::Result<SharedReader> {
        FS.with(|fs| match fs.borrow().entries.get(path) {
            Some(Entry::Blob { id, size }) => Ok(Arc::new(BlobReader { id: *id, size: *size }) as SharedReader),
            Some(Entry::Mem(b)) => Ok(Arc::new(MemReader(b.clone())) as SharedReader),
            None => Err(not_found(path)),
        })
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

    fn write_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
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
                            Entry::Blob { size, .. } => *size,
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

    fn home_dir(&self) -> Option<String> {
        Some("/files".into())
    }
}

/// Offer bytes as a browser download named `name`.
pub fn download(name: &str, data: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let blob = bytes_to_blob(data, "application/octet-stream")?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let a: web_sys::HtmlAnchorElement = doc.create_element("a")?.dyn_into()?;
    a.set_href(&url);
    a.set_download(file_name(name));
    a.style().set_property("display", "none")?;
    doc.body().ok_or("no body")?.append_child(&a)?;
    a.click();
    a.remove();
    wasm_bindgen_futures::spawn_local(async move {
        sleep(60_000).await;
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    Ok(())
}

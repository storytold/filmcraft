//! Origin Private File System (OPFS): auto-save / crash recovery and the media copies that let a
//! recovered project find its media after a reload.
//!
//! Writes are queued and coalesced per path (the newest bytes win) and run one at a time.

use std::cell::RefCell;
use std::collections::BTreeMap;

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle};

/// Called with whether a queued write reached OPFS.
pub type Done = Box<dyn FnOnce(bool)>;

thread_local! {
    /// Pending writes per path (bytes and the callbacks of every write coalesced into them), and
    /// whether the writer task is running.
    static QUEUE: RefCell<(BTreeMap<String, (Vec<u8>, Vec<Done>)>, bool)> = const { RefCell::new((BTreeMap::new(), false)) };
}

/// Whether this browser has OPFS (`navigator.storage.getDirectory`).
pub fn available() -> bool {
    web_sys::window().is_some_and(|w| {
        let storage = js_sys::Reflect::get(&w.navigator(), &"storage".into()).unwrap_or(JsValue::UNDEFINED);
        !storage.is_undefined() && js_sys::Reflect::has(&storage, &"getDirectory".into()).unwrap_or(false)
    })
}

async fn root() -> Result<FileSystemDirectoryHandle, JsValue> {
    if !available() {
        return Err("OPFS not available".into());
    }
    let w = web_sys::window().ok_or("no window")?;
    JsFuture::from(w.navigator().storage().get_directory()).await?.dyn_into()
}

/// The directory holding `rel` (created when `create`) and the file name.
async fn parent(rel: &str, create: bool) -> Result<(FileSystemDirectoryHandle, String), JsValue> {
    let mut dir = root().await?;
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let (name, dirs) = parts.split_last().ok_or("empty path")?;
    for d in dirs {
        let o = web_sys::FileSystemGetDirectoryOptions::new();
        o.set_create(create);
        dir = JsFuture::from(dir.get_directory_handle_with_options(d, &o)).await?.dyn_into()?;
    }
    Ok((dir, name.to_string()))
}

async fn file_handle(rel: &str, create: bool) -> Result<FileSystemFileHandle, JsValue> {
    let (dir, name) = parent(rel, create).await?;
    let o = web_sys::FileSystemGetFileOptions::new();
    o.set_create(create);
    JsFuture::from(dir.get_file_handle_with_options(&name, &o)).await?.dyn_into()
}

async fn write_now(rel: &str, data: &[u8]) -> Result<(), JsValue> {
    let fh = file_handle(rel, true).await?;
    let w: web_sys::FileSystemWritableFileStream = JsFuture::from(fh.create_writable()).await?.dyn_into()?;
    JsFuture::from(w.write_with_u8_array(data)?).await?;
    JsFuture::from(w.close()).await?;
    Ok(())
}

/// Queue a write of `data` to `rel` (relative to the OPFS root).
pub fn write(rel: String, data: Vec<u8>) {
    write_then(rel, data, None);
}

/// [`write`], then call `done` with whether it succeeded. A write replaced by a newer one to the
/// same path reports the newer one's result.
pub fn write_then(rel: String, data: Vec<u8>, done: Option<Done>) {
    let start = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let e = q.0.entry(rel).or_default();
        e.0 = data;
        e.1.extend(done);
        !std::mem::replace(&mut q.1, true)
    });
    if !start {
        return;
    }
    wasm_bindgen_futures::spawn_local(async {
        loop {
            let next = QUEUE.with(|q| {
                let mut q = q.borrow_mut();
                let k = q.0.keys().next().cloned();
                match k {
                    Some(k) => q.0.remove_entry(&k),
                    None => {
                        q.1 = false;
                        None
                    }
                }
            });
            let Some((rel, (data, done))) = next else { break };
            let r = write_now(&rel, &data).await;
            if let Err(e) = &r {
                log::warn!("OPFS write {rel}: {e:?}");
            }
            for d in done {
                d(r.is_ok());
            }
        }
    });
}

/// Recognize a missing OPFS entry without hiding other storage or permission errors.
fn not_found(error: &JsValue) -> bool {
    js_sys::Reflect::get(error, &"name".into()).ok().and_then(|name| name.as_string()).as_deref() == Some("NotFoundError")
}

/// Obtain a file without loading its contents into wasm memory; distinguish absence from I/O failure.
pub async fn file_result(rel: &str) -> Result<Option<web_sys::File>, JsValue> {
    let fh = match file_handle(rel, false).await {
        Ok(fh) => fh,
        Err(error) if not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let file = match JsFuture::from(fh.get_file()).await {
        Ok(file) => file,
        Err(error) if not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    file.dyn_into().map(Some)
}

/// Read a file with a caller-supplied byte cap, preserving I/O errors for recovery startup.
pub async fn read_capped_result(rel: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, JsValue> {
    let Some(file) = file_result(rel).await? else { return Ok(None) };
    if file.size() > max_bytes as f64 {
        return Err(JsValue::from_str("OPFS file exceeds the read size limit"));
    }
    let buf = JsFuture::from(file.array_buffer()).await?;
    Ok(Some(js_sys::Uint8Array::new(&buf).to_vec()))
}

/// Read a file (None when missing or inaccessible).
pub async fn read(rel: &str) -> Option<Vec<u8>> {
    read_capped_result(rel, u64::MAX).await.ok().flatten()
}

/// Remove a file (ignored when missing).
pub async fn remove(rel: &str) {
    if let Ok((dir, name)) = parent(rel, false).await {
        let _ = JsFuture::from(dir.remove_entry(&name)).await;
    }
}

/// Copy a Blob into OPFS (streamed by the browser, never through wasm memory).
pub async fn store_blob(rel: &str, blob: &web_sys::Blob) -> Result<(), JsValue> {
    let fh = file_handle(rel, true).await?;
    let w: web_sys::FileSystemWritableFileStream = JsFuture::from(fh.create_writable()).await?.dyn_into()?;
    JsFuture::from(w.write_with_blob(blob)?).await?;
    JsFuture::from(w.close()).await?;
    Ok(())
}

pub(crate) const MAX_MEDIA_DIRECTORY_ENTRIES: usize = 100_000;
pub(crate) const MEDIA_ENTRY_LIMIT_ERROR: &str = "browser recovery media directory entry limit exceeded";

async fn file_handles_in_result(dir_rel: &str) -> Result<Vec<FileSystemFileHandle>, JsValue> {
    let mut dir = root().await?;
    for part in dir_rel.split('/').filter(|part| !part.is_empty()) {
        dir = match JsFuture::from(dir.get_directory_handle(part)).await {
            Ok(handle) => handle.dyn_into()?,
            Err(error) if not_found(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
    }
    let mut files = Vec::new();
    let values: js_sys::Function = js_sys::Reflect::get(&dir, &"values".into())?.dyn_into()?;
    let entries = values.call0(&dir)?;
    let next_entry: js_sys::Function = js_sys::Reflect::get(&entries, &"next".into())?.dyn_into()?;
    for count in 0..=MAX_MEDIA_DIRECTORY_ENTRIES {
        let next = next_entry.call0(&entries)?;
        let next = JsFuture::from(js_sys::Promise::resolve(&next)).await?;
        let done = js_sys::Reflect::get(&next, &"done".into())?;
        if !done.is_undefined() && done.as_bool().is_none() {
            return Err("invalid OPFS iterator completion flag".into());
        }
        if done.as_bool().unwrap_or(false) {
            return Ok(files);
        }
        if count == MAX_MEDIA_DIRECTORY_ENTRIES {
            return Err(MEDIA_ENTRY_LIMIT_ERROR.into());
        }
        let handle: web_sys::FileSystemHandle = js_sys::Reflect::get(&next, &"value".into())?.dyn_into()?;
        if handle.kind() == web_sys::FileSystemHandleKind::File {
            files.push(handle.dyn_into()?);
        }
    }
    Err("browser recovery media enumeration did not finish".into())
}

/// List file names without reading file contents; a missing directory is empty, other errors are preserved.
pub async fn file_names_in_result(dir_rel: &str) -> Result<Vec<String>, JsValue> {
    Ok(file_handles_in_result(dir_rel).await?.into_iter().map(|handle| handle.name()).collect())
}

/// The readable files in an OPFS directory: (name, File). Enumeration errors return an empty list.
pub async fn files_in(dir_rel: &str) -> Vec<(String, web_sys::File)> {
    let mut out = Vec::new();
    let Ok(files) = file_handles_in_result(dir_rel).await else { return out };
    for handle in files {
        let read = (|| {
            let get_file: js_sys::Function = js_sys::Reflect::get(&handle, &"getFile".into())?.dyn_into()?;
            get_file.call0(&handle)
        })();
        let Ok(read) = read else { continue };
        if let Ok(file) = JsFuture::from(js_sys::Promise::resolve(&read)).await
            && let Ok(file) = file.dyn_into::<web_sys::File>()
        {
            out.push((file.name(), file));
        }
    }
    out
}

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

/// Read a file (None when missing).
pub async fn read(rel: &str) -> Option<Vec<u8>> {
    let fh = file_handle(rel, false).await.ok()?;
    let file: web_sys::File = JsFuture::from(fh.get_file()).await.ok()?.dyn_into().ok()?;
    let buf = JsFuture::from(file.array_buffer()).await.ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
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

/// The files in an OPFS directory: (name, File).
pub async fn files_in(dir_rel: &str) -> Vec<(String, web_sys::File)> {
    let mut out = Vec::new();
    let Ok(dir) = (async {
        let mut d = root().await?;
        for p in dir_rel.split('/').filter(|p| !p.is_empty()) {
            d = JsFuture::from(d.get_directory_handle(p)).await?.dyn_into()?;
        }
        Ok::<_, JsValue>(d)
    })
    .await
    else {
        return out;
    };
    let it = dir.values();
    while let Ok(next) = it.next() {
        let Ok(r) = JsFuture::from(next).await else { break };
        let r: js_sys::IteratorNext = r.unchecked_into();
        if r.done() {
            break;
        }
        let Ok(h) = r.value().dyn_into::<FileSystemFileHandle>() else { continue };
        if let Ok(f) = JsFuture::from(h.get_file()).await
            && let Ok(f) = f.dyn_into::<web_sys::File>()
        {
            out.push((f.name(), f));
        }
    }
    out
}

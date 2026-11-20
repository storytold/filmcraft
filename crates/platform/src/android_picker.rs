//! Android's Storage Access Framework. The activity hands back selected content URIs, and
//! this module streams them to files that the existing path-based media engine can reopen.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};

use android_activity::AndroidApp;
use jni::{
    JNIEnv,
    objects::{JObject, JString, JValue},
};

use crate::android_ui::{Result as AndroidResult, with_activity};

const MAX_DOCUMENTS: usize = 128;
const MAX_URI_BYTES: usize = 32 * 1024;
const MAX_NAME_BYTES: usize = 180;
const ERROR_EXTRA: &str = "org.filmcraft.googlebook.PICKER_ERROR";

/// Launch the system document picker on the Java UI thread. Only launch status goes through
/// this channel. The editor takes the document result separately when it returns to the foreground.
pub fn launch_documents(app: &AndroidApp, multi: bool, mime_types: &[&str]) -> Receiver<Result<(), String>> {
    let (reply, receiver) = channel();
    let activity_app = app.clone();
    let mime_types: Vec<_> = mime_types.iter().take(32).map(|kind| (*kind).to_owned()).collect();
    app.run_on_java_main_thread(Box::new(move || {
        let result = with_activity(&activity_app, |env, activity| {
            env.with_local_frame(40, |env| -> AndroidResult<()> {
                let types = env.new_object_array(mime_types.len() as i32, "java/lang/String", JObject::null())?;
                for (index, mime) in mime_types.iter().enumerate() {
                    let value = env.new_string(mime)?;
                    env.set_object_array_element(&types, index as i32, value)?;
                }
                env.call_method(activity, "openFilmCraftDocuments", "(Z[Ljava/lang/String;)V", &[JValue::Bool(u8::from(multi)), JValue::Object(&types)])?;
                Ok(())
            })
        });
        let _ = reply.send(result.map_err(|err| format!("Could not open the Android file picker: {err}")));
    }));
    receiver
}

/// Take a pending picker result once. `None` means Android has not answered, and an empty
/// selection means cancellation. Only the URI strings are retained after this call.
pub fn poll_documents(app: &AndroidApp) -> Result<Option<Vec<String>>, String> {
    with_activity(app, |env, activity| {
        env.with_local_frame(12, |env| -> AndroidResult<_> {
            let intent = env.call_method(activity, "takeFilmCraftDocumentResult", "()Landroid/content/Intent;", &[])?.l()?;
            if intent.is_null() {
                return Ok(None);
            }
            let error_key = env.new_string(ERROR_EXTRA)?;
            let error = env.call_method(&intent, "getStringExtra", "(Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&error_key)])?.l()?;
            if !error.is_null() {
                return Err(java_string(env, error)?.into());
            }
            let mut documents = Vec::new();
            let uri = env.call_method(&intent, "getData", "()Landroid/net/Uri;", &[])?.l()?;
            if !uri.is_null() {
                documents.push(uri_string(env, &uri)?);
            }
            let clip = env.call_method(&intent, "getClipData", "()Landroid/content/ClipData;", &[])?.l()?;
            if !clip.is_null() {
                collect_clip_documents(env, &clip, &mut documents)?;
            }
            Ok(Some(documents))
        })
    })
    .map_err(|err| format!("Could not read the Android file picker selection: {err}"))
}

/// Clear a result if the editor closes before it can use it.
pub fn cancel_documents(app: &AndroidApp) {
    let activity_app = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        if let Err(err) = with_activity(&activity_app, |env, activity| {
            env.call_method(activity, "cancelFilmCraftDocuments", "()V", &[])?;
            Ok(())
        }) {
            log::debug!("Could not clear the Android file picker result: {err}");
        }
    }));
}

fn collect_clip_documents(env: &mut JNIEnv<'_>, clip: &JObject<'_>, documents: &mut Vec<String>) -> AndroidResult<()> {
    let count = env.call_method(clip, "getItemCount", "()I", &[])?.i()?;
    if !(0..=MAX_DOCUMENTS as i32).contains(&count) {
        return Err(format!("Select at most {MAX_DOCUMENTS} files at once.").into());
    }
    for index in 0..count {
        let value = env.with_local_frame(6, |env| -> AndroidResult<Option<String>> {
            let item = env.call_method(clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[JValue::Int(index)])?.l()?;
            let uri = env.call_method(&item, "getUri", "()Landroid/net/Uri;", &[])?.l()?;
            if uri.is_null() { Ok(None) } else { Ok(Some(uri_string(env, &uri)?)) }
        })?;
        if let Some(value) = value
            && !documents.contains(&value)
        {
            if documents.len() == MAX_DOCUMENTS {
                return Err(format!("Select at most {MAX_DOCUMENTS} files at once.").into());
            }
            documents.push(value);
        }
    }
    Ok(())
}

fn java_string(env: &mut JNIEnv<'_>, value: JObject<'_>) -> AndroidResult<String> {
    Ok(env.get_string(&JString::from(value))?.into())
}

fn uri_string(env: &mut JNIEnv<'_>, uri: &JObject<'_>) -> AndroidResult<String> {
    let value = env.call_method(uri, "toString", "()Ljava/lang/String;", &[])?.l()?;
    let text = java_string(env, value)?;
    check_uri(&text)?;
    Ok(text)
}

fn check_uri(value: &str) -> AndroidResult<()> {
    if !value.starts_with("content://") || value.len() > MAX_URI_BYTES {
        return Err("The picker returned an unsupported document address.".into());
    }
    Ok(())
}

/// Progress for one picker batch. Android document providers may omit sizes.
#[derive(Default)]
pub struct CopyProgress {
    pub copied: AtomicU64,
    pub total: AtomicU64,
    pub cancelled: AtomicBool,
}

impl CopyProgress {
    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Import cancelled."));
        }
        Ok(())
    }
}

/// Copy a batch to its staging directory, and remove partial files on error or cancellation.
pub fn copy_documents(
    app: &AndroidApp,
    documents: Vec<String>,
    destination: &Path,
    allowed_exts: &[String],
    progress: &CopyProgress,
) -> Result<Vec<PathBuf>, String> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    if documents.len() > MAX_DOCUMENTS {
        return Err(format!("Select at most {MAX_DOCUMENTS} files at once."));
    }
    let folder = reserve_folder(destination).map_err(|err| format!("Could not create an import folder: {err}"))?;
    let result = with_activity(app, |env, activity| {
        let resolver = env.call_method(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
        let mut files = Vec::with_capacity(documents.len());
        let mut known_bytes = 0u64;
        let mut known_sizes = true;
        for document in documents {
            progress.check()?;
            let (name, size) = env.with_local_frame(24, |env| {
                let uri = parse_uri(env, &document)?;
                document_metadata(env, &resolver, &uri)
            })?;
            let name = safe_name(&name);
            if !extension_allowed(&name, allowed_exts) {
                return Err(format!("{name} does not have a supported file extension.").into());
            }
            match size {
                Some(size) => known_bytes = known_bytes.checked_add(size).ok_or("Import size exceeds the supported range.")?,
                None => known_sizes = false,
            }
            files.push((document, name));
        }
        check_space(env, &folder, known_bytes)?;
        if known_sizes {
            progress.total.store(known_bytes, Ordering::Relaxed);
        }
        let mut paths = Vec::with_capacity(files.len());
        for (document, name) in files {
            progress.check()?;
            let path = env.with_local_frame(20, |env| {
                let uri = parse_uri(env, &document)?;
                let (path, file) = reserve_file(&folder, &name)?;
                copy_stream(env, &resolver, &uri, file, progress).map_err(|err| format!("{name}: {err}"))?;
                Ok::<_, Box<dyn std::error::Error>>(path)
            })?;
            paths.push(path);
        }
        progress.check()?;
        Ok(paths)
    });
    if let Err(err) = &result
        && let Err(cleanup) = fs::remove_dir_all(&folder)
    {
        log::warn!("Android import failed ({err}) and its staging folder could not be cleared: {cleanup}");
    }
    result.map_err(|err| format!("Could not copy the selected files: {err}"))
}

fn check_space(env: &mut JNIEnv<'_>, path: &Path, required: u64) -> AndroidResult<()> {
    let available = space(env, path)?;
    if available == 0 || required > available {
        return Err(format!("Insufficient space for this import. {:.1} MiB available.", available as f64 / (1 << 20) as f64).into());
    }
    Ok(())
}

pub fn available_bytes(app: &AndroidApp, path: &Path) -> Result<u64, String> {
    with_activity(app, |env, _| space(env, path)).map_err(|error| error.to_string())
}

fn space(env: &mut JNIEnv<'_>, path: &Path) -> AndroidResult<u64> {
    let path = env.new_string(path.to_str().ok_or("The import folder is not a valid Android path.")?)?;
    let stat = env.new_object("android/os/StatFs", "(Ljava/lang/String;)V", &[JValue::Object(&path)])?;
    Ok(env.call_method(&stat, "getAvailableBytes", "()J", &[])?.j()?.max(0) as u64)
}

fn reserve_folder(destination: &Path) -> io::Result<PathBuf> {
    static NEXT_FOLDER: AtomicU64 = AtomicU64::new(1);
    fs::create_dir_all(destination)?;
    for _ in 0..128 {
        let serial = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let folder = destination.join(format!("Import-{}-{serial}", std::process::id()));
        match fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("All candidate import folder names are in use."))
}

fn parse_uri<'local>(env: &mut JNIEnv<'local>, address: &str) -> AndroidResult<JObject<'local>> {
    check_uri(address)?;
    let address = env.new_string(address)?;
    Ok(env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&address)])?.l()?)
}

fn document_metadata(env: &mut JNIEnv<'_>, resolver: &JObject<'_>, uri: &JObject<'_>) -> AndroidResult<(String, Option<u64>)> {
    let name_column = env.new_string("_display_name")?;
    let size_column = env.new_string("_size")?;
    let columns = env.new_object_array(2, "java/lang/String", &name_column)?;
    env.set_object_array_element(&columns, 1, &size_column)?;
    let none = JObject::null();
    let cursor = env
        .call_method(
            resolver,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[JValue::Object(uri), JValue::Object(&columns), JValue::Object(&none), JValue::Object(&none), JValue::Object(&none)],
        )?
        .l()?;
    if cursor.is_null() {
        return Err("The document provider returned no file information.".into());
    }
    let result = (|| -> AndroidResult<(String, Option<u64>)> {
        let column = |env: &mut JNIEnv<'_>, key: &JObject<'_>| -> AndroidResult<i32> {
            Ok(env.call_method(&cursor, "getColumnIndex", "(Ljava/lang/String;)I", &[JValue::Object(key)])?.i()?)
        };
        if !env.call_method(&cursor, "moveToFirst", "()Z", &[])?.z()? {
            return Err("The document provider returned no file information.".into());
        }
        let index = column(env, &name_column)?;
        if index < 0 {
            return Err("The document provider did not supply a file name.".into());
        }
        let value = env.call_method(&cursor, "getString", "(I)Ljava/lang/String;", &[JValue::Int(index)])?.l()?;
        if value.is_null() {
            return Err("The document provider did not supply a file name.".into());
        }
        let name = java_string(env, value)?;
        let index = column(env, &size_column)?;
        let size = if index < 0 || env.call_method(&cursor, "isNull", "(I)Z", &[JValue::Int(index)])?.z()? {
            None
        } else {
            u64::try_from(env.call_method(&cursor, "getLong", "(I)J", &[JValue::Int(index)])?.j()?).ok()
        };
        Ok((name, size))
    })();
    close_after(env, &cursor, result)
}

fn safe_name(name: &str) -> String {
    let normalized: String =
        name.chars().map(|ch| if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control() { '_' } else { ch }).collect();
    let normalized = normalized.trim_matches(['.', ' ']);
    let path = Path::new(normalized);
    let extension = path.extension().and_then(|part| part.to_str()).filter(|part| part.len() <= 16).unwrap_or("");
    let stem = path.file_stem().and_then(|part| part.to_str()).filter(|part| !part.is_empty()).unwrap_or("Document");
    let budget = MAX_NAME_BYTES.saturating_sub(extension.len() + usize::from(!extension.is_empty()));
    let mut end = stem.len().min(budget);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    if extension.is_empty() { stem[..end].to_string() } else { format!("{}.{extension}", &stem[..end]) }
}

fn extension_allowed(name: &str, allowed_exts: &[String]) -> bool {
    if allowed_exts.is_empty() || allowed_exts.iter().any(|ext| ext == "*") {
        return true;
    }
    Path::new(name).extension().and_then(|ext| ext.to_str()).is_some_and(|actual| allowed_exts.iter().any(|ext| actual.eq_ignore_ascii_case(ext)))
}

fn reserve_file(folder: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let name = Path::new(name);
    let stem = name.file_stem().and_then(|part| part.to_str()).unwrap_or("Document");
    let extension = name.extension().and_then(|part| part.to_str());
    for number in 1..=MAX_DOCUMENTS {
        let path = match (number, extension) {
            (1, _) => folder.join(name),
            (_, Some(ext)) => folder.join(format!("{stem} ({number}).{ext}")),
            (_, None) => folder.join(format!("{stem} ({number})")),
        };
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("Too many files with the same name."))
}

fn copy_stream(env: &mut JNIEnv<'_>, resolver: &JObject<'_>, uri: &JObject<'_>, file: File, progress: &CopyProgress) -> AndroidResult<()> {
    let input = env.call_method(resolver, "openInputStream", "(Landroid/net/Uri;)Ljava/io/InputStream;", &[JValue::Object(uri)])?.l()?;
    if input.is_null() {
        return Err("Android could not open the document for reading.".into());
    }
    let result = stream_to_file(env, &input, file, progress);
    close_after(env, &input, result)
}

fn stream_to_file(env: &mut JNIEnv<'_>, input: &JObject<'_>, file: File, progress: &CopyProgress) -> AndroidResult<()> {
    const CHUNK: usize = 64 * 1024;
    let source = env.new_byte_array(CHUNK as i32)?;
    let mut signed = vec![0i8; CHUNK];
    let mut bytes = vec![0u8; CHUNK];
    let mut file = BufWriter::new(file);
    loop {
        progress.check()?;
        let count = env.call_method(input, "read", "([B)I", &[JValue::Object(&source)])?.i()?;
        if count == -1 {
            break;
        }
        let size = usize::try_from(count).ok().filter(|size| (1..=CHUNK).contains(size)).ok_or("The document stream returned an invalid read size.")?;
        env.get_byte_array_region(&source, 0, &mut signed[..size])?;
        for (out, byte) in bytes.iter_mut().zip(&signed).take(size) {
            *out = *byte as u8;
        }
        progress.check()?;
        file.write_all(&bytes[..size])?;
        progress.copied.fetch_add(size as u64, Ordering::Relaxed);
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}

fn close_after<T>(env: &mut JNIEnv<'_>, object: &JObject<'_>, result: AndroidResult<T>) -> AndroidResult<T> {
    if env.exception_check()? {
        env.exception_clear()?;
    }
    let closed = env.call_method(object, "close", "()V", &[]);
    let value = result?;
    closed?;
    Ok(value)
}

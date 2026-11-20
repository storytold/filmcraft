//! Android's MediaStore integration for saving exports and portable backups into Downloads.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;

use android_activity::AndroidApp;
use jni::{
    JNIEnv,
    objects::{JObject, JString, JValue},
};

use crate::android_ui::{Result, with_activity};

/// Publish a completed file and return its content URI. Publishing runs off the UI thread.
pub fn save_mp4(app: &AndroidApp, file: &Path) -> Result<String> {
    let name = file.file_name().and_then(|n| n.to_str()).ok_or("The export has no usable filename.")?;
    let reader = BufReader::new(File::open(file)?);
    save_download(app, name, "video/mp4", false, |env, out| copy(env, out, reader))
}

pub fn save_subtitles(app: &AndroidApp, file: &Path) -> Result<String> {
    let name = file.file_name().and_then(|n| n.to_str()).ok_or("The subtitles have no usable filename.")?;
    let mime = if name.ends_with(".vtt") { "text/vtt" } else { "application/x-subrip" };
    let reader = BufReader::new(File::open(file)?);
    save_download(app, name, mime, false, |env, out| copy(env, out, reader))
}

pub fn save_backup_zip(app: &AndroidApp, folder: &Path) -> Result<String> {
    let name = folder.file_name().and_then(|n| n.to_str()).ok_or("The backup has no usable folder name.")?;
    save_download(app, &format!("{name}.zip"), "application/zip", true, |env, zip| {
        // Most source media is already compressed. Keep CPU use low when archiving it.
        env.call_method(zip, "setLevel", "(I)V", &[JValue::Int(0)])?;
        zip_folder(env, zip, folder, "", 0)
    })
}

fn zip_folder(env: &mut JNIEnv<'_>, zip: &JObject<'_>, folder: &Path, prefix: &str, depth: u8) -> Result<()> {
    if depth > 2 {
        return Err("The backup contains an unexpected nested folder.".into());
    }
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err("The backup contains an unexpected symbolic link.".into());
        }
        let name = entry.file_name();
        let name = name.to_str().ok_or("The backup has a filename that cannot be archived.")?;
        if name.contains(['\\', ':']) {
            return Err(format!("The backup has a filename that cannot be archived. {name}").into());
        }
        let relative = format!("{prefix}{name}");
        if kind.is_dir() {
            zip_folder(env, zip, &entry.path(), &format!("{relative}/"), depth + 1)?;
        } else if kind.is_file() {
            env.with_local_frame(5, |env| -> Result<()> {
                let name = env.new_string(&relative)?;
                let entry_path = env.new_object("java/util/zip/ZipEntry", "(Ljava/lang/String;)V", &[JValue::Object(&name)])?;
                env.call_method(zip, "putNextEntry", "(Ljava/util/zip/ZipEntry;)V", &[JValue::Object(&entry_path)])?;
                copy(env, zip, BufReader::new(File::open(entry.path())?))?;
                env.call_method(zip, "closeEntry", "()V", &[])?;
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn save_download(app: &AndroidApp, name: &str, mime: &str, zip: bool, write: impl FnOnce(&mut JNIEnv, &JObject) -> Result<()>) -> Result<String> {
    with_activity(app, |env, activity| {
        if env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()? < 29 {
            return Err("Saving to Downloads requires Android 10 or later.".into());
        }
        let resolver = env.call_method(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
        let downloads = env.get_static_field("android/provider/MediaStore$Downloads", "EXTERNAL_CONTENT_URI", "Landroid/net/Uri;")?.l()?;
        let values = env.new_object("android/content/ContentValues", "()V", &[])?;
        setting(env, &values, "_display_name", name)?;
        setting(env, &values, "mime_type", mime)?;
        setting(env, &values, "relative_path", "Download/FilmCraft")?;
        setting(env, &values, "is_pending", "1")?;
        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&downloads), JValue::Object(&values)],
            )?
            .l()?;
        if uri.is_null() {
            return Err("Android could not create the Downloads entry.".into());
        }
        let published = (|| -> Result<_> {
            let mode = env.new_string("w")?;
            let out = env
                .call_method(
                    &resolver,
                    "openOutputStream",
                    "(Landroid/net/Uri;Ljava/lang/String;)Ljava/io/OutputStream;",
                    &[JValue::Object(&uri), JValue::Object(&mode)],
                )?
                .l()?;
            if out.is_null() {
                return Err("Android could not open the Downloads entry.".into());
            }
            let stream = if zip {
                match env.new_object("java/util/zip/ZipOutputStream", "(Ljava/io/OutputStream;)V", &[JValue::Object(&out)]) {
                    Ok(zip) => zip,
                    Err(error) => {
                        let _ = env.exception_clear();
                        let _ = env.call_method(&out, "close", "()V", &[]);
                        return Err(error.into());
                    }
                }
            } else {
                out
            };
            let copied = write(env, &stream);
            if matches!(env.exception_check(), Ok(true)) {
                env.exception_clear()?;
            }
            let closed = env.call_method(&stream, "close", "()V", &[]);
            copied?;
            closed?;
            env.call_method(&values, "clear", "()V", &[])?;
            setting(env, &values, "is_pending", "0")?;
            let updated = env
                .call_method(
                    &resolver,
                    "update",
                    "(Landroid/net/Uri;Landroid/content/ContentValues;Ljava/lang/String;[Ljava/lang/String;)I",
                    &[JValue::Object(&uri), JValue::Object(&values), JValue::Object(&JObject::null()), JValue::Object(&JObject::null())],
                )?
                .i()?;
            if updated != 1 {
                return Err("Android did not publish the Downloads entry.".into());
            }
            let value = JString::from(env.call_method(&uri, "toString", "()Ljava/lang/String;", &[])?.l()?);
            Ok(env.get_string(&value)?.into())
        })();
        if published.is_err() {
            let _ = env.exception_clear();
            let _ = env.call_method(
                &resolver,
                "delete",
                "(Landroid/net/Uri;Ljava/lang/String;[Ljava/lang/String;)I",
                &[JValue::Object(&uri), JValue::Object(&JObject::null()), JValue::Object(&JObject::null())],
            );
        }
        published
    })
}

fn setting(env: &mut JNIEnv, values: &JObject, key: &str, value: &str) -> Result<()> {
    let key = env.new_string(key)?;
    let value = env.new_string(value)?;
    env.call_method(values, "put", "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&key), JValue::Object(&value)])?;
    Ok(())
}

fn copy(env: &mut JNIEnv, output: &JObject, mut reader: impl Read) -> Result<()> {
    const SIZE: usize = 64 * 1024;
    let mut bytes = vec![0u8; SIZE];
    let mut signed = vec![0i8; SIZE];
    let array = env.new_byte_array(SIZE as i32)?;
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            return Ok(());
        }
        for (dst, src) in signed.iter_mut().zip(&bytes).take(n) {
            *dst = *src as i8;
        }
        env.set_byte_array_region(&array, 0, &signed[..n])?;
        env.call_method(output, "write", "([BII)V", &[JValue::Object(&array), JValue::Int(0), JValue::Int(n as i32)])?;
    }
}

/// Open the system share sheet for an MP4 the app published.
pub fn share_mp4(app: &AndroidApp, uri: &str) -> Result<()> {
    with_activity(app, |env, activity| {
        let uri = env.new_string(uri)?;
        let content = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&uri)])?.l()?;
        let action = env.new_string("android.intent.action.SEND")?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        let mime = env.new_string("video/mp4")?;
        env.call_method(&intent, "setType", "(Ljava/lang/String;)Landroid/content/Intent;", &[JValue::Object(&mime)])?;
        let key = env.new_string("android.intent.extra.STREAM")?;
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Landroid/os/Parcelable;)Landroid/content/Intent;",
            &[JValue::Object(&key), JValue::Object(&content)],
        )?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(1)])?;
        let title = env.new_string("Share video")?;
        let chooser = env
            .call_static_method(
                "android/content/Intent",
                "createChooser",
                "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;",
                &[JValue::Object(&intent), JValue::Object(&title)],
            )?
            .l()?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&chooser)])?;
        Ok(())
    })
}

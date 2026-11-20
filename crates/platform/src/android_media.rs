//! Shared JVM access and hardware discovery for Android's MediaCodec.

use std::sync::OnceLock;

use android_activity::AndroidApp;
use jni::{
    JNIEnv, JavaVM,
    objects::{JIntArray, JObject, JObjectArray, JString, JValue},
};

use crate::android_ui::Result;

static JVM: OnceLock<JavaVM> = OnceLock::new();
pub(super) const FLEXIBLE_420: i32 = 0x7f42_0888;

pub(super) fn initialize(app: &AndroidApp) -> Result<()> {
    if JVM.get().is_none() {
        // SAFETY: AndroidApp exposes the process JVM, which remains live for the lifetime of the
        // native library. JavaVM borrows the handle and does not destroy it when dropped.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
        let _ = JVM.set(vm);
    }
    Ok(())
}

pub(super) fn with_env<T>(f: impl FnOnce(&mut JNIEnv<'_>) -> Result<T>) -> Result<T> {
    let vm = JVM.get().ok_or("Android MediaCodec has no Java VM")?;
    let mut env = vm.attach_current_thread_permanently()?;
    let result = env.with_local_frame(64, f);
    if matches!(env.exception_check(), Ok(true)) {
        let _ = env.exception_clear();
    }
    result
}

pub(super) fn positive(value: i32) -> Result<usize> {
    usize::try_from(value).ok().filter(|&n| n > 0).ok_or_else(|| "Android MediaCodec returned a non-positive size".into())
}

pub(super) fn codec_names(env: &mut JNIEnv<'_>, mime: &str, encoder: bool, color: i32, size: (i32, i32)) -> Result<Vec<String>> {
    // SDK 29 added isHardwareAccelerated. On older versions do not guess from vendor names.
    if env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()? < 29 {
        return Ok(Vec::new());
    }
    let list = env.new_object("android/media/MediaCodecList", "(I)V", &[JValue::Int(0)])?;
    let infos = JObjectArray::from(env.call_method(list, "getCodecInfos", "()[Landroid/media/MediaCodecInfo;", &[])?.l()?);
    let mut names = Vec::new();
    for index in 0..env.get_array_length(&infos)?.min(256) {
        let name = env.with_local_frame(20, |env| {
            let info = env.get_object_array_element(&infos, index)?;
            codec_name(env, &info, mime, encoder, color, size)
        });
        match name {
            Ok(Some(name)) => names.push(name),
            Ok(None) => {}
            Err(err) => {
                if matches!(env.exception_check(), Ok(true)) {
                    let _ = env.exception_clear();
                }
                log::debug!("Could not inspect an Android MediaCodec component: {err}");
            }
        }
        if names.len() == 8 {
            break;
        }
    }
    Ok(names)
}

fn codec_name(env: &mut JNIEnv<'_>, info: &JObject<'_>, mime: &str, encoder: bool, color: i32, size: (i32, i32)) -> Result<Option<String>> {
    if env.call_method(info, "isEncoder", "()Z", &[])?.z()? != encoder
        || !env.call_method(info, "isHardwareAccelerated", "()Z", &[])?.z()?
        || env.call_method(info, "isAlias", "()Z", &[])?.z()?
        || !has_mime(env, info, mime)?
    {
        return Ok(None);
    }
    let mime = env.new_string(mime)?;
    let caps = env
        .call_method(info, "getCapabilitiesForType", "(Ljava/lang/String;)Landroid/media/MediaCodecInfo$CodecCapabilities;", &[JValue::Object(&mime)])?
        .l()?;
    let formats = JIntArray::from(env.get_field(&caps, "colorFormats", "[I")?.l()?);
    let count = env.get_array_length(&formats)?.clamp(0, 256) as usize;
    let mut codes = vec![0; count];
    env.get_int_array_region(&formats, 0, &mut codes)?;
    if !codes.contains(&color) {
        return Ok(None);
    }
    let video = env.call_method(&caps, "getVideoCapabilities", "()Landroid/media/MediaCodecInfo$VideoCapabilities;", &[])?.l()?;
    if video.is_null() || !env.call_method(video, "isSizeSupported", "(II)Z", &[JValue::Int(size.0), JValue::Int(size.1)])?.z()? {
        return Ok(None);
    }
    let name = JString::from(env.call_method(info, "getName", "()Ljava/lang/String;", &[])?.l()?);
    Ok(Some(env.get_string(&name)?.into()))
}

fn has_mime(env: &mut JNIEnv<'_>, info: &JObject<'_>, wanted: &str) -> Result<bool> {
    let types = JObjectArray::from(env.call_method(info, "getSupportedTypes", "()[Ljava/lang/String;", &[])?.l()?);
    for index in 0..env.get_array_length(&types)?.min(64) {
        let found = env.with_local_frame(5, |env| -> Result<bool> {
            let value = JString::from(env.get_object_array_element(&types, index)?);
            let mime: String = env.get_string(&value)?.into();
            Ok(mime.eq_ignore_ascii_case(wanted))
        })?;
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

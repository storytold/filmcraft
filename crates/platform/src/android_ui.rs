//! Android system services used from the native editor.

use android_activity::AndroidApp;
use jni::{
    JNIEnv, JavaVM,
    objects::{JObject, JValue},
};

pub(super) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) fn with_activity<T>(app: &AndroidApp, f: impl FnOnce(&mut JNIEnv, &JObject) -> Result<T>) -> Result<T> {
    // SAFETY: AndroidApp holds the process JVM and the live NativeActivity reference. The
    // wrappers borrow these handles and do not release the JVM or delete the activity reference.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
    let mut env = vm.attach_current_thread()?;
    // SAFETY: AndroidApp documents this as an unowned JNI reference valid for the app lifecycle.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let result = f(&mut env, &activity);
    if matches!(env.exception_check(), Ok(true)) {
        let _ = env.exception_clear();
    }
    result
}

/// Write a private client configuration to the Android clipboard.
pub fn copy_sensitive_text(app: &AndroidApp, text: &str) -> Result<()> {
    with_activity(app, |env, activity| {
        let service = env.new_string("clipboard")?;
        let manager = env.call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&service)])?.l()?;
        if manager.is_null() {
            return Err("Android clipboard is unavailable.".into());
        }
        let label = env.new_string("FilmCraft MCP config")?;
        let text = env.new_string(text)?;
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&text)],
            )?
            .l()?;
        let description = env.call_method(&clip, "getDescription", "()Landroid/content/ClipDescription;", &[])?.l()?;
        let extras = env.new_object("android/os/PersistableBundle", "()V", &[])?;
        let sensitive = env.new_string("android.content.extra.IS_SENSITIVE")?;
        env.call_method(&extras, "putBoolean", "(Ljava/lang/String;Z)V", &[JValue::Object(&sensitive), JValue::Bool(1)])?;
        env.call_method(&description, "setExtras", "(Landroid/os/PersistableBundle;)V", &[JValue::Object(&extras)])?;
        env.call_method(&manager, "setPrimaryClip", "(Landroid/content/ClipData;)V", &[JValue::Object(&clip)])?;
        Ok(())
    })
}

//! Android's "All files access" grant (`MANAGE_EXTERNAL_STORAGE`), reached over JNI.
//!
//! The in-app file browser (`ui_egui::panels::android_file`) would otherwise only ever see the
//! app-private directory. To let a save, export, project or media import reach shared storage
//! (`/storage/emulated/0`), the user must grant "All files access" — a special permission Android
//! never grants silently: the app can only *check* whether it is held and *open the screen* where
//! the user toggles it (Android 11+), or show the ordinary runtime permission dialog (Android 9,
//! whose legacy `WRITE_EXTERNAL_STORAGE` still opens the whole shared tree). Android 10 targets
//! scoped storage, where no grant opens the shared tree, so nothing is offered there. That is all
//! this module does — it never reads or writes storage itself, so the browser's own root-clamping
//! still keeps every answer inside the root the user is browsing.
//!
//! Every JNI call can fail (no JVM, an API level without the method, a device that refuses the
//! intent) and every failure degrades to "not granted" / "nothing happened", logged at worst —
//! never a panic, which this crate's lints deny too.
//!
//! This is the entry crate's one piece of real `unsafe`: `jni`'s entry points to the activity's
//! raw JVM (`JavaVM::from_raw`, `JObject::from_raw`) have no safe form, which is why AGENTS.md
//! §0.3 names this file as the workspace's second `unsafe` exception (alongside `crates/platform`)
//! and confines it here: the module holds the pointers, never lets them leave, and hands
//! [`filmcraft::SharedStorageAccess`] plain closures, so the shared app crates stay unsafe-free.

use jni::objects::{JObject, JValue};
use jni::{JNIEnv, JavaVM};

/// The app's package, as the manifest declares it (the settings intent's URI names it).
const PACKAGE: &str = "ai.storyteller.filmcraft";

/// The activity's JVM and activity reference, captured before the `AndroidApp` moves into
/// eframe. The activity owns both and lives as long as the app, so every later call is valid.
#[derive(Clone, Copy)]
pub struct AndroidAccess {
    vm: *mut jni::sys::JavaVM,
    activity: jni::sys::jobject,
}

// SAFETY: both pointers belong to the activity and are never freed while the app runs, which is
// the only time they are touched (on the single thread that runs the UI); jni's `from_raw`
// adopts them without owning them, so sharing them between the closures handed to the app is
// sound.
unsafe impl Send for AndroidAccess {}
unsafe impl Sync for AndroidAccess {}

/// Capture the handles from the activity. `None` (and a log) when no JVM is attached, in which
/// case the browser simply keeps its app-private root.
pub fn capture(app: &android_activity::AndroidApp) -> Option<AndroidAccess> {
    let vm = app.vm_as_ptr() as *mut jni::sys::JavaVM;
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    if vm.is_null() || activity.is_null() {
        log::warn!("android access: no JVM on the activity; \"All files access\" stays unavailable");
        return None;
    }
    Some(AndroidAccess { vm, activity })
}

impl AndroidAccess {
    /// Run one JNI session on this thread: attach, hand `f` the environment, then make sure no
    /// Java exception is left pending (the JVM would rethrow it into unrelated code later —
    /// checking and clearing is allowed while an exception is pending). `None` when the JVM
    /// could not be reached or `f` erred.
    fn with_env<T>(self, f: impl FnOnce(&mut JNIEnv) -> jni::errors::Result<T>) -> Option<T> {
        // SAFETY: `vm` was checked non-null in `capture` and belongs to the activity (alive for
        // the app's whole lifetime); `from_raw` adopts the raw pointer without owning or
        // freeing it, and the attach guard scopes this thread's attachment to the call.
        let vm = unsafe { JavaVM::from_raw(self.vm) }.ok()?;
        let mut guard = vm.attach_current_thread().ok()?;
        let out = f(&mut *guard).ok();
        if guard.exception_check().unwrap_or(false) {
            let _ = guard.exception_clear();
        }
        out
    }

    /// The activity object for one call: its own (never freed) reference, adopted by
    /// `from_raw` without owning or deleting it.
    fn activity<'local>(self) -> JObject<'local> {
        // SAFETY: the raw pointer is the activity's own reference, valid while the app is; the
        // value is used for this one call only.
        unsafe { JObject::from_raw(self.activity) }
    }

    /// `Ok(true)` when the preceding Java call did not throw; a throw shows as a pending
    /// exception (which is cleared here, since most JNI calls are illegal while one is set).
    fn started(env: &mut JNIEnv) -> jni::errors::Result<bool> {
        if env.exception_check()? {
            env.exception_clear()?;
            return Ok(false);
        }
        Ok(true)
    }

    /// The API level (`Build.VERSION.SDK_INT`), `None` when the JVM could not answer.
    fn sdk(self) -> Option<i32> {
        self.with_env(|env| env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i())
    }

    /// Whether shared storage may be read and written right now: "All files access" on
    /// Android 11+, the legacy storage permission on Android 9. `false` on Android 10 (scoped
    /// storage, no "All files access" page yet) and when unknown: the browser then keeps its
    /// app-private root.
    pub fn shared_storage_granted(self) -> bool {
        match self.sdk() {
            // `Environment.isExternalStorageManager()` exists only on API 30+.
            Some(sdk) if sdk >= 30 => {
                self.with_env(|env| env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?.z()).unwrap_or(false)
            }
            // Android 9: the legacy runtime permission, which still unlocks the whole tree.
            // Android 10 is not offered: this app targets 35, so scoped storage applies there and
            // direct file paths to shared storage are unavailable (`requestLegacyExternalStorage`
            // is ignored for such targets, and cargo-apk cannot declare it anyway).
            Some(28) => self
                .with_env(|env| -> jni::errors::Result<bool> {
                    let perm = env.new_string("android.permission.WRITE_EXTERNAL_STORAGE")?;
                    let act = self.activity();
                    let code = env.call_method(&act, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&perm)])?.i()?;
                    Ok(code == 0)
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Ask the user to grant it: Android 11+ opens this app's "All files access" settings page,
    /// Android 9 shows the runtime permission dialog (the browser re-checks the grant on every
    /// visit, so the answer needs no callback). Nothing is askable on Android 10, where no grant
    /// opens shared storage. Best effort: a device that refuses the intent or an unreachable JVM
    /// just leaves the grant un-granted, logged.
    pub fn ask_shared_storage_grant(self) {
        let asked = match self.sdk() {
            Some(sdk) if sdk >= 30 => self.with_env(|env| -> jni::errors::Result<bool> {
                // uri = Uri.parse("package:<pkg>")
                let uri_text = format!("package:{PACKAGE}");
                let pkg = env.new_string(uri_text.as_str())?;
                let uri = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&pkg)])?.l()?;
                // intent = new Intent(ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri)
                let action: JObject = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?.into();
                let intent =
                    env.new_object("android/content/Intent", "(Ljava/lang/String;Landroid/net/Uri;)V", &[JValue::Object(&action), JValue::Object(&uri)])?;
                // activity.startActivity(intent)
                let act = self.activity();
                env.call_method(&act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
                Self::started(env)
            }),
            Some(28) => self.with_env(|env| -> jni::errors::Result<bool> {
                // The permission dialog: requestPermissions(new String[]{read, write}, 42).
                let arr = env.new_object_array(2, "java/lang/String", &JObject::null())?;
                let perms = ["android.permission.READ_EXTERNAL_STORAGE", "android.permission.WRITE_EXTERNAL_STORAGE"];
                for (i, perm) in perms.into_iter().enumerate() {
                    let s = env.new_string(perm)?;
                    env.set_object_array_element(&arr, i as jni::sys::jsize, &s)?;
                }
                let act = self.activity();
                env.call_method(&act, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::Object(&arr), JValue::Int(42)])?;
                Self::started(env)
            }),
            _ => None,
        };
        if asked != Some(true) {
            log::warn!("android access: could not ask for shared-storage access (the browser keeps its app-private root)");
        }
    }
}

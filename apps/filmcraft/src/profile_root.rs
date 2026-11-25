//! App-owned nightly profile fences; no stable fallback, migration or process environment changes.

use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};

use std::path::{Component, Path, PathBuf};

/// The portable marker `filmcraft_engine::autosave` looks for beside the executable.
const PORTABLE_MARKER: &str = "portable.txt";
/// The folder a portable install keeps its data in, beside that marker.
const PORTABLE_DATA_DIR: &str = "data";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Windows,
    Linux,
    Mac,
}

impl Platform {
    fn current() -> Result<Self, String> {
        match std::env::consts::OS {
            "windows" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Mac),
            _ => Err("nightly-v1 is not defined for this operating system".into()),
        }
    }

    fn ignores_case(self) -> bool {
        self != Self::Linux
    }
}

fn absolute(path: PathBuf, label: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("{label} must be absolute and contain no parent traversal"));
    }
    Ok(path)
}

fn required_base(env: &impl Fn(&str) -> Option<OsString>, key: &str) -> Result<PathBuf, String> {
    let value = env(key).filter(|v| !v.is_empty()).ok_or_else(|| format!("cannot establish the stable {key} boundary"))?;
    absolute(PathBuf::from(value), key)
}

fn xdg_base(env: &impl Fn(&str) -> Option<OsString>, key: &str, fallback: &str) -> Result<PathBuf, String> {
    match env(key).filter(|v| !v.is_empty()) {
        Some(value) => absolute(PathBuf::from(value), key),
        None => Ok(required_base(env, "HOME")?.join(fallback)),
    }
}

/// Resolve existing ancestors before creating anything. A dangling link or inaccessible ancestor
/// is not a missing directory that may be replaced or followed later.
fn canonical_future(path: &Path) -> Result<PathBuf, String> {
    let mut parent = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(parent) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(parent.file_name().ok_or("the profile root has no existing ancestor")?.to_os_string());
                parent = parent.parent().ok_or("the profile root has no existing ancestor")?;
            }
            Err(error) => return Err(format!("cannot inspect the profile boundary: {error}")),
        }
    }
    let mut resolved = std::fs::canonicalize(parent).map_err(|e| format!("cannot resolve the profile boundary: {e}"))?;
    for part in missing.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn overlaps(a: &Path, b: &Path, platform: Platform) -> bool {
    let parts = |p: &Path| {
        p.components()
            .map(|c| {
                let value = c.as_os_str().to_string_lossy().into_owned();
                if platform.ignores_case() { value.to_lowercase() } else { value }
            })
            .collect::<Vec<_>>()
    };
    let (a, b) = (parts(a), parts(b));
    a.starts_with(&b) || b.starts_with(&a)
}

fn installation_boundary(platform: Platform, exe: &Path) -> Result<PathBuf, String> {
    let parent = exe.parent().ok_or("cannot establish the installation boundary")?;
    if platform == Platform::Mac {
        // A profile in Resources is still inside the versioned install, not beside MacOS.
        if let Some(bundle) = exe.ancestors().find(|p| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("app"))) {
            return Ok(bundle.to_path_buf());
        }
    }
    Ok(parent.to_path_buf())
}

fn forbidden_roots(platform: Platform, env: &impl Fn(&str) -> Option<OsString>, exe: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roots = match platform {
        Platform::Windows => {
            // %APPDATA%\FilmCraft is the per-user data directory (autosave::default_data_dir);
            // %LOCALAPPDATA%\FilmCraft is fenced as well, so no future local cache can land there.
            let mut roots =
                ["APPDATA", "LOCALAPPDATA"].into_iter().map(|key| required_base(env, key).map(|p| p.join("FilmCraft"))).collect::<Result<Vec<_>, _>>()?;
            // The MSI's default per-user and per-machine folders (install-policy.wxi). A custom
            // install folder is not known here; the executable's own folder is fenced below.
            roots.push(required_base(env, "LOCALAPPDATA")?.join("Programs").join("ArtCraft").join("FilmCraft"));
            for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(base) = env(key).filter(|v| !v.is_empty()) {
                    roots.push(absolute(PathBuf::from(base), key)?.join("ArtCraft").join("FilmCraft"));
                }
            }
            roots
        }
        Platform::Linux => {
            // The data directory is real; the config, cache and state roots are fenced so a
            // future one cannot point Nightly at stable state. `~/.var/app` is the stable
            // Flatpak's own per-app folder.
            let mut roots =
                [("XDG_DATA_HOME", ".local/share"), ("XDG_CONFIG_HOME", ".config"), ("XDG_CACHE_HOME", ".cache"), ("XDG_STATE_HOME", ".local/state")]
                    .into_iter()
                    .map(|(key, fallback)| xdg_base(env, key, fallback).map(|p| p.join("filmcraft")))
                    .collect::<Result<Vec<_>, _>>()?;
            roots.extend(["/usr/bin/filmcraft", "/usr/local/bin/filmcraft", "/opt/filmcraft"].map(PathBuf::from));
            let data = xdg_base(env, "XDG_DATA_HOME", ".local/share")?;
            // Dev builds write a desktop entry and its hicolor icons into the XDG data dir
            // (apps/filmcraft/src/dev_icon.rs); a Nightly profile is never inside them.
            roots.push(data.join("applications").join("ai.storyteller.filmcraft.desktop"));
            roots.push(data.join("icons").join("hicolor"));
            roots.push(required_base(env, "HOME")?.join(".var/app/ai.storyteller.filmcraft"));
            roots
        }
        Platform::Mac => {
            let home = required_base(env, "HOME")?;
            let library = home.join("Library");
            vec![
                library.join("Application Support/FilmCraft"),
                library.join("Caches/FilmCraft"),
                library.join("Caches/ai.storyteller.filmcraft"),
                library.join("Preferences/FilmCraft"),
                library.join("Preferences/ai.storyteller.filmcraft.plist"),
                library.join("Saved Application State/ai.storyteller.filmcraft.savedState"),
                home.join("Applications/FilmCraft.app"),
                PathBuf::from("/Applications/FilmCraft.app"),
            ]
        }
    };
    roots.push(installation_boundary(platform, exe)?);
    roots.into_iter().map(|p| canonical_future(&p)).collect()
}

fn private_subdirectory(root: &Path, name: impl AsRef<Path>, label: &str, platform: Platform) -> Result<PathBuf, String> {
    let chosen = canonical_future(&absolute(root.to_path_buf(), "the nightly profile root")?)?;
    let child = canonical_future(&chosen.join(name))?;
    if child == chosen || !child.starts_with(&chosen) {
        return Err(format!("the nightly {label} must stay inside its selected profile"));
    }
    // The target's own ancestors decide this, not the profile's: an alias may otherwise point into
    // a Stable portable install that happens to sit inside the selected profile.
    if is_portable_layout(&child, platform) {
        return Err(format!("the nightly {label} must not use a portable FilmCraft installation or its data folder"));
    }
    Ok(child)
}

/// Every existing entry directly in the profile (logs, preferences and so on) must resolve inside
/// it, so a link there cannot point Nightly's writes at another app's data.
fn entries_stay_inside(root: &Path, platform: Platform) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect the nightly profile: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot inspect the nightly profile: {e}"))?;
        private_subdirectory(root, entry.file_name(), "profile entry", platform)?;
    }
    Ok(())
}

/// Recognise the portable data namespace in both the supplied and resolved path. A marker only
/// counts beside a FilmCraft executable (above the chosen folder itself), so an unrelated
/// `portable.txt` higher up does not refuse the profile.
///
/// A `data` component counts only when its parent holds the portable marker: the Nightly Flatpak's
/// own `~/.var/app/…/data/filmcraft-nightly/profile`, and any home with a `data` folder, are
/// ordinary per-user paths, not a portable install's data directory.
fn is_portable_layout(path: &Path, platform: Platform) -> bool {
    path.ancestors().enumerate().any(|(depth, ancestor)| {
        ancestor.join(PORTABLE_MARKER).is_file() && (depth == 0 || ["filmcraft.exe", "filmcraft", "FilmCraft"].iter().any(|exe| ancestor.join(exe).is_file()))
    }) || path.ancestors().any(|ancestor| {
        let named_data = ancestor.file_name().is_some_and(|name| {
            if platform.ignores_case() { name.to_string_lossy().eq_ignore_ascii_case(PORTABLE_DATA_DIR) } else { name == PORTABLE_DATA_DIR }
        });
        named_data && ancestor.parent().is_some_and(|install| install.join(PORTABLE_MARKER).is_file())
    })
}

/// The app's existing exclusive write probe: create and remove a file in the profile.
pub fn ensure_writable(dir: &Path) -> std::io::Result<()> {
    // The name is unique to this validation: two windows may validate the same writable profile at
    // once, and a shared name would make the second `create_new` fail with AlreadyExists.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let probe = dir.join(format!("exclusive-write-probe-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe)?;
    drop(file);
    std::fs::remove_file(probe)
}

/// The writable hook is injected so the boundary tests exercise the real policy without starting
/// the app or changing global state.
pub fn validate(
    root: &Path,
    env: impl Fn(&str) -> Option<OsString>,
    exe: Option<&Path>,
    writable: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, String> {
    validate_for(Platform::current()?, root, env, exe, writable)
}

fn validate_for(
    platform: Platform,
    root: &Path,
    env: impl Fn(&str) -> Option<OsString>,
    exe: Option<&Path>,
    writable: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, String> {
    absolute(root.to_path_buf(), "the nightly profile root")?;
    // The stable data-directory override would point every write outside the private profile.
    if env("FILMCRAFT_DATA_DIR").is_some_and(|v| !v.is_empty()) {
        return Err("nightly-v1 cannot be combined with FILMCRAFT_DATA_DIR".into());
    }
    if platform != Platform::Windows {
        // A native Nightly package must not inherit a Flatpak runtime: its socket namespace could
        // point this profile at another app's instance. The Nightly Flatpak itself is not that
        // case -- its sandbox data is `~/.var/app/ai.storyteller.filmcraft.nightly`, separate from
        // stable -- so exactly the app's own Nightly id is accepted and every other value refused.
        if let Some(id) = env("FLATPAK_ID").filter(|value| !value.is_empty())
            && id.as_os_str() != std::ffi::OsStr::new(filmcraft_ui_egui::os_window::NIGHTLY_PACKAGE_ID)
        {
            return Err("nightly-v1 native packages cannot inherit a Flatpak instance; launch through the native Launcher".into());
        }
    }
    let chosen = canonical_future(root)?;
    if is_portable_layout(root, platform) || is_portable_layout(&chosen, platform) {
        return Err("the nightly root must not use a portable FilmCraft installation or its data folder".into());
    }
    let exe = absolute(exe.ok_or("cannot establish the installation boundary")?.to_path_buf(), "the executable")?;
    let forbidden = forbidden_roots(platform, &env, &exe)?;
    if forbidden.iter().any(|stable| overlaps(&chosen, stable, platform)) {
        return Err("the nightly root must not overlap stable data or an installation".into());
    }
    entries_stay_inside(&chosen, platform)?;
    // Recovery can read, adopt, overwrite and sweep entries, and the log writer rotates files.
    // Never let their aliases leave this private profile or alias portable data inside it.
    private_subdirectory(&chosen, "Recovery", "recovery folder", platform)?;
    private_subdirectory(&chosen, "Logs", "log folder", platform)?;
    let mut create = std::fs::DirBuilder::new();
    create.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        create.mode(0o700);
    }
    create.create(&chosen).map_err(|e| format!("cannot create the nightly profile: {e}"))?;
    let confirmed = std::fs::canonicalize(&chosen).map_err(|e| format!("cannot resolve the created nightly profile: {e}"))?;
    if confirmed != chosen || forbidden.iter().any(|stable| overlaps(&confirmed, stable, platform)) {
        return Err("the nightly profile changed while it was being validated".into());
    }
    private_subdirectory(&confirmed, "Recovery", "recovery folder", platform)?;
    private_subdirectory(&confirmed, "Logs", "log folder", platform)?;
    entries_stay_inside(&confirmed, platform)?;
    writable(&confirmed).map_err(|e| format!("the nightly profile is not writable: {e}"))?;
    Ok(confirmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        root: PathBuf,
        env: BTreeMap<String, OsString>,
        exe: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = loop {
                let candidate = std::env::temp_dir().join(format!("filmcraft-profile-policy-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
                match std::fs::create_dir(&candidate) {
                    Ok(()) => break std::fs::canonicalize(candidate).unwrap(),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create owned fixture: {error}"),
                }
            };
            let home = root.join("home");
            let exe = root.join("install/filmcraft");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, b"fixture executable, never run").unwrap();
            let env = [("HOME", home), ("APPDATA", root.join("roaming")), ("LOCALAPPDATA", root.join("local"))]
                .into_iter()
                .map(|(key, value)| (key.into(), value.into_os_string()))
                .collect();
            Self { root, env, exe }
        }

        fn get(&self, key: &str) -> Option<OsString> {
            self.env.get(key).cloned()
        }

        fn check(&self, platform: Platform, root: &Path) -> Result<PathBuf, String> {
            validate_for(platform, root, |key| self.get(key), Some(&self.exe), ensure_writable)
        }

        fn refuse(&self, platform: Platform, root: &Path) -> String {
            let probed = std::cell::Cell::new(false);
            let result = validate_for(
                platform,
                root,
                |key| self.get(key),
                Some(&self.exe),
                |_| {
                    probed.set(true);
                    Ok(())
                },
            );
            assert!(!probed.get(), "a refused root must never reach the writable probe");
            result.unwrap_err()
        }

        fn home(&self) -> PathBuf {
            PathBuf::from(self.get("HOME").unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn portable_settings_from_another_install_are_refused_before_any_probe() {
        let fixture = Fixture::new();
        let stable_install = fixture.root.join("stable portable install");
        let stable_data = stable_install.join("data");
        std::fs::create_dir_all(&stable_data).unwrap();
        std::fs::write(stable_install.join("portable.txt"), b"owned portable fixture").unwrap();
        std::fs::write(stable_data.join("preferences.json"), b"stable settings must not change").unwrap();
        for platform in [Platform::Windows, Platform::Linux, Platform::Mac] {
            for candidate in [&stable_data, &stable_data.join("nested nightly")] {
                assert!(!fixture.refuse(platform, candidate).is_empty());
                assert!(!stable_data.join("nested nightly").exists());
                assert_eq!(std::fs::read(stable_data.join("preferences.json")).unwrap(), b"stable settings must not change");
            }
        }
        // A portable marker with no FilmCraft executable beside it is someone else's file.
        let unrelated = fixture.root.join("user folder");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(unrelated.join("portable.txt"), b"another app's marker").unwrap();
        assert!(fixture.check(Platform::Windows, &unrelated.join("Launcher/nightly/filmcraft")).is_ok());
        // A merely similar data folder name is not the portable namespace.
        assert!(fixture.check(Platform::Linux, &fixture.root.join("data-backup/nightly")).is_ok());
    }

    #[test]
    fn a_data_folder_beside_no_portable_marker_is_not_a_portable_install() {
        let mut fixture = Fixture::new();
        // The Nightly Flatpak's own sandbox data root is `…/ai.storyteller.filmcraft.nightly/data`,
        // and a home may hold a plain `data` folder: neither is a portable install (#5225 review).
        let plain = fixture.root.join("data/filmcraft-nightly/profile");
        assert_eq!(fixture.check(Platform::Linux, &plain).unwrap(), plain);
        let sandbox = fixture.home().join(".var/app/ai.storyteller.filmcraft.nightly/data/filmcraft-nightly/profile");
        fixture.env.insert("FLATPAK_ID".into(), filmcraft_ui_egui::os_window::NIGHTLY_PACKAGE_ID.into());
        assert_eq!(fixture.check(Platform::Linux, &sandbox).unwrap(), sandbox);
        // Only the Nightly Flatpak's own id may inherit a sandbox; every other one is refused.
        fixture.env.insert("FLATPAK_ID".into(), "ai.storyteller.filmcraft".into());
        assert!(fixture.refuse(Platform::Linux, &sandbox).contains("Flatpak"));
    }

    #[test]
    fn linux_fences_app_xdg_subtrees_but_allows_launcher_sibling() {
        let mut fixture = Fixture::new();
        for key in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            fixture.env.insert(key.into(), fixture.root.join(key).into_os_string());
        }
        for key in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            let stable = PathBuf::from(fixture.get(key).unwrap()).join("filmcraft");
            std::fs::create_dir_all(&stable).unwrap();
            std::fs::write(stable.join("stable-sentinel"), b"stable bytes").unwrap();
            for candidate in [stable.clone(), stable.join("nightly"), stable.parent().unwrap().to_path_buf()] {
                assert!(fixture.refuse(Platform::Linux, &candidate).contains("overlap"));
            }
            assert_eq!(std::fs::read(stable.join("stable-sentinel")).unwrap(), b"stable bytes");
        }
        let data = PathBuf::from(fixture.get("XDG_DATA_HOME").unwrap());
        let nightly = data.join("ArtCraftLauncher/nightly/profiles/filmcraft");
        assert_eq!(fixture.check(Platform::Linux, &nightly).unwrap(), nightly);
        assert_eq!(std::fs::read_dir(nightly).unwrap().count(), 0);
    }

    #[test]
    fn linux_fences_the_dev_desktop_entry_the_icon_theme_and_the_stable_flatpak() {
        let mut fixture = Fixture::new();
        let data = fixture.root.join("xdg-data");
        fixture.env.insert("XDG_DATA_HOME".into(), data.clone().into_os_string());
        for stable in [data.join("applications/ai.storyteller.filmcraft.desktop"), data.join("icons/hicolor")] {
            assert!(fixture.refuse(Platform::Linux, &stable.join("nightly")).contains("overlap"));
        }
        let flatpak = fixture.home().join(".var/app/ai.storyteller.filmcraft");
        for candidate in [flatpak.clone(), flatpak.join("data/nightly")] {
            assert!(fixture.refuse(Platform::Linux, &candidate).contains("overlap"));
        }
        assert!(!flatpak.exists());
        // These are POSIX absolute paths: on a Windows host `/usr/bin/...` has no root and fails the
        // absolute-path check before the fence, so the case only means anything on unix.
        #[cfg(unix)]
        for install in ["/usr/bin/filmcraft", "/usr/local/bin/filmcraft", "/opt/filmcraft"] {
            assert!(fixture.refuse(Platform::Linux, &PathBuf::from(install).join("profile")).contains("overlap"));
        }
        assert!(fixture.check(Platform::Linux, &fixture.home().join(".var/app/ai.storyteller.launcher/nightly")).is_ok());
    }

    #[test]
    fn mac_fences_only_app_library_subtrees_and_the_entire_bundle() {
        let mut fixture = Fixture::new();
        fixture.exe = fixture.root.join("install/FilmCraft Nightly.app/Contents/MacOS/FilmCraft");
        std::fs::create_dir_all(fixture.exe.parent().unwrap()).unwrap();
        std::fs::write(&fixture.exe, b"fixture executable, never run").unwrap();
        let library = fixture.home().join("Library");
        for suffix in [
            "Application Support/FilmCraft",
            "Caches/FilmCraft",
            "Caches/ai.storyteller.filmcraft",
            "Preferences/FilmCraft",
            "Preferences/ai.storyteller.filmcraft.plist",
            "Saved Application State/ai.storyteller.filmcraft.savedState",
        ] {
            let stable = library.join(suffix);
            assert!(fixture.refuse(Platform::Mac, &stable.join("nightly")).contains("overlap"));
            assert!(!stable.exists());
        }
        let inside_resources = fixture.exe.parent().unwrap().parent().unwrap().join("Resources/profile");
        assert!(fixture.refuse(Platform::Mac, &inside_resources).contains("overlap"));
        assert!(!inside_resources.exists());
        for bundle in [fixture.home().join("Applications/FilmCraft.app"), PathBuf::from("/Applications/FilmCraft.app")] {
            assert!(!fixture.refuse(Platform::Mac, &bundle.join("profile")).is_empty());
        }
        let valid = library.join("Application Support/ArtCraftLauncher/nightly/profiles/filmcraft");
        assert_eq!(fixture.check(Platform::Mac, &valid).unwrap(), valid);
    }

    #[test]
    fn windows_roaming_local_and_install_fences_remain_active() {
        let fixture = Fixture::new();
        for key in ["APPDATA", "LOCALAPPDATA"] {
            let stable = PathBuf::from(fixture.get(key).unwrap()).join("FilmCraft");
            assert!(fixture.refuse(Platform::Windows, &stable.join("nightly")).contains("overlap"));
            assert!(!stable.exists());
        }
        assert!(!fixture.refuse(Platform::Windows, &fixture.exe.parent().unwrap().join("profile")).is_empty());
        assert!(fixture.check(Platform::Windows, &fixture.root.join("Launcher/profiles/filmcraft")).is_ok());
    }

    #[test]
    fn windows_default_msi_install_folders_are_fenced() {
        let mut fixture = Fixture::new();
        let local = PathBuf::from(fixture.get("LOCALAPPDATA").unwrap());
        let per_user = local.join("Programs").join("ArtCraft").join("FilmCraft");
        assert!(fixture.refuse(Platform::Windows, &per_user.join("nightly")).contains("overlap"));
        for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            let base = fixture.root.join(format!("machine {key}"));
            fixture.env.insert(key.into(), base.clone().into_os_string());
            let machine = base.join("ArtCraft").join("FilmCraft");
            assert!(fixture.refuse(Platform::Windows, &machine.join("nightly")).contains("overlap"));
            assert!(!machine.exists());
        }
        let sibling = local.join("Programs").join("ArtCraft").join("ArtCraftLauncher").join("nightly").join("filmcraft");
        assert!(fixture.check(Platform::Windows, &sibling).is_ok(), "the Launcher's own Programs sibling is not FilmCraft's install");
    }

    #[test]
    fn overlap_check_handles_ancestor_descendant_and_prefix_lookalikes() {
        let fixture = Fixture::new();
        let root = fixture.home().join("nightly/profiles/filmcraft");
        for platform in [Platform::Windows, Platform::Linux, Platform::Mac] {
            assert!(overlaps(&root, &root.join("Recovery"), platform));
            assert!(overlaps(&root.join("Recovery"), &root, platform));
            assert!(!overlaps(&root, &root.with_file_name("filmcraft-other"), platform));
        }
    }

    #[cfg(unix)]
    #[test]
    fn recovery_and_log_aliases_cannot_redirect_to_stable_missing_or_profile_root_data() {
        let fixture = Fixture::new();
        let stable = fixture.home().join(".local/share/filmcraft/Recovery");
        std::fs::create_dir_all(&stable).unwrap();
        std::fs::write(stable.join("stable-autosave"), b"stable recovery bytes").unwrap();
        for (index, (name, target)) in
            [("Recovery", stable.clone()), ("Logs", stable.clone()), ("Recovery", fixture.root.join("missing target"))].into_iter().enumerate()
        {
            let private = fixture.root.join(format!("nightly-{index}"));
            std::fs::create_dir(&private).unwrap();
            std::os::unix::fs::symlink(&target, private.join(name)).unwrap();
            assert!(!fixture.refuse(Platform::Linux, &private).is_empty(), "{name} must not escape the private profile");
        }
        let private = fixture.root.join("nightly-self");
        std::fs::create_dir(&private).unwrap();
        std::os::unix::fs::symlink(&private, private.join("Recovery")).unwrap();
        assert!(!fixture.refuse(Platform::Linux, &private).is_empty(), "Recovery cannot treat all profile settings as recovery entries");
        assert_eq!(std::fs::read(stable.join("stable-autosave")).unwrap(), b"stable recovery bytes");
    }

    #[cfg(unix)]
    #[test]
    fn recovery_and_log_aliases_cannot_alias_a_portable_install_inside_the_profile() {
        let fixture = Fixture::new();
        for name in ["Recovery", "Logs"] {
            // A Stable portable install nested in the selected profile: the alias target resolves
            // inside the profile, so only the target's own ancestors can refuse it.
            let private = fixture.root.join(format!("nightly-portable-{name}"));
            let portable = private.join("downloads/stable");
            let stable_data = portable.join("data");
            std::fs::create_dir_all(stable_data.join(name)).unwrap();
            std::fs::write(portable.join("portable.txt"), b"owned portable fixture").unwrap();
            std::fs::write(portable.join("filmcraft"), b"fixture executable, never run").unwrap();
            std::fs::write(stable_data.join("preferences.json"), b"stable settings must not change").unwrap();
            std::os::unix::fs::symlink(stable_data.join(name), private.join(name)).unwrap();
            assert!(fixture.refuse(Platform::Linux, &private).contains("portable"), "{name} must not alias portable data");
            assert_eq!(std::fs::read(stable_data.join("preferences.json")).unwrap(), b"stable settings must not change");
        }
        // Ordinary nested directories stay accepted, as before.
        let private = fixture.root.join("nightly-plain");
        std::fs::create_dir_all(private.join("private-recovery")).unwrap();
        std::os::unix::fs::symlink(private.join("private-recovery"), private.join("Recovery")).unwrap();
        assert!(fixture.check(Platform::Linux, &private).is_ok());
    }

    #[test]
    fn the_write_probe_does_not_collide_with_another_validation() {
        let fixture = Fixture::new();
        let dir = fixture.root.join("probe");
        std::fs::create_dir(&dir).unwrap();
        // A probe another window left in this profile must not refuse this launch.
        std::fs::write(dir.join("exclusive-write-probe"), b"another validation").unwrap();
        ensure_writable(&dir).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "this validation left no probe behind");
    }

    #[cfg(unix)]
    #[test]
    fn aliases_inside_the_private_profile_keep_it_usable() {
        let fixture = Fixture::new();
        let private = fixture.root.join("nightly-private");
        let recovery = private.join("private-recovery");
        let logs = private.join("private-logs");
        std::fs::create_dir_all(&recovery).unwrap();
        std::fs::create_dir_all(&logs).unwrap();
        std::os::unix::fs::symlink(&recovery, private.join("Recovery")).unwrap();
        std::os::unix::fs::symlink(&logs, private.join("Logs")).unwrap();
        assert!(fixture.check(Platform::Linux, &private).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn any_profile_entry_alias_must_stay_inside_the_profile() {
        let fixture = Fixture::new();
        let stable = fixture.home().join(".local/share/filmcraft/effect-presets.json");
        std::fs::create_dir_all(stable.parent().unwrap()).unwrap();
        std::fs::write(&stable, b"stable preset bytes").unwrap();
        for name in ["preferences.json", "workspaces.json", "Media Cache"] {
            let private = fixture.root.join(format!("nightly-{name}"));
            std::fs::create_dir(&private).unwrap();
            std::os::unix::fs::symlink(&stable, private.join(name)).unwrap();
            assert!(!fixture.refuse(Platform::Linux, &private).is_empty(), "{name} must not lead outside the profile");
        }
        let private = fixture.root.join("nightly-inside");
        std::fs::create_dir_all(private.join("Cache")).unwrap();
        std::fs::write(private.join("preferences.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(private.join("Cache"), private.join("Media Cache")).unwrap();
        assert!(fixture.check(Platform::Linux, &private).is_ok(), "regular files and links that stay inside are fine");
        assert_eq!(std::fs::read(&stable).unwrap(), b"stable preset bytes");
    }

    #[test]
    fn ambiguous_environment_and_non_native_sandbox_are_refused_before_creation() {
        let mut fixture = Fixture::new();
        let candidate = fixture.home().join("nightly/profiles/filmcraft");
        fixture.env.insert("XDG_CACHE_HOME".into(), "relative/cache".into());
        assert!(fixture.refuse(Platform::Linux, &candidate).contains("absolute"));
        fixture.env.remove("XDG_CACHE_HOME");
        fixture.env.insert("FILMCRAFT_DATA_DIR".into(), fixture.home().join("custom-stable").into_os_string());
        assert!(fixture.refuse(Platform::Linux, &candidate).contains("FILMCRAFT_DATA_DIR"));
        fixture.env.remove("FILMCRAFT_DATA_DIR");
        fixture.env.insert("FLATPAK_ID".into(), "ai.storyteller.filmcraft".into());
        assert!(fixture.refuse(Platform::Linux, &candidate).contains("Flatpak"));
        fixture.env.insert("FLATPAK_ID".into(), "ai.storyteller.filmcraft.nightly-v1".into());
        assert!(fixture.refuse(Platform::Linux, &candidate).contains("Flatpak"));
        // The Nightly Flatpak itself is a native Nightly: its sandbox is already separate.
        fixture.env.insert("FLATPAK_ID".into(), "ai.storyteller.filmcraft.nightly".into());
        let in_the_nightly_sandbox = fixture.home().join("nightly-flatpak/profile");
        assert!(fixture.check(Platform::Linux, &in_the_nightly_sandbox).is_ok(), "the Nightly Flatpak must start");
        fixture.env.remove("FLATPAK_ID");
        fixture.env.remove("HOME");
        assert!(fixture.refuse(Platform::Linux, &candidate).contains("HOME"));
        assert!(!candidate.exists());
    }

    #[test]
    fn failed_write_probe_never_falls_back_to_stable() {
        let fixture = Fixture::new();
        let candidate = fixture.home().join("nightly-profile");
        let refused = validate_for(Platform::Linux, &candidate, |key| fixture.get(key), Some(&fixture.exe), |_| Err(std::io::Error::other("read-only")));
        assert!(refused.unwrap_err().contains("not writable"));
        // The directory was created (and is empty), but nothing fell back to a stable location.
        assert_eq!(std::fs::read_dir(&candidate).unwrap().count(), 0);
        assert!(!fixture.home().join(".local/share/filmcraft").exists());
    }

    #[test]
    fn created_profile_is_owner_only_on_unix() {
        let fixture = Fixture::new();
        let private = fixture.check(Platform::Linux, &fixture.home().join("nightly/private")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert!(private.is_dir());
    }
}

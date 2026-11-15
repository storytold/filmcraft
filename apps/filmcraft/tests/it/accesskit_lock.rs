//! The lockfile must not resolve `accesskit_macos` below 0.27.2: 0.27.1 crashes FilmCraft on quit
//! on Macs with a Touch Bar.
//!
//! eframe's `accesskit` feature gives the window's `WinitView` an AccessKit subclass. When the
//! window becomes key, AppKit's Touch Bar finder observes the view's `nextResponder` through
//! key-value observing, which subclasses the view again (`NSKVONotifying_…`). In 0.27.1 the
//! adapter's `Drop`, run when eframe destroys the window on quit, resets the view to `WinitView`
//! and so throws away that observation; on the next display cycle AppKit's removal of the observer
//! raises "Cannot remove an observer … for the key path "nextResponder" from <WinitView …> because
//! it is not registered as an observer", and `_crashOnException:` ends the process with SIGILL.
//! 0.27.2 resets the class only while it is still AccessKit's own subclass.
//!
//! The behavioural reproduction (add a `nextResponder` observer, drop the adapter, remove the
//! observer) needs `unsafe` Objective-C calls, which the workspace forbids, so this test pins the
//! fixed version instead.

/// The `version` of the `name` package in a `Cargo.lock`, if it is there exactly once.
fn locked_version(lock: &str, name: &str) -> Result<(u64, u64, u64), String> {
    let mut found = Vec::new();
    for package in lock.split("[[package]]") {
        let mut lines = package.lines().map(str::trim);
        if lines.clone().any(|l| l == format!("name = \"{name}\"")) {
            let version = lines.find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).ok_or(format!("{name} has no version"))?;
            found.push(version.to_string());
        }
    }
    let [version] = found.as_slice() else { return Err(format!("{name} is locked {} times: {found:?}", found.len())) };
    let parts: Vec<u64> = version.split(['.', '-', '+']).take(3).map(str::parse).collect::<Result<_, _>>().map_err(|e| format!("{name} {version}: {e}"))?;
    let [major, minor, patch] = parts.as_slice() else { return Err(format!("{name} {version} is not major.minor.patch")) };
    Ok((*major, *minor, *patch))
}

#[test]
fn accesskit_macos_keeps_appkit_key_value_observers_on_quit() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock");
    let lock = std::fs::read_to_string(path).expect("read the workspace Cargo.lock");
    let version = locked_version(&lock, "accesskit_macos").expect("accesskit_macos in Cargo.lock");
    assert!(version >= (0, 27, 2), "accesskit_macos {version:?} resets the view's class on drop and crashes FilmCraft on quit; need 0.27.2 or later");
}

#[test]
fn locked_version_reads_one_package() {
    let lock = "[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"accesskit_macos\"\nversion = \"0.27.10\"\nsource = \"registry\"\n";
    assert_eq!(locked_version(lock, "accesskit_macos"), Ok((0, 27, 10)));
    assert!(locked_version(lock, "missing").is_err());
    let twice = format!("{lock}\n[[package]]\nname = \"accesskit_macos\"\nversion = \"0.26.0\"\n");
    assert!(locked_version(&twice, "accesskit_macos").is_err());
}

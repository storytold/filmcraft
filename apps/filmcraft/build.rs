//! Windows only: embed the app icon and version info (VERSIONINFO) into `filmcraft.exe`, so it
//! shows in Explorer, the taskbar, the Start menu and Alt-Tab. Also selects the build channel.
//!
//! On every other target the resource step does nothing (`winresource` is only a build-dependency
//! on Windows hosts). A missing resource compiler is a warning, unless `FILMCRAFT_REQUIRE_WINRES=1`
//! (for release and nightly builds) turns it into an error.

// `windows_icon_path` is only called by the `#[cfg(windows)]` resource step below, so a Linux or
// macOS `cargo clippy -- -D warnings` would fail on it without this.
#[cfg_attr(not(windows), allow(dead_code))]
#[path = "src/build_channel.rs"]
mod build_channel;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/filmcraft.ico");
    println!("cargo:rerun-if-changed=../../assets/app-icon/filmcraft-nightly.ico");
    println!("cargo:rerun-if-env-changed=FILMCRAFT_REQUIRE_WINRES");
    println!("cargo:rerun-if-env-changed=FILMCRAFT_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=FILMCRAFT_VERSION");
    println!("cargo:rustc-check-cfg=cfg(filmcraft_nightly_v1)");
    let nightly = match std::env::var("FILMCRAFT_BUILD_CHANNEL").as_deref() {
        Ok("nightly-v1") => {
            println!("cargo:rustc-cfg=filmcraft_nightly_v1");
            true
        }
        // An empty value (an unset CI input) means the default stable build.
        Ok("stable" | "") | Err(std::env::VarError::NotPresent) => false,
        _ => {
            eprintln!("unsupported FILMCRAFT_BUILD_CHANNEL (expected stable or nightly-v1)");
            std::process::exit(1);
        }
    };
    println!("cargo:rerun-if-env-changed=FILMCRAFT_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=FILMCRAFT_BUILD_DATE");
    // The version the binary reports: the exact nightly feed identity, or the workspace version.
    let built_version = if nightly {
        let version = match std::env::var("FILMCRAFT_VERSION") {
            Ok(version) if build_channel::is_nightly_version(&version) => version,
            _ => {
                eprintln!("nightly-v1 requires an explicit FILMCRAFT_VERSION feed identity");
                std::process::exit(1);
            }
        };
        // The feed identity names the commit and the original run's date: refuse a build whose
        // prepared values do not agree, so a rebuilt or relabelled tree cannot ship as Nightly.
        let sha = std::env::var("FILMCRAFT_BUILD_SHA").unwrap_or_default();
        let date = std::env::var("FILMCRAFT_BUILD_DATE").unwrap_or_default();
        let hex = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let digits = |s: &str| s.len() == 10 && s.bytes().all(|b| b.is_ascii_digit() || b == b'-');
        let version_date = version.split("-nightly.").nth(1).and_then(|rest| rest.split('.').next()).unwrap_or_default();
        if !hex(&sha) || sha.get(..12) != version.rsplit(".g").next() || !digits(&date) || date.replace('-', "") != version_date {
            eprintln!("nightly-v1 requires FILMCRAFT_BUILD_SHA and FILMCRAFT_BUILD_DATE matching FILMCRAFT_VERSION");
            std::process::exit(1);
        }
        println!("cargo:rustc-env=FILMCRAFT_BUILT_COMMIT={sha}");
        version
    } else {
        std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
    };
    println!("cargo:rustc-env=FILMCRAFT_BUILT_VERSION={built_version}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    windows::embed_resources(nightly, &built_version);
}

#[cfg(windows)]
mod windows {
    pub fn embed_resources(nightly: bool, version: &str) {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(super::build_channel::windows_icon_path(nightly))
            .set("ProductName", if nightly { "FilmCraft Nightly" } else { "FilmCraft" })
            .set("FileDescription", if nightly { "FilmCraft Nightly video editor" } else { "FilmCraft video editor" })
            .set("LegalCopyright", "Copyright (c) the FilmCraft contributors. MIT OR Apache-2.0.")
            .set("OriginalFilename", "filmcraft.exe")
            .set("InternalName", "filmcraft");
        if nightly {
            // The Launcher verifies the PE ProductVersion against the exact feed identity, not FileVersion.
            res.set("ProductVersion", version);
        }
        if let Err(e) = res.compile() {
            if std::env::var_os("FILMCRAFT_REQUIRE_WINRES").is_some() {
                panic!("embedding Windows resources failed: {e}");
            }
            println!("cargo:warning=filmcraft.exe built without icon/version resources: {e}");
        }
    }
}

/// Cross-compiling for Windows from macOS or Linux: `winresource` is not available on this host.
#[cfg(not(windows))]
mod windows {
    pub fn embed_resources(_nightly: bool, _version: &str) {
        // The release and nightly builds require the icon and the PE `ProductVersion` the Launcher
        // verifies, so a cross-build that cannot embed them fails instead of shipping without.
        if std::env::var_os("FILMCRAFT_REQUIRE_WINRES").is_some() {
            panic!("FILMCRAFT_REQUIRE_WINRES=1 needs a Windows host: a cross-compiled filmcraft.exe has no icon or ProductVersion");
        }
        println!("cargo:warning=filmcraft.exe built without icon/version resources (cross-compiled from a non-Windows host)");
    }
}

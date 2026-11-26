//! Launcher-owned nightly-v1 bootstrap. Selected before logging, updates, fonts or saved state.
//! Stable launches retain their original arguments and directory policy. No environment mutation.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

pub const VERSION: &str = "nightly-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Profile {
    Stable,
    NightlyV1(PathBuf),
}

impl Profile {
    pub fn is_nightly(&self) -> bool {
        matches!(self, Self::NightlyV1(_))
    }

    pub fn root(&self) -> Option<&Path> {
        match self {
            Self::Stable => None,
            Self::NightlyV1(root) => Some(root),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub profile: Profile,
    pub args: Vec<OsString>,
}

static PROFILE: OnceLock<Profile> = OnceLock::new();

pub fn current() -> &'static Profile {
    PROFILE.get().unwrap_or(&Profile::Stable)
}

pub fn install(profile: Profile) -> Result<(), String> {
    PROFILE.set(profile).map_err(|_| "the launch profile was already selected".into())
}

/// Recognize only the agreed literal selector/root pair before `--`. Everything after it is
/// a file, even when it looks like a profile flag. No filesystem or startup hook runs here.
pub fn parse(args: Vec<OsString>) -> Result<Launch, String> {
    let mut selector = None;
    let mut root = None;
    let mut other = Vec::new();
    let mut files = Vec::new();
    let mut iter = args.iter();
    let mut delimiter = false;
    while let Some(arg) = iter.next() {
        if arg == "--" {
            delimiter = true;
            files.extend(iter.cloned());
            break;
        }
        if arg == "--launch-profile" || arg == "--profile-root" {
            let slot = if arg == "--launch-profile" { &mut selector } else { &mut root };
            if slot.is_some() {
                return Err("the profile selector and root must each occur exactly once".into());
            }
            let value = iter.next().filter(|v| !v.is_empty()).ok_or("the profile selector/root needs a value")?;
            if value.to_string_lossy().starts_with("--") {
                return Err("the profile selector/root needs a value".into());
            }
            *slot = Some(value.clone());
        } else if arg.to_string_lossy().starts_with("--launch-profile=") || arg.to_string_lossy().starts_with("--profile-root=") {
            return Err("the nightly protocol requires separate literal selector/root argument tokens".into());
        } else {
            other.push(arg.clone());
        }
    }
    match (selector, root) {
        (None, None) => Ok(Launch { profile: Profile::Stable, args }),
        (Some(version), Some(root)) if version == VERSION => {
            if !delimiter || !other.is_empty() {
                return Err("nightly-v1 accepts only its selector/root followed by -- and file names".into());
            }
            let root = PathBuf::from(root);
            if !root.is_absolute() || root.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err("the nightly profile root must be absolute and contain no parent traversal".into());
            }
            Ok(Launch { profile: Profile::NightlyV1(root), args: std::iter::once(OsString::from("--")).chain(files).collect() })
        }
        (Some(_), Some(_)) => Err("unsupported launch profile (expected nightly-v1)".into()),
        _ => Err("--launch-profile and --profile-root must be supplied together".into()),
    }
}

/// The Launcher's version check: the selector/root pair followed by exactly `--version` (or `-V`),
/// answered like a plain version query. Only [`parse`]'s lexical checks apply; the root is never
/// validated or touched, and `-- --version` stays a file name.
pub fn version_query(args: &[OsString]) -> bool {
    let Some((last, pair)) = args.split_last() else { return false };
    if last != "--version" && last != "-V" {
        return false;
    }
    let lexical = pair.iter().cloned().chain(std::iter::once(OsString::from("--"))).collect();
    matches!(parse(lexical), Ok(Launch { profile: Profile::NightlyV1(_), args }) if args.len() == 1)
}

/// The graphics retry is signalled by its process-local relaunch marker
/// (`FILMCRAFT_GRAPHICS_RELAUNCHED`) and re-executes this program with an internal
/// `--graphics <mode>` before the file delimiter. Ordinary Launcher calls still use the strict
/// selector/root protocol; this is not authentication.
pub fn parse_startup(args: Vec<OsString>, compatibility_retry: bool) -> Result<Launch, String> {
    let Some(mode) = compatibility_retry.then(|| internal_graphics(&args)).flatten() else { return parse(args) };
    let mut launch = parse(without_graphics(&args))?;
    if launch.profile.is_nightly() {
        launch.args.splice(0..0, [OsString::from("--graphics"), mode]);
        Ok(launch)
    } else {
        // Stable's argument order and parser remain unchanged.
        parse(args)
    }
}

/// The `--graphics <mode>` pair the retry inserted before the file delimiter, if it is there once.
fn internal_graphics(args: &[OsString]) -> Option<OsString> {
    let delimiter = args.iter().position(|arg| arg == "--").unwrap_or(args.len());
    let mut found = None;
    let mut iter = args[..delimiter].iter();
    while let Some(arg) = iter.next() {
        if arg == "--graphics" {
            if found.is_some() {
                return None;
            }
            found = Some(iter.next()?.clone());
        }
    }
    found
}

/// `args` without the internal `--graphics <mode>` pair before the file delimiter. Everything
/// after `--` is a file name, even one spelled `--graphics`.
fn without_graphics(args: &[OsString]) -> Vec<OsString> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            out.push(arg.clone());
            out.extend(iter.by_ref().cloned());
        } else if arg == "--graphics" {
            let _ = iter.next();
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// Validation finishes before the caller's first startup effect. Stable never calls the nightly
/// root validator, so its old portable/config resolution stays unchanged.
pub fn prepare(mut launch: Launch, validate: impl FnOnce(&Path) -> Result<PathBuf, String>) -> Result<Launch, String> {
    if let Profile::NightlyV1(root) = &launch.profile {
        launch.profile = Profile::NightlyV1(validate(root)?);
    }
    Ok(launch)
}

/// Resolve the selected private root before the caller's first startup effect.
pub fn validate_root(root: &Path, env: impl Fn(&str) -> Option<OsString>, exe: Option<&Path>) -> Result<PathBuf, String> {
    crate::profile_root::validate(root, env, exe, crate::profile_root::ensure_writable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    fn absolute() -> &'static str {
        if cfg!(windows) { r"C:\NightlyData\profiles\filmcraft" } else { "/nightly-data/profiles/filmcraft" }
    }
    fn nightly() -> Vec<OsString> {
        args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--"])
    }

    #[test]
    fn internal_graphics_retry_preserves_the_nightly_root_and_literal_files() {
        let mut argv = args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--graphics", "dx12", "--"]);
        argv.extend(args(&["project with spaces.fcproj", "--graphics", "--profile-root"]));
        let launch = parse_startup(argv.clone(), true).unwrap();
        assert_eq!(launch.profile, Profile::NightlyV1(PathBuf::from(absolute())));
        assert_eq!(launch.args, args(&["--graphics", "dx12", "--", "project with spaces.fcproj", "--graphics", "--profile-root"]));
        assert!(parse_startup(argv, false).is_err(), "an ordinary invocation must not gain extra protocol flags");
    }

    #[test]
    fn retry_cannot_bypass_invalid_profiles_or_change_stable_arguments() {
        let stable = args(&["--graphics", "dx12", "--new-instance", "stable.fcproj"]);
        assert_eq!(parse_startup(stable.clone(), true).unwrap(), Launch { profile: Profile::Stable, args: stable });
        let mut duplicate = args(&["--graphics", "dx12", "--graphics", "software"]);
        duplicate.extend(nightly());
        assert!(parse_startup(duplicate, true).is_err());
        assert!(parse_startup(args(&["--graphics", "dx12", "--launch-profile", VERSION, "--profile-root", "relative", "--"]), true).is_err());
    }

    #[test]
    fn stable_arguments_and_directory_policy_are_unchanged() {
        for argv in [args(&[]), args(&["--new-instance", "a.fcproj"]), args(&["--", "--launch-profile", "file.fcproj"])] {
            let launch = prepare(parse(argv.clone()).unwrap(), |_| panic!("stable must never validate a nightly root")).unwrap();
            assert_eq!(launch, Launch { profile: Profile::Stable, args: argv });
        }
    }

    #[test]
    fn exact_version_and_root_are_parsed_without_reinterpreting_file_names() {
        let mut argv = nightly();
        argv.extend(args(&["--profile-root", "project with spaces.fcproj"]));
        let launch = parse(argv).unwrap();
        assert_eq!(launch.profile, Profile::NightlyV1(PathBuf::from(absolute())));
        assert_eq!(launch.args, args(&["--", "--profile-root", "project with spaces.fcproj"]));
    }

    #[test]
    fn missing_duplicate_unknown_and_conflicting_profile_arguments_are_refused() {
        for argv in [
            args(&["--launch-profile", VERSION]),
            args(&["--profile-root", absolute()]),
            args(&["--launch-profile", "future-v2", "--profile-root", absolute(), "--"]),
            args(&["--launch-profile", VERSION, "--launch-profile", VERSION, "--profile-root", absolute(), "--"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--profile-root", absolute(), "--"]),
            args(&["--launch-profile", VERSION, "--profile-root", "relative", "--"]),
            args(&["--launch-profile", VERSION, "--profile-root", "", "--"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--new-instance", "--"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute()]),
        ] {
            assert!(parse(argv).is_err());
        }
    }

    #[test]
    fn the_launcher_version_check_is_a_version_query_only_in_its_exact_form() {
        let pair = &["--launch-profile", VERSION, "--profile-root", absolute()];
        let reversed = &["--profile-root", absolute(), "--launch-profile", VERSION];
        for flag in ["--version", "-V"] {
            for form in [pair, reversed] {
                let mut argv = args(form);
                argv.push(flag.into());
                assert!(version_query(&argv), "{argv:?}");
            }
        }
        for argv in [
            args(&["--version"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--", "--version"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--version", "--"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--new-instance", "--version"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--version", "--version"]),
            args(&["--version", "--launch-profile", VERSION, "--profile-root", absolute()]),
            args(&["--launch-profile", "future-v2", "--profile-root", absolute(), "--version"]),
            args(&["--launch-profile", VERSION, "--profile-root", "relative", "--version"]),
            args(&["--launch-profile", VERSION, "--version"]),
            args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--help"]),
        ] {
            assert!(!version_query(&argv), "{argv:?}");
        }
        // Outside the query the strict protocol still refuses the version flag.
        assert!(parse(args(&["--launch-profile", VERSION, "--profile-root", absolute(), "--version"])).is_err());
    }

    #[test]
    fn failed_root_validation_never_reaches_startup_or_falls_back_to_stable() {
        let reached = Cell::new(false);
        let result = prepare(parse(nightly()).unwrap(), |_| Err("read-only or unsafe profile".into())).map(|_| reached.set(true));
        assert!(result.is_err());
        assert!(!reached.get());
    }

    #[test]
    fn successful_root_validation_keeps_the_persistent_root_not_the_binary_tag() {
        let checked = PathBuf::from(absolute()).join("checked");
        let launch = prepare(parse(nightly()).unwrap(), |_| Ok(checked.clone())).unwrap();
        assert_eq!(launch.profile.root(), Some(checked.as_path()));
        assert!(launch.profile.is_nightly());
    }
}

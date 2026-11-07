//! Finding the newest release and the checksum of the file to download.
//!
//! No API token, no JSON and no rate limit: GitHub redirects `/releases/latest` to the newest
//! published, non-pre-release tag, and every release carries a `SHA256SUMS.txt` (see
//! `docs/releasing.md`). Both are fetched over HTTPS from `github.com/storytold/filmcraft` only.

use crate::{Result, UpdateError};
use semver::Version;

/// `https://github.com/<repo>/releases/latest`, which redirects to `…/releases/tag/vX.Y.Z`.
pub const LATEST_URL: &str = "https://github.com/storytold/filmcraft/releases/latest";
/// Every asset URL must start with this; redirects to GitHub's CDN happen after the first hop.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/storytold/filmcraft/releases/download/";
/// The checksum list published with each release.
pub const SUMS_NAME: &str = "SHA256SUMS.txt";
const TAG_PATH_PREFIX: &str = "/storytold/filmcraft/releases/tag/";

/// A release that can be downloaded for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub archive_name: String,
    pub archive_url: String,
    /// Lower-case hex SHA-256 from the release's `SHA256SUMS.txt`.
    pub sha256: String,
}

/// Version from the `Location` of the `/releases/latest` redirect. Anything that is not a plain
/// `vX.Y.Z` tag of this repository (a login page, another repo, a pre-release) is rejected.
pub fn version_from_location(location: &str) -> Result<Version> {
    let path = if let Some(rest) = location.strip_prefix("https://github.com") {
        rest
    } else if location.starts_with('/') {
        location
    } else {
        return Err(UpdateError::Metadata(format!("unexpected redirect target {location:?}")));
    };
    let tag = path.strip_prefix(TAG_PATH_PREFIX).ok_or_else(|| UpdateError::Metadata("the latest-release redirect does not point at a release tag".into()))?;
    let tag = tag.split(['?', '#']).next().unwrap_or(tag);
    let number = tag.strip_prefix('v').ok_or_else(|| UpdateError::Metadata(format!("tag {tag:?} does not start with v")))?;
    let version = Version::parse(number).map_err(|e| UpdateError::Metadata(format!("tag {tag:?}: {e}")))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(UpdateError::Metadata(format!("{version} is a pre-release")));
    }
    Ok(version)
}

/// The URL of `name` in release `version`.
pub fn asset_url(version: &Version, name: &str) -> String {
    format!("{DOWNLOAD_PREFIX}v{version}/{name}")
}

/// True when `url` is an HTTPS asset URL of this repository, with nothing sneaky in it.
pub fn is_allowed_asset_url(url: &str) -> bool {
    url.starts_with(DOWNLOAD_PREFIX) && !url.contains("..") && !url.contains('@') && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The SHA-256 listed for `name` in a `SHA256SUMS.txt` (`<64 hex>  <name>` or `<64 hex> *<name>`).
pub fn sha256_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, rest) = line.trim_end_matches('\r').split_once(char::is_whitespace)?;
        let file = rest.trim_start().trim_start_matches('*');
        let file = file.strip_prefix("./").unwrap_or(file);
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_latest_redirect() {
        let v = version_from_location("https://github.com/storytold/filmcraft/releases/tag/v0.7.3").unwrap();
        assert_eq!(v, Version::new(0, 7, 3));
        let v = version_from_location("/storytold/filmcraft/releases/tag/v1.0.0?x=1").unwrap();
        assert_eq!(v, Version::new(1, 0, 0));
    }

    #[test]
    fn rejects_foreign_or_odd_redirects() {
        for bad in [
            "https://evil.example/storytold/filmcraft/releases/tag/v9.9.9",
            "https://github.com/someone/else/releases/tag/v9.9.9",
            "https://github.com/storytold/filmcraft/releases",
            "https://github.com/storytold/filmcraft/releases/tag/0.7.3",
            "https://github.com/storytold/filmcraft/releases/tag/v0.7.3-rc.1",
            "https://github.com/storytold/filmcraft/releases/tag/vbanana",
            "http://github.com/storytold/filmcraft/releases/tag/v0.7.3",
            "",
        ] {
            assert!(version_from_location(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn asset_urls_are_pinned_to_the_repo() {
        let u = asset_url(&Version::new(0, 7, 3), "filmcraft-0.7.3-linux-x86_64.tar.gz");
        assert!(is_allowed_asset_url(&u));
        assert!(!is_allowed_asset_url("https://github.com/other/repo/releases/download/v1/x.tar.gz"));
        assert!(!is_allowed_asset_url("http://github.com/storytold/filmcraft/releases/download/v1/x"));
        assert!(!is_allowed_asset_url("https://github.com/storytold/filmcraft/releases/download/../../x"));
        assert!(!is_allowed_asset_url("https://github.com/storytold/filmcraft/releases/download/v1/x y"));
    }

    #[test]
    fn reads_sha256sums_in_both_styles() {
        let h1 = "a".repeat(64);
        let h2 = "B".repeat(64);
        let sums = format!("{h1}  filmcraft-0.7.3-linux-x86_64.tar.gz\r\n{h2} *filmcraft-0.7.3-linux-aarch64.tar.gz\nnot a line\n");
        assert_eq!(sha256_for(&sums, "filmcraft-0.7.3-linux-x86_64.tar.gz"), Some(h1));
        assert_eq!(sha256_for(&sums, "filmcraft-0.7.3-linux-aarch64.tar.gz"), Some("b".repeat(64)));
        assert_eq!(sha256_for(&sums, "missing.tar.gz"), None);
        assert_eq!(sha256_for("abc  filmcraft.tar.gz", "filmcraft.tar.gz"), None, "short hashes are ignored");
    }
}

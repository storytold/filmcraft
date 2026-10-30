//! HTTPS access: the latest-release lookup, the checksum list and the guarded archive download.
//!
//! Pure-Rust TLS (rustls + RustCrypto) with the operating system's certificate verifier, HTTPS
//! only (a redirect to plain HTTP is refused), bounded timeouts and hard size caps. A captive
//! portal that answers with an HTML login page fails the redirect / checksum checks and the
//! update is silently skipped.

use crate::install::Target;
use crate::release::{self, Release};
use crate::{Result, UpdateError};
use semver::Version;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Largest archive we will download. Real tarballs are tens of megabytes.
pub const MAX_ARCHIVE_BYTES: u64 = 600 * 1024 * 1024;
/// Largest `SHA256SUMS.txt` we will read.
const MAX_SUMS_BYTES: u64 = 1024 * 1024;

fn builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .https_only(true)
        .user_agent(concat!("filmcraft-updater/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
}

/// Agent for short metadata requests (follows redirects, 30 s total).
fn metadata_agent() -> ureq::Agent {
    builder().timeout_global(Some(Duration::from_secs(30))).build().into()
}

/// Agent that reports a redirect instead of following it (to read `/releases/latest`'s target).
fn no_redirect_agent() -> ureq::Agent {
    builder().max_redirects(0).http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into()
}

/// Agent for the archive: a generous overall limit, since a stalled link must still end.
fn download_agent() -> ureq::Agent {
    builder().timeout_global(Some(Duration::from_secs(20 * 60))).build().into()
}

fn net_err(what: &str, e: impl std::fmt::Display) -> UpdateError {
    UpdateError::Network(format!("{what}: {e}"))
}

/// The newest published stable version, from the redirect of `/releases/latest`.
pub fn latest_version() -> Result<Version> {
    let resp = no_redirect_agent().get(release::LATEST_URL).call().map_err(|e| net_err("latest release", e))?;
    if !resp.status().is_redirection() {
        return Err(UpdateError::Metadata(format!("latest release answered HTTP {} instead of a redirect", resp.status().as_u16())));
    }
    let location =
        resp.headers().get("location").and_then(|v| v.to_str().ok()).ok_or_else(|| UpdateError::Metadata("latest release redirect has no Location".into()))?;
    release::version_from_location(location)
}

/// Resolve the download for `version` on `target`: fetch its `SHA256SUMS.txt` and pick the line for
/// this machine's archive.
pub fn fetch_release(version: &Version, target: &Target) -> Result<Release> {
    let name = target.archive_name(version);
    let sums_url = release::asset_url(version, release::SUMS_NAME);
    let archive_url = release::asset_url(version, &name);
    if !release::is_allowed_asset_url(&sums_url) || !release::is_allowed_asset_url(&archive_url) {
        return Err(UpdateError::Metadata("asset URL outside the release host allowlist".into()));
    }
    let resp = metadata_agent().get(&sums_url).call().map_err(|e| net_err(release::SUMS_NAME, e))?;
    let sums = resp.into_body().with_config().limit(MAX_SUMS_BYTES).read_to_string().map_err(|e| net_err(release::SUMS_NAME, e))?;
    let sha256 = release::sha256_for(&sums, &name)
        .ok_or_else(|| UpdateError::Metadata(format!("{name} is not listed in {} (no build for this machine yet?)", release::SUMS_NAME)))?;
    Ok(Release { version: version.clone(), archive_name: name, archive_url, sha256 })
}

/// Download `release` into `dest` (a `.part` file is renamed into place only after the SHA-256 matches).
/// `progress(done, total)` returns `false` to cancel.
pub fn download_release(release: &Release, dest: &Path, progress: &mut dyn FnMut(u64, Option<u64>) -> bool) -> Result<()> {
    if !release::is_allowed_asset_url(&release.archive_url) {
        return Err(UpdateError::Metadata("asset URL outside the release host allowlist".into()));
    }
    download_verified(&download_agent(), &release.archive_url, dest, &release.archive_name, &release.sha256, MAX_ARCHIVE_BYTES, progress)
}

/// Stream `url` into `<dest>.part` while hashing, stop at `max_bytes`, check the SHA-256 and only
/// then rename to `dest`. Any failure removes the part file.
pub(crate) fn download_verified(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    name: &str,
    expected_sha256: &str,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<()> {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let result = stream_to_part(agent, url, &part, name, expected_sha256, max_bytes, progress);
    match result {
        Ok(()) => {
            std::fs::rename(&part, dest)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn stream_to_part(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    name: &str,
    expected_sha256: &str,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<()> {
    let resp = agent.get(url).call().map_err(|e| net_err(name, e))?;
    if !resp.status().is_success() {
        return Err(net_err(name, format!("HTTP {}", resp.status().as_u16())));
    }
    let total = resp.body().content_length();
    if total.is_some_and(|t| t > max_bytes) {
        return Err(net_err(name, "the server announced a file larger than the allowed maximum"));
    }
    let mut body = resp.into_body();
    let mut reader = body.as_reader();
    let mut out = std::io::BufWriter::new(std::fs::File::create(part)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| net_err(name, e))?;
        if n == 0 {
            break;
        }
        got = got.saturating_add(n as u64);
        if got > max_bytes || total.is_some_and(|t| got > t.saturating_add(1024)) {
            return Err(net_err(name, "the download is larger than announced; aborted"));
        }
        let chunk = buf.get(..n).unwrap_or_default();
        hasher.update(chunk);
        out.write_all(chunk)?;
        if !progress(got, total) {
            return Err(UpdateError::Cancelled);
        }
    }
    if total.is_some_and(|t| got != t) {
        return Err(net_err(name, "the download ended early"));
    }
    out.flush()?;
    let file = out.into_inner().map_err(|e| UpdateError::Io(e.into_error()))?;
    file.sync_all()?;
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(UpdateError::Checksum { name: name.to_string(), expected: expected_sha256.to_string(), actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-shot plain-HTTP server answering every request with `body`.
    fn serve(body: Vec<u8>, announce: Option<usize>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let len = announce.unwrap_or(body.len());
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}/asset")
    }

    fn plain_agent() -> ureq::Agent {
        ureq::Agent::config_builder().build().into()
    }

    fn sha(data: &[u8]) -> String {
        Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn verified_download_lands_only_with_matching_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![7u8; 100_000];
        let url = serve(data.clone(), None);
        let dest = dir.path().join("a.tar.gz");
        let mut seen = 0u64;
        download_verified(&plain_agent(), &url, &dest, "a", &sha(&data), 1 << 20, &mut |done, _| {
            seen = done;
            true
        })
        .unwrap();
        assert_eq!(seen, 100_000);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!dir.path().join("a.tar.gz.part").exists());
    }

    #[test]
    fn wrong_checksum_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let url = serve(vec![1u8; 5000], None);
        let dest = dir.path().join("a.tar.gz");
        let err = download_verified(&plain_agent(), &url, &dest, "a", &"0".repeat(64), 1 << 20, &mut |_, _| true).unwrap_err();
        assert!(matches!(err, UpdateError::Checksum { .. }), "{err:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no .part and no archive");
    }

    #[test]
    fn oversize_download_is_cut_off() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![1u8; 50_000];
        let url = serve(data.clone(), None);
        let dest = dir.path().join("a.tar.gz");
        let err = download_verified(&plain_agent(), &url, &dest, "a", &sha(&data), 10_000, &mut |_, _| true).unwrap_err();
        assert!(matches!(err, UpdateError::Network(_)), "{err:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cancel_stops_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![1u8; 600_000];
        let url = serve(data.clone(), None);
        let dest = dir.path().join("a.tar.gz");
        let err = download_verified(&plain_agent(), &url, &dest, "a", &sha(&data), 1 << 20, &mut |_, _| false).unwrap_err();
        assert!(matches!(err, UpdateError::Cancelled), "{err:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn https_only_agent_refuses_plain_http() {
        let url = serve(vec![1u8; 10], None);
        assert!(metadata_agent().get(&url).call().is_err());
    }
}

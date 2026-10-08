//! The HTTP(S) transport (feature `http`): ureq with pure-Rust TLS (rustls + RustCrypto) and the
//! operating system's certificate verifier, as the speech-model downloader uses.
//!
//! Non-2xx answers are returned as responses (ComfyUI explains a refused workflow in the body of
//! its 400). Bodies are capped: JSON at [`MAX_JSON`], files at [`MAX_FILE`].

use std::time::Duration;

use crate::client::{Response, Transport};
use crate::{ComfyError, Result};

/// Largest JSON answer read (history entries of big graphs stay far below this).
pub const MAX_JSON: u64 = 64 << 20;
/// Largest output file downloaded.
pub const MAX_FILE: u64 = 8 << 30;

/// A ComfyUI server at a base URL (`http://127.0.0.1:8188`).
pub struct HttpTransport {
    base: String,
    agent: ureq::Agent,
}

/// Check and normalise a server URL: `http(s)://host[:port][/prefix]`, without a trailing `/`.
pub fn normalize(url: &str) -> Result<String> {
    let u = url.trim().trim_end_matches('/');
    let rest = u.strip_prefix("http://").or_else(|| u.strip_prefix("https://"));
    match rest {
        Some(r) if !r.is_empty() && !r.contains(char::is_whitespace) && !r.starts_with('/') => Ok(u.to_string()),
        _ => Err(ComfyError::Connection(format!("`{url}` is not an http:// or https:// server address"))),
    }
}

impl HttpTransport {
    pub fn new(base_url: &str) -> Result<Self> {
        let base = normalize(base_url)?;
        let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
        let agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::Rustls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .unversioned_rustls_crypto_provider(provider)
                    .build(),
            )
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .build()
            .into();
        Ok(Self { base, agent })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn read(&self, path: &str, r: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Response> {
        let r = r.map_err(|e| ComfyError::Connection(format!("{}{path}: {e}", self.base)))?;
        let status = r.status().as_u16();
        let limit = if path.starts_with("/view") { MAX_FILE } else { MAX_JSON };
        let body = r.into_body().with_config().limit(limit).read_to_vec().map_err(|e| ComfyError::Connection(format!("{}{path}: {e}", self.base)))?;
        Ok(Response { status, body })
    }
}

impl Transport for HttpTransport {
    fn get(&self, path: &str) -> Result<Response> {
        let url = format!("{}{path}", self.base);
        self.read(path, self.agent.get(&url).call())
    }

    fn post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<Response> {
        let url = format!("{}{path}", self.base);
        self.read(path, self.agent.post(&url).header("Content-Type", content_type).send(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(normalize(" http://127.0.0.1:8188/ ").unwrap(), "http://127.0.0.1:8188");
        assert_eq!(normalize("https://gpu.example/comfy").unwrap(), "https://gpu.example/comfy");
        for bad in ["", "127.0.0.1:8188", "ftp://x", "http://", "http:///x", "http://a b"] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn unreachable_server_is_an_error() {
        // port 9 (discard) on localhost: refused at once on every CI machine
        let t = HttpTransport::new("http://127.0.0.1:9").unwrap();
        assert!(matches!(t.get("/system_stats"), Err(ComfyError::Connection(_))));
    }
}

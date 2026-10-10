//! The HTTP(S) transport (feature `http`): ureq with pure-Rust TLS (rustls + RustCrypto) and the
//! operating system's certificate verifier, as the speech-model downloader uses.
//!
//! Non-2xx answers are returned as responses (ComfyUI explains a refused workflow in the body of
//! its 400). JSON answers are read up to [`MAX_JSON`]; output files are streamed
//! ([`Transport::download`]) to the caller's writer, never held in memory whole.

use std::io::{Read, Write};
use std::time::Duration;

use crate::client::{Response, Transport, too_large};
use crate::{ComfyError, Result};

/// Largest JSON answer read (history entries of big graphs stay far below this).
pub const MAX_JSON: u64 = 64 << 20;

/// A ComfyUI server at a base URL (`http://127.0.0.1:8188`).
pub struct HttpTransport {
    base: String,
    agent: ureq::Agent,
}

/// Check and normalise a server URL (see [`crate::normalize_server`]).
pub fn normalize(url: &str) -> Result<String> {
    crate::normalize_server(url)
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
            // a stalled server can't hold a run forever (a 2 GiB file at 2 MB/s still fits)
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
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
        let body = r.into_body().with_config().limit(MAX_JSON).read_to_vec().map_err(|e| ComfyError::Connection(format!("{}{path}: {e}", self.base)))?;
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

    fn download(&self, path: &str, out: &mut dyn Write, limit: u64) -> Result<u64> {
        let url = format!("{}{path}", self.base);
        let conn = |e: &dyn std::fmt::Display| ComfyError::Connection(format!("{}{path}: {e}", self.base));
        let r = self.agent.get(&url).call().map_err(|e| conn(&e))?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ComfyError::Server(format!("HTTP {status}")));
        }
        if r.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()).is_some_and(|n| n > limit) {
            return Err(too_large(path, limit));
        }
        let mut body = r.into_body().into_reader();
        let mut buf = vec![0u8; 256 << 10];
        let mut total = 0u64;
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => return Ok(total),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(conn(&e)),
            };
            total = total.saturating_add(n as u64);
            if total > limit {
                return Err(too_large(path, limit));
            }
            out.write_all(buf.get(..n).unwrap_or_default()).map_err(|e| ComfyError::Storage(e.to_string()))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(normalize(" http://127.0.0.1:8188/ ").unwrap(), "http://127.0.0.1:8188");
        assert!(normalize("127.0.0.1:8188").is_err());
        assert!(HttpTransport::new("http://a b").is_err());
    }

    #[test]
    fn unreachable_server_is_an_error() {
        // port 9 (discard) on localhost: refused at once on every CI machine
        let t = HttpTransport::new("http://127.0.0.1:9").unwrap();
        assert!(matches!(t.get("/system_stats"), Err(ComfyError::Connection(_))));
        assert!(matches!(t.download("/view?filename=x.png", &mut Vec::new(), 10), Err(ComfyError::Connection(_))));
    }
}

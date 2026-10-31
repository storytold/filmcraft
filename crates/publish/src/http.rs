//! The HTTP requests an uploader makes, behind a trait so the protocol is tested without a network.

/// The response bodies an uploader reads are small JSON documents; anything larger is refused.
pub const MAX_RESPONSE_BYTES: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Post,
    Put,
}

/// One request. Not `Debug`: its headers carry the access token.
pub struct Request<'a> {
    pub method: Method,
    pub url: &'a str,
    pub headers: Vec<(&'static str, String)>,
    pub body: &'a [u8],
}

impl Request<'_> {
    /// The value of header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The value of header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The request never got an HTTP answer (no connection, a dropped connection, a timeout).
/// Uploaders treat it like a server error: wait, ask how much arrived, and go on.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// Sends one request and returns the server's answer, whatever its status: redirects are not
/// followed (YouTube's `308 Resume Incomplete` is an answer, not a redirect) and 4xx/5xx are
/// responses, not errors.
pub trait Transport {
    fn send(&self, request: &Request<'_>) -> Result<Response, TransportError>;
}

#[cfg(feature = "net")]
pub use net::UreqTransport;

#[cfg(feature = "net")]
mod net {
    use super::{MAX_RESPONSE_BYTES, Method, Request, Response, Transport, TransportError};
    use std::time::Duration;

    /// HTTPS over ureq with pure-Rust TLS (rustls + RustCrypto) and the operating system's
    /// certificate verifier.
    pub struct UreqTransport {
        agent: ureq::Agent,
    }

    impl Default for UreqTransport {
        fn default() -> Self {
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
                .max_redirects(0)
                .user_agent(concat!("FilmCraft/", env!("CARGO_PKG_VERSION")))
                .timeout_connect(Some(Duration::from_secs(30)))
                // A chunk is at most 64 MiB; a stalled connection must not block forever.
                .timeout_send_body(Some(Duration::from_secs(15 * 60)))
                .timeout_recv_response(Some(Duration::from_secs(5 * 60)))
                .timeout_recv_body(Some(Duration::from_secs(60)))
                .build()
                .into();
            Self { agent }
        }
    }

    impl Transport for UreqTransport {
        fn send(&self, request: &Request<'_>) -> Result<Response, TransportError> {
            let mut builder = match request.method {
                Method::Post => self.agent.post(request.url),
                Method::Put => self.agent.put(request.url),
            };
            for (name, value) in &request.headers {
                builder = builder.header(*name, value);
            }
            // Error texts may quote the URL, which for an upload session is a credential.
            let fail = |e: ureq::Error| TransportError(e.to_string().replace(request.url, "<upload address>"));
            let response = builder.send(request.body).map_err(fail)?;
            let status = response.status().as_u16();
            let headers = response.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
            let body = response.into_body().with_config().limit(MAX_RESPONSE_BYTES).read_to_vec().map_err(fail)?;
            Ok(Response { status, headers, body })
        }
    }
}

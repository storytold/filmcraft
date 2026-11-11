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

    /// [`send`](Self::send), calling `watch` with the number of body bytes handed to the network
    /// so far while the body goes out (never with 0 or the whole length); when `watch` returns
    /// false the request stops with an error. A transport that can't tell sends the request and
    /// reports nothing.
    fn send_watched(&self, request: &Request<'_>, watch: &mut dyn FnMut(u64) -> bool) -> Result<Response, TransportError> {
        let _ = watch;
        self.send(request)
    }
}

/// Whether this build has the HTTPS transport (feature `net`).
pub const AVAILABLE: bool = cfg!(feature = "net");

/// The HTTPS transport, when this build has one (feature `net`).
pub fn default_transport() -> Option<Box<dyn Transport>> {
    #[cfg(feature = "net")]
    return Some(Box::new(UreqTransport::default()));
    #[cfg(not(feature = "net"))]
    None
}

#[cfg(feature = "net")]
pub use net::UreqTransport;

#[cfg(feature = "net")]
mod net {
    use super::{MAX_RESPONSE_BYTES, Method, Request, Response, Transport, TransportError};
    use std::io::Read;
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
            self.send_body(request, request.body)
        }

        fn send_watched(&self, request: &Request<'_>, watch: &mut dyn FnMut(u64) -> bool) -> Result<Response, TransportError> {
            let mut body = Watched { rest: request.body, sent: 0, watch };
            self.send_body(request, ureq::SendBody::from_reader(&mut body))
        }
    }

    impl UreqTransport {
        fn send_body(&self, request: &Request<'_>, body: impl ureq::AsSendBody) -> Result<Response, TransportError> {
            let mut builder = match request.method {
                Method::Post => self.agent.post(request.url),
                Method::Put => self.agent.put(request.url),
            };
            for (name, value) in &request.headers {
                builder = builder.header(*name, value);
            }
            // A body read as it goes has no length of its own; saying it keeps the request
            // length-delimited rather than chunked.
            builder = builder.header("Content-Length", request.body.len().to_string());
            // Error texts may quote the URL, which for an upload session is a credential.
            let fail = |e: ureq::Error| TransportError(e.to_string().replace(request.url, "<upload address>"));
            let response = builder.send(body).map_err(fail)?;
            let status = response.status().as_u16();
            let headers = response.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
            let body = response.into_body().with_config().limit(MAX_RESPONSE_BYTES).read_to_vec().map_err(fail)?;
            Ok(Response { status, headers, body })
        }
    }

    /// A request body that tells `watch` how much of it was taken before handing out more.
    struct Watched<'a, 'w> {
        rest: &'a [u8],
        sent: u64,
        watch: &'w mut dyn FnMut(u64) -> bool,
    }

    impl Read for Watched<'_, '_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.rest.is_empty() {
                return Ok(0);
            }
            if self.sent > 0 && !(self.watch)(self.sent) {
                return Err(std::io::Error::other("stopped"));
            }
            let n = self.rest.read(buf)?;
            self.sent = self.sent.saturating_add(n as u64);
            Ok(n)
        }
    }
}

//! Signing in with Google: OAuth 2.0 for installed apps
//! (<https://developers.google.com/identity/protocols/oauth2/native-app>) with PKCE (RFC 7636) and
//! a loopback redirect (RFC 8252 §7.3).
//!
//! The app opens [`SignIn::url`] in the user's browser. After the user allows access, Google sends
//! the browser to `http://127.0.0.1:<port>` with a one-time code; [`SignIn::code_from_callback`]
//! checks it belongs to this sign-in and [`SignIn::exchange`] trades it for [`Tokens`]. The
//! refresh token lasts until the user revokes it (7 days while the consent screen is in Testing);
//! [`refresh`] turns it into a new access token, which lasts about an hour.

use crate::PublishError;
use crate::http::{Method, Request, Response, Transport};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub const GOOGLE_AUTH_URI: &str = "https://accounts.google.com/o/oauth2/auth";
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

const AUTH_URIS: &[&str] = &[GOOGLE_AUTH_URI, "https://accounts.google.com/o/oauth2/v2/auth"];
const TOKEN_URIS: &[&str] = &[GOOGLE_TOKEN_URI, "https://www.googleapis.com/oauth2/v4/token"];
const CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";
const MAX_FIELD: usize = 4096;
const DEFAULT_EXPIRY: Duration = Duration::from_secs(3600);

/// A Desktop app OAuth client: the file Google's console downloads (`{"installed": {…}}`).
/// Google treats a desktop client's secret as public, but it still stays out of `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub token_uri: String,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &self.client_id)
            .field("auth_uri", &self.auth_uri)
            .field("token_uri", &self.token_uri)
            .finish_non_exhaustive()
    }
}

impl ClientConfig {
    /// Read the client file. Only a Desktop app client with Google's own endpoints is accepted.
    pub fn from_google_json(bytes: &[u8]) -> Result<Self, PublishError> {
        let config = |message: &str| PublishError::Config(message.into());
        let value: Value = serde_json::from_slice(bytes).map_err(|_| config("the client file isn't the JSON file Google Cloud downloads"))?;
        if value.get("web").is_some() {
            return Err(config("this is a Web application client: create a Desktop app client instead"));
        }
        let installed = value.get("installed").ok_or_else(|| config("the client file has no Desktop app client"))?;
        let field = |name: &str| {
            installed
                .get(name)
                .and_then(Value::as_str)
                .filter(|s| token_text(s))
                .map(str::to_string)
                .ok_or_else(|| config(&format!("the client file has no valid {name}")))
        };
        let client =
            Self { client_id: field("client_id")?, client_secret: field("client_secret")?, auth_uri: field("auth_uri")?, token_uri: field("token_uri")? };
        if !client.client_id.ends_with(CLIENT_ID_SUFFIX) {
            return Err(config("the client id isn't a Google OAuth client id"));
        }
        if !AUTH_URIS.contains(&client.auth_uri.as_str()) || !TOKEN_URIS.contains(&client.token_uri.as_str()) {
            return Err(config("the client file points to sign-in addresses that aren't Google's"));
        }
        Ok(client)
    }
}

/// The PKCE secret of one sign-in (RFC 7636): only its SHA-256 goes to the browser.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    verifier: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pkce { .. }")
    }
}

impl Pkce {
    /// A verifier of 43–128 unreserved characters (`A–Z a–z 0–9 - . _ ~`).
    pub fn from_verifier(verifier: &str) -> Option<Self> {
        ((43..=128).contains(&verifier.len()) && verifier.bytes().all(unreserved)).then(|| Self { verifier: verifier.into() })
    }

    /// 32 random bytes as base64url: 43 characters.
    #[cfg(not(target_family = "wasm"))]
    pub fn generate() -> Result<Self, PublishError> {
        Ok(Self { verifier: random_text()? })
    }

    /// `BASE64URL(SHA256(verifier))`, the `S256` code challenge.
    pub fn challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()))
    }
}

/// 32 random bytes from the operating system as base64url.
#[cfg(not(target_family = "wasm"))]
fn random_text() -> Result<String, PublishError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| PublishError::SignIn(format!("no random source: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// One sign-in in progress.
pub struct SignIn {
    /// Open this in the user's browser.
    pub url: String,
    pub redirect_uri: String,
    state: String,
    pkce: Pkce,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn").field("redirect_uri", &self.redirect_uri).finish_non_exhaustive()
    }
}

impl SignIn {
    /// The browser address asking for `scope`, with Google's answer going to `redirect_uri`.
    /// `access_type=offline` and `prompt=consent` make Google return a refresh token every time.
    pub fn new(client: &ClientConfig, scope: &str, redirect_uri: &str, state: &str, pkce: Pkce) -> Self {
        let query = form(&[
            ("client_id", &client.client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope),
            ("code_challenge", &pkce.challenge()),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ]);
        Self { url: format!("{}?{query}", client.auth_uri), redirect_uri: redirect_uri.into(), state: state.into(), pkce }
    }

    /// A sign-in answered on a new loopback listener, with a random state and PKCE verifier.
    #[cfg(not(target_family = "wasm"))]
    pub fn start(client: &ClientConfig, scope: &str) -> Result<(Self, Loopback), PublishError> {
        let loopback = Loopback::bind()?;
        let sign_in = Self::new(client, scope, &loopback.redirect_uri()?, &random_text()?, Pkce::generate()?);
        Ok((sign_in, loopback))
    }

    /// The code in the browser's request to the redirect address (`/?state=…&code=…`), if it
    /// belongs to this sign-in.
    pub fn code_from_callback(&self, target: &str) -> Result<String, PublishError> {
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
        let query = query.split_once('#').map(|(q, _)| q).unwrap_or(query);
        let params = parse_query(query).ok_or_else(|| PublishError::SignIn("the browser's answer is malformed".into()))?;
        let param = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        if param("state") != Some(self.state.as_str()) {
            return Err(PublishError::SignIn("the browser's answer doesn't belong to this sign-in".into()));
        }
        match (param("error"), param("code")) {
            (Some("access_denied"), _) => Err(PublishError::SignInDenied),
            (Some(error), _) => Err(PublishError::SignIn(format!("Google answered {}", printable(error, 100)))),
            (None, Some(code)) if token_text(code) => Ok(code.into()),
            (None, _) => Err(PublishError::SignIn("the browser's answer has no code".into())),
        }
    }

    /// Trade the code for tokens.
    pub fn exchange(&self, transport: &dyn Transport, client: &ClientConfig, code: &str) -> Result<Tokens, PublishError> {
        let body = form(&[
            ("code", code),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
            ("redirect_uri", &self.redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", &self.pkce.verifier),
        ]);
        let tokens = token_request(transport, client, &body)?;
        if tokens.refresh_token.is_none() {
            return Err(PublishError::SignIn("Google sent no refresh token".into()));
        }
        Ok(tokens)
    }
}

/// A new access token for a stored refresh token. A revoked or expired refresh token is
/// [`PublishError::Unauthorized`]: the user signs in again.
pub fn refresh(transport: &dyn Transport, client: &ClientConfig, refresh_token: &str) -> Result<Tokens, PublishError> {
    if !token_text(refresh_token) {
        return Err(PublishError::Unauthorized);
    }
    let body =
        form(&[("client_id", &client.client_id), ("client_secret", &client.client_secret), ("refresh_token", refresh_token), ("grant_type", "refresh_token")]);
    token_request(transport, client, &body)
}

/// What the token endpoint gave. `Debug` leaves the tokens out.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    /// Sent on sign-in; a refresh usually sends none (keep the old one).
    pub refresh_token: Option<String>,
    pub expires_in: Duration,
    /// The scopes the user granted.
    pub scope: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").field("expires_in", &self.expires_in).field("scope", &self.scope).finish_non_exhaustive()
    }
}

impl Tokens {
    /// Whether the user granted `scope` (Google lets them untick scopes on the consent page).
    pub fn grants(&self, scope: &str) -> bool {
        self.scope.as_deref().is_none_or(|granted| granted.split(' ').any(|s| s == scope))
    }
}

fn token_request(transport: &dyn Transport, client: &ClientConfig, body: &str) -> Result<Tokens, PublishError> {
    let headers = vec![("Content-Type", "application/x-www-form-urlencoded".to_string()), ("Accept", "application/json".to_string())];
    let request = Request { method: Method::Post, url: &client.token_uri, headers, body: body.as_bytes() };
    let response = transport.send(&request).map_err(|e| PublishError::Network(e.0))?;
    tokens_from(&response)
}

fn tokens_from(response: &Response) -> Result<Tokens, PublishError> {
    #[derive(Deserialize)]
    struct Body {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
        scope: Option<String>,
        token_type: Option<String>,
    }
    if response.status != 200 {
        return Err(token_error(response));
    }
    let bad = || PublishError::Protocol("the token answer isn't valid".into());
    let body: Body = serde_json::from_slice(&response.body).map_err(|_| bad())?;
    if !token_text(&body.access_token) || body.refresh_token.as_deref().is_some_and(|t| !token_text(t)) {
        return Err(bad());
    }
    if body.token_type.as_deref().is_some_and(|t| !t.eq_ignore_ascii_case("bearer")) {
        return Err(bad());
    }
    let expires_in = body.expires_in.map_or(DEFAULT_EXPIRY, |s| Duration::from_secs(s.clamp(1, 86_400)));
    let scope = body.scope.map(|s| printable(&s, MAX_FIELD));
    Ok(Tokens { access_token: body.access_token, refresh_token: body.refresh_token, expires_in, scope })
}

fn token_error(response: &Response) -> PublishError {
    let value: Value = serde_json::from_slice(&response.body).unwrap_or_default();
    let text = |name: &str| value.get(name).and_then(Value::as_str).map(|s| printable(s, 200));
    match text("error").as_deref() {
        Some("invalid_grant") => PublishError::Unauthorized,
        Some("invalid_client" | "unauthorized_client") => PublishError::Config("Google doesn't accept this OAuth client: check the client file".into()),
        _ if response.status >= 500 => PublishError::Network(format!("the sign-in server answered {}", response.status)),
        error => PublishError::SignIn(
            text("error_description").or(error.map(str::to_string)).unwrap_or_else(|| format!("the sign-in server answered {}", response.status)),
        ),
    }
}

/// A token, code or id: 1–4096 visible ASCII characters.
fn token_text(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_FIELD && s.bytes().all(|b| b.is_ascii_graphic())
}

/// `s` without control characters, at most `max` characters.
fn printable(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// Percent-encoding for a query or form value: everything but unreserved characters.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if unreserved(b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `k=v&…`, each part percent-encoded.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

/// The inverse of [`encode`], with `+` as a space; `None` for a bad escape or invalid UTF-8.
pub(crate) fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok())?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// `k=v&…` decoded; at most 32 parameters.
pub(crate) fn parse_query(query: &str) -> Option<Vec<(String, String)>> {
    let params: Vec<_> = query.split('&').filter(|p| !p.is_empty()).collect();
    if params.len() > 32 {
        return None;
    }
    params.into_iter().map(|p| p.split_once('=').map_or_else(|| Some((decode(p)?, String::new())), |(k, v)| Some((decode(k)?, decode(v)?)))).collect()
}

#[cfg(not(target_family = "wasm"))]
pub use loopback::Loopback;

#[cfg(not(target_family = "wasm"))]
mod loopback {
    use super::SignIn;
    use crate::PublishError;
    use std::io::{ErrorKind, Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const HEAD_MAX: usize = 8192;

    /// The local address Google sends the browser back to.
    pub struct Loopback {
        listener: TcpListener,
    }

    impl Loopback {
        /// Listen on `127.0.0.1` on a free port.
        pub fn bind() -> Result<Self, PublishError> {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            listener.set_nonblocking(true)?;
            Ok(Self { listener })
        }

        pub fn redirect_uri(&self) -> Result<String, PublishError> {
            Ok(format!("http://127.0.0.1:{}", self.listener.local_addr()?.port()))
        }

        /// Wait until the browser comes back for `sign_in` and return the code. Other paths
        /// (`/favicon.ico`) get a 404 and the wait goes on.
        pub fn wait(&self, sign_in: &SignIn, cancel: &AtomicBool, timeout: Duration) -> Result<String, PublishError> {
            let deadline = Instant::now().checked_add(timeout);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(PublishError::Cancelled);
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    return Err(PublishError::SignIn("the browser didn't come back in time".into()));
                }
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        if let Some(result) = serve(stream, sign_in) {
                            return result;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    /// Answer one browser request: `None` when it isn't the sign-in answer.
    fn serve(mut stream: TcpStream, sign_in: &SignIn) -> Option<Result<String, PublishError>> {
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
        let head = read_head(&mut stream)?;
        let mut words = head.lines().next().unwrap_or_default().split(' ');
        let (method, target) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
        let path = target.split(['?', '#']).next().unwrap_or_default();
        if method != "GET" || path != "/" {
            let _ = respond(&mut stream, "404 Not Found", "Not found.");
            return None;
        }
        let result = sign_in.code_from_callback(target);
        let page = match &result {
            Ok(_) => "FilmCraft is signed in. You can close this tab and go back to FilmCraft.".to_string(),
            Err(e) => format!("Sign-in didn't finish: {}. You can close this tab and try again in FilmCraft.", escape(&e.to_string())),
        };
        let _ = respond(&mut stream, "200 OK", &page);
        Some(result)
    }

    fn read_head(stream: &mut TcpStream) -> Option<String> {
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).ok()?;
            if n == 0 || head.len() + n > HEAD_MAX {
                return None;
            }
            head.extend_from_slice(buf.get(..n)?);
        }
        String::from_utf8(head).ok()
    }

    fn respond(stream: &mut TcpStream, status: &str, text: &str) -> std::io::Result<()> {
        let body = format!("<!doctype html><meta charset=\"utf-8\"><title>FilmCraft</title><p style=\"font: 16px system-ui; margin: 3em\">{text}</p>");
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        stream.flush()
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
    }
}

//! The whole sign-in on real sockets: a fake browser follows the sign-in address to the app's
//! loopback listener, and a local fake token server checks the PKCE verifier against the
//! challenge the browser carried.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use filmcraft_publish::PublishError;
use filmcraft_publish::http::UreqTransport;
use filmcraft_publish::oauth::{ClientConfig, SignIn};
use filmcraft_publish::youtube::UPLOAD_SCOPE;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const CODE: &str = "4/0Ae-fake-code";

fn query_param(url: &str, name: &str) -> String {
    let query = url.split_once('?').unwrap().1;
    query
        .split('&')
        .find_map(|p| p.strip_prefix(&format!("{name}=")))
        .map(|v| {
            let mut out = Vec::new();
            let b = v.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        })
        .unwrap()
}

/// A fake Google token endpoint: answers a code exchange when the verifier matches `challenge`.
fn token_server(challenge: Arc<Mutex<String>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let form = format!("x?{}", String::from_utf8(body).unwrap());
            let verifier = query_param(&form, "code_verifier");
            let expected = challenge.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let ok = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == expected && query_param(&form, "code") == CODE;
            let (status, json) = if ok {
                (
                    "200 OK",
                    format!(r#"{{"access_token":"ya29.fake","refresh_token":"1//fake","expires_in":3599,"token_type":"Bearer","scope":"{UPLOAD_SCOPE}"}}"#),
                )
            } else {
                ("400 Bad Request", r#"{"error":"invalid_grant","error_description":"Bad code verifier."}"#.to_string())
            };
            let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len());
        }
    });
    url
}

fn client(token_uri: &str) -> ClientConfig {
    ClientConfig {
        client_id: "123-abc.apps.googleusercontent.com".into(),
        client_secret: "fake-secret".into(),
        auth_uri: "https://accounts.google.com/o/oauth2/auth".into(),
        token_uri: token_uri.into(),
    }
}

/// What a browser does: GET a path on the redirect address and return the page.
fn browse(redirect_uri: &str, path: &str) -> String {
    let addr = redirect_uri.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/html\r\n\r\n").unwrap();
    let mut page = String::new();
    stream.read_to_string(&mut page).unwrap();
    page
}

/// Google's side of a sign-in: the browser lands on the redirect address with `query`.
fn browser_returns(sign_in: &SignIn, query: impl FnOnce(&str) -> String + Send + 'static) -> std::thread::JoinHandle<(String, String)> {
    let redirect = query_param(&sign_in.url, "redirect_uri");
    let state = query_param(&sign_in.url, "state");
    std::thread::spawn(move || {
        let favicon = browse(&redirect, "/favicon.ico");
        let page = browse(&redirect, &format!("/?{}", query(&state)));
        (favicon, page)
    })
}

#[test]
fn signs_in_through_the_browser_and_exchanges_the_code() {
    let challenge = Arc::new(Mutex::new(String::new()));
    let client = client(&token_server(challenge.clone()));
    let (sign_in, loopback) = SignIn::start(&client, UPLOAD_SCOPE).unwrap();
    assert!(sign_in.url.starts_with("https://accounts.google.com/o/oauth2/auth?"));
    assert!(sign_in.redirect_uri.starts_with("http://127.0.0.1:"));
    *challenge.lock().unwrap() = query_param(&sign_in.url, "code_challenge");

    let browser =
        browser_returns(&sign_in, |state| format!("state={state}&code=4%2F0Ae-fake-code&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube.upload"));
    let code = loopback.wait(&sign_in, &AtomicBool::new(false), Duration::from_secs(20)).unwrap();
    let (favicon, page) = browser.join().unwrap();
    assert!(favicon.starts_with("HTTP/1.1 404"), "{favicon}");
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("FilmCraft is signed in"), "{page}");
    assert_eq!(code, CODE);

    let tokens = sign_in.exchange(&UreqTransport::default(), &client, &code).unwrap();
    assert_eq!(tokens.access_token, "ya29.fake");
    assert_eq!(tokens.refresh_token.as_deref(), Some("1//fake"));
    assert!(tokens.grants(UPLOAD_SCOPE));
}

#[test]
fn a_wrong_verifier_is_refused_by_the_token_server() {
    let challenge = Arc::new(Mutex::new("not-the-challenge".to_string()));
    let client = client(&token_server(challenge));
    let (sign_in, _loopback) = SignIn::start(&client, UPLOAD_SCOPE).unwrap();
    assert!(matches!(sign_in.exchange(&UreqTransport::default(), &client, CODE), Err(PublishError::Unauthorized)));
}

#[test]
fn a_denied_sign_in_tells_the_browser_and_the_app() {
    let client = client("http://127.0.0.1:9/token");
    let (sign_in, loopback) = SignIn::start(&client, UPLOAD_SCOPE).unwrap();
    let browser = browser_returns(&sign_in, |state| format!("error=access_denied&state={state}"));
    let r = loopback.wait(&sign_in, &AtomicBool::new(false), Duration::from_secs(20));
    assert!(matches!(r, Err(PublishError::SignInDenied)), "{r:?}");
    let (_, page) = browser.join().unwrap();
    assert!(page.contains("Sign-in didn't finish: sign-in was cancelled in the browser"), "{page}");
}

#[test]
fn a_hostile_answer_is_escaped_on_the_page() {
    let client = client("http://127.0.0.1:9/token");
    let (sign_in, loopback) = SignIn::start(&client, UPLOAD_SCOPE).unwrap();
    let browser = browser_returns(&sign_in, |state| format!("error=%3Cimg%20src%3Dx%3E&state={state}"));
    assert!(loopback.wait(&sign_in, &AtomicBool::new(false), Duration::from_secs(20)).is_err());
    let (_, page) = browser.join().unwrap();
    assert!(page.contains("&lt;img src=x&gt;") && !page.contains("<img"), "{page}");
}

#[test]
fn waiting_stops_on_cancel_and_on_timeout() {
    let client = client("http://127.0.0.1:9/token");
    let (sign_in, loopback) = SignIn::start(&client, UPLOAD_SCOPE).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        setter.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    assert!(matches!(loopback.wait(&sign_in, &cancel, Duration::from_secs(60)), Err(PublishError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(5));

    let r = loopback.wait(&sign_in, &AtomicBool::new(false), Duration::from_millis(200));
    assert!(matches!(&r, Err(PublishError::SignIn(m)) if m.contains("in time")), "{r:?}");
}

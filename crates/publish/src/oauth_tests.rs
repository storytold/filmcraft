//! Sign-in pieces against scripted token-server replies (no network).

use crate::PublishError;
use crate::oauth::{ClientConfig, GOOGLE_AUTH_URI, GOOGLE_TOKEN_URI, Pkce, SignIn, decode, encode, parse_query, refresh};
use crate::test_support::{Script, reply};
use crate::youtube::UPLOAD_SCOPE;
use serde_json::json;
use std::time::Duration;

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const SECRET: &str = "GOCSPX-not-a-real-secret";

fn client() -> ClientConfig {
    ClientConfig {
        client_id: "123-abc.apps.googleusercontent.com".into(),
        client_secret: SECRET.into(),
        auth_uri: GOOGLE_AUTH_URI.into(),
        token_uri: GOOGLE_TOKEN_URI.into(),
    }
}

fn sign_in() -> SignIn {
    SignIn::new(&client(), UPLOAD_SCOPE, "http://127.0.0.1:5555", "state-123", Pkce::from_verifier(VERIFIER).unwrap())
}

fn form_of(body: &[u8]) -> Vec<(String, String)> {
    parse_query(std::str::from_utf8(body).unwrap()).unwrap()
}

fn field<'a>(form: &'a [(String, String)], name: &str) -> Option<&'a str> {
    form.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn token_reply(extra: serde_json::Value) -> Result<crate::http::Response, crate::http::TransportError> {
    let mut body = json!({"access_token": "ya29.access", "expires_in": 3599, "token_type": "Bearer", "scope": UPLOAD_SCOPE});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    reply(200, &[("Content-Type", "application/json")], &body.to_string())
}

#[test]
fn pkce_matches_rfc_7636() {
    // RFC 7636 appendix B.
    assert_eq!(Pkce::from_verifier(VERIFIER).unwrap().challenge(), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert!(Pkce::from_verifier("short").is_none());
    assert!(Pkce::from_verifier(&"a".repeat(129)).is_none());
    assert!(Pkce::from_verifier(&format!("{VERIFIER}+")).is_none(), "only unreserved characters");
    assert_eq!(format!("{:?}", Pkce::from_verifier(VERIFIER).unwrap()), "Pkce { .. }");
}

#[cfg(feature = "net")]
#[test]
fn generated_secrets_are_random_and_valid() {
    let a = Pkce::generate().unwrap();
    let b = Pkce::generate().unwrap();
    assert_ne!(a, b);
    assert_eq!(a.challenge().len(), 43);
}

#[test]
fn the_desktop_client_file_is_read() {
    let file = json!({"installed": {
        "client_id": "123-abc.apps.googleusercontent.com",
        "project_id": "filmcraft-test",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
        "client_secret": SECRET,
        "redirect_uris": ["http://localhost"],
    }});
    let c = ClientConfig::from_google_json(file.to_string().as_bytes()).unwrap();
    assert_eq!(c, client());
    assert!(!format!("{c:?}").contains(SECRET));
}

#[test]
fn other_client_files_are_refused_with_a_reason() {
    let installed = |patch: serde_json::Value| {
        let mut v = json!({"client_id": "1.apps.googleusercontent.com", "client_secret": "s", "auth_uri": GOOGLE_AUTH_URI, "token_uri": GOOGLE_TOKEN_URI});
        if let (Some(v), Some(p)) = (v.as_object_mut(), patch.as_object()) {
            v.extend(p.clone());
        }
        json!({ "installed": v }).to_string()
    };
    let cases = [
        ("not json".to_string(), "isn't the JSON"),
        (json!({"web": {"client_id": "1.apps.googleusercontent.com"}}).to_string(), "Desktop app"),
        (json!({}).to_string(), "no Desktop app client"),
        (installed(json!({"client_secret": ""})), "client_secret"),
        (installed(json!({"client_id": "1.evil.example"})), "client id"),
        (installed(json!({"token_uri": "https://evil.example/token"})), "aren't Google's"),
        (installed(json!({"auth_uri": "http://accounts.google.com/o/oauth2/auth"})), "aren't Google's"),
        (installed(json!({"client_id": "a b.apps.googleusercontent.com"})), "client_id"),
    ];
    for (file, expected) in cases {
        let e = ClientConfig::from_google_json(file.as_bytes()).unwrap_err();
        assert!(matches!(&e, PublishError::Config(m) if m.contains(expected)), "{file}: {e:?}");
    }
}

#[test]
fn the_browser_address_asks_for_offline_access_with_pkce() {
    let s = sign_in();
    let (base, query) = s.url.split_once('?').unwrap();
    assert_eq!(base, GOOGLE_AUTH_URI);
    let q = parse_query(query).unwrap();
    assert_eq!(field(&q, "client_id"), Some("123-abc.apps.googleusercontent.com"));
    assert_eq!(field(&q, "redirect_uri"), Some("http://127.0.0.1:5555"));
    assert_eq!(field(&q, "response_type"), Some("code"));
    assert_eq!(field(&q, "scope"), Some(UPLOAD_SCOPE));
    assert_eq!(field(&q, "code_challenge"), Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
    assert_eq!(field(&q, "code_challenge_method"), Some("S256"));
    assert_eq!(field(&q, "state"), Some("state-123"));
    assert_eq!(field(&q, "access_type"), Some("offline"));
    assert!(!s.url.contains(VERIFIER) && !s.url.contains(SECRET), "the verifier and secret never go to the browser");
}

#[test]
fn the_callback_gives_the_code_only_for_this_sign_in() {
    let s = sign_in();
    assert_eq!(
        s.code_from_callback("/?state=state-123&code=4%2F0AeanS0b&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube.upload").unwrap(),
        "4/0AeanS0b"
    );
    assert!(matches!(s.code_from_callback("/?state=state-123&error=access_denied"), Err(PublishError::SignInDenied)));
    let cases =
        ["/?state=other&code=x", "/?code=x", "/", "/?state=state-123", "/?state=state-123&code=", "/?state=state-123&code=%zz", "/?state=state-123&code=a%20b"];
    for target in cases {
        assert!(matches!(s.code_from_callback(target), Err(PublishError::SignIn(_))), "{target}");
    }
    let e = s.code_from_callback("/?state=state-123&error=%3Cscript%3E%0Aalert(1)").unwrap_err();
    assert_eq!(e.to_string(), "sign-in: Google answered <script>alert(1)", "control characters dropped; the page escapes it");
    let many = format!("/?{}", vec!["a=b"; 40].join("&"));
    assert!(s.code_from_callback(&many).is_err());
}

#[test]
fn the_code_is_exchanged_with_the_verifier() {
    let server = Script::new(vec![token_reply(json!({"refresh_token": "1//refresh"}))]);
    let tokens = sign_in().exchange(&server, &client(), "4/code").unwrap();
    assert_eq!(tokens.access_token, "ya29.access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("1//refresh"));
    assert_eq!(tokens.expires_in, Duration::from_secs(3599));
    assert!(tokens.grants(UPLOAD_SCOPE));
    let debug = format!("{tokens:?}");
    assert!(!debug.contains("ya29") && !debug.contains("1//refresh"));

    let seen = server.seen();
    assert_eq!(seen[0].url, GOOGLE_TOKEN_URI);
    assert_eq!(seen[0].content_type.as_deref(), Some("application/x-www-form-urlencoded"));
    let f = form_of(&seen[0].body);
    assert_eq!(field(&f, "grant_type"), Some("authorization_code"));
    assert_eq!(field(&f, "code"), Some("4/code"));
    assert_eq!(field(&f, "code_verifier"), Some(VERIFIER));
    assert_eq!(field(&f, "redirect_uri"), Some("http://127.0.0.1:5555"));
    assert_eq!(field(&f, "client_id"), Some("123-abc.apps.googleusercontent.com"));
    assert_eq!(field(&f, "client_secret"), Some(SECRET));
}

#[test]
fn a_sign_in_without_a_refresh_token_fails() {
    let server = Script::new(vec![token_reply(json!({}))]);
    assert!(matches!(sign_in().exchange(&server, &client(), "4/code"), Err(PublishError::SignIn(_))));
}

#[test]
fn refresh_keeps_the_scope_check_to_the_caller() {
    let server = Script::new(vec![token_reply(json!({"scope": "openid"}))]);
    let tokens = refresh(&server, &client(), "1//refresh").unwrap();
    assert!(tokens.refresh_token.is_none());
    assert!(!tokens.grants(UPLOAD_SCOPE), "the user unticked YouTube on the consent page");
    let f = form_of(&server.seen()[0].body);
    assert_eq!(field(&f, "grant_type"), Some("refresh_token"));
    assert_eq!(field(&f, "refresh_token"), Some("1//refresh"));
    assert!(matches!(refresh(&Script::default(), &client(), "bad\ntoken"), Err(PublishError::Unauthorized)));
}

#[test]
fn token_errors_say_what_to_do() {
    let cases: Vec<(_, fn(&PublishError) -> bool)> = vec![
        (reply(400, &[], r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#), |e| matches!(e, PublishError::Unauthorized)),
        (reply(401, &[], r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#), |e| matches!(e, PublishError::Config(_))),
        (
            reply(400, &[], r#"{"error":"invalid_request","error_description":"Missing code verifier."}"#),
            |e| matches!(e, PublishError::SignIn(m) if m == "Missing code verifier."),
        ),
        (reply(503, &[], "<html>busy</html>"), |e| matches!(e, PublishError::Network(_))),
        (reply(418, &[], ""), |e| matches!(e, PublishError::SignIn(m) if m.contains("418"))),
        (Err(crate::http::TransportError("dns".into())), |e| matches!(e, PublishError::Network(_))),
    ];
    for (answer, expected) in cases {
        let e = refresh(&Script::new(vec![answer]), &client(), "1//refresh").unwrap_err();
        assert!(expected(&e), "{e:?}");
    }
}

#[test]
fn hostile_token_answers_are_errors() {
    for body in [
        "not json",
        "{}",
        r#"{"access_token": ""}"#,
        r#"{"access_token": "a\nb"}"#,
        r#"{"access_token": "ok", "token_type": "mac"}"#,
        r#"{"access_token": "ok", "refresh_token": "a b"}"#,
        r#"{"access_token": 5}"#,
    ] {
        let e = refresh(&Script::new(vec![reply(200, &[], body)]), &client(), "1//refresh").unwrap_err();
        assert!(matches!(e, PublishError::Protocol(_)), "{body}: {e:?}");
    }
    let t = refresh(&Script::new(vec![reply(200, &[], r#"{"access_token": "ok", "expires_in": 99999999999}"#)]), &client(), "1//r").unwrap();
    assert_eq!(t.expires_in, Duration::from_secs(86_400));
    let t = refresh(&Script::new(vec![reply(200, &[], r#"{"access_token": "ok"}"#)]), &client(), "1//r").unwrap();
    assert_eq!(t.expires_in, Duration::from_secs(3600));
}

#[test]
fn percent_encoding_round_trips() {
    for s in ["", "abc-._~", "a b+c&d=e/f?g", "Καλημέρα", "4/0AeanS0b", "%"] {
        assert_eq!(decode(&encode(s)).as_deref(), Some(s), "{s}");
    }
    assert_eq!(encode("a b/ü"), "a%20b%2F%C3%BC");
    assert_eq!(decode("a+b").as_deref(), Some("a b"));
    for bad in ["%", "%4", "%zz", "%C3", "%FF"] {
        assert_eq!(decode(bad), None, "{bad}");
    }
}

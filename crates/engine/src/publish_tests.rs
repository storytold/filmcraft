//! Tests of [`crate::publish`] against a local fake Google (token endpoint and resumable upload
//! endpoint on one socket) and a fake browser.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

const CLIENT_ID: &str = "123-test.apps.googleusercontent.com";

/// What the fake Google saw and how it should answer.
#[derive(Default)]
struct Google {
    refreshes: usize,
    exchanges: usize,
    /// Answer refreshes with `invalid_grant` (the user revoked access).
    revoked: bool,
    /// Refuse the first upload start made with this access token.
    reject_token: Option<String>,
    challenge: String,
    title: String,
    privacy: String,
    total: usize,
    received: Vec<u8>,
}

struct Fake {
    base: String,
    google: Arc<Mutex<Google>>,
}

impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let google = Arc::new(Mutex::new(Google::default()));
        let (shared, b) = (google.clone(), base.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut g = shared.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = serve(stream, &mut g, &b);
            }
        });
        Self { base, google }
    }

    fn g<T>(&self, f: impl FnOnce(&mut Google) -> T) -> T {
        f(&mut self.google.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn client(&self) -> ClientConfig {
        ClientConfig {
            client_id: CLIENT_ID.into(),
            client_secret: "test-secret".into(),
            auth_uri: "https://accounts.google.com/o/oauth2/auth".into(),
            token_uri: format!("{}/token", self.base),
        }
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn param(query: &str, name: &str) -> String {
    query.split('&').find_map(|p| p.strip_prefix(&format!("{name}="))).map(decode).unwrap_or_default()
}

fn serve(stream: TcpStream, g: &mut Google, base: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut words = line.split(' ');
    let (method, target) = (words.next().unwrap_or_default().to_string(), words.next().unwrap_or_default().to_string());
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        if h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let header = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap_or_default();
    let mut body = vec![0; header("content-length").parse().unwrap_or(0)];
    reader.read_exact(&mut body)?;
    let mut out = stream;
    let mut answer =
        |status: &str, extra: &str, body: &str| write!(out, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len());
    if method == "POST" && target == "/token" {
        let form = String::from_utf8_lossy(&body).to_string();
        if param(&form, "grant_type") == "refresh_token" {
            g.refreshes += 1;
            if g.revoked {
                return answer("400 Bad Request", "", r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#);
            }
            let token = format!("ya29.n{}", g.refreshes);
            return answer(
                "200 OK",
                "",
                &format!(r#"{{"access_token":"{token}","expires_in":3599,"token_type":"Bearer","scope":"https://www.googleapis.com/auth/youtube.upload"}}"#),
            );
        }
        g.exchanges += 1;
        use base64::Engine as _;
        let verifier = param(&form, "code_verifier");
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
        if param(&form, "code") != "4/good" || challenge != g.challenge {
            return answer("400 Bad Request", "", r#"{"error":"invalid_grant"}"#);
        }
        return answer(
            "200 OK",
            "",
            r#"{"access_token":"ya29.signin","refresh_token":"1//fresh","expires_in":3599,"token_type":"Bearer","scope":"https://www.googleapis.com/auth/youtube.upload"}"#,
        );
    }
    if method == "POST" && target.starts_with("/upload/youtube/v3/videos?") {
        let token = header("authorization").trim_start_matches("Bearer ").to_string();
        if g.reject_token.as_deref() == Some(token.as_str()) {
            g.reject_token = None;
            return answer("401 Unauthorized", "", r#"{"error":{"code":401,"message":"Invalid Credentials","errors":[{"reason":"authError"}]}}"#);
        }
        let video: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        g.title = video["snippet"]["title"].as_str().unwrap_or_default().into();
        g.privacy = video["status"]["privacyStatus"].as_str().unwrap_or_default().into();
        g.total = header("x-upload-content-length").parse().unwrap_or(0);
        g.received.clear();
        return answer("200 OK", &format!("Location: {base}/upload/session?upload_id=1\r\n"), "");
    }
    if method == "PUT" {
        let range = header("content-range");
        if !range.starts_with("bytes */") {
            g.received.extend_from_slice(&body);
        }
        if g.received.len() == g.total {
            return answer("201 Created", "", r#"{"id":"engineVid_1"}"#);
        }
        return match g.received.len() {
            0 => answer("308 Resume Incomplete", "", ""),
            n => answer("308 Resume Incomplete", &format!("Range: bytes=0-{}\r\n", n - 1), ""),
        };
    }
    answer("404 Not Found", "", "")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-publish-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A session whose publishing files live in a fresh directory.
fn session(name: &str) -> (Session, PathBuf) {
    let dir = tmp(name);
    let mut s = Session::default();
    s.publish.dir = Some(dir.join("publish"));
    (s, dir)
}

/// A session talking to `fake`, signed in with a stored refresh token.
fn signed_in(name: &str, fake: &Fake) -> (Session, PathBuf) {
    let (mut s, dir) = session(name);
    s.publish.client = Some(fake.client());
    s.publish.upload_url = Some(format!("{}/upload/youtube/v3/videos", fake.base));
    write_private(&dir.join("publish").join(ACCOUNT_FILE), json!({"clientId": CLIENT_ID, "refreshToken": "1//stored"}).to_string().as_bytes()).unwrap();
    (s, dir)
}

fn video(dir: &Path, len: usize) -> (String, Vec<u8>) {
    let data: Vec<u8> = (0..len).map(|i| (i * 13 % 256) as u8).collect();
    let path = dir.join("film.mp4");
    std::fs::write(&path, &data).unwrap();
    (path.to_string_lossy().into_owned(), data)
}

fn job(s: &Session, id: u64) -> Value {
    s.jobs.iter().find(|j| j.id == id).unwrap().to_json()
}

/// Wait for job `id` to finish and return its JSON.
fn finished(s: &Session, id: u64) -> Value {
    let started = std::time::Instant::now();
    loop {
        let j = job(s, id);
        if j["finished"] == true {
            return j;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "job {id} still running: {j}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn desktop_client_file() -> String {
    json!({"installed": {
        "client_id": CLIENT_ID, "project_id": "filmcraft-test", "client_secret": "GOCSPX-test",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth", "token_uri": "https://oauth2.googleapis.com/token",
        "redirect_uris": ["http://localhost"],
    }})
    .to_string()
}

#[test]
fn the_client_file_is_checked_and_kept() {
    let (mut s, dir) = session("client");
    let st = s.execute("publish.youtube.status", json!({})).unwrap();
    assert_eq!(st["client"], Value::Null);
    assert_eq!(st["signedIn"], false);
    assert_eq!(st["available"], true, "the tests build the HTTPS transport");

    let web = dir.join("web.json");
    std::fs::write(&web, json!({"web": {"client_id": CLIENT_ID}}).to_string()).unwrap();
    let e = s.execute("publish.youtube.setClient", json!({"path": web})).unwrap_err().to_string();
    assert!(e.contains("Desktop app"), "{e}");
    assert!(s.execute("publish.youtube.setClient", json!({"path": dir.join("missing.json")})).is_err());

    let good = dir.join("client_secret.json");
    std::fs::write(&good, desktop_client_file()).unwrap();
    assert_eq!(s.execute("publish.youtube.setClient", json!({"path": good})).unwrap()["clientId"], CLIENT_ID);
    std::fs::remove_file(&good).unwrap();
    let st = s.execute("publish.youtube.status", json!({})).unwrap();
    assert_eq!(st["client"]["clientId"], CLIENT_ID, "a copy is kept, so moving the download doesn't matter");
    assert!(!st.to_string().contains("GOCSPX"), "the secret is never shown");
}

#[test]
fn uploads_need_an_account_and_valid_details() {
    let fake = Fake::start();
    let (mut s, dir) = session("checks");
    s.publish.client = Some(fake.client());
    let (path, _) = video(&dir, 1000);
    let e = s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip"})).unwrap_err().to_string();
    assert!(e.contains("sign in to YouTube first"), "{e}");
    for (p, expected) in [
        (json!({"path": path, "title": ""}), "title is empty"),
        (json!({"path": path, "title": "a <b>"}), "can't contain"),
        (json!({"path": path, "title": "Trip", "privacy": "secret"}), "unknown variant"),
        (json!({"path": dir.join("none.mp4"), "title": "Trip"}), "can't read"),
        (json!({"title": "Trip"}), "need `path`"),
    ] {
        let e = s.execute("publish.youtube.upload", p).unwrap_err().to_string();
        assert!(e.contains(expected), "{e}");
    }
    assert!(s.jobs.is_empty());
}

#[test]
fn an_upload_refreshes_the_token_once_and_sends_the_whole_file() {
    let fake = Fake::start();
    let (mut s, dir) = signed_in("upload", &fake);
    assert_eq!(s.execute("publish.youtube.status", json!({})).unwrap()["signedIn"], true);
    let (path, data) = video(&dir, 600_000);
    let r = s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip to Crete", "privacy": "unlisted", "wait": true})).unwrap();
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert_eq!(j["result"]["path"], "https://www.youtube.com/watch?v=engineVid_1", "{j}");
    assert_eq!(j["status"], "Uploaded to YouTube: https://www.youtube.com/watch?v=engineVid_1");
    assert_eq!(j["label"], "Uploading “Trip to Crete” to YouTube");
    assert_eq!(j["progress"], 1.0);
    fake.g(|g| {
        assert_eq!(g.received, data);
        assert_eq!((g.title.as_str(), g.privacy.as_str()), ("Trip to Crete", "unlisted"));
        assert_eq!(g.refreshes, 1);
    });

    // The access token is reused while it is fresh.
    let r = s.execute("publish.youtube.upload", json!({"path": path, "title": "Again", "wait": true})).unwrap();
    assert!(finished(&s, r["job"].as_u64().unwrap())["result"]["path"].is_string());
    fake.g(|g| {
        assert_eq!(g.refreshes, 1);
        assert_eq!(g.privacy, "private", "private unless asked otherwise");
    });
    assert!(!s.jobs.iter().any(|j| j.to_json().to_string().contains("ya29")), "no token in any job");
}

/// An export job the test finishes by hand.
fn export_job(s: &mut Session) -> crate::Job {
    let export = new_job(s, "Export film".into());
    s.jobs.push(export.clone());
    export
}

fn end_export(export: &crate::Job, result: std::result::Result<filmcraft_export::Report, String>) {
    *export.result.lock().unwrap() = Some(result);
    export.progress.finished.store(true, Ordering::Relaxed);
}

#[test]
fn an_upload_after_an_export_waits_for_the_file() {
    let fake = Fake::start();
    let (mut s, dir) = signed_in("after-export", &fake);
    let export = export_job(&mut s);
    let planned = dir.join("film.mp4").to_string_lossy().into_owned();
    assert!(!Path::new(&planned).exists(), "the export hasn't written it yet");
    let r = s.execute("publish.youtube.upload", json!({"path": planned, "title": "Trip", "afterJob": export.id})).unwrap();
    let id = r["job"].as_u64().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(job(&s, id)["status"], WAITING_FOR_EXPORT);
    fake.g(|g| assert_eq!(g.refreshes, 0, "nothing goes to Google before the file exists"));

    let (path, data) = video(&dir, 300_000);
    let report = filmcraft_export::Report { path, frames: 24, seconds: 1.0, bytes: data.len() as u64, render_fps: 24.0, extra_files: Vec::new() };
    end_export(&export, Ok(report));
    let j = finished(&s, id);
    assert_eq!(j["result"]["path"], "https://www.youtube.com/watch?v=engineVid_1", "{j}");
    assert_eq!(j["progress"], 1.0);
    fake.g(|g| assert_eq!(g.received, data));
}

/// The export marks itself finished a moment before it stores its report (as
/// `filmcraft_export::export` does): the upload waits for the report instead of reading nothing.
#[test]
fn an_upload_waits_for_the_report_of_a_finished_export() {
    let fake = Fake::start();
    let (mut s, dir) = signed_in("report-late", &fake);
    let export = export_job(&mut s);
    let (path, data) = video(&dir, 300_000);
    let r = s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip", "afterJob": export.id})).unwrap();
    export.progress.finished.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    let report = filmcraft_export::Report { path, frames: 24, seconds: 1.0, bytes: data.len() as u64, render_fps: 24.0, extra_files: Vec::new() };
    *export.result.lock().unwrap() = Some(Ok(report));
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert_eq!(j["result"]["path"], "https://www.youtube.com/watch?v=engineVid_1", "{j}");
    fake.g(|g| assert_eq!(g.received, data));
}

#[test]
fn a_failed_or_unknown_export_uploads_nothing() {
    let fake = Fake::start();
    let (mut s, dir) = signed_in("export-failed", &fake);
    let planned = dir.join("film.mp4").to_string_lossy().into_owned();

    let export = export_job(&mut s);
    let r = s.execute("publish.youtube.upload", json!({"path": planned, "title": "Trip", "afterJob": export.id})).unwrap();
    end_export(&export, Err("cancelled".into()));
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert_eq!(j["result"]["error"], "the export was cancelled, so nothing was uploaded", "{j}");

    let export = export_job(&mut s);
    let r = s.execute("publish.youtube.upload", json!({"path": planned, "title": "Trip", "afterJob": export.id})).unwrap();
    end_export(&export, Err("disk full".into()));
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert_eq!(j["result"]["error"], "the export failed (disk full), so nothing was uploaded", "{j}");

    // Cancelling the upload while it waits leaves the export alone.
    let export = export_job(&mut s);
    let r = s.execute("publish.youtube.upload", json!({"path": planned, "title": "Trip", "afterJob": export.id})).unwrap();
    let id = r["job"].as_u64().unwrap();
    s.execute("jobs.cancel", json!({"job": id})).unwrap();
    assert_eq!(finished(&s, id)["result"]["error"], "stopped");
    assert!(!export.progress.cancel.load(Ordering::Relaxed));

    let e = s.execute("publish.youtube.upload", json!({"path": planned, "title": "Trip", "afterJob": 999})).unwrap_err();
    assert!(e.to_string().contains("no job 999"), "{e}");
    fake.g(|g| {
        assert_eq!(g.refreshes, 0);
        assert!(g.received.is_empty());
    });
}

#[test]
fn a_refused_access_token_is_refreshed_and_the_upload_goes_on() {
    let fake = Fake::start();
    fake.g(|g| g.reject_token = Some("ya29.n1".into()));
    let (mut s, dir) = signed_in("refused", &fake);
    let (path, data) = video(&dir, 1000);
    let r = s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip", "wait": true})).unwrap();
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert!(j["result"]["path"].is_string(), "{j}");
    fake.g(|g| {
        assert_eq!(g.refreshes, 2);
        assert_eq!(g.received, data);
    });
}

#[test]
fn a_revoked_account_is_forgotten() {
    let fake = Fake::start();
    fake.g(|g| g.revoked = true);
    let (mut s, dir) = signed_in("revoked", &fake);
    let (path, _) = video(&dir, 1000);
    let r = s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip", "wait": true})).unwrap();
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert_eq!(j["result"]["error"], "the account needs to sign in again", "{j}");
    assert_eq!(s.execute("publish.youtube.status", json!({})).unwrap()["signedIn"], false);
    assert!(!dir.join("publish").join(ACCOUNT_FILE).exists());
}

/// Google's side of a sign-in: the browser comes back to the app with `code`.
fn browser_returns(url: &str, code: &str) -> std::thread::JoinHandle<String> {
    let query = url.split_once('?').unwrap().1.to_string();
    let (redirect, state, code) = (param(&query, "redirect_uri"), param(&query, "state"), code.to_string());
    std::thread::spawn(move || {
        let addr = redirect.trim_start_matches("http://").to_string();
        let mut stream = TcpStream::connect(&addr).unwrap();
        write!(stream, "GET /?state={state}&code={code}&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube.upload HTTP/1.1\r\nHost: {addr}\r\n\r\n")
            .unwrap();
        let mut page = String::new();
        stream.read_to_string(&mut page).unwrap();
        page
    })
}

#[test]
fn sign_in_opens_the_browser_and_keeps_the_account() {
    let fake = Fake::start();
    let (mut s, dir) = session("sign-in");
    s.publish.client = Some(fake.client());
    let r = s.execute("publish.youtube.signIn", json!({})).unwrap();
    let url = r["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/auth?"), "{url}");
    assert!(s.drain_events().iter().any(|e| matches!(e, crate::Event::OpenUrl(u) if *u == url)), "the UI is asked to open it");
    let id = r["job"].as_u64().unwrap();
    assert_eq!(job(&s, id)["status"], "Waiting for the browser");

    fake.g(|g| g.challenge = param(url.split_once('?').unwrap().1, "code_challenge"));
    let page = browser_returns(&url, "4%2Fgood").join().unwrap();
    assert!(page.contains("FilmCraft is signed in"), "{page}");
    let j = finished(&s, id);
    assert_eq!(j["status"], "Signed in to YouTube", "{j}");
    assert_eq!(s.execute("publish.youtube.status", json!({})).unwrap()["signedIn"], true);
    let account = std::fs::read_to_string(dir.join("publish").join(ACCOUNT_FILE)).unwrap();
    assert!(account.contains("1//fresh") && account.contains(CLIENT_ID));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("publish").join(ACCOUNT_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The sign-in's access token is used at once: no refresh.
    let (path, _) = video(&dir, 1000);
    s.publish.upload_url = Some(format!("{}/upload/youtube/v3/videos", fake.base));
    let r = s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip", "wait": true})).unwrap();
    assert!(finished(&s, r["job"].as_u64().unwrap())["result"]["path"].is_string());
    fake.g(|g| assert_eq!((g.exchanges, g.refreshes), (1, 0)));

    assert_eq!(s.execute("publish.youtube.signOut", json!({})).unwrap()["signedIn"], false);
    assert!(!dir.join("publish").join(ACCOUNT_FILE).exists());
    assert!(s.execute("publish.youtube.upload", json!({"path": path, "title": "Trip"})).is_err());
}

#[test]
fn a_sign_in_can_be_cancelled() {
    let fake = Fake::start();
    let (mut s, _) = session("cancel");
    s.publish.client = Some(fake.client());
    let id = s.execute("publish.youtube.signIn", json!({})).unwrap()["job"].as_u64().unwrap();
    s.execute("jobs.cancel", json!({"job": id})).unwrap();
    assert_eq!(finished(&s, id)["result"]["error"], "stopped");
    assert_eq!(s.execute("publish.youtube.status", json!({})).unwrap()["signedIn"], false);
}

#[test]
fn a_sign_in_the_token_server_refuses_keeps_no_account() {
    let fake = Fake::start();
    let (mut s, _) = session("denied");
    s.publish.client = Some(fake.client());
    let r = s.execute("publish.youtube.signIn", json!({})).unwrap();
    fake.g(|g| g.challenge = "something else".into());
    browser_returns(r["url"].as_str().unwrap(), "4%2Fgood").join().unwrap();
    let j = finished(&s, r["job"].as_u64().unwrap());
    assert_eq!(j["result"]["error"], "the account needs to sign in again", "the token server refused the verifier: {j}");
    assert_eq!(s.execute("publish.youtube.status", json!({})).unwrap()["signedIn"], false);
}

#[test]
fn file_types_are_named_for_youtube() {
    assert_eq!(content_type("a/b/Film.MP4"), "video/mp4");
    assert_eq!(content_type("x.mov"), "video/quicktime");
    assert_eq!(content_type("x.webm"), "video/webm");
    assert_eq!(content_type("x.mxf"), "application/octet-stream");
    assert_eq!(content_type("noext"), "application/octet-stream");
}

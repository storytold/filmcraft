//! The real HTTP transport (ureq) against a local fake YouTube upload server: a whole upload in
//! chunks, a connection cut in the middle of a chunk, a 503 with `Retry-After`, and an upload
//! resumed by a new uploader from the saved session (as after an app restart).

use filmcraft_publish::youtube::{CHUNK_UNIT, UploadSession, Uploader, VideoMetadata};
use filmcraft_publish::{Control, PublishError, http::UreqTransport};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const TOKEN: &str = "ya29.fake";
const VIDEO_ID: &str = "fakeVideo_1";

/// What the fake server has and what it should do wrong.
#[derive(Default)]
struct Server {
    total: usize,
    received: Vec<u8>,
    /// One line per request: `POST`, `PUT */<total> len=0`, `PUT <first>-<last>/<total>`.
    log: Vec<String>,
    chunks: usize,
    /// Read only half of this chunk (1-based) and close the connection without an answer.
    cut_chunk: Option<usize>,
    /// Answer this chunk (1-based) with `503 Retry-After: 1`.
    busy_chunk: Option<usize>,
    authorized: bool,
}

struct Fake {
    url: String,
    state: Arc<Mutex<Server>>,
}

impl Fake {
    fn start(server: Server) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(server));
        let shared = state.clone();
        let session = format!("{base}/upload/youtube/v3/videos?uploadType=resumable&upload_id=fake");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut server = shared.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = handle(stream, &mut server, &session);
            }
        });
        Self { url: format!("{base}/upload/youtube/v3/videos"), state }
    }

    fn with<T>(&self, f: impl FnOnce(&Server) -> T) -> T {
        f(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

fn handle(stream: TcpStream, server: &mut Server, session: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let method = line.split(' ').next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    server.authorized = header("authorization").as_deref() == Some("Bearer ya29.fake");
    let len: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut out = stream;
    if method == "POST" {
        let mut body = vec![0; len];
        reader.read_exact(&mut body)?;
        server.total = header("x-upload-content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        server.log.push("POST".into());
        return answer(&mut out, "200 OK", &[("Location", session)], "");
    }
    let range = header("content-range").unwrap_or_default();
    if range.starts_with("bytes */") {
        server.log.push(format!("PUT {} len={}", range.trim_start_matches("bytes "), header("content-length").unwrap_or_default()));
        return resume_incomplete(&mut out, server);
    }
    server.log.push(format!("PUT {}", range.trim_start_matches("bytes ")));
    server.chunks += 1;
    let first: usize = range.trim_start_matches("bytes ").split('-').next().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    if server.cut_chunk == Some(server.chunks) {
        let mut half = vec![0; len / 2];
        reader.read_exact(&mut half)?;
        if first == server.received.len() {
            server.received.extend_from_slice(&half);
        }
        return Ok(()); // the connection drops with no answer
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    if server.busy_chunk == Some(server.chunks) {
        return answer(&mut out, "503 Service Unavailable", &[("Retry-After", "1")], "");
    }
    if first == server.received.len() {
        server.received.extend_from_slice(&body);
    }
    if server.received.len() == server.total {
        return answer(&mut out, "201 Created", &[("Content-Type", "application/json")], &format!(r#"{{"kind":"youtube#video","id":"{VIDEO_ID}"}}"#));
    }
    resume_incomplete(&mut out, server)
}

fn resume_incomplete(out: &mut TcpStream, server: &Server) -> std::io::Result<()> {
    match server.received.len() {
        0 => answer(out, "308 Resume Incomplete", &[], ""),
        n => answer(out, "308 Resume Incomplete", &[("Range", &format!("bytes=0-{}", n - 1))], ""),
    }
}

fn answer(out: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &str) -> std::io::Result<()> {
    let mut text = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    text.push_str(body);
    out.write_all(text.as_bytes())?;
    out.flush()
}

#[derive(Default)]
struct Recorder {
    waits: Vec<Duration>,
    progress: Vec<u64>,
    stop_at_progress: Option<usize>,
}

impl Control for Recorder {
    fn progress(&mut self, sent: u64, _total: u64) -> bool {
        self.progress.push(sent);
        self.stop_at_progress.is_none_or(|n| self.progress.len() < n)
    }

    fn wait(&mut self, delay: Duration) -> bool {
        self.waits.push(delay);
        true
    }
}

fn video_file() -> Vec<u8> {
    (0..3 * CHUNK_UNIT as usize + 777).map(|i| (i * 7 % 256) as u8).collect()
}

fn meta() -> VideoMetadata {
    VideoMetadata { title: "Fake upload".into(), ..Default::default() }
}

#[test]
fn uploads_in_chunks_through_a_cut_connection_and_a_busy_server() {
    let fake = Fake::start(Server { cut_chunk: Some(2), busy_chunk: Some(4), ..Default::default() });
    let transport = UreqTransport::default();
    let up = Uploader::new(&transport, TOKEN).with_upload_url(&fake.url).with_chunk_size(CHUNK_UNIT);
    let data = video_file();
    let total = data.len() as u64;
    let mut control = Recorder::default();
    let session = up.start(&meta(), total, "video/mp4", &mut control).unwrap();
    let video = up.upload(&session, &mut Cursor::new(data.clone()), &mut control).unwrap();
    assert_eq!(video.id, VIDEO_ID);
    fake.with(|s| {
        assert!(s.authorized);
        assert_eq!(s.received, data, "the server ends up with the exact file");
        let c = CHUNK_UNIT;
        let half = c + c / 2;
        assert_eq!(
            s.log,
            [
                "POST".to_string(),
                format!("PUT */{total} len=0"),
                format!("PUT 0-{}/{total}", c - 1),
                format!("PUT {c}-{}/{total}", 2 * c - 1),
                // cut after half the chunk: ask, then go on from what arrived
                format!("PUT */{total} len=0"),
                format!("PUT {half}-{}/{total}", half + c - 1),
                format!("PUT {}-{}/{total}", half + c, total - 1),
                // 503 Retry-After: 1, ask again, send the same bytes
                format!("PUT */{total} len=0"),
                format!("PUT {}-{}/{total}", half + c, total - 1),
            ]
        );
    });
    assert_eq!(control.waits, [Duration::from_secs(1), Duration::from_secs(1)], "backoff after the cut, Retry-After after the 503");
}

#[test]
fn a_saved_session_resumes_in_a_new_uploader() {
    let fake = Fake::start(Server::default());
    let transport = UreqTransport::default();
    let data = video_file();
    let total = data.len() as u64;
    let up = Uploader::new(&transport, TOKEN).with_upload_url(&fake.url).with_chunk_size(CHUNK_UNIT);
    // The app quits after the first chunk: the cancel comes before the second.
    let mut first_run = Recorder { stop_at_progress: Some(2), ..Default::default() };
    let session = up.start(&meta(), total, "video/mp4", &mut first_run).unwrap();
    let saved = serde_json::to_string(&session).unwrap();
    assert!(matches!(up.upload(&session, &mut Cursor::new(data.clone()), &mut first_run), Err(PublishError::Cancelled)));
    assert_eq!(fake.with(|s| s.received.len() as u64), CHUNK_UNIT);

    let session: UploadSession = serde_json::from_str(&saved).unwrap();
    let transport = UreqTransport::default();
    let up = Uploader::new(&transport, TOKEN).with_upload_url(&fake.url).with_chunk_size(CHUNK_UNIT);
    let mut second_run = Recorder::default();
    assert_eq!(up.upload(&session, &mut Cursor::new(data.clone()), &mut second_run).unwrap().id, VIDEO_ID);
    assert_eq!(second_run.progress.first(), Some(&CHUNK_UNIT), "starts after the chunk the server has");
    fake.with(|s| assert_eq!(s.received, data));
}

#[test]
fn a_server_that_is_gone_is_a_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/upload/youtube/v3/videos", listener.local_addr().unwrap());
    drop(listener);
    let transport = UreqTransport::default();
    let mut control = Recorder::default();
    let e = Uploader::new(&transport, TOKEN).with_upload_url(&url).with_max_failures(2).start(&meta(), 10, "video/mp4", &mut control).unwrap_err();
    assert!(matches!(e, PublishError::Network(_)), "{e:?}");
    assert_eq!(control.waits.len(), 2);
}

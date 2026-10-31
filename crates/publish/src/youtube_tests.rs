//! The upload protocol against scripted server replies (no network).

use crate::http::{Method, Response, TransportError};
use crate::test_support::{Script, reply};
use crate::youtube::{
    CHUNK_UNIT, MAX_CHUNK_SIZE, Privacy, UPLOAD_URL, UploadSession, UploadedVideo, Uploader, VideoMetadata, google_error, received_bytes, retry_after,
};
use crate::{Control, PublishError};
use serde_json::json;
use std::io::Cursor;
use std::time::Duration;

const SESSION: &str = "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&upload_id=xyz";
const TOKEN: &str = "ya29.test-token";

fn has(n: u64) -> Result<Response, TransportError> {
    reply(308, &[("Range", &format!("bytes=0-{}", n - 1))], "")
}

fn fresh() -> Result<Response, TransportError> {
    reply(308, &[], "")
}

fn done(id: &str) -> Result<Response, TransportError> {
    reply(200, &[], &json!({"kind": "youtube#video", "id": id, "status": {"uploadStatus": "uploaded"}}).to_string())
}

fn google(status: u16, reason: &str, message: &str) -> Result<Response, TransportError> {
    reply(status, &[], &json!({"error": {"code": status, "message": message, "errors": [{"reason": reason, "domain": "youtube"}]}}).to_string())
}

fn dropped() -> Result<Response, TransportError> {
    Err(TransportError("connection reset".into()))
}

/// Records progress and waits; never sleeps.
#[derive(Default)]
struct Recorder {
    progress: Vec<u64>,
    waits: Vec<Duration>,
    cancel_after_progress: Option<usize>,
    cancel_waits: bool,
}

impl Control for Recorder {
    fn progress(&mut self, sent: u64, _total: u64) -> bool {
        self.progress.push(sent);
        self.cancel_after_progress.is_none_or(|n| self.progress.len() <= n)
    }

    fn wait(&mut self, delay: Duration) -> bool {
        self.waits.push(delay);
        !self.cancel_waits
    }
}

fn file(len: usize) -> Cursor<Vec<u8>> {
    Cursor::new((0..len).map(|i| (i % 251) as u8).collect())
}

fn session(total: u64) -> UploadSession {
    UploadSession { uri: SESSION.into(), total, content_type: "video/mp4".into() }
}

fn meta() -> VideoMetadata {
    VideoMetadata { title: "My film".into(), ..Default::default() }
}

const C: u64 = CHUNK_UNIT;

#[test]
fn metadata_follows_youtubes_limits() {
    let check = |m: VideoMetadata| m.validate().map_err(|e| e.to_string());
    assert!(check(meta()).is_ok());
    assert!(check(VideoMetadata { title: "  ".into(), ..meta() }).unwrap_err().contains("empty"));
    assert!(check(VideoMetadata { title: "α".repeat(100), ..meta() }).is_ok(), "100 characters, not bytes");
    assert!(check(VideoMetadata { title: "a".repeat(101), ..meta() }).is_err());
    assert!(check(VideoMetadata { title: "a <b>".into(), ..meta() }).is_err());
    assert!(check(VideoMetadata { description: "α".repeat(2500), ..meta() }).is_ok(), "5000 bytes");
    assert!(check(VideoMetadata { description: "α".repeat(2501), ..meta() }).is_err());
    assert!(check(VideoMetadata { description: "1 > 0".into(), ..meta() }).is_err());
    // 4 tags of 124 characters + 3 commas = 499; a space adds two quotes.
    let tags = vec!["t".repeat(124); 4];
    assert!(check(VideoMetadata { tags: tags.clone(), ..meta() }).is_ok());
    let mut spaced = tags.clone();
    spaced[0] = format!("a {}", "t".repeat(122));
    assert!(check(VideoMetadata { tags: spaced, ..meta() }).is_err(), "quotes count");
    assert!(check(VideoMetadata { tags: vec!["a,b".into()], ..meta() }).is_err());
    assert!(check(VideoMetadata { tags: vec![" ".into()], ..meta() }).is_err());
    assert!(check(VideoMetadata { category_id: Some("22".into()), ..meta() }).is_ok());
    assert!(check(VideoMetadata { category_id: Some("music".into()), ..meta() }).is_err());
}

#[test]
fn the_resource_is_private_by_default() {
    let m = VideoMetadata { title: " Trip ".into(), tags: vec![" sea ".into()], made_for_kids: true, ..Default::default() };
    assert_eq!(
        m.resource(),
        json!({"snippet": {"title": "Trip", "description": "", "tags": ["sea"]}, "status": {"privacyStatus": "private", "selfDeclaredMadeForKids": true}})
    );
    let unlisted = VideoMetadata { privacy: Privacy::Unlisted, category_id: Some("22".into()), ..meta() };
    assert_eq!(unlisted.resource()["status"]["privacyStatus"], "unlisted");
    assert_eq!(unlisted.resource()["snippet"]["categoryId"], "22");
    let parsed: VideoMetadata = serde_json::from_value(json!({"title": "x", "privacy": "public", "madeForKids": false})).unwrap();
    assert_eq!(parsed.privacy, Privacy::Public);
}

#[test]
fn start_opens_a_session_with_the_file_size_and_type() {
    let server = Script::new(vec![reply(200, &[("Location", SESSION)], "")]);
    let s = Uploader::new(&server, TOKEN).start(&meta(), 1234, "video/mp4", &mut Recorder::default()).unwrap();
    assert_eq!(s, session(1234));
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, Method::Post);
    assert_eq!(seen[0].url, format!("{UPLOAD_URL}?uploadType=resumable&part=snippet,status"));
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer ya29.test-token"));
    let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["snippet"]["title"], "My film");
    assert_eq!(body["status"]["privacyStatus"], "private");
}

#[test]
fn start_checks_before_sending_anything() {
    let server = Script::default();
    let up = Uploader::new(&server, TOKEN);
    let mut c = Recorder::default();
    assert!(matches!(up.start(&VideoMetadata::default(), 10, "video/mp4", &mut c), Err(PublishError::InvalidMetadata(_))));
    assert!(matches!(up.start(&meta(), 0, "video/mp4", &mut c), Err(PublishError::EmptyFile)));
    assert!(matches!(up.start(&meta(), 10, "text/html", &mut c), Err(PublishError::InvalidMetadata(_))));
    assert!(matches!(up.start(&meta(), 10, "video/mp4\r\nX-Evil: 1", &mut c), Err(PublishError::InvalidMetadata(_))));
    assert!(matches!(Uploader::new(&server, "").start(&meta(), 10, "video/mp4", &mut c), Err(PublishError::Unauthorized)));
    assert!(matches!(Uploader::new(&server, "a\r\nb").start(&meta(), 10, "video/mp4", &mut c), Err(PublishError::Unauthorized)));
    assert!(server.seen().is_empty());
}

#[test]
fn start_refuses_a_session_on_another_server() {
    for location in ["https://evil.example/upload?upload_id=1", "https://www.googleapis.com@evil.example/x", "ftp://www.googleapis.com/x", "https://"] {
        let server = Script::new(vec![reply(200, &[("Location", location)], "")]);
        let r = Uploader::new(&server, TOKEN).start(&meta(), 10, "video/mp4", &mut Recorder::default());
        assert!(matches!(r, Err(PublishError::Protocol(_))), "{location}: {r:?}");
    }
    let server = Script::new(vec![reply(200, &[], "")]);
    assert!(matches!(Uploader::new(&server, TOKEN).start(&meta(), 10, "video/mp4", &mut Recorder::default()), Err(PublishError::Protocol(_))));
}

#[test]
fn start_retries_server_errors_with_backoff() {
    let server = Script::new(vec![reply(503, &[], ""), dropped(), reply(200, &[("Location", SESSION)], "")]);
    let mut c = Recorder::default();
    assert!(Uploader::new(&server, TOKEN).start(&meta(), 10, "video/mp4", &mut c).is_ok());
    assert_eq!(c.waits, [Duration::from_secs(1), Duration::from_secs(2)]);
    assert_eq!(server.seen().len(), 3);
}

#[test]
fn start_reports_account_problems() {
    let cases: Vec<(Result<Response, TransportError>, fn(&PublishError) -> bool)> = vec![
        (google(401, "authError", "Invalid Credentials"), |e| matches!(e, PublishError::Unauthorized)),
        (google(401, "youtubeSignupRequired", "no channel"), |e| matches!(e, PublishError::NoChannel)),
        (google(403, "quotaExceeded", "quota"), |e| matches!(e, PublishError::QuotaExceeded)),
        (google(400, "uploadLimitExceeded", "limit"), |e| matches!(e, PublishError::UploadLimitExceeded)),
        (google(404, "notFound", "wrong address"), |e| matches!(e, PublishError::Api { status: 404, .. })),
    ];
    for (answer, expected) in cases {
        let server = Script::new(vec![answer]);
        let e = Uploader::new(&server, TOKEN).start(&meta(), 10, "video/mp4", &mut Recorder::default()).unwrap_err();
        assert!(expected(&e), "{e:?}");
    }
}

#[test]
fn upload_sends_chunks_and_returns_the_video() {
    let total = 2 * C + 1000;
    let server = Script::new(vec![fresh(), has(C), has(2 * C), done("dQw4w9WgXcQ")]);
    let mut c = Recorder::default();
    let video = Uploader::new(&server, TOKEN).with_chunk_size(C).upload(&session(total), &mut file(total as usize), &mut c).unwrap();
    assert_eq!(video, UploadedVideo { id: "dQw4w9WgXcQ".into() });
    assert_eq!(video.watch_url(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    let total_s = total.to_string();
    assert_eq!(
        server.ranges(),
        [
            format!("bytes */{total_s}"),
            format!("bytes 0-{}/{total_s}", C - 1),
            format!("bytes {C}-{}/{total_s}", 2 * C - 1),
            format!("bytes {}-{}/{total_s}", 2 * C, total - 1),
        ]
    );
    let seen = server.seen();
    assert!(seen.iter().all(|s| s.method == Method::Put && s.url == SESSION && s.authorization.as_deref() == Some("Bearer ya29.test-token")));
    assert!(seen[0].body.is_empty());
    let sent: Vec<u8> = seen[1..].iter().flat_map(|s| s.body.clone()).collect();
    assert_eq!(sent, file(total as usize).into_inner());
    assert_eq!(c.progress, [0, C, 2 * C, total]);
    assert!(c.waits.is_empty());
}

#[test]
fn a_partly_received_chunk_continues_where_the_server_stopped() {
    let total = 2 * C;
    let server = Script::new(vec![fresh(), has(1000), done("abc")]);
    Uploader::new(&server, TOKEN).with_chunk_size(4 * C).upload(&session(total), &mut file(total as usize), &mut Recorder::default()).unwrap();
    assert_eq!(server.ranges()[2], format!("bytes 1000-{}/{total}", total - 1));
    assert_eq!(server.seen()[2].body, file(total as usize).into_inner()[1000..]);
}

#[test]
fn a_dropped_connection_resumes_from_what_the_server_has() {
    let total = 3 * C;
    // The second chunk is cut off; the server kept part of it.
    let server = Script::new(vec![fresh(), has(C), dropped(), has(C + 5000), has(2 * C + 5000), done("abc")]);
    let mut c = Recorder::default();
    Uploader::new(&server, TOKEN).with_chunk_size(C).upload(&session(total), &mut file(total as usize), &mut c).unwrap();
    let r = server.ranges();
    assert_eq!(r[2], format!("bytes {C}-{}/{total}", 2 * C - 1));
    assert_eq!(r[3], format!("bytes */{total}"), "asks before resending");
    assert_eq!(r[4], format!("bytes {}-{}/{total}", C + 5000, 2 * C + 4999));
    assert_eq!(c.waits, [Duration::from_secs(1)]);
}

#[test]
fn resuming_a_session_after_a_restart_skips_what_arrived() {
    let total = 2 * C;
    let server = Script::new(vec![has(C), done("abc")]);
    Uploader::new(&server, TOKEN).with_chunk_size(C).upload(&session(total), &mut file(total as usize), &mut Recorder::default()).unwrap();
    assert_eq!(server.ranges(), [format!("bytes */{total}"), format!("bytes {C}-{}/{total}", total - 1)]);
}

#[test]
fn retry_after_is_honoured_and_capped() {
    let server = Script::new(vec![fresh(), reply(503, &[("Retry-After", "7")], ""), reply(429, &[("Retry-After", "999999")], ""), fresh(), done("abc")]);
    let mut c = Recorder::default();
    Uploader::new(&server, TOKEN).upload(&session(10), &mut file(10), &mut c).unwrap();
    assert_eq!(c.waits, [Duration::from_secs(7), Duration::from_secs(600)]);
    assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None, "dates fall back to the backoff");
}

#[test]
fn rate_limits_are_retried() {
    let server = Script::new(vec![fresh(), google(403, "userRateLimitExceeded", "slow down"), fresh(), done("abc")]);
    let mut c = Recorder::default();
    Uploader::new(&server, TOKEN).upload(&session(10), &mut file(10), &mut c).unwrap();
    assert_eq!(c.waits.len(), 1);
}

#[test]
fn retries_run_out() {
    let server = Script::new((0..4).map(|_| reply(500, &[], "")).collect());
    let mut c = Recorder::default();
    let e = Uploader::new(&server, TOKEN).with_max_failures(3).upload(&session(10), &mut file(10), &mut c).unwrap_err();
    assert!(matches!(&e, PublishError::Network(m) if m.contains("500")), "{e:?}");
    assert_eq!(c.waits, [1, 2, 4].map(Duration::from_secs));
}

#[test]
fn backoff_doubles_up_to_a_minute() {
    let server = Script::new((0..9).map(|_| dropped()).collect());
    let mut c = Recorder::default();
    let e = Uploader::new(&server, TOKEN).with_max_failures(8).upload(&session(10), &mut file(10), &mut c).unwrap_err();
    assert!(matches!(&e, PublishError::Network(m) if m == "connection reset"), "{e:?}");
    assert_eq!(c.waits, [1, 2, 4, 8, 16, 32, 64, 64].map(Duration::from_secs));
}

#[test]
fn a_server_that_keeps_nothing_stalls() {
    let total = 2 * C;
    let mut replies = vec![fresh()];
    replies.extend((0..4).map(|_| has(1000)));
    let server = Script::new(replies);
    let e = Uploader::new(&server, TOKEN)
        .with_chunk_size(C)
        .with_max_failures(2)
        .upload(&session(total), &mut file(total as usize), &mut Recorder::default())
        .unwrap_err();
    assert!(matches!(e, PublishError::Stalled), "{e:?}");
}

#[test]
fn session_errors_end_the_upload() {
    let cases: Vec<(Result<Response, TransportError>, fn(&PublishError) -> bool)> = vec![
        (reply(404, &[], ""), |e| matches!(e, PublishError::SessionExpired)),
        (reply(410, &[], ""), |e| matches!(e, PublishError::SessionExpired)),
        (google(401, "authError", "expired"), |e| matches!(e, PublishError::Unauthorized)),
        (google(403, "quotaExceeded", "quota"), |e| matches!(e, PublishError::QuotaExceeded)),
        (
            google(403, "forbiddenPrivacySetting", "nope"),
            |e| matches!(e, PublishError::Api { status: 403, reason: Some(r), message } if r == "forbiddenPrivacySetting" && message == "nope"),
        ),
    ];
    for (answer, expected) in cases {
        let server = Script::new(vec![fresh(), answer]);
        let e = Uploader::new(&server, TOKEN).upload(&session(10), &mut file(10), &mut Recorder::default()).unwrap_err();
        assert!(expected(&e), "{e:?}");
    }
}

#[test]
fn hostile_replies_are_errors_not_crashes() {
    let total = 2 * C;
    for range in [
        "bytes=0-",
        "bytes=5-10",
        "bytes=0-99999999999999999999999",
        &format!("bytes=0-{total}"),
        "bytes=0-18446744073709551615",
        "items=0-1",
        "",
        "bytes=0--1",
    ] {
        let server = Script::new(vec![reply(308, &[("Range", range)], "")]);
        let e = Uploader::new(&server, TOKEN).upload(&session(total), &mut file(total as usize), &mut Recorder::default()).unwrap_err();
        assert!(matches!(e, PublishError::Protocol(_)), "{range:?}: {e:?}");
    }
    for body in ["", "not json", "{}", r#"{"id": ""}"#, r#"{"id": "../../x"}"#, r#"{"id": 42}"#, &format!(r#"{{"id": "{}"}}"#, "a".repeat(65))] {
        let server = Script::new(vec![reply(201, &[], body)]);
        let e = Uploader::new(&server, TOKEN).upload(&session(total), &mut file(total as usize), &mut Recorder::default()).unwrap_err();
        assert!(matches!(e, PublishError::Protocol(_)), "{body:?}: {e:?}");
    }
    assert_eq!(received_bytes("bytes=0-0", 1), Some(1));
    assert_eq!(received_bytes(" bytes=0-99 ", 100), Some(100));
    assert_eq!(google_error(b"<html>502</html>"), (None, String::new()));
    let (reason, message) = google_error(json!({"error": {"message": "x".repeat(1000), "errors": [{"reason": "bad reason!"}]}}).to_string().as_bytes());
    assert_eq!(reason, None);
    assert_eq!(message.chars().count(), 301, "capped, with an ellipsis");
}

#[test]
fn cancel_stops_between_chunks_and_during_waits() {
    let total = 3 * C;
    let server = Script::new(vec![fresh(), has(C)]);
    let mut c = Recorder { cancel_after_progress: Some(1), ..Default::default() };
    let e = Uploader::new(&server, TOKEN).with_chunk_size(C).upload(&session(total), &mut file(total as usize), &mut c).unwrap_err();
    assert!(matches!(e, PublishError::Cancelled));
    assert_eq!(server.seen().len(), 2, "no chunk after the cancel");

    let server = Script::new(vec![fresh(), dropped()]);
    let mut c = Recorder { cancel_waits: true, ..Default::default() };
    let e = Uploader::new(&server, TOKEN).upload(&session(10), &mut file(10), &mut c).unwrap_err();
    assert!(matches!(e, PublishError::Cancelled));
}

#[test]
fn a_file_that_changes_size_stops_the_upload() {
    let server = Script::default();
    let e = Uploader::new(&server, TOKEN).upload(&session(100), &mut file(99), &mut Recorder::default()).unwrap_err();
    assert!(matches!(e, PublishError::FileChanged));
    let e = Uploader::new(&server, TOKEN).upload(&session(0), &mut file(0), &mut Recorder::default()).unwrap_err();
    assert!(matches!(e, PublishError::EmptyFile));
    let elsewhere = UploadSession { uri: "https://evil.example/upload".into(), ..session(10) };
    let e = Uploader::new(&server, TOKEN).upload(&elsewhere, &mut file(10), &mut Recorder::default()).unwrap_err();
    assert!(matches!(e, PublishError::Protocol(_)));
    assert!(server.seen().is_empty());
}

#[test]
fn chunk_sizes_are_whole_units_within_limits() {
    let total = 3 * C;
    for (asked, expected) in [(0, C), (C + 1, C), (2 * C + 5, 2 * C), (u64::MAX, MAX_CHUNK_SIZE)] {
        let server = Script::new(vec![fresh(), done("abc")]);
        let total = total.max(expected);
        let _ = Uploader::new(&server, TOKEN).with_chunk_size(asked).upload(&session(total), &mut file(total as usize), &mut Recorder::default());
        assert_eq!(server.seen()[1].body.len() as u64, expected.min(total), "asked {asked}");
    }
}

#[test]
fn secrets_stay_out_of_debug_output_and_errors() {
    let s = session(10);
    assert!(!format!("{s:?}").contains("upload_id"));
    let server = Script::new(vec![fresh(), google(500, "backendError", "oops"), fresh(), google(400, "invalidTitle", "bad title")]);
    let e = Uploader::new(&server, TOKEN).upload(&s, &mut file(10), &mut Recorder::default()).unwrap_err();
    let text = e.to_string();
    assert_eq!(text, "the server answered 400 (invalidTitle): bad title");
    assert!(!text.contains(TOKEN) && !text.contains("upload_id"));
}

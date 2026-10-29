//! WebDAV parsing and ranged-response checks, including hostile inputs.

use crate::remote_config::{Config, parse};
use crate::remote_dav::{MAX_ENTRIES, check_range_response, children_of, parse_content_range, parse_http_date, parse_multistatus};

const ORIGIN: &str = "https://app.example";

/// The test config: one mount at `/data`.
fn cfg() -> Config {
    parse(br#"{"remote":{"mounts":[{"path":"/data","url":"/dav/"}]}}"#, ORIGIN, "/").unwrap()
}

/// One `d:response` element of a multistatus body.
fn resp(href: &str, props: &str, status: &str) -> String {
    format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>{status}</d:status></d:propstat></d:response>")
}

/// A multistatus document around `body`.
fn multi(body: &str) -> String {
    format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{body}</d:multistatus>"#)
}

const DIR: &str = "<d:resourcetype><d:collection/></d:resourcetype>";

/// Listing is parsed.
#[test]
fn listing_is_parsed() {
    let xml = multi(&[
        resp("/dav/", DIR, "HTTP/1.1 200 OK"),
        resp("/dav/clip%201.mp4", "<d:getcontentlength>1234</d:getcontentlength><d:getlastmodified>Wed, 21 Oct 2015 07:28:00 GMT</d:getlastmodified><d:getetag>\"abc\"</d:getetag><d:resourcetype/>", "HTTP/1.1 200 OK"),
        resp("/dav/sub/", DIR, "HTTP/1.1 200 OK"),
    ].concat());
    let e = parse_multistatus(xml.as_bytes()).unwrap();
    assert_eq!(e.len(), 3);
    assert!(e[0].is_dir);
    assert_eq!((e[1].size, e[1].modified, e[1].etag.as_deref()), (Some(1234), Some(1_445_412_480), Some("\"abc\"")));
    assert!(!e[1].is_dir);
    let c = cfg();
    let kids = children_of(&c, &c.mounts[0], "/data", ORIGIN, e);
    let names: Vec<&str> = kids.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(names, ["clip 1.mp4", "sub"]);
    assert_eq!(kids[0].path, "/data/clip 1.mp4");
}

/// Foreign deeper and traversing hrefs are dropped.
#[test]
fn foreign_deeper_and_traversing_hrefs_are_dropped() {
    let p = "<d:getcontentlength>1</d:getcontentlength>";
    let xml = multi(
        &[
            resp("/dav/", DIR, "HTTP/1.1 200 OK"),
            resp("/elsewhere/a.mp4", p, "HTTP/1.1 200 OK"),
            resp("https://evil.example/dav/a.mp4", p, "HTTP/1.1 200 OK"),
            resp("/dav/../etc/passwd", p, "HTTP/1.1 200 OK"),
            resp("/dav/deep/er/a.mp4", p, "HTTP/1.1 200 OK"),
            resp("/dav/ok.mp4", p, "HTTP/1.1 200 OK"),
        ]
        .concat(),
    );
    let c = cfg();
    let kids = children_of(&c, &c.mounts[0], "/data", ORIGIN, parse_multistatus(xml.as_bytes()).unwrap());
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].path, "/data/ok.mp4");
}

/// Bad properties are ignored not trusted.
#[test]
fn bad_properties_are_ignored_not_trusted() {
    let xml = multi(
        &[
            resp("/dav/a", "<d:getcontentlength>-5</d:getcontentlength>", "HTTP/1.1 200 OK"),
            resp("/dav/b", "<d:getcontentlength>99999999999999999999999</d:getcontentlength>", "HTTP/1.1 200 OK"),
            resp("/dav/c", "<d:getcontentlength>7</d:getcontentlength>", "HTTP/1.1 404 Not Found"),
            resp("/dav/d", "<d:getlastmodified>yesterday</d:getlastmodified>", "garbage"),
        ]
        .concat(),
    );
    let e = parse_multistatus(xml.as_bytes()).unwrap();
    assert!(e.iter().all(|x| x.size.is_none() && x.modified.is_none()));
}

/// Malformed and hostile xml is an error.
#[test]
fn malformed_and_hostile_xml_is_an_error() {
    assert!(parse_multistatus(b"").is_err());
    assert!(parse_multistatus(b"<d:multistatus xmlns:d=\"DAV:\"><d:response>").is_err());
    assert!(parse_multistatus(b"<html></html>").is_err());
    assert!(parse_multistatus(&[0xff, 0xfe]).is_err());
    // an entity-expansion DTD is refused outright
    let bomb = r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;&a;">]><d:multistatus xmlns:d="DAV:">&b;</d:multistatus>"#;
    assert!(parse_multistatus(bomb.as_bytes()).is_err());
    // wrong namespace
    assert!(parse_multistatus(br#"<multistatus xmlns="urn:x"/>"#).is_err());
}

/// Entry count and body size are capped.
#[test]
fn entry_count_and_body_size_are_capped() {
    let body: String = (0..=MAX_ENTRIES).map(|i| resp(&format!("/dav/f{i}"), "", "HTTP/1.1 200 OK")).collect();
    assert!(parse_multistatus(multi(&body).as_bytes()).is_err());
    let few: String = (0..10).map(|i| resp(&format!("/dav/f{i}"), "", "HTTP/1.1 200 OK")).collect();
    assert_eq!(parse_multistatus(multi(&few).as_bytes()).unwrap().len(), 10);
    assert!(parse_multistatus(&vec![b' '; (16 << 20) + 1]).is_err());
}

/// Http dates.
#[test]
fn http_dates() {
    assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:01 GMT"), Some(1));
    assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
    assert_eq!(parse_http_date("Mon, 01 Jan 1900 00:00:00 GMT"), None);
    assert_eq!(parse_http_date("nonsense"), None);
}

/// Content range parsing.
#[test]
fn content_range_parsing() {
    let r = parse_content_range("bytes 0-99/1000").unwrap();
    assert_eq!((r.start, r.end, r.total), (0, 99, Some(1000)));
    assert_eq!(parse_content_range("bytes 5-5/*").unwrap().total, None);
    for h in ["", "bytes", "bytes 10-5/100", "bytes 0-100/100", "bytes a-b/c", "bytes 0-1", "items 0-1/5", "bytes -5/100", "bytes 0-99999999999999999999/5"] {
        assert!(parse_content_range(h).is_none(), "{h:?}");
    }
}

/// Ranged responses are validated.
#[test]
fn ranged_responses_are_validated() {
    let mib = 1 << 20;
    // a full chunk
    assert_eq!(check_range_response(206, Some("bytes 0-1048575/5000000"), 0, mib, 5_000_000), Ok(mib));
    // the short last chunk
    assert_eq!(check_range_response(206, Some("bytes 4194304-4999999/5000000"), 4 * mib, mib, 5_000_000), Ok(805_696));
    // a server that ignores Range and sends 200: refused
    assert!(check_range_response(200, None, 0, mib, 5_000_000).is_err());
    assert!(check_range_response(200, Some("bytes 0-1048575/5000000"), 0, mib, 5_000_000).is_err());
    assert!(check_range_response(416, None, 0, mib, 5_000_000).is_err());
    assert!(check_range_response(206, None, 0, mib, 5_000_000).is_err());
    // wrong window, wrong total, malformed
    assert!(check_range_response(206, Some("bytes 1-1048576/5000000"), 0, mib, 5_000_000).is_err());
    assert!(check_range_response(206, Some("bytes 0-99/5000000"), 0, mib, 5_000_000).is_err());
    assert!(check_range_response(206, Some("bytes 0-1048575/9999999"), 0, mib, 5_000_000).is_err());
    assert!(check_range_response(206, Some("bytes 0-1048575/*"), 0, mib, 5_000_000).is_ok());
    assert!(check_range_response(206, Some("garbage"), 0, mib, 5_000_000).is_err());
    // overflow and past-the-end requests
    assert!(check_range_response(206, Some("bytes 0-1/2"), u64::MAX, 2, 2).is_err());
    assert!(check_range_response(206, Some("bytes 0-1/2"), 10, mib, 2).is_err());
    assert!(check_range_response(206, Some("bytes 0-0/0"), 0, mib, 0).is_err());
}

/// Metadata requests time out quickly and file transfers get a long cap.
#[test]
fn metadata_requests_time_out_quickly_and_file_transfers_get_a_long_cap() {
    use crate::remote_dav::timeout_ms;
    for m in ["PROPFIND", "HEAD", "MOVE", "DELETE"] {
        assert_eq!(timeout_ms(m), 20_000, "{m}");
    }
    assert_eq!(timeout_ms("GET"), 300_000);
    assert_eq!(timeout_ms("PUT"), 300_000);
}

/// A time budget shortens a request's timeout but never lengthens it or lets it reach zero.
#[test]
fn a_time_budget_shortens_the_request_timeout() {
    use crate::remote_dav::capped_timeout_ms;
    assert_eq!(capped_timeout_ms(20_000, None), 20_000);
    assert_eq!(capped_timeout_ms(20_000, Some(5_000)), 5_000);
    assert_eq!(capped_timeout_ms(20_000, Some(60_000)), 20_000);
    assert_eq!(capped_timeout_ms(20_000, Some(u64::MAX)), 20_000);
    assert_eq!(capped_timeout_ms(20_000, Some(0)), 1);
}

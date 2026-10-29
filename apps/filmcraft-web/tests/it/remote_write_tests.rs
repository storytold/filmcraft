//! Request building and answer handling of a save to a remote mount.

use crate::remote_config::{Config, parse};
use crate::remote_write::{
    Known, Outcome, Precondition, check_writable, classify_move, classify_put, delete_part, failure_message, head_target, move_part, part_path, precondition,
    put_part,
};

const ORIGIN: &str = "https://app.example";

/// The test config: `/rw` writable, `/ro` read-only.
fn cfg() -> Config {
    parse(br#"{"remote":{"mounts":[{"path":"/rw","url":"/dav/rw/","writable":true,"name":"Work"},{"path":"/ro","url":"/dav/ro/"}]}}"#, ORIGIN, "/").unwrap()
}

/// Part file is beside the target.
#[test]
fn part_file_is_beside_the_target() {
    assert_eq!(part_path("/rw/a/x.fcproj", "k3").unwrap(), "/rw/a/x.fcproj.k3.part");
    assert!(part_path("/rw/a/", "k3").is_err());
    assert!(part_path("", "k3").is_err());
}

/// Only writable mounts take writes.
#[test]
fn only_writable_mounts_take_writes() {
    let c = cfg();
    assert!(check_writable(&c, "/rw/x.fcproj").is_ok());
    let e = check_writable(&c, "/ro/x.fcproj").unwrap_err();
    assert!(e.contains("read-only"), "{e}");
    assert!(check_writable(&c, "/elsewhere/x").is_err());
    assert!(put_part(&c, "/ro/x.fcproj", "k3").is_err());
    assert!(move_part(&c, ORIGIN, "/ro/x.fcproj", "k3", true).is_err());
    assert!(delete_part(&c, "/ro/x.fcproj", "k3").is_err());
}

/// The upload goes to the part file with an encoded url.
#[test]
fn the_upload_goes_to_the_part_file_with_an_encoded_url() {
    let r = put_part(&cfg(), "/rw/My Films/a#b.fcproj", "k3").unwrap();
    assert_eq!((r.method, r.url.as_str()), ("PUT", "/dav/rw/My%20Films/a%23b.fcproj.k3.part"));
    let h = head_target(&cfg(), "/rw/My Films/a#b.fcproj").unwrap();
    assert_eq!((h.method, h.url.as_str()), ("HEAD", "/dav/rw/My%20Films/a%23b.fcproj"));
}

/// Each upload gets its own part file.
#[test]
fn each_upload_gets_its_own_part_file() {
    assert_ne!(part_path("/rw/x.fcproj", "a1").unwrap(), part_path("/rw/x.fcproj", "b2").unwrap());
    for t in ["", "a/b", "a.b", "..", "a b"] {
        assert!(part_path("/rw/x.fcproj", t).is_err(), "{t:?}");
    }
}

/// The move names the destination and overwrite.
#[test]
fn the_move_names_the_destination_and_overwrite() {
    let m = move_part(&cfg(), ORIGIN, "/rw/x.fcproj", "k3", true).unwrap();
    assert_eq!((m.method, m.url.as_str()), ("MOVE", "/dav/rw/x.fcproj.k3.part"));
    assert!(m.headers.contains(&("Destination", "https://app.example/dav/rw/x.fcproj".to_string())));
    assert!(m.headers.contains(&("Overwrite", "T".to_string())));
    let m = move_part(&cfg(), ORIGIN, "/rw/x.fcproj", "k3", false).unwrap();
    assert!(m.headers.contains(&("Overwrite", "F".to_string())));
    // a conditional header on a MOVE would be tested against the .part file: never sent
    assert!(m.headers.iter().all(|(k, _)| !k.eq_ignore_ascii_case("if-match") && !k.eq_ignore_ascii_case("if-none-match")));
    let d = delete_part(&cfg(), "/rw/x.fcproj", "k3").unwrap();
    assert_eq!((d.method, d.url.as_str()), ("DELETE", "/dav/rw/x.fcproj.k3.part"));
}

/// Hostile paths build no request.
#[test]
fn hostile_paths_build_no_request() {
    for p in ["/rw/../ro/x", "/rw/a//b", "/rw/a\\b", "/rw/"] {
        assert!(put_part(&cfg(), p, "k3").is_err(), "{p}");
        assert!(move_part(&cfg(), ORIGIN, p, "k3", true).is_err(), "{p}");
    }
}

/// A changed target is a conflict not an overwrite.
#[test]
fn a_changed_target_is_a_conflict_not_an_overwrite() {
    let was = Known::Etag("\"v1\"".into());
    assert_eq!(precondition(&was, 200, Some("\"v1\"")), Precondition::Proceed { overwrite: true });
    assert_eq!(precondition(&was, 200, Some("W/\"v1\"")), Precondition::Proceed { overwrite: true });
    assert_eq!(precondition(&was, 200, Some("\"v2\"")), Precondition::Conflict);
    // no ETag from the server now: it cannot be checked
    assert_eq!(precondition(&was, 200, None), Precondition::Proceed { overwrite: true });
    assert_eq!(precondition(&Known::NoEtag, 200, Some("\"x\"")), Precondition::Proceed { overwrite: true });
    // a file this session never saw must not exist
    assert_eq!(precondition(&Known::Unknown, 200, Some("\"x\"")), Precondition::Exists);
    assert_eq!(precondition(&Known::Unknown, 404, None), Precondition::Proceed { overwrite: false });
    assert_eq!(precondition(&was, 404, None), Precondition::Proceed { overwrite: false });
    for s in [401, 403, 500, 503, 301] {
        assert!(matches!(precondition(&was, s, None), Precondition::Failed(_)), "{s}");
    }
}

/// Answers are classified exactly.
#[test]
fn answers_are_classified_exactly() {
    for s in [200, 201, 204] {
        assert!(classify_put(s).is_ok());
    }
    for s in [202, 207, 400, 403, 409, 412, 413, 507] {
        assert!(classify_put(s).is_err(), "{s}");
    }
    assert_eq!(classify_move(201, true), Outcome::Saved);
    assert_eq!(classify_move(204, false), Outcome::Saved);
    assert_eq!(classify_move(412, true), Outcome::Conflict);
    assert_eq!(classify_move(412, false), Outcome::Exists);
    for s in [200, 207, 403, 404, 409, 423, 500] {
        assert!(matches!(classify_move(s, true), Outcome::Failed(_)), "{s}");
    }
}

/// Failures say what to do.
#[test]
fn failures_say_what_to_do() {
    assert_eq!(failure_message(&Outcome::Saved), None);
    assert!(failure_message(&Outcome::Conflict).unwrap().contains("Save As"));
    assert!(failure_message(&Outcome::Exists).unwrap().contains("already exists"));
    assert!(failure_message(&Outcome::Failed("x".into())).unwrap().contains("x"));
}

/// The version a save leaves is the move's own, or the target's only when it is the upload's.
#[test]
fn a_saved_version_is_tied_to_the_upload() {
    use crate::remote_write::{SavedVersion as V, head_part, saved_version};
    let v = |s: &str| V::Etag(s.to_string());
    assert_eq!(saved_version(Some("\"m\""), Some("\"p\""), Some(Some("\"x\""))), v("\"m\""));
    // the move's own version needs no HEAD
    assert_eq!(saved_version(Some("\"m\""), None, None), v("\"m\""));
    assert_eq!(saved_version(None, Some("\"p\""), Some(Some("\"p\""))), v("\"p\""));
    // the target's ETag differs from the upload's, or the upload's is unknown or weak: its bytes decide
    assert_eq!(saved_version(None, Some("\"p\""), Some(Some("\"x\""))), V::Verify);
    assert_eq!(saved_version(None, None, Some(Some("\"x\""))), V::Verify);
    assert_eq!(saved_version(Some("W/\"m\""), Some("W/\"p\""), Some(Some("W/\"p\""))), V::Verify);
    // a failed HEAD shows nothing: never "no ETag"
    assert_eq!(saved_version(None, Some("\"p\""), None), V::Unbound);
    assert_eq!(saved_version(None, None, None), V::Unbound);
    // a server that answers without ETags is as when the file was opened
    assert_eq!(saved_version(None, None, Some(None)), V::NoEtag);
    assert_eq!(saved_version(None, Some("\"p\""), Some(Some(" "))), V::NoEtag);
    let h = head_part(&cfg(), "/rw/x.fcproj", "a1b2").unwrap();
    assert_eq!((h.method, h.url.as_str()), ("HEAD", "/dav/rw/x.fcproj.a1b2.part"));
}

/// A server that gives the moved target a new ETag must not make the next save of the same session
/// fail, and a file another client wrote must still be a conflict.
#[test]
fn a_regenerated_etag_is_bound_only_when_the_bytes_are_ours() {
    use crate::remote_write::{Known, Precondition, SavedVersion as V, get_target, precondition, saved_version, verify_saved};
    let ours: &[u8] = b"project bytes";
    let g = get_target(&cfg(), "/rw/x.fcproj").unwrap();
    assert_eq!((g.method, g.url.as_str()), ("GET", "/dav/rw/x.fcproj"));
    // MOVE answered without an ETag and the target got a new one: same bytes, so bind the new ETag
    assert_eq!(saved_version(None, Some("\"p\""), Some(Some("\"new\""))), V::Verify);
    let bound = verify_saved(ours, Some((200, Some("\"new\""), ours)));
    assert_eq!(bound, V::Etag("\"new\"".into()));
    // the next save's HEAD shows that ETag: it goes ahead
    assert_eq!(precondition(&Known::Etag("\"new\"".into()), 200, Some("\"new\"")), Precondition::Proceed { overwrite: true });
    // another client changed it between our two saves: still a conflict
    assert_eq!(precondition(&Known::Etag("\"new\"".into()), 200, Some("\"theirs\"")), Precondition::Conflict);
    // the same ETag after the move needs no read-back
    assert_eq!(saved_version(None, Some("\"p\""), Some(Some("\"p\""))), V::Etag("\"p\"".into()));
    // no ETag at all on the target: as for a file opened without one
    assert_eq!(saved_version(None, None, Some(None)), V::NoEtag);
    assert_eq!(verify_saved(ours, Some((200, None, ours))), V::NoEtag);
    // a weak ETag on matching bytes is bound (the bytes were checked)
    assert_eq!(verify_saved(ours, Some((200, Some("W/\"w\""), ours))), V::Etag("W/\"w\"".into()));
    // another writer got in between (even same size), or the read-back failed: never bound
    assert_eq!(verify_saved(ours, Some((200, Some("\"x\""), b"project BYTES"))), V::Unbound);
    assert_eq!(verify_saved(ours, Some((200, Some("\"x\""), b"other"))), V::Unbound);
    assert_eq!(verify_saved(ours, Some((404, None, b""))), V::Unbound);
    assert_eq!(verify_saved(ours, None), V::Unbound);
    // HEAD failed after the move: the target is not read, the version is dropped
    assert_eq!(saved_version(None, Some("\"p\""), None), V::Unbound);
}

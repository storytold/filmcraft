//! Media URL handling at the public interchange boundary.

use filmcraft_interchange::{Format, file_url_to_path, import, resolve_path};
use filmcraft_project::MediaRef;

#[test]
fn unicode_file_urls_do_not_panic_or_lose_the_path() {
    for (url, path) in [("file://srv/日本語.mov", "//srv/日本語.mov"), ("file://localhost/media/漢字.mov", "/media/漢字.mov")] {
        let parsed = std::panic::catch_unwind(|| file_url_to_path(url));
        assert!(parsed.is_ok(), "file URL panicked: {url}");
        assert_eq!(parsed.unwrap(), path);
        assert_eq!(resolve_path(url, Some("/project")), path);
    }
    for path in ["/media/漢字.mov", "/日本語.mov", "//srv/日本語.mov"] {
        let url = format!("file://{path}");
        let parsed = std::panic::catch_unwind(|| file_url_to_path(&url));
        assert!(parsed.is_ok(), "file URL panicked: {url}");
        assert_eq!(parsed.unwrap(), path);
        assert_eq!(resolve_path(&url, Some("/project")), path);
    }
}

#[test]
fn edl_import_keeps_unicode_file_url_media() {
    let doc = "TITLE: Unicode media
FCM: NON-DROP FRAME
001 AX V C 00:00:00:00 00:00:01:00 00:00:00:00 00:00:01:00
* SOURCE FILE: file:///日本語.mov
";
    let parsed = std::panic::catch_unwind(|| import(doc.as_bytes(), Format::Edl, None));
    assert!(parsed.is_ok(), "importing a Unicode file URL panicked");
    let (imported, report) = parsed.unwrap().unwrap();
    let paths: Vec<_> = imported
        .project
        .items
        .values()
        .filter_map(|item| item.as_media())
        .filter_map(|media| match &media.media {
            MediaRef::File { path } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(paths, ["/日本語.mov"]);
    assert!(!report.has_warnings(), "{report}");
}

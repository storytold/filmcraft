//! The page host mode's pure rules (the `data-host` opt-in, open names, project or media). They run
//! natively even though the rest of the crate is wasm32-only.

#[path = "../../src/hostmode.rs"]
mod rules;

use rules::{DEFAULT_OPEN_NAME, MAX_OPEN_BYTES, is_project, is_save_as, open_name, open_size_ok, page_host_requested};

#[test]
fn only_the_exact_page_value_opts_in() {
    assert!(page_host_requested(Some("page")));
    for off in [None, Some(""), Some("Page"), Some("PAGE"), Some(" page"), Some("page "), Some("parent")] {
        assert!(!page_host_requested(off), "{off:?} must keep the page host mode off");
    }
}

#[test]
fn open_names_fall_back_to_the_default_and_lose_path_separators() {
    assert_eq!(open_name(Some("Trailer v2.fcproj")), "Trailer v2.fcproj");
    assert_eq!(open_name(Some(" Sheet2/../../etc.fcproj ")), " Sheet2_.._.._etc.fcproj ");
    assert_eq!(open_name(Some("")), DEFAULT_OPEN_NAME);
    assert_eq!(open_name(Some("   ")), DEFAULT_OPEN_NAME);
    assert_eq!(open_name(None), DEFAULT_OPEN_NAME);
}

#[test]
fn only_project_files_open_as_projects() {
    assert!(is_project("Trailer.fcproj"));
    assert!(is_project("TRAILER.FCPROJ"));
    assert!(!is_project("clip.mp4"));
    assert!(!is_project("notes.txt"));
    assert!(!is_project("fcproj"));
    assert!(!is_project(""));
}

#[test]
fn save_as_only_when_the_host_gains_a_new_document_file() {
    // Save of the document the host opened, under its own name — and a Save As back to the
    // file the host already has as the document: it stays the document, so `false`.
    assert!(!is_save_as("/projects/Movie.fcproj", Some("/projects/Movie.fcproj"), Some("/projects/Movie.fcproj")));
    // Save As (the session adopts the written path), and the first Save of an untitled
    // project: the file becomes the document's file, new to the host.
    assert!(is_save_as("/projects/Trailer.fcproj", Some("/projects/Movie.fcproj"), Some("/projects/Trailer.fcproj")));
    assert!(is_save_as("/projects/Untitled.fcproj", None, Some("/projects/Untitled.fcproj")));
    // Save a Copy, exports, captions, the schema backup: copies; the document's file stays.
    assert!(!is_save_as("/projects/Movie copy.fcproj", Some("/projects/Movie.fcproj"), Some("/projects/Movie.fcproj")));
    assert!(!is_save_as("/exports/trailer.mp4", Some("/projects/Movie.fcproj"), Some("/projects/Movie.fcproj")));
    assert!(!is_save_as("/exports/captions.srt", None, None));
}

#[test]
fn open_sizes_up_to_the_limit_are_accepted() {
    assert!(open_size_ok(0));
    assert!(open_size_ok(MAX_OPEN_BYTES));
    assert!(!open_size_ok(MAX_OPEN_BYTES + 1));
    assert_eq!(MAX_OPEN_BYTES >> 20, 512);
}

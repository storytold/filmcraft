//! Pure host-mode rules, shared by the bridge ([`crate::host`]) and testable without a browser:
//! what opts a page into hosting FilmCraft in its own document, and the name and kind the
//! bridge reads out of an open request. Pure, so it is tested natively in `tests/it/hostmode.rs`.

/// The one `data-host` value that opts a page in: the exact string `"page"`.
pub const PAGE_HOST: &str = "page";

/// The file name used when an open request carries no usable one: an untitled project — what
/// a host that never names a file still opens (FilmCraft's document is its project file).
pub const DEFAULT_OPEN_NAME: &str = "Untitled.fcproj";

/// The extension that makes a host's file open like File › Open; anything else imports like a
/// file dropped on the page.
pub const PROJECT_EXT: &str = ".fcproj";

/// Does this `data-host` value ask for page host mode? Only the exact value [`PAGE_HOST`]
/// counts; anything else is ignored, so the page host mode stays opt-in. `?host=parent`, the
/// parent bridge's URL opt-in, is answered before this is asked.
pub fn page_host_requested(data_host: Option<&str>) -> bool {
    data_host == Some(PAGE_HOST)
}

/// A usable file name for an open request: the host's name when it isn't blank, else the
/// [`DEFAULT_OPEN_NAME`]. `/` and `\` become `_`, as [`crate::fs::register_blob`] does for
/// dropped names — the name becomes a path in the virtual file table.
pub fn open_name(name: Option<&str>) -> String {
    let name = name.filter(|n| !n.trim().is_empty()).unwrap_or(DEFAULT_OPEN_NAME);
    name.chars().map(|c| if c == '/' || c == '\\' { '_' } else { c }).collect()
}

/// Is this host file a project (open like File › Open) rather than media to import (like a
/// drop)? The app's own drop rule: the name's extension, case-insensitive.
pub fn is_project(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(PROJECT_EXT)
}

/// The largest `filmcraft:open` payload, refused before any byte is copied. FilmCraft has no
/// single open limit (media is read through a chunked cache, projects whole at 256 MiB), so
/// the bridge takes the family's sane default.
pub const MAX_OPEN_BYTES: u64 = 512 << 20;

/// Whether a payload may be copied: the check runs before any byte is copied.
pub fn open_size_ok(len: u64) -> bool {
    len <= MAX_OPEN_BYTES
}

/// Whether a written file becomes the document's file (`filmcraft:save` with `saveAs: true`):
/// a write the session adopted as its project path that the host did not already have as the
/// document.
pub fn is_save_as(write: &str, host_document: Option<&str>, project_path: Option<&str>) -> bool {
    host_document != Some(write) && project_path == Some(write)
}

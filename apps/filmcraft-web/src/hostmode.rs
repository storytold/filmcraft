//! Pure host-mode rules, shared by the bridge ([`crate::host`]) and testable without a browser:
//! the name and kind the bridge reads out of an open request, and the `saveAs` rule. Which pages
//! host the app is `craft-web-host`'s. Pure, so it is tested natively in `tests/it/hostmode.rs`.

/// The file name used when an open request carries no usable one: an untitled project — what
/// a host that never names a file still opens (FilmCraft's document is its project file).
pub const DEFAULT_OPEN_NAME: &str = "Untitled.fcproj";

/// The extension that makes a host's file open like File › Open; anything else imports like a
/// file dropped on the page.
pub const PROJECT_EXT: &str = ".fcproj";

/// The name `craft-web-host` gives an open request that carries no usable one (it cleans a host's
/// name and falls back to this).
const NAMELESS: &str = "document";

/// A usable file name for an open request: the cleaned name the crate hands over, or
/// [`DEFAULT_OPEN_NAME`] when the host sent none. `/` and `\` become `_`, as
/// [`crate::fs::register_blob`] does for dropped names — the name becomes a path in the virtual
/// file table.
pub fn open_name(name: &str) -> String {
    let name = if name == NAMELESS || name.trim().is_empty() { DEFAULT_OPEN_NAME } else { name };
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

/// The largest project read whole, the same bound as `fs::WHOLE_FILE_MAX`. A larger `.fcproj`
/// is refused before its bytes are copied; media up to [`MAX_OPEN_BYTES`] stays a `Blob`.
pub const PROJECT_OPEN_MAX: u64 = 256 << 20;

/// Whether a project may be copied into the file table and read whole.
pub fn project_open_ok(len: u64) -> bool {
    len <= PROJECT_OPEN_MAX
}

/// Whether a written file becomes the document's file (`filmcraft:save` with `saveAs: true`):
/// a write the session adopted as its project path that the host did not already have as the
/// document.
pub fn is_save_as(write: &str, host_document: Option<&str>, project_path: Option<&str>) -> bool {
    host_document != Some(write) && project_path == Some(write)
}

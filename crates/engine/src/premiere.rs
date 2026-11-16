//! Native Premiere import with owned snapshots and read-only source handling. MIT OR Apache-2.0.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use filmcraft_interchange::ImportOptions;
use filmcraft_project::{
    ItemKind, MediaRef,
    workflow::{PremiereImport, SourceMedia},
};
use filmcraft_time::FrameRate;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::commands::{CommandSpec, always, bad, u64_p};
use crate::{EngineError, Result, Session};

const COMMAND: &str = "premiere.import";
fn io(e: std::io::Error) -> EngineError {
    EngineError::Other(format!("Premiere import: {e}"))
}
fn string<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s)),
        _ => Err(bad(COMMAND, format!("{key} must be a nonempty string"))),
    }
}
/// Whether `path` names a Premiere project (a `.prproj` file, in any letter case), which
/// `file.import` hands to [`import`].
pub(crate) fn is_project(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("prproj"))
}

/// Canonical Windows paths are verbatim; store ordinary drive/UNC paths for interchange.
fn stored_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") { format!(r"\\{unc}") } else { path.strip_prefix(r"\\?\").unwrap_or(&path).to_string() }
}

fn canonical_media_path(path: &str) -> std::io::Result<PathBuf> {
    let result = Path::new(path).canonicalize();
    #[cfg(target_os = "macos")]
    let result = result
        .or_else(|error| if let Some(path) = path.strip_prefix("/Volumes/Macintosh HD/") { Path::new("/").join(path).canonicalize() } else { Err(error) });
    result
}

/// A document reference is not a file the user explicitly picked. Refuse it before filesystem
/// access when policy or extension disallows it, and probe before any hashing or copying. Returns
/// the canonical file and the source opened from its stored path.
fn probe_media(s: &Session, original: &str, document: &Path) -> std::result::Result<(PathBuf, filmcraft_media::SharedSource), String> {
    let resolved = crate::doc_path::resolve(original, Some(document)).map_err(|e| format!("document path refused: {e}"))?;
    if !filmcraft_media::is_importable(Path::new(&resolved)) {
        return Err("unrecognized media extension".into());
    }
    let canonical = canonical_media_path(&resolved).map_err(|e| format!("missing or inaccessible media: {e}"))?;
    let path = stored_path(&canonical);
    crate::doc_path::resolve(&path, Some(document)).map_err(|e| format!("document path refused: {e}"))?;
    if !filmcraft_media::is_importable(&canonical) || !canonical.is_file() {
        return Err("not a regular file with a recognized media extension".into());
    }
    let source = s.media.open_file(&path, &*s.services).map_err(|e| format!("media probe failed: {e}"))?;
    Ok((canonical, source))
}

/// Probe the stored path, then the project-relative path (a project moved between computers),
/// then either one with its file name matched without regard to letter case (a project saved
/// on Windows or macOS keeps `clip.mov` for a file that is `clip.MOV` on a case-sensitive disk).
/// Every candidate passes the same document policy and probe.
fn probe_with_fallbacks(
    s: &Session,
    original: &str,
    relative: Option<&String>,
    document: &Path,
    report: &mut filmcraft_interchange::Report,
    listings: &mut FolderListings,
) -> std::result::Result<(PathBuf, filmcraft_media::SharedSource), String> {
    let error = match probe_media(s, original, document) {
        Ok(found) => return Ok(found),
        Err(error) => error,
    };
    let mut errors = error.clone();
    if let Some(relative) = relative {
        match probe_media(s, relative, document) {
            Ok(found) => {
                filmcraft_interchange::premiere::warning(report, format!("{original}: {error}; using its project-relative path {relative}"));
                return Ok(found);
            }
            Err(relative_error) => errors = format!("{error}; project-relative path {relative}: {relative_error}"),
        }
    }
    for candidate in std::iter::once(original).chain(relative.map(String::as_str)) {
        let Some(variant) = crate::doc_path::resolve(candidate, Some(document)).ok().and_then(|p| case_variant(Path::new(&p), listings)) else { continue };
        let variant = stored_path(&variant);
        if let Ok(found) = probe_media(s, &variant, document) {
            filmcraft_interchange::premiere::warning(report, format!("{original}: {error}; using {variant}, whose name differs only in letter case"));
            return Ok(found);
        }
    }
    Err(errors)
}

/// The one entry of `path`'s folder whose name equals its file name apart from letter case, when
/// `path` itself does not exist. None when there is no such entry, more than one, or the folder
/// could not be listed in full.
fn case_variant(path: &Path, listings: &mut FolderListings) -> Option<PathBuf> {
    if std::fs::symlink_metadata(path).is_ok() {
        return None;
    }
    let name = path.file_name()?.to_str()?.to_lowercase();
    let folder = path.parent()?;
    listings.find(folder, &name).map(|entry| folder.join(entry))
}

/// Most entries read from one folder when looking for a letter-case variant; a folder with more
/// offers none.
const MAX_FOLDER_ENTRIES: usize = 100_000;
/// Most names the folder listings of one import keep (tens of megabytes at most); past it, a
/// further folder is read again for each lookup, as without a cache.
const MAX_LISTED_NAMES: usize = 250_000;

/// A folder's entries by lower-case name: the entry's own name, or None when several entries
/// have that lower-case name (ambiguous).
type CaseIndex = std::collections::HashMap<String, Option<std::ffi::OsString>>;

/// Folder listings one import reads while looking for letter-case variants: each folder is read
/// once, however many of its files are missing, up to a bounded number of names in all.
struct FolderListings {
    /// Folder → its entries, or None when it could not be read in full.
    folders: std::collections::HashMap<PathBuf, Option<CaseIndex>>,
    /// Names kept so far (a folder that could not be read in full counts as one).
    names: usize,
    /// Most entries read from one folder ([`MAX_FOLDER_ENTRIES`]).
    entries: usize,
    /// Most names kept.
    limit: usize,
}

impl FolderListings {
    /// No listings yet; each folder is read up to `entries` entries, and at most `limit` names
    /// will be kept.
    fn new(entries: usize, limit: usize) -> Self {
        Self { folders: std::collections::HashMap::new(), names: 0, entries, limit }
    }

    /// The one entry of `folder` whose name is `lower` (lower case) apart from letter case; None
    /// when there is no such entry, more than one, or the folder cannot be read in full (kept or
    /// read again, the listing follows the same rule).
    fn find(&mut self, folder: &Path, lower: &str) -> Option<std::ffi::OsString> {
        if let Some(listing) = self.folders.get(folder) {
            return listing.as_ref()?.get(lower).cloned().flatten();
        }
        let listing =
            std::fs::read_dir(folder).ok().and_then(|entries| index_by_lower_case(entries.map(|entry| entry.map(|entry| entry.file_name())), self.entries));
        let found = listing.as_ref().and_then(|listing| listing.get(lower).cloned().flatten());
        let cost = listing.as_ref().map_or(0, CaseIndex::len).max(1);
        if let Some(names) = self.names.checked_add(cost).filter(|names| *names <= self.limit) {
            self.names = names;
            self.folders.insert(folder.to_path_buf(), listing);
        }
        found
    }
}

/// Index entry names by lower case; names that are not valid Unicode are left out. None unless
/// every entry was read: an entry that could not be read, or one past the first `max`, may be a
/// second name that differs only in letter case, so a partial listing makes no match unique.
fn index_by_lower_case(names: impl IntoIterator<Item = std::io::Result<std::ffi::OsString>>, max: usize) -> Option<CaseIndex> {
    let mut index = CaseIndex::new();
    for (read, name) in names.into_iter().enumerate() {
        let name = name.ok().filter(|_| read < max)?;
        let Some(lower) = name.to_str().map(str::to_lowercase) else { continue };
        index.entry(lower).and_modify(|unique| *unique = None).or_insert(Some(name));
    }
    Some(index)
}

/// SHA-256 (lower-case hex) and size of a whole regular file.
pub(crate) fn hash_file(path: &Path) -> Result<(String, u64)> {
    hash_file_within(path, u64::MAX).map_err(|failure| failure.error)
}

/// Why a file could not be hashed, with how many of its bytes were read first: they count against
/// the caller's byte limit all the same.
#[derive(Debug)]
pub(super) struct HashFailure {
    pub error: EngineError,
    pub read: u64,
}

/// [`hash_file`], refusing a file longer than `max` bytes once that many have been read (a file
/// that grows while it is hashed cannot extend the work past the caller's limit). A failure says
/// how many bytes were read before it.
pub(super) fn hash_file_within(path: &Path, max: u64) -> std::result::Result<(String, u64), HashFailure> {
    let mut read = 0;
    hash_counting(path, max, &mut read).map_err(|error| HashFailure { error, read })
}

/// [`hash_file_within`], counting the bytes it reads in `read`.
fn hash_counting(path: &Path, max: u64, read: &mut u64) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path).map_err(io)?;
    if !f.metadata().map_err(io)?.is_file() {
        return Err(bad(COMMAND, "source must be a regular file"));
    }
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(io)?;
        let chunk = buf.get(..n).ok_or_else(|| EngineError::Other(format!("{}: read past the buffer", path.display())))?;
        if chunk.is_empty() {
            break;
        }
        *read = read.saturating_add(chunk.len() as u64);
        if *read > max {
            return Err(EngineError::Other(format!("{}: over the {max} byte inspection limit", path.display())));
        }
        hash.update(chunk);
    }
    Ok((format!("{:x}", hash.finalize()), *read))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)
}
/// Why copying media failed.
#[derive(Debug)]
pub(super) enum CopyError {
    /// The source could not be read: the media stays an offline placeholder.
    Unreadable(String),
    /// The copy could not be written, or its source changed meanwhile: the import is cancelled.
    Cancel(EngineError),
}

/// Copy `source` into the new file `dest` in one pass, hashing the bytes as they are written, and
/// return their SHA-256 and count: the copy holds exactly the bytes the digest covers. A source
/// whose size or modification time changes during the copy cancels the import.
pub(super) fn copy_hashing(source: &Path, dest: &Path) -> std::result::Result<(String, u64), CopyError> {
    let unreadable = |e: std::io::Error| CopyError::Unreadable(e.to_string());
    let unwritable = |e: std::io::Error| CopyError::Cancel(io(e));
    let mut input = std::fs::File::open(source).map_err(unreadable)?;
    let before = input.metadata().map_err(unreadable)?;
    if !before.is_file() {
        return Err(CopyError::Unreadable("not a regular file".into()));
    }
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(dest).map_err(unwritable)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = input.read(&mut buf).map_err(unreadable)?;
        let chunk = buf.get(..n).ok_or_else(|| CopyError::Unreadable("read past the buffer".into()))?;
        if chunk.is_empty() {
            break;
        }
        hash.update(chunk);
        output.write_all(chunk).map_err(unwritable)?;
        size = size.saturating_add(chunk.len() as u64);
    }
    output.sync_all().map_err(unwritable)?;
    let after = input.metadata().map_err(unreadable)?;
    if before.len() != size || after.len() != size || before.modified().ok() != after.modified().ok() {
        return Err(CopyError::Cancel(bad(COMMAND, "media changed while copying; import cancelled")));
    }
    Ok((format!("{:x}", hash.finalize()), size))
}

/// Remove what an unreadable source left of its copy (nothing when it could not be opened).
fn remove_partial(partial: &Path) -> Result<()> {
    match std::fs::remove_file(partial) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
        _ => Ok(()),
    }
}

/// Give a finished copy its content name `target`. When this import already holds that file (the
/// same bytes from another source path), the existing copy is verified and kept instead.
fn publish_copy(partial: &Path, target: &Path, digest: &str, hash: &dyn Fn(&Path) -> Result<(String, u64)>) -> Result<()> {
    if std::fs::symlink_metadata(target).is_err() {
        return std::fs::rename(partial, target).map_err(io);
    }
    std::fs::remove_file(partial).map_err(io)?;
    if hash(target)?.0 != digest {
        return Err(bad(COMMAND, "media changed while copying; import cancelled"));
    }
    Ok(())
}

/// A source copy mode copied: the finished copy's stored path, SHA-256 and size, and the source
/// its probe opened.
struct Copied {
    path: String,
    digest: String,
    size: u64,
    source: filmcraft_media::SharedSource,
}

/// Copy mode: copy `src` into the new file `partial` in `media_dir`, hashing it on the way, give
/// the copy its digest name, then probe the copy. The item describes the bytes the copy holds, not
/// the source as its earlier probe saw it: the source may have been replaced in between. `Ok(Err)`
/// says why the source could not be read (it stays an offline placeholder); a copy that cannot be
/// opened as media means its source changed after the probe, and cancels the import.
fn copy_media(s: &Session, src: &Path, partial: &Path, media_dir: &Path, file_io: &FileIo) -> Result<std::result::Result<Copied, String>> {
    // One read: the copy is hashed as it is written, then named by its digest.
    let (digest, size) = match (file_io.copy)(src, partial) {
        Ok(copied) => copied,
        Err(CopyError::Unreadable(error)) => {
            remove_partial(partial)?;
            return Ok(Err(error));
        }
        Err(CopyError::Cancel(error)) => return Err(error),
    };
    let ext = src.extension().and_then(|e| e.to_str()).filter(|s| s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("media");
    let target = media_dir.join(format!("{digest}.{ext}"));
    publish_copy(partial, &target, &digest, file_io.hash)?;
    let path = stored_path(&target);
    let source = s
        .media
        .open_file(&path, &*s.services)
        .map_err(|e| bad(COMMAND, format!("media changed while copying; import cancelled: the copy of {} cannot be opened: {e}", stored_path(src))))?;
    Ok(Ok(Copied { path, digest, size, source }))
}

/// Only a directory created by this invocation is removed on failure. Existing destinations are
/// always refused, including dangling symlinks. Successful snapshots survive undo.
struct OwnedDirectory {
    path: PathBuf,
    committed: bool,
}
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// The import report's entries, in order, as `{level, message, count}` with the lower-case level
/// names `events.list` entries use (`info`, `warning`); `report` lists the messages alone.
fn report_entries(report: &filmcraft_interchange::Report) -> Vec<Value> {
    let level = |level| match level {
        filmcraft_interchange::Level::Info => "info",
        filmcraft_interchange::Level::Warning => "warning",
    };
    report.entries.iter().map(|e| json!({"level":level(e.level),"message":e.message,"count":e.count})).collect()
}

/// The whole-file reads `premiere.import` makes, replaceable in tests.
pub(super) struct FileIo<'a> {
    /// SHA-256 and size of a whole file: the project file, and a copy this import already made.
    pub hash: &'a dyn Fn(&Path) -> Result<(String, u64)>,
    /// Copy media into a new file, hashing it on the way (copy mode).
    pub copy: &'a dyn Fn(&Path, &Path) -> std::result::Result<(String, u64), CopyError>,
}

/// `premiere.import`: snapshot a Premiere project into a new destination directory, link or copy
/// its media, and merge its bins, media and sequences into the project as one undoable edit.
pub fn import(s: &mut Session, p: &Value) -> Result<Value> {
    import_with_io(s, p, &FileIo { hash: &hash_file, copy: &copy_hashing })
}

/// [`import`] with its whole-file reads replaceable in tests.
pub(super) fn import_with_io(s: &mut Session, p: &Value, file_io: &FileIo) -> Result<Value> {
    let hash = file_io.hash;
    if s.services.export_in_memory() {
        return Err(bad(COMMAND, "Premiere link/copy import requires a native filesystem host"));
    }
    if !p.is_object() {
        return Err(bad(COMMAND, "parameters must be an object"));
    }
    let source = Path::new(string(p, "path")?.ok_or_else(|| bad(COMMAND, "need path"))?).canonicalize().map_err(io)?;
    if !source.is_file() {
        return Err(bad(COMMAND, "source must be a regular project file"));
    }
    let mode = string(p, "mediaMode")?.unwrap_or("link");
    if !matches!(mode, "link" | "copy") {
        return Err(bad(COMMAND, "mediaMode must be link or copy"));
    }
    // As for other documents (`file.import`, `file.importAaf`): the project's items go into this
    // bin, or the root when it is absent or no longer exists.
    let bin = u64_p(p, "bin").map(filmcraft_project::BinId);
    let fps: Option<FrameRate> =
        p.get("frameRate").map(|v| serde_json::from_value(v.clone())).transpose().map_err(|e| bad(COMMAND, format!("frameRate: {e}")))?;
    let bytes = filmcraft_interchange::premiere::read_input(std::fs::File::open(&source).map_err(io)?).map_err(|e| bad(COMMAND, e.to_string()))?;
    let opts =
        ImportOptions { base_dir: source.parent().map(stored_path), name: source.file_stem().map(|p| p.to_string_lossy().into_owned()), ..Default::default() };
    let (mut fragment, tree, mut report) = filmcraft_interchange::premiere::import(&bytes, &opts, fps).map_err(|e| bad(COMMAND, e.to_string()))?;
    let requested = string(p, "sequence")?;
    let selected = if let Some(selector) = requested {
        if let Some(index) = tree.sequences.iter().position(|seq| seq.object_uid == selector) {
            index
        } else {
            let mut matching = tree.sequences.iter().enumerate().filter(|(_, seq)| seq.name == selector);
            let (index, _) = matching.next().ok_or_else(|| bad(COMMAND, "unknown sequence name or ObjectUID"))?;
            if matching.next().is_some() {
                return Err(bad(COMMAND, "ambiguous sequence name; pass its ObjectUID instead"));
            }
            index
        }
    } else {
        tree.sequences
            .iter()
            .enumerate()
            .max_by_key(|(_, seq)| {
                let items = seq.tracks.iter().filter(|t| t.kind == "video").flat_map(|t| &t.items);
                (items.clone().count(), seq.name == "Sequence 01", items.map(|i| i.end).max().unwrap_or_default())
            })
            .map(|(index, _)| index)
            .unwrap_or(0)
    };
    if selected < fragment.sequences.len() {
        fragment.sequences.swap(0, selected);
    }
    let source_hash = format!("{:x}", Sha256::digest(&bytes));
    let destination = if let Some(d) = string(p, "destination")? {
        PathBuf::from(d)
    } else {
        let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("Premiere");
        let parent = source.parent().ok_or_else(|| bad(COMMAND, "source has no parent directory"))?;
        (1..=10000)
            .map(|n| parent.join(format!("{stem}.filmcraft-import-{n}")))
            .find(|p| std::fs::symlink_metadata(p).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound))
            .ok_or_else(|| bad(COMMAND, "choose an unused destination directory"))?
    };
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).canonicalize().map_err(io)?;
    let destination = parent.join(destination.file_name().ok_or_else(|| bad(COMMAND, "destination must name a new directory"))?);
    std::fs::create_dir(&destination).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            bad(COMMAND, format!("destination {} already exists; choose a new directory", stored_path(&destination)))
        } else {
            io(e)
        }
    })?;
    let mut owned = OwnedDirectory { path: destination.clone(), committed: false };
    let snapshot = destination.join("source.prproj");
    write_new(&snapshot, &bytes)?;
    let mut records = BTreeMap::<String, SourceMedia>::new();
    let mut probed = BTreeMap::new();
    // The sources probes opened (a linked file, or a finished copy), by the path their items keep:
    // the media pool takes them instead of opening those files again.
    let mut opened = BTreeMap::<String, filmcraft_media::SharedSource>::new();
    let mut listings = FolderListings::new(MAX_FOLDER_ENTRIES, MAX_LISTED_NAMES);
    let media_dir = destination.join("media");
    if mode == "copy" {
        std::fs::create_dir(&media_dir).map_err(io)?;
    }
    for item in fragment.project.items.values_mut() {
        let ItemKind::Media(m) = &mut item.kind else { continue };
        let MediaRef::File { path } = &mut m.media else { continue };
        let original = path.clone();
        if !records.contains_key(&original) {
            let mut record = SourceMedia { original_path: original.clone(), imported_path: original.clone(), sha256: None, bytes: None, identity: None };
            // A project moved between computers keeps a project-relative path. It is tried only
            // when the stored path fails, and passes the same document policy and probe.
            let probed_media = probe_with_fallbacks(s, &original, tree.relative_media.get(&original), &source, &mut report, &mut listings);
            match probed_media {
                Ok((src, media_source)) => {
                    record.original_path = stored_path(&src);
                    record.imported_path = record.original_path.clone();
                    let media_source = if mode == "copy" {
                        // The pool takes the copy's source, not this one.
                        drop(media_source);
                        // `records` grows by one per source, so the partial name is unique.
                        let partial = media_dir.join(format!("{}.partial", records.len()));
                        match copy_media(s, &src, &partial, &media_dir, file_io)? {
                            Ok(copied) => {
                                record.sha256 = Some(copied.digest);
                                record.bytes = Some(copied.size);
                                record.imported_path = copied.path;
                                copied.source
                            }
                            Err(error) => {
                                // The placeholder keeps the source's fingerprint when it can be read.
                                record.identity = crate::relink::identity_of(&*s.services, &record.original_path).ok();
                                filmcraft_interchange::premiere::warning(
                                    &mut report,
                                    format!("unreadable media: {}; {error}; offline placeholder retained", stored_path(&src)),
                                );
                                records.insert(original, record);
                                m.offline = true;
                                continue;
                            }
                        }
                    } else {
                        media_source
                    };
                    // Like its media information, the fingerprint is of the file the item names:
                    // the source when linked, the finished copy when copied.
                    record.identity = crate::relink::identity_of(&*s.services, &record.imported_path).ok();
                    if mode != "copy" {
                        // Linked media stays online after a successful probe without a full-file
                        // read for provenance. A later hash is an observation, not import-time proof.
                        record.bytes = record.identity.as_ref().map(|i| i.size);
                    }
                    probed.insert(original.clone(), media_source.info().clone());
                    opened.insert(record.imported_path.clone(), media_source);
                }
                Err(error) => filmcraft_interchange::premiere::warning(&mut report, format!("{original}: {error}; offline placeholder retained")),
            }
            records.insert(original.clone(), record);
        }
        if let Some(record) = records.get(&original) {
            *path = record.imported_path.clone();
            m.identity = record.identity;
        }
        if let Some(info) = probed.get(&original) {
            m.info = info.clone();
            m.offline = false;
        } else {
            m.offline = true;
        }
    }
    // Offline placeholder dimensions are provisional; leave their source anchors automatic.
    fragment.project.resolve_placed_auto_points(|_| true, |it| it.as_media().is_none_or(|m| !m.offline));
    // Refuse a changed project input rather than attaching the wrong source hash to its snapshot.
    if hash(&source)?.0 != source_hash {
        return Err(bad(COMMAND, "project changed during import; try again"));
    }
    let warnings: Vec<String> = report.entries.iter().map(|e| e.message.clone()).collect();
    let entries = report_entries(&report);
    let record = PremiereImport {
        source_path: stored_path(&source),
        source_sha256: source_hash.clone(),
        snapshot_path: stored_path(&snapshot),
        media_mode: mode.into(),
        media: records.into_values().collect(),
        source_tree: serde_json::to_value(tree).map_err(|e| bad(COMMAND, e.to_string()))?,
        warnings: warnings.clone(),
    };
    let record_json = serde_json::to_vec_pretty(&record).map_err(|e| bad(COMMAND, e.to_string()))?;
    write_new(&destination.join("provenance.json"), &record_json)?;
    let before: std::collections::BTreeSet<_> = s.project.items.keys().copied().collect();
    let sequences = s.edit("Import Premiere project", |project, state| {
        let sequences = filmcraft_interchange::merge_into(project, fragment, bin);
        std::sync::Arc::make_mut(project.workflow.get_or_insert_default()).imports.push(std::sync::Arc::new(record));
        if let Some(&id) = sequences.first() {
            state.active_sequence = Some(id);
            if !state.open_sequences.contains(&id) {
                state.open_sequences.push(id);
            }
        }
        Ok(sequences)
    })?;
    owned.committed = true;
    // The first item of each file takes the source its probe opened (in copy mode, the copy's);
    // any other item of that file opens its own, as the pool keeps one source per item.
    for item in s.project.items.values().filter(|i| !before.contains(&i.id)) {
        if let ItemKind::Media(m) = &item.kind
            && !m.offline
            && let MediaRef::File { path } = &m.media
            && let Some(source) = opened.remove(path).or_else(|| s.media.open_file(path, &*s.services).ok())
        {
            s.media.insert_file(item.id, path, source);
        }
    }
    // Only this import's items: offline media already in the project is not part of its result.
    let offline: Vec<_> = s
        .project
        .items
        .values()
        .filter(|i| !before.contains(&i.id))
        .filter_map(|i| match &i.kind {
            ItemKind::Media(m) if m.offline => Some(i.name.clone()),
            _ => None,
        })
        .collect();
    Ok(
        json!({"format":"Premiere", "sequences":sequences, "destination":stored_path(&destination), "sourceSha256":source_hash, "mediaMode":mode, "offlineMedia":offline, "report":warnings, "reportEntries":entries}),
    )
}

const HASH_COMMAND: &str = "premiere.hashMedia";

/// The import a `premiere.hashMedia` selector names, with its index: a zero-based index into the
/// project's imports, or the import's destination directory (as `premiere.import` returned it) or
/// the snapshot inside it. A destination stays with its import when other imports are undone or
/// added; an index does not.
fn select_import<'a>(imports: &'a [std::sync::Arc<PremiereImport>], selector: Option<&Value>) -> Result<(usize, &'a PremiereImport)> {
    let index = match selector {
        Some(Value::String(path)) if !path.is_empty() => import_at(imports, Path::new(path))?,
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| bad(HASH_COMMAND, "import must be a nonnegative integer index or an import's destination path"))?,
        None => return Err(bad(HASH_COMMAND, "need import (an index or an import's destination path)")),
    };
    let record = imports.get(index).ok_or_else(|| bad(HASH_COMMAND, "unknown import index"))?;
    Ok((index, record))
}

/// The index of the one import whose destination directory or snapshot is `path`, as stored or,
/// for an existing path spelled another way, after canonicalization.
fn import_at(imports: &[std::sync::Arc<PremiereImport>], path: &Path) -> Result<usize> {
    let matching = |path: &Path| -> Vec<usize> {
        let names = |record: &PremiereImport| {
            let snapshot = Path::new(&record.snapshot_path);
            snapshot == path || snapshot.parent() == Some(path)
        };
        imports.iter().enumerate().filter(|(_, record)| names(record)).map(|(index, _)| index).collect()
    };
    let mut found = matching(path);
    if found.is_empty()
        && let Ok(canonical) = path.canonicalize()
    {
        found = matching(Path::new(&stored_path(&canonical)));
    }
    match found.as_slice() {
        [index] => Ok(*index),
        [] => Err(bad(HASH_COMMAND, "no import has that destination")),
        _ => Err(bad(HASH_COMMAND, "several imports have that destination; pass its index instead")),
    }
}

/// The destination directory of an import: the folder holding its source snapshot.
fn destination_of(record: &PremiereImport) -> Option<String> {
    Path::new(&record.snapshot_path).parent().map(|folder| folder.to_string_lossy().into_owned())
}

/// Most bytes one `premiere.hashMedia` call reads, counting each file once.
const MAX_HASH_BYTES: u64 = 1 << 40;

/// Explicit, synchronous inspection of current files. Never backfills immutable provenance:
/// linked media without an import-time hash cannot be certified unchanged since import.
fn hash_media(s: &mut Session, p: &Value) -> Result<Value> {
    hash_media_with(s, p, &hash_file_within, MAX_HASH_BYTES)
}

/// `premiere.hashMedia` with its hasher and byte limit replaceable in tests.
pub(super) fn hash_media_with(
    s: &Session,
    p: &Value,
    hash: &dyn Fn(&Path, u64) -> std::result::Result<(String, u64), HashFailure>,
    max_bytes: u64,
) -> Result<Value> {
    const COMMAND: &str = HASH_COMMAND;
    if s.services.export_in_memory() {
        return Err(bad(COMMAND, "media hashing requires a native filesystem host"));
    }
    let imports = s.project.workflow.as_ref().map(|w| w.imports.as_slice()).unwrap_or_default();
    let (index, record) = select_import(imports, p.get("import"))?;
    if record.media.len() > 100_000 {
        return Err(bad(COMMAND, "import exceeds media inspection limit"));
    }
    // Project records are untrusted too; repeat policy, extension and opener checks even when the
    // same path was accepted by an earlier import (once per stored path in this inspection).
    let document = Path::new(&record.snapshot_path);
    let mut files = std::collections::HashMap::<&str, std::result::Result<PathBuf, String>>::new();
    for reference in &record.media {
        let path = reference.imported_path.as_str();
        if !files.contains_key(path) {
            files.insert(path, probe_media(s, path, document).map(|(file, _)| file));
        }
    }
    let observed = hash_files(files.values().filter_map(|file| file.as_ref().ok()), hash, max_bytes)?;
    let media: Vec<Value> = record
        .media
        .iter()
        .map(|reference| {
            let observation = match files.get(reference.imported_path.as_str()) {
                Some(Ok(file)) => observed.get(file).cloned().unwrap_or_else(|| Err("not hashed".into())),
                Some(Err(error)) => Err(error.clone()),
                None => Err("not probed".into()),
            };
            observation_row(reference, observation)
        })
        .collect();
    Ok(json!({"import":index,"destination":destination_of(record),"sourceSha256":record.source_sha256,"observation":"current files","media":media}))
}

/// A file's current SHA-256 and size, or why it could not be read.
type Observation = std::result::Result<(String, u64), String>;

/// Hash each distinct file once (references that name one file by different paths share it).
/// When the files' sizes total more than `max_bytes`, the inspection is refused before any file is
/// read; a file that grows meanwhile is cut off where the remaining budget ends. Every byte read
/// counts, also those of a read that fails, and once the budget is spent the files left are not
/// read.
fn hash_files<'a>(
    files: impl Iterator<Item = &'a PathBuf>,
    hash: &dyn Fn(&Path, u64) -> std::result::Result<(String, u64), HashFailure>,
    max_bytes: u64,
) -> Result<BTreeMap<&'a PathBuf, Observation>> {
    let distinct: std::collections::BTreeSet<&PathBuf> = files.collect();
    let total = distinct.iter().map(|file| std::fs::metadata(file).map_or(0, |m| m.len())).fold(0u64, u64::saturating_add);
    if total > max_bytes {
        return Err(bad(HASH_COMMAND, format!("the import's media total {total} bytes, over the {max_bytes} byte inspection limit")));
    }
    let mut budget = max_bytes;
    let mut observed = BTreeMap::new();
    for file in distinct {
        let observation = if budget == 0 {
            Err(format!("not read: this inspection already read its {max_bytes} byte limit"))
        } else {
            let (observation, read) = match hash(file, budget) {
                Ok((digest, size)) => (Ok((digest, size)), size),
                Err(failure) => (Err(failure.error.to_string()), failure.read),
            };
            budget = budget.saturating_sub(read);
            observation
        };
        observed.insert(file, observation);
    }
    Ok(observed)
}

/// One `premiere.hashMedia` row: the provenance reference and what its file holds now.
fn observation_row(reference: &SourceMedia, observation: Observation) -> Value {
    let mut row = json!({"originalPath":reference.original_path,"path":reference.imported_path,
        "importSha256":reference.sha256,"sha256":null,"bytes":null,"matchesImport":null});
    match observation {
        Ok((digest, size)) => {
            row["matchesImport"] = json!(reference.sha256.as_ref().map(|expected| *expected == digest));
            row["sha256"] = json!(digest);
            row["bytes"] = json!(size);
        }
        Err(error) => row["error"] = json!(error),
    }
    row
}

/// The Premiere commands: `premiere.import` and `premiere.hashMedia`.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: COMMAND,
            label: "Import Premiere Project",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str,"destination":str? (new directory),"mediaMode":"link"|"copy","sequence":str? (ObjectUID or unique name),"frameRate":{"num":int,"den":int}?,"bin":binId?}"#,
            enabled: always,
            run: import,
            journal: true,
        },
        CommandSpec {
            id: HASH_COMMAND,
            label: "Hash Imported Premiere Media",
            menu: &[],
            shortcut: None,
            params: r#"{"import":int|path (zero-based provenance import index, or the destination premiere.import returned, which keeps naming that import when others are undone or added; reads current media in full)}"#,
            enabled: always,
            run: hash_media,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stored_paths_remove_windows_verbatim_prefixes_and_preserve_unc() {
        for (path, expected) in [
            (r"\\?\D:\media\clip.mov", r"D:\media\clip.mov"),
            (r"\\?\UNC\server\share\clip.mov", r"\\server\share\clip.mov"),
            ("/media/clip.mov", "/media/clip.mov"),
            (r"D:\media\clip.mov", r"D:\media\clip.mov"),
        ] {
            assert_eq!(stored_path(Path::new(path)), expected);
        }
    }
    #[test]
    fn copies_hold_exactly_the_hashed_bytes_and_never_replace_a_file() {
        let dir = filmcraft_testkit::workspace_root().join("target/premiere-copy").join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.bin");
        // Longer than one read buffer.
        std::fs::write(&source, (0..200_000u32).map(|i| i as u8).collect::<Vec<_>>()).unwrap();
        let partial = dir.join("0.partial");
        let copied = copy_hashing(&source, &partial).unwrap();
        assert_eq!(copied, hash_file(&source).unwrap());
        assert_eq!(std::fs::read(&partial).unwrap(), std::fs::read(&source).unwrap());
        assert!(matches!(copy_hashing(&source, &partial), Err(CopyError::Cancel(_))), "an existing file is never replaced");
        let absent = dir.join("1.partial");
        assert!(matches!(copy_hashing(&dir.join("missing.bin"), &absent), Err(CopyError::Unreadable(_))));
        assert!(!absent.exists(), "an unreadable source writes nothing");
        // A finished copy takes its digest name; the same bytes again keep the existing copy once
        // it is verified, and a copy changed meanwhile cancels the import.
        let target = dir.join(format!("{}.bin", copied.0));
        publish_copy(&partial, &target, &copied.0, &hash_file).unwrap();
        assert!(!partial.exists() && hash_file(&target).unwrap() == copied);
        copy_hashing(&source, &partial).unwrap();
        publish_copy(&partial, &target, &copied.0, &hash_file).unwrap();
        assert!(!partial.exists() && hash_file(&target).unwrap() == copied);
        std::fs::write(&target, b"changed").unwrap();
        copy_hashing(&source, &partial).unwrap();
        assert!(publish_copy(&partial, &target, &copied.0, &hash_file).unwrap_err().to_string().contains("changed while copying"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn folder_listings_read_each_folder_once_and_keep_the_one_match_rule() {
        let dir = filmcraft_testkit::workspace_root().join("target/premiere-listings").join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Clip.MOV"), b"").unwrap();
        std::fs::write(dir.join("other.mov"), b"").unwrap();
        let mut listings = FolderListings::new(MAX_FOLDER_ENTRIES, MAX_LISTED_NAMES);
        assert_eq!(listings.find(&dir, "clip.mov"), Some("Clip.MOV".into()));
        assert_eq!(listings.find(&dir, "missing.mov"), None);
        // The folder was read once: a file added during the import is not listed.
        std::fs::write(dir.join("Late.MOV"), b"").unwrap();
        assert_eq!(listings.find(&dir, "late.mov"), None);
        assert_eq!(listings.find(&dir.join("absent"), "clip.mov"), None, "a folder that cannot be read has no match");
        // Past the limit a folder is read for every lookup, as without the cache.
        let mut full = FolderListings::new(MAX_FOLDER_ENTRIES, 1);
        assert_eq!(full.find(&dir, "late.mov"), Some("Late.MOV".into()));
        std::fs::write(dir.join("Later.MOV"), b"").unwrap();
        assert_eq!(full.find(&dir, "later.mov"), Some("Later.MOV".into()));
        // Two entries whose names differ only in letter case are ambiguous: no match. Built from
        // names, as a case-insensitive disk cannot hold both files.
        let index = index_by_lower_case(names(&["Take.mov", "take.MOV", "b.png"]), MAX_FOLDER_ENTRIES).unwrap();
        assert_eq!(index.get("take.mov"), Some(&None));
        assert_eq!(index.get("b.png"), Some(&Some("b.png".into())));
        std::fs::remove_dir_all(&dir).unwrap();
    }
    /// Folder entries read without error, as `index_by_lower_case` takes them.
    fn names(names: &[&str]) -> Vec<std::io::Result<std::ffi::OsString>> {
        names.iter().map(|name| Ok(std::ffi::OsString::from(name))).collect()
    }
    #[test]
    fn a_folder_listed_in_part_offers_no_letter_case_variant() {
        // An entry past the entry limit, or one that could not be read, may be a second name that
        // differs only in letter case (`clip.MOV` read, `Clip.mov` not): no match is unique then.
        let whole = index_by_lower_case(names(&["clip.MOV", "b.png"]), 2);
        assert_eq!(whole.and_then(|index| index.get("clip.mov").cloned().flatten()), Some("clip.MOV".into()), "every entry read, at the limit exactly");
        assert_eq!(index_by_lower_case(names(&["clip.MOV", "b.png", "Clip.mov"]), 2), None, "more entries than the limit");
        let mut failed = names(&["clip.MOV"]);
        failed.push(Err(std::io::Error::other("entry could not be read")));
        assert_eq!(index_by_lower_case(failed, MAX_FOLDER_ENTRIES), None, "an entry that could not be read");
        // A real folder past the entry limit offers neither of its names (whichever is read
        // first), from a kept listing and from one read again for each lookup past the names limit.
        let dir = filmcraft_testkit::workspace_root().join("target/premiere-partial-listing").join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Clip.MOV"), b"").unwrap();
        std::fs::write(dir.join("other.mov"), b"").unwrap();
        for limit in [MAX_LISTED_NAMES, 0] {
            let mut listings = FolderListings::new(1, limit);
            assert_eq!((listings.find(&dir, "clip.mov"), listings.find(&dir, "other.mov")), (None, None), "names limit {limit}");
        }
        assert_eq!(FolderListings::new(2, MAX_LISTED_NAMES).find(&dir, "clip.mov"), Some("Clip.MOV".into()), "listed in full");
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn snapshot_bytes_survive_link_copy_undo_and_save() {
        let dir = filmcraft_testkit::workspace_root().join("target/premiere-snapshot").join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.prproj");
        let xml = b"<PremiereData><Sequence ObjectUID=\"s\"><Name>Sequence</Name></Sequence></PremiereData>";
        std::fs::write(&source, xml).unwrap();
        let expected = hash_file(&source).unwrap();
        for mode in ["link", "copy"] {
            let dest = dir.join(mode);
            let mut session = Session::default();
            let result = import(&mut session, &json!({"path":source,"destination":dest,"mediaMode":mode})).unwrap();
            let snapshot = dest.join("source.prproj");
            assert_eq!(std::fs::read(&snapshot).unwrap(), xml);
            assert_eq!(hash_file(&snapshot).unwrap(), expected);
            assert_eq!(hash_file(&source).unwrap(), expected);
            assert_eq!(result["sourceSha256"], expected.0);
            let provenance = session.project.workflow.clone();
            let encoded = filmcraft_format::encode(&session.project, false);
            assert_eq!(filmcraft_format::decode(&encoded).unwrap().project.workflow, provenance);
            session.execute("edit.undo", json!({})).unwrap();
            assert!(session.project.workflow.is_none());
            assert_eq!(hash_file(&snapshot).unwrap(), expected);
            session.execute("edit.redo", json!({})).unwrap();
            assert_eq!(session.project.workflow, provenance);
            assert_eq!(hash_file(&snapshot).unwrap(), expected);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn offline_anchor_stays_auto_until_source_size_is_known() {
        let dir = filmcraft_testkit::workspace_root().join("target/premiere-anchor").join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.prproj");
        // Deliberately missing source, sequence size is not the source size.
        let xml = r#"<PremiereData><Sequence ObjectUID="s"><Name>S</Name><TrackGroups><TrackGroup><Second ObjectRef="v"/></TrackGroup></TrackGroups></Sequence><VideoTrackGroup ObjectID="v"><TrackGroup><Tracks><Track ObjectURef="t"/></Tracks></TrackGroup></VideoTrackGroup><VideoClipTrack ObjectUID="t"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="i"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack><VideoClipTrackItem ObjectID="i"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="sub"/></ClipTrackItem></VideoClipTrackItem><SubClip ObjectID="sub"><Name>missing.mov</Name></SubClip></PremiereData>"#;
        std::fs::write(&source, xml).unwrap();
        let destination = dir.join("import");
        let _ = std::fs::remove_dir_all(&destination);
        let mut session = Session::default();
        import(&mut session, &json!({"path":source,"destination":destination})).unwrap();
        let seq = session.project.sequence(session.state.active_sequence.unwrap()).unwrap();
        let clip = &seq.video_tracks[0].items[0];
        let anchor = clip.effects.iter().find_map(|e| e.params.get("anchor")).unwrap();
        assert!(matches!(&anchor.value, filmcraft_project::ParamValue::Vec2(v) if v.x.is_nan() && v.y.is_nan()));
        // Relink-equivalent size discovery must be able to resolve the anchor to the actual media centre.
        let media = clip.item;
        let mut project = (*session.project).clone();
        if let ItemKind::Media(m) = &mut project.item_mut(media).unwrap().kind {
            let v = m.info.video.as_mut().unwrap();
            v.width = 3840;
            v.height = 2160;
            m.offline = false;
        }
        project.resolve_placed_auto_points(|_| true, |it| it.as_media().is_none_or(|m| !m.offline));
        let seq = project.sequence(session.state.active_sequence.unwrap()).unwrap();
        let anchor = seq.video_tracks[0].items[0].effects.iter().find_map(|e| e.params.get("anchor")).unwrap();
        assert!(matches!(&anchor.value, filmcraft_project::ParamValue::Vec2(v) if v.x == 1920.0 && v.y == 1080.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! Interchange documents (CMX 3600 EDL, FCP7 XML, FCPXML, OTIO, AAF, OMF) ↔ the session's project.

use serde_json::{Value, json};

use filmcraft_interchange::{ExportOptions, Format, ImportOptions};
use filmcraft_project::{ItemId, ItemKind, MediaRef};

use crate::{EngineError, Result, Session};

/// The interchange format of a file, if it is one (by content, with the extension as a hint).
pub fn detect(path: &str, bytes: &[u8]) -> Option<Format> {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("edl" | "xml" | "fcpxml" | "otio" | "aaf" | "omf" | "omfi")) {
        return None;
    }
    filmcraft_interchange::detect(bytes, ext.as_deref())
}

fn report_json(r: &filmcraft_interchange::Report) -> Vec<String> {
    r.entries.iter().map(|e| if e.count > 1 { format!("{} (×{})", e.message, e.count) } else { e.message.clone() }).collect()
}

fn is_file_media(item: &filmcraft_project::ProjectItem) -> bool {
    matches!(&item.kind, ItemKind::Media(m) if matches!(m.media, MediaRef::File { .. }))
}

/// Write embedded audio or reuse an identical regular file; never replace an occupied path.
/// Return the path used. A numbered
/// name changes only the file name: the folder keeps its separators as written (`/` or `\`).
fn write_embedded_audio(s: &Session, path: &str, wav: &[u8]) -> Result<String> {
    let (folder, name) = path.split_at(path.rfind(['/', '\\']).map_or(0, |i| i + 1));
    if matches!(name, "" | "." | "..") {
        return Err(EngineError::Other(format!("{path}: missing audio file name")));
    }
    let (stem, extension) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let candidate_name = |number| if number == 1 { name.to_owned() } else { format!("{stem} ({number}){extension}") };
    const MAX_LISTED_ENTRIES: usize = 10_000;
    const MAX_LISTED_BYTES: usize = 1_000_000;
    let dir = if folder.is_empty() { "." } else { folder };
    let occupied = match s.services.list_dir_bounded(dir, MAX_LISTED_ENTRIES, MAX_LISTED_BYTES) {
        Some(Ok(names)) if name.is_ascii() && names.iter().all(|name| name.is_ascii()) => {
            Some(names.into_iter().map(|name| name.to_ascii_lowercase()).collect::<std::collections::HashSet<_>>())
        }
        Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Some(std::collections::HashSet::new()),
        _ => None,
    };
    for number in 1..=1_000 {
        let name = candidate_name(number);
        if occupied.as_ref().is_some_and(|names| !names.contains(&name.to_ascii_lowercase())) {
            continue;
        }
        let candidate = format!("{folder}{name}");
        if s.services.existing_file_matches(&candidate, wav).unwrap_or(false) {
            return Ok(candidate);
        }
    }
    for number in 1..=1_000 {
        let candidate = format!("{folder}{}", candidate_name(number));
        match s.services.write_file_new(&candidate, wav) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if s.services.existing_file_matches(&candidate, wav).unwrap_or(false) {
                    return Ok(candidate);
                }
            }
            Err(e) => return Err(EngineError::Other(format!("{candidate}: {e}"))),
        }
    }
    Err(EngineError::Other(format!("{path}: no unused embedded-audio filename after 1000 attempts")))
}

/// Import a document: merge its bins, media and sequences into the project (one undo step), then
/// link each media file that exists on disk. Returns the new sequences.
pub fn import(s: &mut Session, path: &str, bytes: &[u8], format: Format) -> Result<Value> {
    let p = std::path::Path::new(path);
    let opts = ImportOptions {
        base_dir: p.parent().map(|d| d.to_string_lossy().to_string()),
        name: p.file_stem().map(|n| n.to_string_lossy().to_string()),
        ..Default::default()
    };
    let (fragment, report) = match format {
        // AAF / OMF may embed audio: write it next to the document (the media items point there)
        Format::Aaf | Format::Omf => {
            let r = if format == Format::Aaf { filmcraft_interchange::aaf::import(bytes, &opts) } else { filmcraft_interchange::omf::import(bytes, &opts) };
            let (mut fragment, extracted, report) = r.map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            for m in extracted {
                let media = fragment
                    .project
                    .item_mut(m.item)
                    .and_then(|item| item.as_media_mut())
                    .ok_or_else(|| EngineError::Other(format!("{}: embedded audio has no media item", m.path)))?;
                let written_path = write_embedded_audio(s, &m.path, &m.wav)?;
                if written_path != m.path {
                    media.media = MediaRef::File { path: written_path };
                }
            }
            (fragment, report)
        }
        _ => filmcraft_interchange::import_with(bytes, format, &opts).map_err(|e| EngineError::Other(format!("{path}: {e}")))?,
    };
    for item in fragment.project.items.values() {
        if let ItemKind::Sequence(sequence) = &item.kind {
            sequence.settings.validate().map_err(|e| EngineError::Other(format!("{path}: invalid imported sequence settings: {e}")))?;
        }
    }
    let before: std::collections::HashSet<ItemId> = s.project.items.keys().copied().collect();
    let name = opts.name.clone().unwrap_or_else(|| format.name().to_string());
    let seqs = s.edit(&format!("Import {name}"), |proj, _| {
        let seqs = filmcraft_interchange::merge_into(proj, fragment, None);
        // A document that sets only some Motion parameters leaves the others at their "auto"
        // (NaN) defaults. Resolve them as placing a clip does, for every format. A media file's
        // picture size is only what the document says so far (an EDL says nothing: the importer
        // assumes the sequence size), so its clips' anchors wait for Link Media below.
        proj.resolve_placed_auto_points(|seq| !before.contains(&seq), |source| !is_file_media(source));
        Ok(seqs)
    })?;
    // Link media: probe each new file-backed item that exists, through the host's reader as a
    // direct import does (only the container index is read now, not the whole file: #461).
    let new_media: Vec<(ItemId, String)> = s
        .project
        .items
        .values()
        .filter(|i| !before.contains(&i.id))
        .filter_map(|i| match &i.kind {
            ItemKind::Media(m) => match &m.media {
                MediaRef::File { path } => Some((i.id, path.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    // The document's media paths are opened as stored paths of the document.
    s.media.add_stored_paths(new_media.iter().map(|(_, p)| p.clone()), Some(path));
    let mut linked = 0;
    let mut offline = Vec::new();
    for (id, mpath) in new_media {
        let file = match s.media.follow(&mpath) {
            Ok(file) => file,
            Err(why) => {
                log::warn!("{mpath} is not opened: {why}");
                offline.push(mpath);
                continue;
            }
        };
        match s.media.open_file(&file, &*s.services).ok() {
            Some(src) => {
                let info = src.info().clone();
                let identity = crate::relink::identity_of(&*s.services, &file).ok();
                let rebase = (format == Format::Edl).then_some(info.start_timecode).flatten();
                s.edit("Link Media", |proj, _| {
                    if let Some(item) = proj.items.get_mut(&id)
                        && let ItemKind::Media(m) = &mut item.kind
                    {
                        let rate = info.video.as_ref().map(|v| v.frame_rate);
                        m.info = info.clone();
                        m.offline = false;
                        m.identity = identity;
                        if let (Some(tc), Some(rate)) = (rebase, rate) {
                            filmcraft_interchange::rebase_source_timecode(proj, id, tc, rate);
                        }
                    }
                    // the picture size is now the file's own: centre the anchors of its clips in it
                    proj.resolve_placed_auto_points(|seq| !before.contains(&seq), |source| source.id == id);
                    Ok(())
                })?;
                s.media.insert_file(id, &mpath, src);
                linked += 1;
            }
            None => offline.push(mpath),
        }
    }
    if let Some(&first) = seqs.first() {
        s.state.active_sequence = Some(first);
        if !s.state.open_sequences.contains(&first) {
            s.state.open_sequences.push(first);
        }
    }
    Ok(json!({
        "format": format.name(),
        "sequences": seqs.iter().map(|i| i.0).collect::<Vec<_>>(),
        "linkedMedia": linked,
        "offlineMedia": offline,
        "report": report_json(&report),
    }))
}

/// `file.exportInterchange {format: "edl"|"xml"|"fcpxml"|"otio"|"aaf"|"omf" = "xml", path, sequence?}`
pub fn export(s: &mut Session, p: &Value) -> Result<Value> {
    // FCP7 XML only when no format is asked for: a format we don't know is an error, not XML.
    // The names are the documented ones (each format's extension, in either case) and nothing
    // else, so the error can list exactly what is accepted.
    let format = match p.get("format") {
        None | Some(Value::Null) => Format::Fcp7Xml,
        Some(v) => v.as_str().and_then(|name| Format::ALL.into_iter().find(|f| f.extension().eq_ignore_ascii_case(name))).ok_or_else(|| {
            let names: Vec<&str> = Format::ALL.iter().map(|f| f.extension()).collect();
            crate::commands::bad("file.exportInterchange", format!("unknown format {v}: use one of {}", names.join(", ")))
        })?,
    };
    let path = p.get("path").and_then(Value::as_str).ok_or_else(|| EngineError::Other("need `path`".into()))?.to_string();
    let seq = p.get("sequence").and_then(Value::as_u64).map(ItemId).or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
    let opts = ExportOptions {
        relative_to: std::path::Path::new(&path).parent().map(|d| d.to_string_lossy().to_string()),
        name: s.project.item(seq).map(|i| i.name.clone()),
        ..Default::default()
    };
    let (bytes, report) = filmcraft_interchange::export(&s.project, seq, format, &opts).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({"path": path, "bytes": bytes.len(), "report": report_json(&report)}))
}

#[cfg(test)]
mod embedded_audio_tests {
    use std::collections::HashMap;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::{Services, Session};

    use super::write_embedded_audio;

    #[derive(Default)]
    struct VirtualFiles(Mutex<HashMap<String, Vec<u8>>>, AtomicUsize, AtomicBool);

    impl VirtualFiles {
        fn key(&self, path: &str) -> String {
            if self.2.load(Ordering::Relaxed) { path.replace("café", "cafe\u{301}") } else { path.to_owned() }
        }
    }

    impl Services for VirtualFiles {
        fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
            self.0.lock().unwrap().get(&self.key(path)).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
        }

        fn write_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
            self.0.lock().unwrap().insert(self.key(path), data.to_vec());
            Ok(())
        }

        fn write_file_new(&self, path: &str, data: &[u8]) -> io::Result<()> {
            let mut files = self.0.lock().unwrap();
            let key = self.key(path);
            if files.contains_key(&key) {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            files.insert(key, data.to_vec());
            Ok(())
        }

        fn existing_file_matches(&self, path: &str, data: &[u8]) -> io::Result<bool> {
            self.1.fetch_add(1, Ordering::Relaxed);
            Ok(self.0.lock().unwrap().get(&self.key(path)).is_some_and(|bytes| bytes == data))
        }

        fn list_dir_bounded(&self, dir: &str, max_entries: usize, max_bytes: usize) -> Option<io::Result<Vec<String>>> {
            Some((|| {
                let prefix = if dir == "." {
                    String::new()
                } else if dir.ends_with(['/', '\\']) {
                    dir.to_owned()
                } else {
                    format!("{dir}/")
                };
                let mut names = Vec::new();
                let mut bytes = 0_usize;
                for path in self.0.lock().unwrap().keys() {
                    let Some(name) = path.strip_prefix(&prefix).filter(|name| !name.is_empty() && !name.contains(['/', '\\'])) else { continue };
                    bytes = bytes.checked_add(name.len()).ok_or_else(|| io::Error::other("directory listing byte limit exceeded"))?;
                    if names.len() >= max_entries || bytes > max_bytes {
                        return Err(io::Error::other("directory listing limit exceeded"));
                    }
                    names.push(name.to_owned());
                }
                Ok(names)
            })())
        }
    }

    #[test]
    fn new_embedded_audio_does_not_probe_absent_candidate_files() {
        let files = Arc::new(VirtualFiles::default());
        let session = Session::new(files.clone());
        let path = write_embedded_audio(&session, "/files/cut Audio.wav", b"new audio").unwrap();
        assert_eq!(path, "/files/cut Audio.wav");
        assert_eq!(files.1.load(Ordering::Relaxed), 0, "an empty directory needs no byte comparisons");
    }

    #[test]
    fn listed_candidates_skip_absent_slots_and_reuse_later_match() {
        let files = Arc::new(VirtualFiles::default());
        let root = "/files/cut Audio";
        files.0.lock().unwrap().insert(format!("{root}.wav"), b"different".to_vec());
        files.0.lock().unwrap().insert(format!("{root} (9).wav"), b"matching".to_vec());
        let session = Session::new(files.clone());
        let path = write_embedded_audio(&session, &format!("{root}.wav"), b"matching").unwrap();
        assert_eq!(path, format!("{root} (9).wav"));
        assert_eq!(files.1.load(Ordering::Relaxed), 2, "only occupied candidate paths need comparison");
        assert_eq!(files.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn oversized_listing_falls_back_to_candidate_probes() {
        let files = Arc::new(VirtualFiles::default());
        {
            let mut stored = files.0.lock().unwrap();
            for number in 0..10_000 {
                stored.insert(format!("/files/unrelated-{number}.bin"), Vec::new());
            }
            stored.insert("/files/cut Audio (9).wav".into(), b"matching".to_vec());
        }
        let session = Session::new(files.clone());
        let path = write_embedded_audio(&session, "/files/cut Audio.wav", b"matching").unwrap();
        assert_eq!(path, "/files/cut Audio (9).wav");
        assert_eq!(files.1.load(Ordering::Relaxed), 9, "incomplete listings must not hide a later match");
        assert_eq!(files.0.lock().unwrap().len(), 10_001);
    }

    #[test]
    fn unicode_normalization_alias_reuses_later_match_after_gap() {
        let files = Arc::new(VirtualFiles::default());
        files.2.store(true, Ordering::Relaxed);
        let root = "/files/cafe\u{301} Audio";
        files.0.lock().unwrap().insert(format!("{root}.wav"), b"different".to_vec());
        files.0.lock().unwrap().insert(format!("{root} (3).wav"), b"matching".to_vec());
        let session = Session::new(files.clone());
        let path = write_embedded_audio(&session, "/files/café Audio.wav", b"matching").unwrap();
        assert_eq!(path, "/files/café Audio (3).wav");
        assert_eq!(files.0.lock().unwrap().len(), 2, "the free (2) slot must stay empty");
    }

    #[test]
    fn virtual_extraction_keeps_existing_media() {
        let files = Arc::new(VirtualFiles::default());
        files.0.lock().unwrap().insert("/files/cut Audio.wav".into(), b"first".to_vec());
        files.0.lock().unwrap().insert("/files/cut Audio (2).wav".into(), b"second".to_vec());
        let session = Session::new(files.clone());
        let path = write_embedded_audio(&session, "/files/cut Audio.wav", b"new audio").unwrap();
        assert_eq!(path, "/files/cut Audio (3).wav");
        let stored = files.0.lock().unwrap();
        assert_eq!(stored["/files/cut Audio.wav"], b"first");
        assert_eq!(stored["/files/cut Audio (2).wav"], b"second");
        assert_eq!(stored["/files/cut Audio (3).wav"], b"new audio");
        drop(stored);
        let again = write_embedded_audio(&session, "/files/cut Audio.wav", b"new audio").unwrap();
        assert_eq!(again, path);
        assert_eq!(files.0.lock().unwrap().len(), 3);
    }

    #[test]
    fn numbered_names_keep_the_path_separators() {
        for path in ["C:/files/cut Audio.wav", "C:\\files\\cut Audio.wav"] {
            let files = Arc::new(VirtualFiles::default());
            files.0.lock().unwrap().insert(path.into(), b"first".to_vec());
            let session = Session::new(files.clone());
            let numbered = write_embedded_audio(&session, path, b"new audio").unwrap();
            assert_eq!(numbered, path.replace("cut Audio.wav", "cut Audio (2).wav"));
            assert_eq!(files.0.lock().unwrap()[path], b"first");
            assert_eq!(write_embedded_audio(&session, path, b"new audio").unwrap(), numbered);
            let later = path.replace("cut Audio.wav", "cut Audio (9).wav");
            files.0.lock().unwrap().insert(later.clone(), b"later match".to_vec());
            files.1.store(0, Ordering::Relaxed);
            assert_eq!(write_embedded_audio(&session, path, b"later match").unwrap(), later);
            assert_eq!(files.1.load(Ordering::Relaxed), 3, "only the three occupied names need comparison");
            assert_eq!(files.0.lock().unwrap().len(), 3, "the gap before (9) stays empty");
        }
    }
}

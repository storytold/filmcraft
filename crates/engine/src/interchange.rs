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

/// Write embedded audio without replacing an occupied path; return the path used. A numbered
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
    for number in 1..=1_000 {
        let candidate = if number == 1 { path.to_owned() } else { format!("{folder}{stem} ({number}){extension}") };
        match s.services.write_file_new(&candidate, wav) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
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
    let kind = Kind { format_name: format.name(), name: opts.name.clone(), rebase_timecode: format == Format::Edl };
    merge_fragment(s, path, fragment, report, kind, &mut std::collections::HashMap::new())
}

/// A media file of an imported document, opened ahead of [`merge_fragment`] (on a worker
/// thread, for an import that runs in the background).
pub(crate) enum Probe {
    /// The stored path leads nowhere usable (why).
    NotFollowed(String),
    /// `file`, the path `stored` led to, is there but cannot be read as media.
    Unreadable { file: String },
    /// `file` is the path `stored` led to.
    Opened { file: String, src: filmcraft_media::SharedSource, identity: Option<filmcraft_project::MediaIdentity> },
}

impl Probe {
    /// Whether this is still what opening `stored` would give: it led to the same file (or, for
    /// a path that led nowhere, still does). Only then may a merge use it as it is.
    fn still_holds(&self, media: &crate::media_pool::MediaPool, stored: &str) -> bool {
        match (self, media.follow(stored)) {
            (Probe::NotFollowed(_), Err(_)) => true,
            (Probe::Unreadable { file } | Probe::Opened { file, .. }, Ok(now)) => *file == now,
            _ => false,
        }
    }
}

/// Open the media file a document stores as `stored`. With `document`, the path is resolved as
/// a stored path of that document without recording it (an import that may still be dropped);
/// without, it must be recorded already ([`crate::media_pool::MediaPool::add_stored_paths`]).
pub(crate) fn probe(media: &crate::media_pool::MediaPool, services: &dyn crate::Services, stored: &str, document: Option<&str>) -> Probe {
    let followed = match document {
        Some(d) => media.follow_as(stored, d),
        None => media.follow(stored),
    };
    let file = match followed {
        Ok(file) => file,
        Err(why) => return Probe::NotFollowed(why.to_string()),
    };
    match media.open_file(&file, services) {
        Ok(src) => {
            let identity = crate::relink::identity_of(services, &file).ok();
            Probe::Opened { file, src, identity }
        }
        Err(_) => Probe::Unreadable { file },
    }
}

/// What kind of document [`merge_fragment`] merges.
pub(crate) struct Kind<'a> {
    pub format_name: &'a str,
    /// For the undo step (`Import <name>`); the format's name when `None`.
    pub name: Option<String>,
    /// Source positions count from the media's start timecode (EDL), so they are rebased once
    /// the file is probed.
    pub rebase_timecode: bool,
}

/// Merge an imported fragment into the project (one undo step), link each media file that
/// exists on disk (using `probed` for files opened already), and open the first new sequence.
pub(crate) fn merge_fragment(
    s: &mut Session,
    path: &str,
    fragment: filmcraft_interchange::Imported,
    report: filmcraft_interchange::Report,
    kind: Kind,
    probed: &mut std::collections::HashMap<String, Probe>,
) -> Result<Value> {
    let Kind { format_name, name, rebase_timecode } = kind;
    for item in fragment.project.items.values() {
        if let ItemKind::Sequence(sequence) = &item.kind {
            sequence.settings.validate().map_err(|e| EngineError::Other(format!("{path}: invalid imported sequence settings: {e}")))?;
        }
    }
    let before: std::collections::HashSet<ItemId> = s.project.items.keys().copied().collect();
    let name = name.unwrap_or_else(|| format_name.to_string());
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
        // Opened already (in the background, failures included, so a slow or damaged file is
        // not read again here) while the path still leads where it did; else now.
        let opened = match probed.remove(&mpath) {
            Some(p) if p.still_holds(&s.media, &mpath) => p,
            _ => probe(&s.media, &*s.services, &mpath, None),
        };
        match opened {
            Probe::NotFollowed(why) => {
                log::warn!("{mpath} is not opened: {why}");
                offline.push(mpath);
            }
            Probe::Unreadable { .. } => offline.push(mpath),
            Probe::Opened { file: _, src, identity } => {
                let info = src.info().clone();
                let rebase = rebase_timecode.then_some(info.start_timecode).flatten();
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
        }
    }
    if let Some(&first) = seqs.first() {
        s.state.active_sequence = Some(first);
        if !s.state.open_sequences.contains(&first) {
            s.state.open_sequences.push(first);
        }
    }
    Ok(json!({
        "format": format_name,
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
    use std::sync::{Arc, Mutex};

    use crate::{Services, Session};

    use super::write_embedded_audio;

    #[derive(Default)]
    struct VirtualFiles(Mutex<HashMap<String, Vec<u8>>>);

    impl Services for VirtualFiles {
        fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
            self.0.lock().unwrap().get(path).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
        }

        fn write_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
            self.0.lock().unwrap().insert(path.to_owned(), data.to_vec());
            Ok(())
        }

        fn write_file_new(&self, path: &str, data: &[u8]) -> io::Result<()> {
            let mut files = self.0.lock().unwrap();
            if files.contains_key(path) {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            files.insert(path.to_owned(), data.to_vec());
            Ok(())
        }
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
        }
    }
}

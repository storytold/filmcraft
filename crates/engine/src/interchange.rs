//! Interchange documents (CMX 3600 EDL, FCP7 XML, FCPXML, OTIO, AAF, OMF) ↔ the session's project.

use serde_json::{Value, json};

use filmcraft_interchange::{ExportOptions, Format, ImportOptions};
use filmcraft_project::{BinId, ItemId, ItemKind, MediaRef};

use crate::{EngineError, Result, Session};

/// Read a Premiere project through the host with a cap before allocating its contents. A range
/// read stays bounded even if a file grows between its metadata and content reads.
pub fn read_premiere(s: &Session, path: &str) -> std::io::Result<Vec<u8>> {
    let size = s.services.file_size(path)?;
    if size > filmcraft_interchange::prproj::MAX_INPUT_BYTES as u64 {
        return Err(std::io::Error::other("Premiere project exceeds the 64 MiB input limit"));
    }
    let size = usize::try_from(size).map_err(|_| std::io::Error::other("Premiere project size is unsupported"))?;
    s.services.read_range(path, 0, size)
}

pub fn import_premiere(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "file.importPremiereProject";
    let path = p
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty() && path.len() <= 32768)
        .ok_or_else(|| crate::commands::bad(CMD, "need a non-empty `path` of at most 32768 bytes"))?;
    if !std::path::Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("prproj")) {
        return Err(crate::commands::bad(CMD, "choose a .prproj file"));
    }
    let mut params = json!({"paths": [path]});
    if let Some(bin) = p.get("bin") {
        let id = bin.as_u64().ok_or_else(|| crate::commands::bad(CMD, "`bin` must be a non-negative integer"))?;
        if s.project.root.find_bin(BinId(id)).is_none() {
            return Err(crate::commands::bad(CMD, "the destination bin does not exist"));
        }
        params["bin"] = bin.clone();
    }
    s.execute("file.import", params)
}

/// The interchange format of a file, if it is one (by content, with the extension as a hint).
pub fn detect(path: &str, bytes: &[u8]) -> Option<Format> {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("edl" | "xml" | "fcpxml" | "otio" | "aaf" | "omf" | "omfi" | "prproj")) {
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

/// Import a document and link its media. Premiere imports group the merge and links into one
/// undo step; existing interchange formats retain their separate link undo steps.
pub fn import(s: &mut Session, path: &str, bytes: &[u8], format: Format, bin: Option<BinId>) -> Result<Value> {
    let p = std::path::Path::new(path);
    let opts = ImportOptions {
        base_dir: p.parent().map(|d| d.to_string_lossy().to_string()),
        name: p.file_stem().map(|n| n.to_string_lossy().to_string()),
        ..Default::default()
    };
    let mut preferred_sequence_index = None;
    let (fragment, report) = match format {
        Format::Premiere => {
            let (fragment, view, report) =
                filmcraft_interchange::prproj::import_with_view(bytes, &opts).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            preferred_sequence_index = fragment.sequences.iter().position(|id| Some(*id) == view.active_sequence);
            (fragment, report)
        }
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
    let name = opts.name.clone().unwrap_or_else(|| format.name().to_string());
    // Parsing must succeed before the undo group starts: malformed input leaves even the
    // revision untouched. Merge and media links then form one undoable import.
    let label = format!("Import {name}");
    let apply = |s: &mut Session| {
        let before: std::collections::HashSet<ItemId> = s.project.items.keys().copied().collect();
        let seqs = s.edit(&format!("Import {name}"), |proj, _| {
            let seqs = filmcraft_interchange::merge_into(proj, fragment, bin);
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
        if let Some(&first) = seqs.get(preferred_sequence_index.unwrap_or(0)) {
            s.state.active_sequence = Some(first);
            if !s.state.open_sequences.contains(&first) {
                s.state.open_sequences.push(first);
            }
            s.events.push(crate::Event::OpenSequence(first));
        }
        let mut result = json!({
            "format": format.name(),
            "sequences": seqs.iter().map(|i| i.0).collect::<Vec<_>>(),
            "linkedMedia": linked,
            "offlineMedia": offline,
            "report": report_json(&report),
        });
        if format == Format::Premiere {
            result["documentEpoch"] = json!(s.document_epoch);
            result["sourcePath"] = json!(path);
            result["activeSequence"] = json!(s.state.active_sequence.map(|id| id.0));
            result["sequenceSummaries"] = json!(
                seqs.iter()
                    .filter_map(|id| {
                        let item = s.project.item(*id)?;
                        let seq = item.as_sequence()?;
                        Some(json!({"id": id.0, "name": item.name,
                "videoClips": seq.video_tracks.iter().map(|t| t.items.len()).sum::<usize>(),
                "audioClips": seq.audio_tracks.iter().map(|t| t.items.len()).sum::<usize>()}))
                    })
                    .collect::<Vec<_>>()
            );
            for entry in report_json(&report) {
                s.log.push(crate::panels::Level::Warning, "Premiere import", entry);
            }
            s.events.push(crate::Event::ImportReport(result.clone()));
        }
        Ok(result)
    };
    if format == Format::Premiere { s.edit_group(&label, apply) } else { apply(s) }
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

//! Native `.prproj` open/save: keep the expanded Premiere XML for round-trip, patch mapped fields, gzip on write.
//! Clean-room from user-owned / hand-written XML and the FilmCraft Premiere reader. MIT OR Apache-2.0.

use std::collections::BTreeMap;
use std::io::Write;

use filmcraft_project::{ClipId, ItemId, ItemKind, MediaRef, Project};
use filmcraft_time::Tick;
use flate2::Compression;
use flate2::write::GzEncoder;
use roxmltree::Node;

use super::{Document, warning};
use crate::{Error, Report, Result};

/// Expanded Premiere XML kept for open/save. Unmodeled siblings stay in the text across patches.
#[derive(Clone, Debug)]
pub struct NativeSource {
    /// UTF-8 PremiereData document (never gzip here).
    pub xml: String,
}

/// Maps FilmCraft ids to Premiere ObjectID / ObjectUID strings so Save can patch the XML.
#[derive(Clone, Debug, Default)]
pub struct NativeBindings {
    /// Timeline clip → ClipTrackItem ObjectID (`Start` / `End`).
    pub track_items: BTreeMap<ClipId, String>,
    /// Timeline clip → VideoClip / AudioClip ObjectID (`InPoint` / `OutPoint`).
    pub clips: BTreeMap<ClipId, String>,
    /// Media project item → Media ObjectUID (`FilePath` / `ActualMediaFilePath`).
    pub media: BTreeMap<ItemId, String>,
    /// Sequence project item → Sequence ObjectUID.
    pub sequences: BTreeMap<ItemId, String>,
}

/// Gunzip or accept plain XML with the same bounds as [`super::read`].
pub fn expand(bytes: &[u8]) -> Result<NativeSource> {
    let xml = expand_xml(bytes)?;
    Ok(NativeSource { xml })
}

/// Gzip (or plain) bytes for a `.prproj` file from the expanded source.
pub fn encode(source: &NativeSource, gzip: bool) -> Result<Vec<u8>> {
    if source.xml.len() > super::MAX_DOCUMENT_BYTES {
        return Err(Error::Other("project exceeds 512 MiB limit".into()));
    }
    if !gzip {
        return Ok(source.xml.as_bytes().to_vec());
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(source.xml.as_bytes()).map_err(|e| Error::Other(format!("gzip: {e}")))?;
    encoder.finish().map_err(|e| Error::Other(format!("gzip: {e}")))
}

/// Patch mapped timeline and media fields from `project` into `source`. Unmapped Premiere data is left alone and listed in `report`.
pub fn sync_from_project(source: &mut NativeSource, bindings: &NativeBindings, project: &Project, report: &mut Report) -> Result<()> {
    let mut patched = 0usize;
    for (clip_id, object_id) in &bindings.track_items {
        let Some(clip) = find_clip(project, *clip_id) else {
            warning(report, format!("Premiere track item {object_id}: FilmCraft clip gone; left unchanged in the project file"));
            continue;
        };
        let end = clip.start + clip.duration;
        if set_field(&mut source.xml, "ObjectID", object_id, "Start", &clip.start.0.to_string())? {
            patched += 1;
        }
        if set_field(&mut source.xml, "ObjectID", object_id, "End", &end.0.to_string())? {
            patched += 1;
        }
        if let Some(clip_oid) = bindings.clips.get(clip_id) {
            let source_out = source_out_ticks(clip);
            if set_field(&mut source.xml, "ObjectID", clip_oid, "InPoint", &clip.source_in.0.to_string())? {
                patched += 1;
            }
            if set_field(&mut source.xml, "ObjectID", clip_oid, "OutPoint", &source_out.0.to_string())? {
                patched += 1;
            }
        }
    }
    for (item_id, media_uid) in &bindings.media {
        let Some(item) = project.item(*item_id) else {
            warning(report, format!("Premiere media {media_uid}: FilmCraft item gone; left unchanged in the project file"));
            continue;
        };
        let ItemKind::Media(media) = &item.kind else { continue };
        let MediaRef::File { path } = &media.media else { continue };
        // Prefer ActualMediaFilePath when the element already has one; else FilePath.
        let field = if has_child_field(&source.xml, "ObjectUID", media_uid, "ActualMediaFilePath")? { "ActualMediaFilePath" } else { "FilePath" };
        if set_field(&mut source.xml, "ObjectUID", media_uid, field, path)? {
            patched += 1;
        }
    }
    for (item_id, seq_uid) in &bindings.sequences {
        let Some(item) = project.item(*item_id) else {
            warning(report, format!("Premiere sequence {seq_uid}: FilmCraft sequence gone; left unchanged in the project file"));
            continue;
        };
        let Some(seq) = project.sequence(*item_id) else { continue };
        if set_field(&mut source.xml, "ObjectUID", seq_uid, "Name", &item.name)? {
            patched += 1;
        }
        // Frame size lives on the video track group FrameRect, not the Sequence element; report when we cannot write it yet.
        if seq.settings.width != 0 && seq.settings.height != 0 {
            warning(
                report,
                format!(
                    "{}: sequence frame size {}x{} is not rewritten into Premiere track-group FrameRect yet",
                    item.name, seq.settings.width, seq.settings.height
                ),
            );
        }
    }
    warning(
        report,
        format!("Premiere project save updated {patched} mapped field(s); effects, transitions, bins and other unmapped Premiere data were left as opened"),
    );
    Ok(())
}

/// Whether a round-trip of expand → encode(gzip) → expand keeps element names under PremiereData (unknown siblings included).
pub fn retains_unknown_elements(bytes: &[u8], names: &[&str]) -> Result<bool> {
    let source = expand(bytes)?;
    let encoded = encode(&source, true)?;
    let again = expand(&encoded)?;
    Ok(names.iter().all(|name| again.xml.contains(&format!("<{name}"))))
}

fn expand_xml(bytes: &[u8]) -> Result<String> {
    let (limit, message) = input_limit(bytes);
    if bytes.len() > limit {
        return Err(Error::Other(message.into()));
    }
    let expanded = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut expanded = Vec::new();
        use std::io::Read;
        flate2::read::MultiGzDecoder::new(bytes)
            .take(super::MAX_DOCUMENT_BYTES as u64 + 1)
            .read_to_end(&mut expanded)
            .map_err(|e| Error::Other(format!("gzip: {e}")))?;
        if expanded.len() > super::MAX_DOCUMENT_BYTES {
            return Err(Error::Other("expanded project exceeds 512 MiB limit".into()));
        }
        expanded
    } else {
        bytes.to_vec()
    };
    String::from_utf8(expanded).map_err(|e| Error::Other(e.to_string()))
}

fn input_limit(header: &[u8]) -> (usize, &'static str) {
    if header.starts_with(&[0x1f, 0x8b]) {
        (64 * 1024 * 1024, "compressed project exceeds 64 MiB input limit")
    } else {
        (super::MAX_DOCUMENT_BYTES, "project exceeds 512 MiB input limit")
    }
}

fn find_clip(project: &Project, id: ClipId) -> Option<&filmcraft_project::TrackItem> {
    for item in project.items.values() {
        let ItemKind::Sequence(seq) = &item.kind else { continue };
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            if let Some(clip) = track.items.iter().find(|c| c.id == id) {
                return Some(clip);
            }
        }
    }
    None
}

fn source_out_ticks(clip: &filmcraft_project::TrackItem) -> Tick {
    let span = (clip.duration.0 as f64 * clip.speed.abs()).round() as i64;
    Tick(clip.source_in.0.saturating_add(span.max(0)))
}

/// Replace the text of the first descendant element named `field` under the object with `attr`=`id`.
fn set_field(xml: &mut String, attr: &str, id: &str, field: &str, value: &str) -> Result<bool> {
    let patch = {
        let doc = roxmltree::Document::parse(xml).map_err(|e| Error::Other(e.to_string()))?;
        let Some(object) = doc.descendants().find(|n| n.is_element() && n.attribute(attr) == Some(id)) else {
            return Ok(false);
        };
        let Some(node) = object.descendants().find(|n| n.is_element() && n.tag_name().name() == field) else {
            return Ok(false);
        };
        text_patch(xml, node, value)?
    };
    match patch {
        None => Ok(false),
        Some((range, replacement)) => {
            xml.replace_range(range, &replacement);
            Ok(true)
        }
    }
}

fn has_child_field(xml: &str, attr: &str, id: &str, field: &str) -> Result<bool> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| Error::Other(e.to_string()))?;
    let Some(object) = doc.descendants().find(|n| n.is_element() && n.attribute(attr) == Some(id)) else {
        return Ok(false);
    };
    Ok(object.descendants().any(|n| n.is_element() && n.tag_name().name() == field))
}

/// Compute a replacement for an element's text without mutating `xml` yet.
fn text_patch(xml: &str, node: Node<'_, '_>, value: &str) -> Result<Option<(std::ops::Range<usize>, String)>> {
    let range = node.range();
    let slice = &xml[range.clone()];
    let name = node.tag_name().name();
    let open_end = slice.find('>').ok_or_else(|| Error::Other(format!("malformed <{name}>")))? + 1;
    let close = format!("</{name}>");
    let escaped = escape_text(value);
    let Some(close_at) = slice.rfind(&close) else {
        return Ok(Some((range, format!("<{name}>{escaped}</{name}>"))));
    };
    if close_at < open_end {
        return Err(Error::Other(format!("malformed <{name}>")));
    }
    let from = range.start + open_end;
    let to = range.start + close_at;
    if xml[from..to] == *escaped {
        return Ok(None);
    }
    Ok(Some((from..to, escaped)))
}

fn escape_text(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            c => o.push(c),
        }
    }
    o
}

/// Record FilmCraft ↔ Premiere ids while building the import.
impl NativeBindings {
    pub fn note_sequence(&mut self, item: ItemId, object_uid: &str) {
        if !object_uid.is_empty() {
            self.sequences.insert(item, object_uid.to_string());
        }
    }

    pub fn note_track_item(&mut self, clip: ClipId, track_item_object_id: &str, clip_object_id: Option<&str>) {
        if !track_item_object_id.is_empty() {
            self.track_items.insert(clip, track_item_object_id.to_string());
        }
        if let Some(id) = clip_object_id.filter(|s| !s.is_empty()) {
            self.clips.insert(clip, id.to_string());
        }
    }

    pub fn note_media(&mut self, item: ItemId, media_uid: &str) {
        if !media_uid.is_empty() {
            self.media.insert(item, media_uid.to_string());
        }
    }
}

/// Open bytes as a native source plus the mapped document (no Project build).
pub fn read_native(bytes: &[u8], base_dir: Option<&str>, report: &mut Report) -> Result<(NativeSource, Document)> {
    let source = expand(bytes)?;
    let doc = super::read(source.xml.as_bytes(), base_dir, report)?;
    Ok((source, doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImportOptions;
    use crate::premiere::{import_into, read};

    fn fixture() -> &'static str {
        r#"<PremiereData>
<Sequence ObjectUID="parent"><Name>Parent</Name><TrackGroups><TrackGroup><Second ObjectRef="group"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="group"><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="one"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="one"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="sub-one"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sub-one"><Name>FIRST.MOV</Name><Clip ObjectRef="clip-one"/></SubClip>
<VideoClip ObjectID="clip-one"><Clip><InPoint>0</InPoint><OutPoint>254016000000</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="source-one"/></Clip></VideoClip>
<VideoMediaSource ObjectID="source-one"><MediaSource><Media ObjectURef="media-one"/></MediaSource></VideoMediaSource>
<Media ObjectUID="media-one"><Title>first.mov</Title><FilePath>first.mov</FilePath></Media>
<OrphanCustom ObjectUID="keep-me"><Payload>unmodeled</Payload></OrphanCustom>
</PremiereData>"#
    }

    #[test]
    fn gzip_round_trip_keeps_unknown_elements() {
        let source = expand(fixture().as_bytes()).unwrap();
        let gz = encode(&source, true).unwrap();
        assert!(gz.starts_with(&[0x1f, 0x8b]));
        assert!(retains_unknown_elements(&gz, &["OrphanCustom", "Sequence", "Media"]).unwrap());
    }

    #[test]
    fn patch_start_end_and_media_path_then_reopen() {
        let mut source = expand(fixture().as_bytes()).unwrap();
        assert!(set_field(&mut source.xml, "ObjectID", "one", "Start", "100").unwrap());
        assert!(set_field(&mut source.xml, "ObjectID", "one", "End", "200").unwrap());
        assert!(set_field(&mut source.xml, "ObjectUID", "media-one", "FilePath", "relocated.mov").unwrap());
        let mut report = Report::default();
        let doc = read(source.xml.as_bytes(), None, &mut report).unwrap();
        assert_eq!(doc.sequences[0].tracks[0].items[0].start, Tick(100));
        assert_eq!(doc.sequences[0].tracks[0].items[0].end, Tick(200));
        assert_eq!(doc.sequences[0].tracks[0].items[0].media_path.as_deref(), Some("relocated.mov"));
        assert!(source.xml.contains("OrphanCustom"));
        assert!(source.xml.contains("unmodeled"));
    }

    #[test]
    fn import_bindings_sync_clip_timing_through_encode() {
        let mut bindings = NativeBindings::default();
        let (fragment, _tree, mut report) = import_into(fixture().as_bytes(), &ImportOptions::default(), None, Some(&mut bindings)).unwrap();
        assert!(!bindings.track_items.is_empty());
        // Shift the clip in the FilmCraft project, then sync back into XML.
        let mut project = fragment.project;
        {
            let seq = project.sequence_mut(fragment.sequences[0]).unwrap();
            seq.video_tracks[0].items[0].start = Tick(50);
            seq.video_tracks[0].items[0].duration = Tick(150);
        }
        let mut source = expand(fixture().as_bytes()).unwrap();
        sync_from_project(&mut source, &bindings, &project, &mut report).unwrap();
        let encoded = encode(&source, true).unwrap();
        let mut report2 = Report::default();
        let (again, _) = read_native(&encoded, None, &mut report2).unwrap();
        let mut report3 = Report::default();
        let doc = read(again.xml.as_bytes(), None, &mut report3).unwrap();
        assert_eq!(doc.sequences[0].tracks[0].items[0].start, Tick(50));
        assert_eq!(doc.sequences[0].tracks[0].items[0].end, Tick(200));
        assert!(again.xml.contains("OrphanCustom"));
    }
}

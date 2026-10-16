//! Clean-room reader for Premiere Pro projects.
//!
//! Premiere projects are gzip streams containing a reference-based `PremiereData` XML document.
//! This importer reads the project/bin/media/sequence graph and maps its basic clips and tracks
//! into FilmCraft. Effects and unsupported project objects are reported instead of guessed.

use std::{collections::HashMap, io::Read};

use filmcraft_media::MediaKind;
use filmcraft_project::{ItemId, ItemKind, Label, MediaRef, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use roxmltree::{Document, Node};

use crate::common::{Builder, MediaSpec, empty_sequence, file_name, kind_from_ext, media_clip, media_info, resolve_path};
use crate::{Error, ImportOptions, Imported, Report, Result};

const MAX_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_OBJECTS: usize = 250_000;

/// Import the historical gzip-wrapped PremiereData XML representation.
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Report)> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Err(Error::parse(crate::Format::Prproj, "expected a gzip-compressed project"));
    }
    let xml = decompress(bytes, MAX_XML_BYTES)?;
    let text = std::str::from_utf8(&xml).map_err(|e| Error::parse(crate::Format::Prproj, format!("project XML is not UTF-8: {e}")))?;
    let doc = Document::parse(text).map_err(|e| Error::parse(crate::Format::Prproj, format!("invalid project XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err(Error::parse(crate::Format::Prproj, "XML root is not PremiereData"));
    }
    let nodes: Vec<Node<'_, '_>> = root
        .descendants()
        .filter(|n| n.is_element() && (n.attribute("ObjectID").is_some() || n.attribute("ObjectUID").is_some()))
        .take(MAX_OBJECTS + 1)
        .collect();
    if nodes.len() > MAX_OBJECTS {
        return Err(Error::parse(crate::Format::Prproj, "project contains too many objects"));
    }
    let mut by_id = HashMap::new();
    let mut by_uid = HashMap::new();
    for n in nodes.iter().copied() {
        if let Some(id) = n.attribute("ObjectID") {
            by_id.insert(id, n);
        }
        if let Some(uid) = n.attribute("ObjectUID") {
            by_uid.insert(uid, n);
        }
    }
    let name = opts.name.as_deref().unwrap_or("Imported Premiere Project");
    let mut builder = Builder::new(name);
    let mut report = Report::default();

    // Map media object UUIDs to paths, then Premiere's master clips to those media objects.
    let mut media_paths = HashMap::<&str, (String, String)>::new();
    for n in nodes.iter().copied().filter(|n| n.tag_name().name() == "Media") {
        let Some(uid) = n.attribute("ObjectUID") else { continue };
        let raw = child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath")).or_else(|| child_text(n, "RelativePath"));
        let Some(raw) = raw else { continue };
        let path = resolve_path(raw, opts.base_dir.as_deref());
        let title = child_text(n, "Title").map(str::to_string).unwrap_or_else(|| file_name(&path).to_string());
        media_paths.insert(uid, (path, title));
    }
    let mut master_media = HashMap::<&str, &str>::new();
    for n in nodes.iter().copied().filter(|n| n.tag_name().name() == "MasterClip") {
        let Some(master_uid) = n.attribute("ObjectUID") else { continue };
        for clip_ref in n.descendants().filter(|x| x.tag_name().name() == "Clip") {
            let Some(clip) = ref_node(clip_ref, &by_id, &by_uid) else { continue };
            if let Some(media_uid) = source_media_uid(clip, &by_id, &by_uid) {
                master_media.insert(master_uid, media_uid);
                break;
            }
        }
    }
    let mut media_items = HashMap::<&str, ItemId>::new();
    // Import the media items shared by the project bin and timeline references.
    for n in nodes.iter().copied().filter(|n| n.tag_name().name() == "ClipProjectItem") {
        let Some(master_ref) = n.descendants().find(|x| x.tag_name().name() == "MasterClip").and_then(|x| x.attribute("ObjectURef")) else {
            continue;
        };
        let item_name = child_text(n, "Name").unwrap_or("Media");
        let Some(media_uid) = master_media.get(master_ref).copied() else {
            report.warn(format!("media item \"{item_name}\" has no recognised source and was skipped"));
            continue;
        };
        let Some((path, title)) = media_paths.get(media_uid) else { continue };
        let spec = MediaSpec { kind: Some(kind_from_ext(title)), ..Default::default() };
        let id = builder.p.add_item(item_name, Label::Iris, ItemKind::Media(media_clip(MediaRef::File { path: path.clone() }, media_info(title, &spec))), None);
        media_items.insert(media_uid, id);
    }

    let sequence_nodes: Vec<Node<'_, '_>> = nodes.iter().copied().filter(|n| n.tag_name().name() == "Sequence").collect();
    for sequence_node in sequence_nodes {
        let seq_name = child_text(sequence_node, "Name").unwrap_or("Sequence");
        let Some(track_groups) = child(sequence_node, "TrackGroups") else { continue };
        let mut video_group = None;
        let mut audio_group = None;
        for group_ref in track_groups.children().filter(|n| n.tag_name().name() == "TrackGroup") {
            let Some(group) = child(group_ref, "Second").and_then(|n| n.attribute("ObjectRef")).and_then(|id| by_id.get(id).copied()) else { continue };
            match group.tag_name().name() {
                "VideoTrackGroup" => video_group = Some(group),
                "AudioTrackGroup" => audio_group = Some(group),
                _ => {}
            }
        }
        let (width, height, frame_rate) = video_group.map(|g| sequence_video_settings(g)).unwrap_or((1920, 1080, FrameRate::FPS_24));
        let sample_rate = audio_group
            .and_then(|g| child(child(g, "TrackGroup")?, "FrameRate").and_then(parse_i64))
            .filter(|v| *v > 0)
            .map(|v| (i128::from(TICKS_PER_SECOND) / i128::from(v)).clamp(1, i128::from(u32::MAX)) as u32)
            .unwrap_or(48_000);
        let settings = SequenceSettings { width, height, frame_rate, sample_rate, preset: "Premiere Pro sequence".into(), ..Default::default() };
        let seq_id = builder.reserve_sequence(seq_name, settings.clone(), None);
        builder.top.push(seq_id);
        let mut seq = empty_sequence(settings);
        if let Some(group) = video_group {
            import_tracks(group, TrackKind::Video, &by_id, &by_uid, &media_items, &mut builder, &mut seq)?;
        }
        if let Some(group) = audio_group {
            import_tracks(group, TrackKind::Audio, &by_id, &by_uid, &media_items, &mut builder, &mut seq)?;
        }
        builder.put_sequence(seq_id, seq);
    }
    if builder.top.is_empty() && media_items.is_empty() {
        return Err(Error::Empty);
    }
    report.info("Premiere project import currently keeps media and basic sequence clips; effects, markers and nested bins may be omitted");
    Ok((builder.finish(), report))
}

fn decompress(bytes: &[u8], limit: usize) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut limited = decoder.take((limit as u64).saturating_add(1));
    let mut xml = Vec::new();
    limited.read_to_end(&mut xml).map_err(|e| Error::parse(crate::Format::Prproj, format!("cannot decompress project: {e}")))?;
    if xml.len() > limit {
        return Err(Error::parse(crate::Format::Prproj, format!("decompressed project exceeds the {limit}-byte safety limit")));
    }
    Ok(xml)
}

fn child<'a, 'input>(n: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    n.children().find(|x| x.is_element() && x.tag_name().name() == name)
}
fn child_text<'a, 'input>(n: Node<'a, 'input>, name: &str) -> Option<&'a str> {
    child(n, name)?.text().map(str::trim).filter(|s| !s.is_empty())
}
fn parse_i64(n: Node<'_, '_>) -> Option<i64> {
    n.text()?.trim().parse().ok()
}
fn ref_node<'a, 'input>(n: Node<'a, 'input>, by_id: &HashMap<&str, Node<'a, 'input>>, by_uid: &HashMap<&str, Node<'a, 'input>>) -> Option<Node<'a, 'input>> {
    n.attribute("ObjectRef").and_then(|id| by_id.get(id).copied()).or_else(|| n.attribute("ObjectURef").and_then(|id| by_uid.get(id).copied()))
}
fn source_media_uid<'a, 'input>(clip: Node<'a, 'input>, by_id: &HashMap<&str, Node<'a, 'input>>, by_uid: &HashMap<&str, Node<'a, 'input>>) -> Option<&'a str> {
    let source = clip.descendants().find(|n| n.tag_name().name() == "Source")?;
    let media_source = ref_node(source, by_id, by_uid)?;
    let media_ref = media_source.descendants().find(|n| n.tag_name().name() == "Media")?;
    ref_node(media_ref, by_id, by_uid)?.attribute("ObjectUID")
}
fn sequence_video_settings(group: Node<'_, '_>) -> (u32, u32, FrameRate) {
    let frame_rect = child_text(group, "FrameRect").unwrap_or("0,0,1920,1080");
    let dims: Vec<u32> = frame_rect.split(',').filter_map(|s| s.trim().parse().ok()).take(4).collect();
    let width = dims.get(2).copied().filter(|v| *v > 0 && *v <= 32768).unwrap_or(1920);
    let height = dims.get(3).copied().filter(|v| *v > 0 && *v <= 32768).unwrap_or(1080);
    let frame_ticks = child(child(group, "TrackGroup").unwrap_or(group), "FrameRate").and_then(parse_i64).filter(|v| *v > 0).unwrap_or(TICKS_PER_SECOND / 24);
    (width, height, FrameRate::new(TICKS_PER_SECOND, frame_ticks))
}
#[allow(clippy::too_many_arguments)]
fn import_tracks<'a, 'input>(
    group: Node<'a, 'input>,
    kind: TrackKind,
    by_id: &HashMap<&str, Node<'a, 'input>>,
    by_uid: &HashMap<&str, Node<'a, 'input>>,
    media_items: &HashMap<&str, ItemId>,
    builder: &mut Builder,
    seq: &mut filmcraft_project::Sequence,
) -> Result<()> {
    let track_group = child(group, "TrackGroup").ok_or_else(|| Error::parse(crate::Format::Prproj, "track group has no TrackGroup data"))?;
    let tracks_node = child(track_group, "Tracks");
    let Some(tracks_node) = tracks_node else { return Ok(()) };
    for track_ref in tracks_node.children().filter(|n| n.tag_name().name() == "Track") {
        let Some(track) = ref_node(track_ref, by_id, by_uid) else { continue };
        let clip_track = child(track, "ClipTrack").unwrap_or(track);
        let index = child(clip_track, "Track").and_then(|n| child(n, "Index")).and_then(parse_i64).and_then(|value| usize::try_from(value).ok()).unwrap_or(0);
        if index >= 256 {
            continue;
        }
        builder.ensure_tracks(seq, kind, index.saturating_add(1));
        let dst = seq.tracks_mut(kind).get_mut(index).ok_or_else(|| Error::parse(crate::Format::Prproj, "track index is invalid"))?;
        let items = child(clip_track, "ClipItems").and_then(|n| child(n, "TrackItems"));
        let Some(items) = items else { continue };
        for item_ref in items.children().filter(|n| n.tag_name().name() == "TrackItem") {
            let Some(track_item) = ref_node(item_ref, by_id, by_uid) else { continue };
            let Some(subclip) = child(track_item, "ClipTrackItem").and_then(|n| child(n, "SubClip")).and_then(|n| ref_node(n, by_id, by_uid)) else { continue };
            let Some(clip) = child(subclip, "Clip").and_then(|n| ref_node(n, by_id, by_uid)) else { continue };
            let Some(media_uid) = source_media_uid(clip, by_id, by_uid) else { continue };
            let Some(item_id) = media_items.get(media_uid).copied() else { continue };
            let timing = child(track_item, "ClipTrackItem")
                .and_then(|n| child(n, "TrackItem"))
                .ok_or_else(|| Error::parse(crate::Format::Prproj, "clip track item has no timing"))?;
            let start = child(timing, "Start").and_then(parse_i64).unwrap_or(0);
            let end = child(timing, "End").and_then(parse_i64).unwrap_or(start);
            let duration = end.checked_sub(start).filter(|v| *v > 0).map(Tick).unwrap_or(Tick::ZERO);
            if duration == Tick::ZERO {
                continue;
            }
            let source_in = if builder.p.item(item_id).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == MediaKind::Still) {
                0
            } else {
                child(clip, "Clip").and_then(|n| child(n, "InPoint")).and_then(parse_i64).filter(|v| *v >= 0).unwrap_or(0)
            };
            if source_in.checked_add(duration.0).is_none() {
                continue;
            }
            let name = child(subclip, "Name").and_then(|n| n.text()).unwrap_or("Clip");
            let mut new_clip = builder.clip(item_id, kind, name, Tick(start.max(0)), duration, Tick(source_in));
            new_clip.effects.clear();
            dst.items.push(new_clip);
        }
        dst.sort();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn malformed_gzip_and_oversized_decompression_return_errors() {
        assert!(import(&[0x1f, 0x8b, 0, 1], &ImportOptions::default()).is_err());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let payload = vec![b' '; 1025];
        assert!(encoder.write_all(&payload).is_ok());
        let compressed = encoder.finish();
        if let Ok(compressed) = compressed {
            assert!(decompress(&compressed, 1024).is_err());
        }
    }

    #[test]
    fn imports_media_and_basic_timeline_from_gzip_xml() {
        let xml = r#"<PremiereData Version="3">
          <RootProjectItem ObjectUID="root"><ProjectItemContainer><Items><Item Index="0" ObjectURef="pi"/></Items></ProjectItemContainer></RootProjectItem>
          <ClipProjectItem ObjectUID="pi"><ProjectItem><Name>shot.mov</Name></ProjectItem><MasterClip ObjectURef="master"/></ClipProjectItem>
          <MasterClip ObjectUID="master"><Clips><Clip ObjectRef="10"/></Clips></MasterClip>
          <VideoClip ObjectID="10"><Clip><Source ObjectRef="11"/><InPoint>0</InPoint><OutPoint>2540160000000</OutPoint></Clip></VideoClip>
          <VideoMediaSource ObjectID="11"><MediaSource><Media ObjectURef="media"/></MediaSource></VideoMediaSource>
          <Media ObjectUID="media"><ActualMediaFilePath>/media/shot.mov</ActualMediaFilePath><Title>shot.mov</Title></Media>
          <Sequence ObjectUID="seq"><Name>Cut</Name><TrackGroups><TrackGroup><First>video</First><Second ObjectRef="12"/></TrackGroup></TrackGroups></Sequence>
          <VideoTrackGroup ObjectID="12"><TrackGroup><Tracks><Track Index="0" ObjectURef="track"/></Tracks><FrameRate>10584000000</FrameRate></TrackGroup><FrameRect>0,0,1920,1080</FrameRect></VideoTrackGroup>
          <VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="13"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
          <VideoClipTrackItem ObjectID="13"><ClipTrackItem><TrackItem><Start>0</Start><End>2540160000000</End></TrackItem><SubClip ObjectRef="14"/></ClipTrackItem></VideoClipTrackItem>
          <SubClip ObjectID="14"><Clip ObjectRef="15"/><Name>shot.mov</Name></SubClip>
          <VideoClip ObjectID="15"><Clip><Source ObjectRef="11"/><InPoint>0</InPoint><OutPoint>2540160000000</OutPoint></Clip></VideoClip>
        </PremiereData>"#;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        assert!(encoder.write_all(xml.as_bytes()).is_ok());
        let compressed = encoder.finish();
        let Ok(compressed) = compressed else { return };
        let result = import(&compressed, &ImportOptions { name: Some("test".into()), ..Default::default() });
        let Ok((imported, report)) = result else { panic!("synthetic Premiere project should import: {result:?}") };
        assert_eq!(imported.sequences.len(), 1);
        let Some(seq_id) = imported.sequences.first().copied() else { panic!("sequence missing") };
        let Some(sequence) = imported.project.sequence(seq_id) else { panic!("sequence item missing") };
        assert_eq!(sequence.video_tracks.first().map(|t| t.items.len()), Some(1));
        assert_eq!(imported.project.items.values().filter(|i| matches!(i.kind, ItemKind::Media(_))).count(), 1);
        assert!(!report.is_empty());
    }

    #[test]
    fn truncated_and_mutated_gzip_never_panics() {
        let xml = b"<PremiereData Version=\"3\"><Project/></PremiereData>";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        assert!(encoder.write_all(xml).is_ok());
        let Ok(gzip) = encoder.finish() else { return };
        for end in 0..gzip.len() {
            let truncated = &gzip[..end];
            assert!(std::panic::catch_unwind(|| import(truncated, &ImportOptions::default())).is_ok());
        }
        for offset in 0..gzip.len() {
            let mut damaged = gzip.clone();
            if let Some(byte) = damaged.get_mut(offset) {
                *byte ^= 0x80;
            }
            assert!(std::panic::catch_unwind(|| import(&damaged, &ImportOptions::default())).is_ok());
        }
    }
}

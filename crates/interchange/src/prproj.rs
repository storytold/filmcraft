//! Premiere Pro project (`.prproj`) interchange.
//!
//! A `.prproj` file is a gzip-compressed XML document rooted at `<PremiereData>`.
//! Objects are defined with `ObjectID="<id>"` and referenced via `ObjectRef="<id>"`
//! or `ObjectURef="<id>"`.

use std::collections::HashMap;
use std::io::Read;

use filmcraft_project::{Sequence, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use flate2::read::GzDecoder;
use roxmltree::{Document, Node};

use crate::common::{self, Builder, MediaSpec};
use crate::{Error, Format, ImportOptions, Imported, Report, Result};

/// Maximum decompressed size (256 MB) to prevent compression bombs.
const MAX_DECOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// Decompresses gzip bytes or falls back to raw bytes if uncompressed.
pub fn decompress_prproj(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let decoder = GzDecoder::new(bytes);
        let mut take = decoder.take(MAX_DECOMPRESSED_BYTES + 1);
        let mut text = String::new();
        take.read_to_string(&mut text).map_err(|e| Error::parse(Format::Prproj, format!("gzip decompression failed: {e}")))?;
        if text.len() as u64 > MAX_DECOMPRESSED_BYTES {
            return Err(Error::parse(Format::Prproj, format!("decompressed size exceeds limit of {MAX_DECOMPRESSED_BYTES} bytes")));
        }
        Ok(text)
    } else {
        // Uncompressed XML document
        String::from_utf8(bytes.to_vec()).map_err(|e| Error::parse(Format::Prproj, format!("invalid UTF-8 in uncompressed project: {e}")))
    }
}

/// Checks whether the bytes or decompressed stream match PremiereData.
pub fn sniff(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(bytes);
        let mut buf = [0u8; 1024];
        if let Ok(n) = decoder.read(&mut buf) {
            let sample = String::from_utf8_lossy(&buf[..n]);
            return sample.contains("<PremiereData");
        }
        false
    } else {
        let sample = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
        sample.contains("<PremiereData")
    }
}

/// Import a Premiere Pro project document.
pub fn import(bytes: &[u8], opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let xml_text = decompress_prproj(bytes)?;
    let doc = Document::parse(&xml_text).map_err(|e| Error::parse(Format::Prproj, format!("XML syntax error: {e}")))?;

    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err(Error::parse(Format::Prproj, format!("expected <PremiereData> root element, found <{}>", root.tag_name().name())));
    }

    // Pass 1: index all elements with an ObjectID attribute.
    let mut id_map: HashMap<&str, Node<'_, '_>> = HashMap::new();
    for node in doc.descendants() {
        if let Some(id) = node.attribute("ObjectID") {
            id_map.insert(id, node);
        }
    }

    let proj_name = opts.name.as_deref().unwrap_or("Premiere Project");
    let mut b = Builder::new(proj_name);
    let mut seq_ids = Vec::new();

    // Pass 2: locate all Sequence nodes in the document.
    let sequence_nodes: Vec<Node<'_, '_>> = doc.descendants().filter(|n| n.tag_name().name() == "Sequence").collect();

    if sequence_nodes.is_empty() {
        report.warn("No sequence definitions found in Premiere project");
    }

    for seq_node in sequence_nodes {
        let name = child_text(seq_node, "Name").unwrap_or_else(|| opts.name.clone().unwrap_or_else(|| "Sequence".to_string()));

        let frame_rate = parse_frame_rate(seq_node);
        let (width, height) = parse_frame_dimensions(seq_node);

        let settings = common::settings_for(frame_rate, width, height, false);
        let seq_item_id = b.reserve_sequence(&name, settings.clone(), None);
        let mut sequence = common::empty_sequence(settings);

        let mut ctx = ParseContext { id_map: &id_map, b: &mut b, frame_rate, opts, report };

        // Parse video tracks
        parse_track_group(seq_node, TrackKind::Video, &mut sequence, &mut ctx);

        // Parse audio tracks
        parse_track_group(seq_node, TrackKind::Audio, &mut sequence, &mut ctx);

        b.put_sequence(seq_item_id, sequence);
        seq_ids.push(seq_item_id);
    }

    let mut imported = b.finish();
    imported.sequences = seq_ids;
    Ok(imported)
}

/// Extracts trimmed text content from the first immediate child matching `tag`.
fn child_text(node: Node<'_, '_>, tag: &str) -> Option<String> {
    node.children().find(|c| c.tag_name().name() == tag).and_then(|c| c.text()).map(|t| t.trim().to_string())
}

/// Parses the sequence frame rate from a sequence node's `<Timebase>` or `<FrameRate>` tags.
fn parse_frame_rate(node: Node<'_, '_>) -> FrameRate {
    if let Some(tb_str) = child_text(node, "Timebase")
        && let Ok(tb) = tb_str.parse::<u64>()
        && tb > 0
    {
        if tb < 1000 {
            return FrameRate::from_f64(tb as f64);
        }
        let fps_approx = (TICKS_PER_SECOND as f64 / tb as f64).round();
        return FrameRate::from_f64(fps_approx);
    }

    if let Some(fr_str) = child_text(node, "FrameRate")
        && let Ok(fr) = fr_str.parse::<f64>()
        && fr > 0.0
    {
        return FrameRate::from_f64(fr);
    }

    FrameRate::FPS_24
}

/// Extracts sequence frame dimensions `(width, height)` from `VideoFrameSize` or `Width`/`Height`.
fn parse_frame_dimensions(node: Node<'_, '_>) -> (u32, u32) {
    if let Some(dim_str) = child_text(node, "VideoFrameSize") {
        let parts: Vec<&str> = dim_str.split(',').map(str::trim).collect();
        if parts.len() == 2
            && let Ok(w) = parts[0].parse::<u32>()
            && let Ok(h) = parts[1].parse::<u32>()
            && w > 0
            && h > 0
        {
            return (w, h);
        }
    }

    if let (Some(w_str), Some(h_str)) = (child_text(node, "Width"), child_text(node, "Height"))
        && let Ok(w) = w_str.parse::<u32>()
        && let Ok(h) = h_str.parse::<u32>()
        && w > 0
        && h > 0
    {
        return (w, h);
    }

    (1920, 1080)
}

/// Resolves an `ObjectRef` or `ObjectURef` attribute to its target node in `id_map`.
fn resolve_ref<'a, 'input>(node: Node<'a, 'input>, id_map: &HashMap<&'input str, Node<'a, 'input>>) -> Option<Node<'a, 'input>> {
    if let Some(r) = node.attribute("ObjectRef").or_else(|| node.attribute("ObjectURef")) { id_map.get(r).copied() } else { Some(node) }
}

struct ParseContext<'a, 'input> {
    id_map: &'a HashMap<&'input str, Node<'a, 'input>>,
    b: &'a mut Builder,
    frame_rate: FrameRate,
    #[allow(dead_code)]
    opts: &'a ImportOptions,
    #[allow(dead_code)]
    report: &'a mut Report,
}

/// Scans sequence children for video or audio track groups and populates the sequence tracks.
fn parse_track_group(seq_node: Node<'_, '_>, kind: TrackKind, sequence: &mut Sequence, ctx: &mut ParseContext<'_, '_>) {
    let group_tag = match kind {
        TrackKind::Video => "VideoTrackGroup",
        TrackKind::Audio => "AudioTrackGroup",
    };
    let tracks_tag = match kind {
        TrackKind::Video => "VideoTracks",
        TrackKind::Audio => "AudioTracks",
    };
    let track_tag = match kind {
        TrackKind::Video => "VideoTrack",
        TrackKind::Audio => "AudioTrack",
    };

    let mut track_nodes: Vec<Node<'_, '_>> = Vec::new();

    for child in seq_node.children() {
        if child.tag_name().name() == group_tag || child.tag_name().name() == tracks_tag {
            let target = resolve_ref(child, ctx.id_map).unwrap_or(child);
            for t in target.descendants() {
                if t.tag_name().name() == track_tag || t.tag_name().name() == "Track" {
                    let resolved = resolve_ref(t, ctx.id_map).unwrap_or(t);
                    track_nodes.push(resolved);
                }
            }
        }
    }

    if track_nodes.is_empty() {
        for t in seq_node.descendants() {
            if t.tag_name().name() == track_tag {
                let resolved = resolve_ref(t, ctx.id_map).unwrap_or(t);
                track_nodes.push(resolved);
            }
        }
    }

    let track_count = track_nodes.len().max(1);
    ctx.b.ensure_tracks(sequence, kind, track_count);

    for (idx, tr_node) in track_nodes.into_iter().enumerate() {
        parse_track_items(tr_node, kind, idx, sequence, ctx);
    }
}

/// Locates clip track items within a track node and adds them as clips in the timeline sequence.
fn parse_track_items(track_node: Node<'_, '_>, kind: TrackKind, track_idx: usize, sequence: &mut Sequence, ctx: &mut ParseContext<'_, '_>) {
    let item_nodes: Vec<Node<'_, '_>> = track_node
        .descendants()
        .filter(|n| {
            let tag = n.tag_name().name();
            tag == "ClipTrackItem" || tag == "TrackItem" || tag == "VideoClip" || tag == "AudioClip"
        })
        .collect();

    for item_node in item_nodes {
        let item = resolve_ref(item_node, ctx.id_map).unwrap_or(item_node);

        let start_tick = parse_time(item, "Start", ctx.frame_rate);
        let end_tick = parse_time(item, "End", ctx.frame_rate);
        let in_tick = parse_time(item, "In", ctx.frame_rate);
        let out_tick = parse_time(item, "Out", ctx.frame_rate);

        let duration = if end_tick > start_tick {
            end_tick - start_tick
        } else if out_tick > in_tick {
            out_tick - in_tick
        } else {
            Tick::ZERO
        };

        if duration.0 <= 0 {
            continue;
        }

        let name = child_text(item, "Name").unwrap_or_else(|| format!("Clip {}", sequence.tracks(kind)[track_idx].items.len() + 1));

        let media_path = find_media_path(item, ctx.id_map);
        let media_key = media_path.clone().unwrap_or_else(|| name.clone());
        let resolved_path = media_path.as_deref().unwrap_or(&name);

        let spec = MediaSpec {
            video: match kind {
                TrackKind::Video => Some((1920, 1080, ctx.frame_rate)),
                TrackKind::Audio => None,
            },
            audio: match kind {
                TrackKind::Audio => Some((48000, 2)),
                TrackKind::Video => None,
            },
            ..Default::default()
        };

        let media_id = ctx.b.file_media(&media_key, &name, resolved_path, &spec, None);
        let track_item = ctx.b.clip(media_id, kind, &name, start_tick, duration, in_tick);

        if let Some(tr) = sequence.tracks_mut(kind).get_mut(track_idx) {
            tr.items.push(track_item);
        }
    }
}

/// Parses a timestamp or frame number string into exact timeline [`Tick`] duration.
fn parse_time(node: Node<'_, '_>, tag: &str, rate: FrameRate) -> Tick {
    if let Some(s) = child_text(node, tag)
        && let Ok(v) = s.parse::<i64>()
    {
        if v.abs() > 10_000_000 {
            return Tick(v.max(0));
        }
        return rate.tick_of(v.max(0));
    }
    Tick::ZERO
}

/// Resolves file path or media URI from clip track item or referenced media object.
fn find_media_path(node: Node<'_, '_>, id_map: &HashMap<&str, Node<'_, '_>>) -> Option<String> {
    for desc in node.descendants() {
        for tag in ["ActualMediaFilePath", "FilePath", "Path", "URL"] {
            if let Some(t) = child_text(desc, tag)
                && !t.is_empty()
            {
                return Some(common::file_url_to_path(&t));
            }
        }
        if let Some(r) = desc.attribute("ObjectRef").or_else(|| desc.attribute("ObjectURef"))
            && let Some(ref_node) = id_map.get(r)
        {
            for tag in ["ActualMediaFilePath", "FilePath", "Path", "URL"] {
                if let Some(t) = child_text(*ref_node, tag)
                    && !t.is_empty()
                {
                    return Some(common::file_url_to_path(&t));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn gz_compress(input: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap()
    }

    const MINIMAL_PRPROJ_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PremiereData Version="3">
    <Sequence ObjectID="1">
        <Name>Test Cut</Name>
        <Timebase>254016000000</Timebase>
        <VideoFrameSize>1920,1080</VideoFrameSize>
        <VideoTracks>
            <VideoTrack ObjectID="2">
                <ClipTrackItem ObjectID="3">
                    <Name>Shot 01</Name>
                    <Start>0</Start>
                    <End>254016000000</End>
                    <In>0</In>
                    <Out>254016000000</Out>
                    <FilePath>/media/shot01.mov</FilePath>
                </ClipTrackItem>
            </VideoTrack>
        </VideoTracks>
    </Sequence>
</PremiereData>
"#;

    #[test]
    fn sniffs_gz_and_xml_prproj() {
        let gz = gz_compress(MINIMAL_PRPROJ_XML.as_bytes());
        assert!(sniff(&gz));
        assert!(sniff(MINIMAL_PRPROJ_XML.as_bytes()));
        assert!(!sniff(b"not a project file"));
    }

    #[test]
    fn imports_minimal_compressed_prproj() {
        let gz = gz_compress(MINIMAL_PRPROJ_XML.as_bytes());
        let mut report = Report::default();
        let imported = import(&gz, &ImportOptions::default(), &mut report).unwrap();

        assert_eq!(imported.sequences.len(), 1);
        let seq_id = imported.sequences[0];
        let item = imported.project.item(seq_id).unwrap();
        assert_eq!(item.name, "Test Cut");

        let seq = imported.project.sequence(seq_id).unwrap();
        assert_eq!(seq.settings.width, 1920);
        assert_eq!(seq.settings.height, 1080);
        assert_eq!(seq.video_tracks.len(), 1);

        let clip = &seq.video_tracks[0].items[0];
        assert_eq!(clip.name, "Shot 01");
        assert_eq!(clip.duration, Tick(TICKS_PER_SECOND));
    }

    #[test]
    fn imports_uncompressed_xml_fallback() {
        let mut report = Report::default();
        let imported = import(MINIMAL_PRPROJ_XML.as_bytes(), &ImportOptions::default(), &mut report).unwrap();
        assert_eq!(imported.sequences.len(), 1);
    }

    #[test]
    fn protects_against_decompression_bomb() {
        let zeros = vec![0u8; 1024 * 1024]; // 1MB block
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        for _ in 0..260 {
            encoder.write_all(&zeros).unwrap();
        }
        let gz = encoder.finish().unwrap();

        let mut report = Report::default();
        let err = import(&gz, &ImportOptions::default(), &mut report).unwrap_err();
        assert!(err.to_string().contains("decompressed size exceeds limit"));
    }
}

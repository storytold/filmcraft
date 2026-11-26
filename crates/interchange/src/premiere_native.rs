//! Native `.prproj` open/save: keep the expanded Premiere XML for round-trip, patch mapped fields, gzip on write.
//! Clean-room from user-owned / hand-written XML and the FilmCraft Premiere reader. MIT OR Apache-2.0.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use filmcraft_project::{ClipId, ItemId, ItemKind, MediaRef, Project};
use filmcraft_time::Tick;
use flate2::Compression;
use flate2::write::GzEncoder;
use roxmltree::Node;
use serde::{Deserialize, Serialize};

use super::{Document, warning};
use crate::{Error, Report, Result};

/// Expanded Premiere XML kept for open/save. Unmodeled siblings stay in the text across patches.
#[derive(Clone, Debug)]
pub struct NativeSource {
    /// UTF-8 PremiereData document (never gzip here).
    pub xml: String,
}

/// Maps FilmCraft ids to Premiere ObjectID / ObjectUID strings so Save can patch the XML.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NativeBindings {
    /// Timeline clip → ClipTrackItem ObjectID (`Start` / `End`).
    pub track_items: BTreeMap<ClipId, String>,
    /// Timeline clip → VideoClip / AudioClip ObjectID (`InPoint` / `OutPoint`).
    pub clips: BTreeMap<ClipId, String>,
    /// Media project item → every Media ObjectUID that resolved to it (`FilePath` / `ActualMediaFilePath`).
    /// Two Premiere Media elements can share one file path and one FilmCraft item; Save must patch each UID.
    pub media: BTreeMap<ItemId, Vec<String>>,
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
    // One parse + object index for the whole Save; patches applied high-to-low so offsets stay valid.
    let doc = roxmltree::Document::parse(&source.xml).map_err(|e| Error::Other(e.to_string()))?;
    let objects = index_xml_objects(&doc);
    let clips = index_project_clips(project);
    let conflicting_clips = conflicting_shared_clip_oids(bindings, &clips);
    let mut warned_shared = BTreeSet::<String>::new();
    // Deduped by byte range: last queued value for a range wins (agreeing shared clips write the same text).
    let mut patches: BTreeMap<(usize, usize), String> = BTreeMap::new();

    for (clip_id, object_id) in &bindings.track_items {
        let Some(clip) = clips.get(clip_id).copied() else {
            warning(report, format!("Premiere track item {object_id}: FilmCraft clip gone; left unchanged in the project file"));
            continue;
        };
        let end = clip.start + clip.duration;
        queue_field_patch(&source.xml, &objects, "ObjectID", object_id, "Start", &clip.start.0.to_string(), &mut patches)?;
        queue_field_patch(&source.xml, &objects, "ObjectID", object_id, "End", &end.0.to_string(), &mut patches)?;
        if let Some(clip_oid) = bindings.clips.get(clip_id) {
            if conflicting_clips.contains(clip_oid) {
                if warned_shared.insert(clip_oid.clone()) {
                    let names = shared_clip_labels(bindings, &clips, clip_oid);
                    warning(
                        report,
                        format!(
                            "{}: shared Premiere clip object {clip_oid} has disagreeing source ranges; InPoint/OutPoint left unchanged",
                            names.join(", ")
                        ),
                    );
                }
                continue;
            }
            let source_out = source_out_ticks(clip);
            queue_field_patch(&source.xml, &objects, "ObjectID", clip_oid, "InPoint", &clip.source_in.0.to_string(), &mut patches)?;
            queue_field_patch(&source.xml, &objects, "ObjectID", clip_oid, "OutPoint", &source_out.0.to_string(), &mut patches)?;
        }
    }
    for (item_id, media_uids) in &bindings.media {
        let Some(item) = project.item(*item_id) else {
            for media_uid in media_uids {
                warning(report, format!("Premiere media {media_uid}: FilmCraft item gone; left unchanged in the project file"));
            }
            continue;
        };
        let ItemKind::Media(media) = &item.kind else { continue };
        let MediaRef::File { path } = &media.media else { continue };
        // Unresolved import placeholders must not overwrite empty Premiere FilePath elements.
        if path.starts_with("__missing_premiere__/") {
            continue;
        }
        for media_uid in media_uids {
            let field = if object_has_child(&objects, "ObjectUID", media_uid, "ActualMediaFilePath") { "ActualMediaFilePath" } else { "FilePath" };
            queue_field_patch(&source.xml, &objects, "ObjectUID", media_uid, field, path, &mut patches)?;
        }
    }
    for (item_id, seq_uid) in &bindings.sequences {
        let Some(item) = project.item(*item_id) else {
            warning(report, format!("Premiere sequence {seq_uid}: FilmCraft sequence gone; left unchanged in the project file"));
            continue;
        };
        let Some(seq) = project.sequence(*item_id) else { continue };
        queue_field_patch(&source.xml, &objects, "ObjectUID", seq_uid, "Name", &item.name, &mut patches)?;
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
    // Sequences / clips added in FilmCraft have no Premiere ObjectUID/ObjectID: name them so Save does not look clean.
    for item in project.sequences() {
        if !bindings.sequences.contains_key(&item.id) {
            warning(report, format!("{}: sequence has no Premiere binding; not written to the project file", item.name));
        }
        let Some(seq) = item.as_sequence() else { continue };
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            for clip in &track.items {
                if bindings.track_items.contains_key(&clip.id) {
                    continue;
                }
                let clip_label = if clip.name.is_empty() { "unnamed clip" } else { clip.name.as_str() };
                warning(
                    report,
                    format!("{clip_label} in {}: clip has no Premiere binding; not written to the project file", item.name),
                );
            }
        }
    }
    let patched = apply_patches_descending(&mut source.xml, patches);
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

fn index_project_clips(project: &Project) -> BTreeMap<ClipId, &filmcraft_project::TrackItem> {
    let mut clips = BTreeMap::new();
    for item in project.items.values() {
        let ItemKind::Sequence(seq) = &item.kind else { continue };
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            for clip in &track.items {
                clips.insert(clip.id, clip);
            }
        }
    }
    clips
}

/// First element with each ObjectID / ObjectUID (same “first match” rule as a linear scan).
struct XmlObjects<'a, 'input> {
    by_id: BTreeMap<&'a str, Node<'a, 'input>>,
    by_uid: BTreeMap<&'a str, Node<'a, 'input>>,
}

fn index_xml_objects<'a, 'input>(doc: &'a roxmltree::Document<'input>) -> XmlObjects<'a, 'input> {
    let mut by_id = BTreeMap::new();
    let mut by_uid = BTreeMap::new();
    for node in doc.descendants().filter(Node::is_element) {
        if let Some(id) = node.attribute("ObjectID") {
            by_id.entry(id).or_insert(node);
        }
        if let Some(uid) = node.attribute("ObjectUID") {
            by_uid.entry(uid).or_insert(node);
        }
    }
    XmlObjects { by_id, by_uid }
}

fn object_node<'a, 'input>(objects: &XmlObjects<'a, 'input>, attr: &str, id: &str) -> Option<Node<'a, 'input>> {
    match attr {
        "ObjectID" => objects.by_id.get(id).copied(),
        "ObjectUID" => objects.by_uid.get(id).copied(),
        _ => None,
    }
}

fn object_has_child(objects: &XmlObjects<'_, '_>, attr: &str, id: &str, field: &str) -> bool {
    object_node(objects, attr, id).is_some_and(|object| object.descendants().any(|n| n.is_element() && n.tag_name().name() == field))
}

fn queue_field_patch(
    xml: &str,
    objects: &XmlObjects<'_, '_>,
    attr: &str,
    id: &str,
    field: &str,
    value: &str,
    patches: &mut BTreeMap<(usize, usize), String>,
) -> Result<()> {
    let Some(object) = object_node(objects, attr, id) else { return Ok(()) };
    let Some(node) = object.descendants().find(|n| n.is_element() && n.tag_name().name() == field) else {
        return Ok(());
    };
    if let Some((range, replacement)) = text_patch(xml, node, value)? {
        patches.insert((range.start, range.end), replacement);
    }
    Ok(())
}

/// Apply collected text replacements from high offset to low so earlier edits do not shift later ranges.
fn apply_patches_descending(xml: &mut String, patches: BTreeMap<(usize, usize), String>) -> usize {
    let mut list: Vec<_> = patches.into_iter().map(|((start, end), text)| (start..end, text)).collect();
    list.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut applied = 0usize;
    let mut next_safe = usize::MAX;
    for (range, replacement) in list {
        if range.end > next_safe {
            continue;
        }
        next_safe = range.start;
        xml.replace_range(range, &replacement);
        applied += 1;
    }
    applied
}

fn source_out_ticks(clip: &filmcraft_project::TrackItem) -> Tick {
    let span = (clip.duration.0 as f64 * clip.speed.abs()).round() as i64;
    Tick(clip.source_in.0.saturating_add(span.max(0)))
}

/// Premiere Clip ObjectIDs bound to more than one timeline item with different source ranges.
fn conflicting_shared_clip_oids(bindings: &NativeBindings, clips: &BTreeMap<ClipId, &filmcraft_project::TrackItem>) -> BTreeSet<String> {
    let mut ranges: BTreeMap<&str, Vec<(Tick, Tick)>> = BTreeMap::new();
    for (clip_id, oid) in &bindings.clips {
        let Some(clip) = clips.get(clip_id).copied() else { continue };
        ranges.entry(oid.as_str()).or_default().push((clip.source_in, source_out_ticks(clip)));
    }
    ranges
        .into_iter()
        .filter_map(|(oid, rs)| {
            let disagree = rs.len() > 1 && rs.windows(2).any(|w| w[0] != w[1]);
            disagree.then(|| oid.to_string())
        })
        .collect()
}

/// Display names for FilmCraft clips that share `clip_oid` (for save warnings).
fn shared_clip_labels(bindings: &NativeBindings, clips: &BTreeMap<ClipId, &filmcraft_project::TrackItem>, clip_oid: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (clip_id, oid) in &bindings.clips {
        if oid != clip_oid {
            continue;
        }
        let label = clips.get(clip_id).map(|c| if c.name.is_empty() { "unnamed clip".into() } else { c.name.clone() }).unwrap_or_else(|| "missing clip".into());
        if !names.contains(&label) {
            names.push(label);
        }
    }
    names
}

/// Replace the text of the first descendant element named `field` under the object with `attr`=`id`.
#[cfg(test)]
fn set_field(xml: &mut String, attr: &str, id: &str, field: &str, value: &str) -> Result<bool> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| Error::Other(e.to_string()))?;
    let objects = index_xml_objects(&doc);
    let mut patches = BTreeMap::new();
    queue_field_patch(xml, &objects, attr, id, field, value, &mut patches)?;
    Ok(apply_patches_descending(xml, patches) > 0)
}

/// Compute a replacement for an element's text without mutating `xml` yet.
fn text_patch(xml: &str, node: Node<'_, '_>, value: &str) -> Result<Option<(std::ops::Range<usize>, String)>> {
    let range = node.range();
    let slice = &xml[range.clone()];
    let name = node.tag_name().name();
    let open_end = open_tag_end(slice, name)?;
    let close = format!("</{name}>");
    let escaped = escape_text(value);
    let Some(close_at) = slice.rfind(&close) else {
        // Self-closing: keep attributes from the original open tag (`<Name vendorFlag="keep"/>`).
        let open = &slice[..open_end];
        let open_prefix = open.trim_end_matches('>').trim_end_matches('/').trim_end();
        return Ok(Some((range, format!("{open_prefix}>{escaped}</{name}>"))));
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

/// Index just past the opening tag's `>`, ignoring `>` inside quoted attribute values (`hint="a>b"`).
fn open_tag_end(slice: &str, name: &str) -> Result<usize> {
    let bytes = slice.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] != b'<' {
        i += 1;
    }
    if i >= bytes.len() {
        return Err(Error::Other(format!("malformed <{name}>")));
    }
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Ok(i + 1),
            _ => {}
        }
        i += 1;
    }
    Err(Error::Other(format!("malformed <{name}>")))
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
        if media_uid.is_empty() {
            return;
        }
        let uids = self.media.entry(item).or_default();
        if !uids.iter().any(|u| u == media_uid) {
            uids.push(media_uid.to_string());
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
    fn text_patch_ignores_greater_than_inside_quoted_attributes() {
        // A naive find('>') would cut at hint="a>b" and leave an unterminated attribute.
        let xml = r#"<PremiereData><Sequence ObjectUID="s"><Name hint="a>b">Parent</Name></Sequence></PremiereData>"#;
        let mut source = NativeSource { xml: xml.into() };
        assert!(set_field(&mut source.xml, "ObjectUID", "s", "Name", "Renamed").unwrap());
        assert!(source.xml.contains(r#"<Name hint="a>b">Renamed</Name>"#), "{}", source.xml);
        assert!(roxmltree::Document::parse(&source.xml).is_ok(), "patched XML must still parse");
        assert_eq!(open_tag_end(r#"<Name hint='x>y'>T</Name>"#, "Name").unwrap(), r#"<Name hint='x>y'>"#.len());
    }

    #[test]
    fn text_patch_keeps_attributes_when_expanding_self_closing_fields() {
        let xml = r#"<PremiereData><Sequence ObjectUID="s"><Name vendorFlag="keep"/></Sequence></PremiereData>"#;
        let mut source = NativeSource { xml: xml.into() };
        assert!(set_field(&mut source.xml, "ObjectUID", "s", "Name", "Renamed").unwrap());
        assert!(source.xml.contains(r#"<Name vendorFlag="keep">Renamed</Name>"#), "{}", source.xml);
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

    #[test]
    fn sync_skips_conflicting_shared_clip_source_ranges() {
        // Two track items share one VideoClip; disagreeing FilmCraft source ranges must not last-win overwrite.
        let xml = r#"<PremiereData>
<Sequence ObjectUID="parent"><Name>Parent</Name><TrackGroups><TrackGroup><Second ObjectRef="group"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="group"><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="one"/><TrackItem ObjectRef="two"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="one"><ClipTrackItem><TrackItem><Start>0</Start><End>100</End></TrackItem><SubClip ObjectRef="sub"/></ClipTrackItem></VideoClipTrackItem>
<VideoClipTrackItem ObjectID="two"><ClipTrackItem><TrackItem><Start>200</Start><End>300</End></TrackItem><SubClip ObjectRef="sub"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sub"><Name>shared.mov</Name><Clip ObjectRef="clip"/></SubClip>
<VideoClip ObjectID="clip"><Clip><InPoint>0</InPoint><OutPoint>100</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="source"/></Clip></VideoClip>
<VideoMediaSource ObjectID="source"><MediaSource><Media ObjectURef="media"/></MediaSource></VideoMediaSource>
<Media ObjectUID="media"><Title>shared.mov</Title><FilePath>shared.mov</FilePath></Media>
</PremiereData>"#;
        let mut bindings = NativeBindings::default();
        let (fragment, _, _) = import_into(xml.as_bytes(), &ImportOptions::default(), None, Some(&mut bindings)).unwrap();
        let mut project = fragment.project;
        let seq_id = fragment.sequences[0];
        {
            let seq = project.sequence_mut(seq_id).unwrap();
            assert_eq!(seq.video_tracks[0].items.len(), 2);
            seq.video_tracks[0].items[0].source_in = Tick(10);
            seq.video_tracks[0].items[0].duration = Tick(50);
            seq.video_tracks[0].items[1].source_in = Tick(80);
            seq.video_tracks[0].items[1].duration = Tick(40);
        }
        let mut source = expand(xml.as_bytes()).unwrap();
        let mut report = Report::default();
        sync_from_project(&mut source, &bindings, &project, &mut report).unwrap();
        assert!(
            report.entries.iter().any(|e| e.message.contains("shared Premiere clip object") && e.message.contains("disagreeing source ranges")),
            "{:?}",
            report.entries
        );
        assert!(source.xml.contains("<InPoint>0</InPoint>") && source.xml.contains("<OutPoint>100</OutPoint>"), "shared clip range left unchanged: {}", source.xml);
        assert!(!source.xml.contains("<InPoint>10</InPoint>") && !source.xml.contains("<InPoint>80</InPoint>"));
    }

    #[test]
    fn sync_warns_for_unbound_sequence_and_clip_by_name() {
        let mut bindings = NativeBindings::default();
        let (fragment, _tree, _) = import_into(fixture().as_bytes(), &ImportOptions::default(), None, Some(&mut bindings)).unwrap();
        let mut project = fragment.project;
        let seq_id = fragment.sequences[0];
        let media_id = project.items.values().find(|i| matches!(i.kind, ItemKind::Media(_))).map(|i| i.id).expect("media");
        let rate = project.sequence(seq_id).unwrap().settings.frame_rate;
        let mut unbound = project
            .make_track_item(media_id, filmcraft_project::TrackKind::Video, Tick(300), filmcraft_time::TimeRange::new(Tick::ZERO, Tick(100)), rate)
            .unwrap();
        unbound.name = "ExtraClip".into();
        {
            let seq = project.sequence_mut(seq_id).unwrap();
            seq.video_tracks[0].items.push(unbound);
        }
        let settings = project.sequence(seq_id).unwrap().settings.clone();
        project.new_sequence("AddedSeq", settings, 1, 0, None);

        let mut source = expand(fixture().as_bytes()).unwrap();
        let mut report = Report::default();
        sync_from_project(&mut source, &bindings, &project, &mut report).unwrap();
        let messages: Vec<_> = report.entries.iter().map(|e| e.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("AddedSeq") && m.contains("no Premiere binding")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("ExtraClip") && m.contains("no Premiere binding")), "{messages:?}");
    }

    #[test]
    fn sync_patches_every_media_uid_sharing_one_file_item() {
        // Two Media elements, one path → one FilmCraft item; relink must rewrite both UIDs.
        let xml = r#"<PremiereData>
<Sequence ObjectUID="parent"><Name>Parent</Name><TrackGroups><TrackGroup><Second ObjectRef="group"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="group"><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="one"/><TrackItem ObjectRef="two"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="one"><ClipTrackItem><TrackItem><Start>0</Start><End>100</End></TrackItem><SubClip ObjectRef="sub-a"/></ClipTrackItem></VideoClipTrackItem>
<VideoClipTrackItem ObjectID="two"><ClipTrackItem><TrackItem><Start>200</Start><End>300</End></TrackItem><SubClip ObjectRef="sub-b"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sub-a"><Name>same.mov</Name><Clip ObjectRef="clip-a"/></SubClip>
<SubClip ObjectID="sub-b"><Name>same.mov</Name><Clip ObjectRef="clip-b"/></SubClip>
<VideoClip ObjectID="clip-a"><Clip><InPoint>0</InPoint><OutPoint>100</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="src-a"/></Clip></VideoClip>
<VideoClip ObjectID="clip-b"><Clip><InPoint>0</InPoint><OutPoint>100</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="src-b"/></Clip></VideoClip>
<VideoMediaSource ObjectID="src-a"><MediaSource><Media ObjectURef="media-a"/></MediaSource></VideoMediaSource>
<VideoMediaSource ObjectID="src-b"><MediaSource><Media ObjectURef="media-b"/></MediaSource></VideoMediaSource>
<Media ObjectUID="media-a"><Title>same.mov</Title><FilePath>same.mov</FilePath></Media>
<Media ObjectUID="media-b"><Title>same.mov</Title><FilePath>same.mov</FilePath></Media>
</PremiereData>"#;
        let mut bindings = NativeBindings::default();
        let (fragment, _, _) = import_into(xml.as_bytes(), &ImportOptions::default(), None, Some(&mut bindings)).unwrap();
        let media_id = fragment
            .project
            .items
            .values()
            .find(|i| matches!(&i.kind, ItemKind::Media(m) if matches!(&m.media, MediaRef::File { path } if path == "same.mov")))
            .map(|i| i.id)
            .expect("shared file item");
        let uids = bindings.media.get(&media_id).expect("media binding");
        assert!(uids.contains(&"media-a".into()) && uids.contains(&"media-b".into()), "{uids:?}");
        assert_eq!(uids.len(), 2, "duplicate note_media must not grow the list: {uids:?}");

        let mut project = fragment.project;
        {
            let ItemKind::Media(m) = &mut project.item_mut(media_id).unwrap().kind else { panic!("media") };
            let MediaRef::File { path } = &mut m.media else { panic!("file") };
            *path = "relocated.mov".into();
        }
        let mut source = expand(xml.as_bytes()).unwrap();
        let mut report = Report::default();
        sync_from_project(&mut source, &bindings, &project, &mut report).unwrap();
        assert!(source.xml.contains(r#"ObjectUID="media-a""#) && source.xml.contains(r#"ObjectUID="media-b""#));
        assert_eq!(source.xml.matches("<FilePath>relocated.mov</FilePath>").count(), 2, "{}", source.xml);
        assert!(!source.xml.contains("<FilePath>same.mov</FilePath>"), "{}", source.xml);
    }
}

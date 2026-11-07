//! Project panel bins and items from contributor-authored project XML:
//! `Project/RootProjectItem@ObjectURef` lists `ProjectItemContainer/Items/Item@ObjectURef` entries,
//! each a `BinProjectItem` (whose own container is read the same way) or a `ClipProjectItem`
//! pointing to its `MasterClip`. A subclip's master clip has a media source whose `Content` holds
//! `StartBoundary`/`EndBoundary` in media ticks. Original work, MIT OR Apache-2.0.
use super::{ProjectEntry, SourceBin, SubclipRange, SyntheticMedia, error, generator, media_path, reference, referred, spend, synthetic_code, warning};
use crate::xml::{child, child_text};
use crate::{Report, Result};
use filmcraft_time::Tick;
use roxmltree::{Node, NodeId};
use std::collections::{BTreeMap, HashSet};

const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 64;

struct Walk<'a, 'i, 'r> {
    ids: &'r BTreeMap<&'a str, Node<'a, 'i>>,
    uids: &'r BTreeMap<&'a str, Node<'a, 'i>>,
    base_dir: Option<&'r str>,
    work: &'r mut usize,
    report: &'r mut Report,
    seen: HashSet<NodeId>,
    bins: Vec<SourceBin>,
    out: Vec<ProjectEntry>,
}

/// The project's bins (parents before children) and the panel items the importer understands, in
/// panel order. A project without a root bin has none.
pub(super) fn read<'a, 'i>(
    root: Node<'a, 'i>,
    ids: &BTreeMap<&'a str, Node<'a, 'i>>,
    uids: &BTreeMap<&'a str, Node<'a, 'i>>,
    base_dir: Option<&str>,
    work: &mut usize,
    report: &mut Report,
) -> Result<(Vec<SourceBin>, Vec<ProjectEntry>)> {
    let Some(project) = root.children().find(|n| n.has_tag_name("Project") && n.attribute("ObjectID").is_some()) else { return Ok(Default::default()) };
    let Some(top) = reference(child(project, "RootProjectItem"), ids, uids) else { return Ok(Default::default()) };
    let mut walk = Walk { ids, uids, base_dir, work, report, seen: HashSet::new(), bins: Vec::new(), out: Vec::new() };
    walk.seen.insert(top.id());
    walk.container(top, None, 0)?;
    Ok((walk.bins, walk.out))
}

/// A panel item's name; a smart bin keeps it in its `BinProjectItem` part.
fn name<'a>(item: Node<'a, '_>) -> &'a str {
    let base = child(item, "ProjectItem").or_else(|| child(item, "BinProjectItem").and_then(|b| child(b, "ProjectItem")));
    base.and_then(|p| child_text(p, "Name")).unwrap_or("")
}

fn checked_name(name: &str) -> Result<&str> {
    if name.len() > 4096 { Err(error("project item name is too long")) } else { Ok(name) }
}

fn flag(n: Node<'_, '_>, name: &str) -> bool {
    child_text(n, name).is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

impl<'a, 'i> Walk<'a, 'i, '_> {
    fn container(&mut self, container: Node<'a, 'i>, bin: Option<usize>, depth: usize) -> Result<()> {
        spend(self.work, container)?;
        let items = child(container, "ProjectItemContainer").and_then(|c| child(c, "Items"));
        for entry in items.into_iter().flat_map(|i| i.children()).filter(|n| n.has_tag_name("Item")) {
            let Some(item) = reference(Some(entry), self.ids, self.uids) else { continue };
            // A bin listing itself or an ancestor, or an item listed twice, is read once.
            if !self.seen.insert(item.id()) {
                continue;
            }
            let label = checked_name(name(item))?;
            match item.tag_name().name() {
                "BinProjectItem" if depth + 1 >= MAX_DEPTH => {
                    warning(self.report, format!("bins nested deeper than {MAX_DEPTH} levels were not read"));
                }
                "BinProjectItem" => {
                    if self.bins.len() >= MAX_ENTRIES {
                        return Err(error("too many bins"));
                    }
                    self.bins.push(SourceBin { name: if label.is_empty() { "Bin".into() } else { label.to_string() }, parent: bin });
                    self.container(item, Some(self.bins.len() - 1), depth + 1)?;
                }
                "ClipProjectItem" => {
                    spend(self.work, item)?;
                    if let Some(entry) = self.clip(item, bin)? {
                        if self.out.len() >= MAX_ENTRIES {
                            return Err(error("too many project items"));
                        }
                        self.out.push(entry);
                    }
                }
                "SmartBinProjectItem" | "SearchBinProjectItem" => warning(self.report, format!("search bin \"{label}\" was not imported")),
                other => warning(self.report, format!("project item \"{label}\" ({other}) was not imported")),
            }
        }
        Ok(())
    }

    /// The entry for a clip project item: a sequence, media, a subclip or an adjustment layer.
    fn clip(&mut self, item: Node<'a, 'i>, bin: Option<usize>) -> Result<Option<ProjectEntry>> {
        let Some(master) = reference(child(item, "MasterClip"), self.ids, self.uids) else { return Ok(None) };
        spend(self.work, master)?;
        // Renaming a subclip renames its master clip; the panel item can keep the media's name.
        let label = checked_name(child_text(master, "Name").filter(|n| !n.is_empty()).unwrap_or_else(|| name(item)))?.to_string();
        let mut sources = Vec::new();
        for pointer in child(master, "Clips").into_iter().flat_map(|c| c.children()).filter(Node::is_element) {
            let Some(clip) = referred(Some(pointer), self.ids, "ObjectRef") else { continue };
            spend(self.work, clip)?;
            let out = child(clip, "Clip").and_then(|c| child_text(c, "OutPoint")).and_then(|t| t.parse::<i64>().ok()).filter(|t| (0..=Tick::MAX.0).contains(t));
            if let Some(source) = reference(child(clip, "Clip").and_then(|c| child(c, "Source")), self.ids, self.uids) {
                spend(self.work, source)?;
                sources.push((source, out.map(Tick)));
            }
        }
        let video = sources.iter().any(|(s, _)| s.tag_name().name().starts_with("Video"));
        let mut entry = ProjectEntry {
            name: label,
            bin,
            media_path: None,
            synthetic: None,
            nested_sequence: None,
            adjustment_layer: None,
            duration: None,
            video,
            subclip: None,
        };
        if let Some(sequence) = sources.iter().find_map(|(s, _)| {
            let pointer = child(*s, "SequenceSource").and_then(|n| child(n, "Sequence"));
            reference(pointer, self.ids, self.uids).filter(|n| n.has_tag_name("Sequence"))
        }) {
            entry.nested_sequence = sequence.attribute("ObjectUID").map(str::to_string);
            return Ok(entry.nested_sequence.is_some().then_some(entry));
        }
        // A subclip's boundaries are on the media source that has them; otherwise the first media source.
        let content = |s: Node<'a, 'i>| child(s, "MediaSource").and_then(|m| child(m, "Content"));
        let bounded = sources.iter().find(|(s, _)| content(*s).and_then(|c| child(c, "StartBoundary")).is_some());
        let Some(&(source, out)) = bounded.or_else(|| sources.iter().find(|(s, _)| child(*s, "MediaSource").is_some())) else { return Ok(None) };
        if let Some(content) = content(source)
            && let (Some(start), Some(end)) = (child_text(content, "StartBoundary"), child_text(content, "EndBoundary"))
        {
            let bound = |s: &str| s.parse::<i64>().ok().filter(|t| (0..=Tick::MAX.0).contains(t)).map(Tick);
            let (Some(start), Some(end)) = (bound(start), bound(end)) else {
                warning(self.report, format!("{}: subclip boundaries could not be read; subclip skipped", entry.name));
                return Ok(None);
            };
            if end <= start {
                warning(self.report, format!("{}: subclip ends before it starts; subclip skipped", entry.name));
                return Ok(None);
            }
            entry.subclip = Some(SubclipRange { start, end, hard: flag(content, "BoundariesAreHard") });
        }
        // An adjustment layer's master clip is flagged over generated black video.
        if entry.subclip.is_none() && flag(master, "IsAdjustmentLayer") {
            entry.adjustment_layer = master.attribute("ObjectUID").map(str::to_string);
            entry.duration = out;
            return Ok(entry.adjustment_layer.is_some().then_some(entry));
        }
        let media = reference(child(source, "MediaSource").and_then(|m| child(m, "Media")), self.ids, self.uids).filter(|m| m.has_tag_name("Media"));
        if let Some(media) = media {
            spend(self.work, media)?;
        }
        if let Some(media) = media
            && let Some(code) = synthetic_code(media)
        {
            // Graphic canvases and caption sources are not project panel media.
            if matches!(code.as_str(), "GRFV" | "SCPF") {
                return Ok(None);
            }
            if generator(&code).is_none() {
                warning(self.report, format!("{}: generated media {code} is not supported; not imported", entry.name));
                return Ok(None);
            }
            let object = media.attribute("ObjectUID").or_else(|| media.attribute("ObjectID")).unwrap_or("").to_string();
            entry.synthetic = Some(SyntheticMedia { code, media: object });
            return Ok(Some(entry));
        }
        entry.media_path = media.and_then(|m| media_path(m, self.base_dir));
        if entry.media_path.is_none() {
            let what = if entry.subclip.is_some() { "subclip" } else { "media" };
            warning(self.report, format!("{}: {what} media unresolved; {what} skipped", entry.name));
            return Ok(None);
        }
        Ok(Some(entry))
    }
}

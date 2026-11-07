//! Project panel items from contributor-authored project XML: `Project/RootProjectItem@ObjectURef`
//! lists `ProjectItemContainer/Items/Item@ObjectURef` entries, each a `BinProjectItem` (whose own
//! container is read the same way) or a `ClipProjectItem` pointing to its `MasterClip`. A subclip's
//! master clip has a media source whose `Content` holds `StartBoundary`/`EndBoundary` in media
//! ticks. Original work, MIT OR Apache-2.0.
use super::{ProjectEntry, SubclipRange, SyntheticMedia, error, media_path, reference, referred, spend, synthetic_code, warning};
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
    out: Vec<ProjectEntry>,
}

/// The project panel items the importer understands, in panel order. Bins are read for their
/// contents; a project without a root bin has none.
pub(super) fn read<'a, 'i>(
    root: Node<'a, 'i>,
    ids: &BTreeMap<&'a str, Node<'a, 'i>>,
    uids: &BTreeMap<&'a str, Node<'a, 'i>>,
    base_dir: Option<&str>,
    work: &mut usize,
    report: &mut Report,
) -> Result<Vec<ProjectEntry>> {
    let Some(project) = root.children().find(|n| n.has_tag_name("Project") && n.attribute("ObjectID").is_some()) else { return Ok(Vec::new()) };
    let Some(top) = reference(child(project, "RootProjectItem"), ids, uids) else { return Ok(Vec::new()) };
    let mut walk = Walk { ids, uids, base_dir, work, report, seen: HashSet::new(), out: Vec::new() };
    walk.container(top, 0)?;
    Ok(walk.out)
}

fn name<'a>(item: Node<'a, '_>) -> &'a str {
    child(item, "ProjectItem").and_then(|p| child_text(p, "Name")).unwrap_or("")
}

impl<'a, 'i> Walk<'a, 'i, '_> {
    fn container(&mut self, container: Node<'a, 'i>, depth: usize) -> Result<()> {
        // A bin listing itself or an ancestor is read once.
        if !self.seen.insert(container.id()) {
            return Ok(());
        }
        if depth >= MAX_DEPTH {
            warning(self.report, format!("bins nested deeper than {MAX_DEPTH} levels were not read"));
            return Ok(());
        }
        spend(self.work, container)?;
        let items = child(container, "ProjectItemContainer").and_then(|c| child(c, "Items"));
        for entry in items.into_iter().flat_map(|i| i.children()).filter(|n| n.has_tag_name("Item")) {
            let Some(item) = reference(Some(entry), self.ids, self.uids) else { continue };
            match item.tag_name().name() {
                "BinProjectItem" => self.container(item, depth + 1)?,
                "ClipProjectItem" if self.seen.insert(item.id()) => {
                    spend(self.work, item)?;
                    if let Some(entry) = self.clip(item)? {
                        if self.out.len() >= MAX_ENTRIES {
                            return Err(error("too many project items"));
                        }
                        self.out.push(entry);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// A subclip entry for a clip project item; other master clips are left to the timeline.
    fn clip(&mut self, item: Node<'a, 'i>) -> Result<Option<ProjectEntry>> {
        let Some(master) = reference(child(item, "MasterClip"), self.ids, self.uids) else { return Ok(None) };
        spend(self.work, master)?;
        // Renaming a subclip renames its master clip; the panel item can keep the media's name.
        let label = child_text(master, "Name").filter(|n| !n.is_empty()).unwrap_or_else(|| name(item));
        if label.len() > 4096 {
            return Err(error("project item name is too long"));
        }
        for pointer in child(master, "Clips").into_iter().flat_map(|c| c.children()).filter(Node::is_element) {
            let Some(clip) = referred(Some(pointer), self.ids, "ObjectRef") else { continue };
            spend(self.work, clip)?;
            let Some(source) = reference(child(clip, "Clip").and_then(|c| child(c, "Source")), self.ids, self.uids) else { continue };
            spend(self.work, source)?;
            let video = match source.tag_name().name() {
                "VideoMediaSource" => true,
                "AudioMediaSource" => false,
                _ => continue,
            };
            let Some(content) = child(source, "MediaSource").and_then(|m| child(m, "Content")) else { continue };
            let (Some(start), Some(end)) = (child_text(content, "StartBoundary"), child_text(content, "EndBoundary")) else { continue };
            let bound = |s: &str| s.parse::<i64>().ok().filter(|t| (0..=Tick::MAX.0).contains(t)).map(Tick);
            let (Some(start), Some(end)) = (bound(start), bound(end)) else {
                warning(self.report, format!("{label}: subclip boundaries could not be read; subclip skipped"));
                return Ok(None);
            };
            if end <= start {
                warning(self.report, format!("{label}: subclip ends before it starts; subclip skipped"));
                return Ok(None);
            }
            let hard = child_text(content, "BoundariesAreHard").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
            let media = reference(child(source, "MediaSource").and_then(|m| child(m, "Media")), self.ids, self.uids).filter(|m| m.has_tag_name("Media"));
            if let Some(media) = media {
                spend(self.work, media)?;
            }
            let synthetic = media.and_then(|m| {
                let media = m.attribute("ObjectUID").or_else(|| m.attribute("ObjectID")).unwrap_or("").to_string();
                Some(SyntheticMedia { code: synthetic_code(m)?, media })
            });
            let media_path = media.and_then(|m| media_path(m, self.base_dir));
            if media_path.is_none() && synthetic.is_none() {
                warning(self.report, format!("{label}: subclip media unresolved; subclip skipped"));
                return Ok(None);
            }
            return Ok(Some(ProjectEntry { name: label.to_string(), media_path, synthetic, video, subclip: Some(SubclipRange { start, end, hard }) }));
        }
        Ok(None)
    }
}

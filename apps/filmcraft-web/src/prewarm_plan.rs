//! Which media files of a remote project to fetch first. Pure (no browser types), so it runs
//! natively in `tests/it`.
//!
//! A project holds several sequences, but only the active one is shown when it opens. Its media
//! (the media under the playhead first) is fetched before the media of the other sequences, so the
//! first frame does not wait for bytes it does not need.

use std::collections::{BTreeSet, HashSet};

use filmcraft_project::{ItemId, ItemKind, MediaClip, MediaRef, Project, ProjectView, Sequence};

/// Media paths one plan lists (a project file is not trusted).
const PATHS_MAX: usize = 10_000;
/// Nesting depth followed from a sequence into the sequences it contains.
const DEPTH_MAX: usize = 16;

/// The media file paths of a project in the order to fetch them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Media of the active sequence: under the playhead first, then the rest of its clips.
    pub focus: Vec<String>,
    /// Every other media file of the project.
    pub rest: Vec<String>,
}

/// Collects media paths in first-seen order, each once.
struct Walk<'a> {
    project: &'a Project,
    out: Vec<String>,
    seen: HashSet<String>,
    /// Sequences entered in the current pass (a nest that contains itself ends here).
    visited: BTreeSet<ItemId>,
}

impl<'a> Walk<'a> {
    /// An empty walk over `project`.
    fn new(project: &'a Project) -> Self {
        Self { project, out: Vec::new(), seen: HashSet::new(), visited: BTreeSet::new() }
    }

    /// Add one path unless it is known or the cap is reached.
    fn push(&mut self, path: &str) {
        if self.out.len() < PATHS_MAX && self.seen.insert(path.to_string()) {
            self.out.push(path.to_string());
        }
    }

    /// Add the file of a media item and its proxy.
    fn media(&mut self, clip: &MediaClip) {
        for r in std::iter::once(&clip.media).chain(clip.proxy.as_ref()) {
            if let MediaRef::File { path } = r {
                self.push(path);
            }
        }
    }

    /// Add what a project item shows: its file, a subclip's parent file, or the contents of a
    /// nested sequence.
    fn item(&mut self, id: ItemId, depth: usize) {
        let project = self.project;
        let Some(it) = project.items.get(&id) else { return };
        match &it.kind {
            ItemKind::Media(m) => self.media(m),
            ItemKind::Subclip { parent, .. } => {
                if let Some(ItemKind::Media(m)) = project.items.get(parent).map(|p| &p.kind) {
                    self.media(m);
                }
            }
            ItemKind::Sequence(q) if depth < DEPTH_MAX && self.visited.insert(id) => self.sequence(q, None, depth + 1),
            _ => {}
        }
    }

    /// Add the clips of a sequence; with a playhead (ticks) only the clips that cover it.
    fn sequence(&mut self, q: &Sequence, playhead: Option<i64>, depth: usize) {
        for t in q.all_tracks() {
            for c in &t.items {
                if playhead.is_some_and(|p| !(c.start.0 <= p && p < c.start.0.saturating_add(c.duration.0))) {
                    continue;
                }
                self.item(c.item, depth);
            }
        }
        if playhead.is_none() {
            let merged = q.merged.iter().flat_map(|m| m.video.iter().chain(m.audio.iter()));
            let cameras = q.multicam.iter().flat_map(|m| m.cameras.iter().filter_map(|c| c.source));
            for id in merged.copied().chain(cameras) {
                self.item(id, depth);
            }
        }
    }
}

/// The plan of a decoded project. The active sequence is the one the project view names; without
/// one (or when it is gone) the first sequence, as the engine opens it. Its playhead is the view's,
/// else zero.
pub fn plan(project: &Project, view: Option<&ProjectView>) -> Plan {
    let active = view.and_then(|v| v.active_sequence).filter(|id| project.sequence(*id).is_some()).or_else(|| project.sequences().next().map(|i| i.id));
    let mut w = Walk::new(project);
    if let Some(id) = active
        && let Some(q) = project.sequence(id)
    {
        let head = view.and_then(|v| v.playheads.get(&id)).map_or(0, |t| t.0);
        w.sequence(q, Some(head), 0);
        // a second pass over the whole sequence: nests entered above are entered again
        w.visited.clear();
        w.visited.insert(id);
        w.sequence(q, None, 0);
    }
    let focus = w.out.clone();
    for it in project.items.values() {
        if let ItemKind::Media(m) = &it.kind {
            w.media(m);
        }
    }
    let rest = w.out.split_off(focus.len().min(w.out.len()));
    Plan { focus, rest }
}

/// The plan of a project file's bytes; empty when they are not a project.
pub fn plan_bytes(bytes: &[u8]) -> Plan {
    match filmcraft_format::decode(bytes) {
        Ok(l) => plan(&l.project, l.view.as_ref()),
        Err(_) => Plan::default(),
    }
}

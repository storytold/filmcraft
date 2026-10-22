//! Small, bounded memo of the project dependencies that determine a sequence's picture.
//!
//! Retained snapshots keep shared sequence identities alive while they are compared. A raw
//! pointer is never used as the frame-cache revision. Ordinary source opens do not invalidate
//! the memo; explicit pool changes do. No project serialization or whole-project item walk.
use filmcraft_engine::MediaPool;
use filmcraft_project::{ItemId, ItemKind, Project};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Weak};

const ENTRIES: usize = 8;
const NODES: usize = 256;
const EDGES: usize = 1024;
const DEPTH: usize = 32;
const EFFECTS: usize = 4096;
const NAME_BYTES: usize = 4096;

struct Entry {
    item: ItemId,
    project: Arc<Project>,
    checked_revision: u64,
    pool: Weak<MediaPool>,
    media_generation: u64,
    token: u64,
}

struct Fallback {
    project: Weak<Project>,
    checked_revision: u64,
    pool: Weak<MediaPool>,
    media_generation: u64,
    token: u64,
}

#[derive(Default)]
pub(crate) struct ThumbnailRevisions {
    entries: VecDeque<Entry>,
    fallback: Option<Fallback>,
    next: u64,
}

impl ThumbnailRevisions {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.fallback = None;
    }

    pub fn revision(&mut self, project: &Arc<Project>, revision: u64, pool: &Arc<MediaPool>, item: ItemId) -> u64 {
        let generation = pool.content_generation();
        let pool_ref = Arc::downgrade(pool);
        let index = self.entries.iter().position(|e| e.item == item);
        if index.is_none() && self.entries.len() >= ENTRIES {
            // Do not churn admitted cards when more than ENTRIES are visible. Overflow
            // keeps the old conservative project revision behavior, stable while idle.
            // Weak references preserve identity without retaining another project snapshot.
            let project_ref = Arc::downgrade(project);
            if let Some(old) = &self.fallback
                && Weak::ptr_eq(&old.project, &project_ref)
                && old.checked_revision == revision
                && Weak::ptr_eq(&old.pool, &pool_ref)
                && old.media_generation == generation
            {
                return old.token;
            }
            self.next = self.next.wrapping_add(1);
            self.fallback = Some(Fallback { project: project_ref, checked_revision: revision, pool: pool_ref, media_generation: generation, token: self.next });
            return self.next;
        }
        let previous = index.and_then(|i| self.entries.remove(i));
        let reusable = previous.as_ref().is_some_and(|old| {
            if !Weak::ptr_eq(&old.pool, &pool_ref) || old.media_generation != generation {
                return false;
            }
            if Arc::ptr_eq(&old.project, project) {
                return old.checked_revision == revision;
            }
            dependencies_unchanged(&old.project, project, item)
        });
        let token = if reusable {
            previous.as_ref().map_or(self.next, |e| e.token)
        } else {
            // Like the engine revision, this is an opaque process-local u64 identity. It does
            // not come from untrusted project numbers and is never a pointer address.
            self.next = self.next.wrapping_add(1);
            self.next
        };
        self.entries.push_back(Entry { item, project: project.clone(), checked_revision: revision, pool: pool_ref, media_generation: generation, token });
        token
    }
}

pub(crate) fn dependencies_unchanged(old: &Project, new: &Project, root: ItemId) -> bool {
    // These assets live outside individual sequence nodes. Conservatively invalidate on a
    // change (and on oversized libraries); their content is not hashed into every thumbnail.
    if old.luts.len() > NODES
        || new.luts.len() > NODES
        || old.source_graphics.len() > NODES
        || new.source_graphics.len() > NODES
        || old.settings != new.settings
        || old.luts != new.luts
        || old.source_graphics != new.source_graphics
    {
        return false;
    }
    let mut stack = vec![(root, 0, false)];
    let mut active = HashSet::new();
    let mut done = HashSet::new();
    let mut edges = 0usize;
    let mut effects = 0usize;
    while let Some((id, depth, leave)) = stack.pop() {
        if leave {
            active.remove(&id);
            done.insert(id);
            continue;
        }
        if active.contains(&id) {
            return false;
        }
        if done.contains(&id) {
            continue;
        }
        if depth > DEPTH || active.len().saturating_add(done.len()) >= NODES {
            return false;
        }
        let (Some(a), Some(b)) = (old.item(id), new.item(id)) else {
            return false;
        };
        // Names can be burned into the picture even though they live outside Sequence data.
        if a.name.len() > NAME_BYTES || b.name.len() > NAME_BYTES || a.name != b.name {
            return false;
        }
        active.insert(id);
        stack.push((id, depth, true));
        match (&a.kind, &b.kind) {
            (ItemKind::Sequence(a), ItemKind::Sequence(b)) => {
                if !Arc::ptr_eq(a, b) {
                    return false;
                }
                for track in a.video_tracks.iter().chain(&a.audio_tracks).chain(&a.submix_tracks) {
                    edges = edges.saturating_add(1).saturating_add(track.items.len());
                    if edges > EDGES {
                        return false;
                    }
                    for clip in &track.items {
                        effects = effects.saturating_add(clip.effects.len());
                        // Burn-in's sequence-name lookup may resolve a shared Sequence through
                        // another project item, outside this graph. Keep it conservative across
                        // snapshots rather than scanning the whole project for aliases.
                        if effects > EFFECTS || clip.effects.iter().any(|e| e.effect == "metadata_burnin") {
                            return false;
                        }
                        stack.push((clip.item, depth + 1, false));
                    }
                }
                if let Some(merged) = &a.merged {
                    edges = edges.saturating_add(merged.audio.len()).saturating_add(1);
                    if edges > EDGES {
                        return false;
                    }
                    for id in merged.video.iter().chain(&merged.audio) {
                        stack.push((*id, depth + 1, false));
                    }
                }
            }
            _ => {
                if a != b {
                    return false;
                }
                if let ItemKind::Subclip { parent, .. } = &a.kind {
                    edges = edges.saturating_add(1);
                    if edges > EDGES {
                        return false;
                    }
                    stack.push((*parent, depth + 1, false));
                }
            }
        }
    }
    true
}

//! Bounded Rate Stretch previews shared by pointer updates and the next draw.
//! Keep failures as well as successful edits: a blocked pointer request can alternate with the
//! unchanged geometry drawn at delta zero. Project edits, sequence changes, marker ripple and link
//! selection invalidate the inputs; ending a stretch or resetting media caches releases the snapshots.

use std::collections::VecDeque;
use std::sync::{Arc, Weak};

use filmcraft_edit::Edge;
use filmcraft_project::{ClipId, ItemId, Project, Sequence};
use filmcraft_time::Tick;

use crate::FilmcraftApp;

type Preview = Result<Arc<Sequence>, Arc<str>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    revision: u64,
    sequence: Option<ItemId>,
    linked_selection: bool,
    ripple_sequence_markers: bool,
    clip: ClipId,
    edge: Edge,
    delta: Tick,
    ripple: bool,
    linked: bool,
}

#[derive(Default)]
pub(super) struct Cache {
    project: Weak<Project>,
    entries: VecDeque<(Key, Preview)>,
}

impl Cache {
    /// Release snapshots when their drag ends or is cancelled.
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.project = Weak::new();
    }

    /// Retain the two most recently used requests, including a clamped edit's accepted delta.
    fn remember(&mut self, key: Key, preview: Preview) {
        self.entries.retain(|(old, _)| *old != key);
        if self.entries.len() >= 2 {
            self.entries.pop_front();
        }
        self.entries.push_back((key, preview));
    }
}

/// Return the same snapshot for unchanged stretch inputs without cloning/editing the sequence again.
pub(super) fn cached(app: &FilmcraftApp, clip: ClipId, edge: Edge, delta: Tick, ripple: bool, linked: bool) -> Preview {
    let project = Arc::downgrade(&app.session.project);
    let key = Key {
        revision: app.session.revision,
        sequence: app.session.state.active_sequence,
        linked_selection: app.session.state.linked_selection,
        ripple_sequence_markers: app.session.state.ripple_sequence_markers,
        clip,
        edge,
        delta,
        ripple,
        linked,
    };
    {
        let mut cache = app.tl.stretch_preview.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, preview)) = cache.entries.iter().find(|(old, _)| cache.project.ptr_eq(&project) && *old == key) {
            let preview = preview.clone();
            cache.remember(key, preview.clone());
            return preview;
        }
    }
    let preview = filmcraft_engine::commands::rate_stretch_preview_linked(&app.session, clip, edge, delta, ripple, linked)
        .map(Arc::new)
        .map_err(|error| Arc::<str>::from(error.to_string()));
    // The pointer can request beyond a duration bound. Drawing uses the accepted delta, which
    // describes this same edit and should not cause a second calculation at that bound.
    let accepted = preview.as_ref().ok().and_then(|work| {
        let old = app.session.active_sequence()?.find_item(clip)?.1;
        let new = work.find_item(clip)?.1;
        let delta = if edge == Edge::In { new.start - old.start } else { new.duration - old.duration };
        Some(Key { delta, ..key })
    });
    let mut cache = app.tl.stretch_preview.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cache.project.ptr_eq(&project) {
        cache.clear();
        cache.project = project;
    }
    cache.remember(key, preview.clone());
    if let Some(accepted) = accepted.filter(|accepted| *accepted != key) {
        cache.remember(accepted, preview.clone());
    }
    preview
}

#[cfg(test)]
mod tests;

use super::item::{ItemKind, MediaClip, ProjectItem, retime};
use super::sequence::{conformed_size, sane_par};
use super::track::Marker;
use super::{BTreeMap, ClipId, EffectInstance, FrameRate, ItemId, ParamValue, Project, Tick, TimeRange, Vec2, effect};

/// Resolve NaN "auto" point defaults (frame centre / source centre) in an effect instance.
pub fn resolve_auto_points(e: &mut EffectInstance, frame: (u32, u32), source: (u32, u32)) {
    resolve_auto_points_sized(e, frame, Some(source));
}

/// [`resolve_auto_points`] for a source whose size may not be known yet (`None`): the points
/// measured in the source (`anchor`) then stay "auto".
fn resolve_auto_points_sized(e: &mut EffectInstance, frame: (u32, u32), source: Option<(u32, u32)>) {
    for (k, p) in e.params.iter_mut() {
        if let ParamValue::Vec2(v) = &mut p.value {
            let Some((w, h)) = (if k == "anchor" { source } else { Some(frame) }) else { continue };
            if let Some((fx, fy)) = effect::auto_point(&e.effect, k) {
                if v.x.is_nan() {
                    v.x = w as f64 * fx;
                }
                if v.y.is_nan() {
                    v.y = h as f64 * fy;
                }
            }
            if v.x.is_nan() {
                v.x = w as f64 / 2.0;
            }
            if v.y.is_nan() {
                v.y = if k == "end" { h as f64 } else { h as f64 / 2.0 };
            }
        }
    }
    let _ = Vec2::ZERO;
}

impl Project {
    /// Lazy upgrade for graphic layers written before they had stable uids: assign a unique
    /// [`crate::graphic_design::LayerExtra::uid`] to every uid-less or duplicated graphic layer
    /// in sequences and source graphics.
    ///
    /// Called when a project file is opened, before undo history starts, so every later undo
    /// snapshot carries the same identity. The field is already optional in the file schema, so
    /// this changes no schema version and older builds still read the file. Returns whether any
    /// layer changed.
    pub fn ensure_graphic_layer_uids(&mut self) -> bool {
        let mut changed = false;
        for item in self.items.values_mut() {
            if let ItemKind::Sequence(seq) = &mut item.kind {
                let seq = std::sync::Arc::make_mut(seq);
                for track in seq.video_tracks.iter_mut().chain(seq.audio_tracks.iter_mut()) {
                    for it in &mut track.items {
                        changed |= crate::graphic::ensure_layer_uids(&mut it.effects);
                    }
                }
            }
        }
        for graphic in self.source_graphics.values_mut() {
            changed |= crate::graphic::ensure_layer_uids(&mut graphic.layers);
        }
        changed
    }

    /// Available picture and sound, resolving a subclip to its parent media. Missing parents,
    /// cycles and overly deep subclip chains carry neither stream.
    pub fn source_streams(&self, item: ItemId) -> (bool, bool) {
        let Some(pi) = self.item(item) else { return (false, false) };
        match &pi.kind {
            ItemKind::Subclip { .. } => self.resolve_media(item).map_or((false, false), |(_, m, _)| (m.info.has_video(), m.info.has_audio())),
            _ => (pi.has_video(), pi.has_audio()),
        }
    }

    /// Resolve media through at most sixteen items, preserving the outermost subclip range.
    /// Missing parents, cycles and longer chains are unavailable media.
    pub fn resolve_media(&self, item: ItemId) -> Option<(ItemId, &MediaClip, Option<TimeRange>)> {
        let mut id = item;
        let mut range = None;
        for _ in 0..16 {
            match &self.item(id)?.kind {
                ItemKind::Media(media) => return Some((id, media, range)),
                ItemKind::Subclip { parent, range: span, .. } => {
                    range.get_or_insert(*span);
                    id = *parent;
                }
                _ => return None,
            }
        }
        None
    }

    /// Interpret Footage changed the rate `media` plays at from `from` to `to` (each the assumed
    /// rate, or the file's): every media time pointing into it is scaled by `from / to` so it keeps
    /// its frame. That is its marks and markers, its subclips' ranges, and in every sequence the
    /// clips that play it (source In, frame hold, clip markers, effect and mask keyframes). Clips
    /// keep their place and length on the timeline; one that would now run past the media's end
    /// is shortened to end with it.
    pub fn retime_media(&mut self, media: ItemId, from: FrameRate, to: FrameRate) {
        if !from.is_usable() || !to.is_usable() || FrameRate::new(from.num, from.den) == FrameRate::new(to.num, to.den) {
            return;
        }
        let scale = |t: &mut Tick| *t = retime(*t, from, to);
        let scale_markers = |markers: &mut Vec<Marker>| {
            for mk in markers {
                scale(&mut mk.start);
                scale(&mut mk.duration);
            }
        };
        // the media and every item that plays it (subclips, subclips of subclips)
        let playing: std::collections::BTreeSet<ItemId> =
            self.items.keys().copied().filter(|id| self.resolve_media(*id).is_some_and(|(m, _, _)| m == media)).collect();
        let limit = match self.item_mut(media).and_then(|i| i.as_media_mut()) {
            Some(m) => {
                m.mark_in.as_mut().map(scale);
                m.mark_out.as_mut().map(scale);
                scale_markers(&mut m.markers);
                m.duration()
            }
            None => return,
        };
        if let Some(item) = self.item_mut(media) {
            let s = &mut item.split;
            for t in [&mut s.video_in, &mut s.video_out, &mut s.audio_in, &mut s.audio_out].into_iter().flatten() {
                scale(t);
            }
        }
        if let Some(t) = self.transcripts.get_mut(&media) {
            for w in &mut std::sync::Arc::make_mut(t).words {
                scale(&mut w.start);
                scale(&mut w.end);
            }
        }
        for id in &playing {
            if let Some(ItemKind::Subclip { range, .. }) = self.item_mut(*id).map(|i| &mut i.kind) {
                scale(&mut range.start);
                scale(&mut range.duration);
            }
        }
        let sequences: Vec<ItemId> = self.items.iter().filter(|(_, i)| matches!(i.kind, ItemKind::Sequence(_))).map(|(id, _)| *id).collect();
        for sid in sequences {
            let touches =
                self.sequence(sid).is_some_and(|q| q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.items).any(|ti| playing.contains(&ti.item)));
            if !touches {
                continue;
            }
            let Some(seq) = self.sequence_mut(sid) else { continue };
            for ti in seq.video_tracks.iter_mut().chain(seq.audio_tracks.iter_mut()).flat_map(|t| t.items.iter_mut()) {
                if !playing.contains(&ti.item) {
                    continue;
                }
                scale(&mut ti.source_in);
                ti.frame_hold.as_mut().map(scale);
                scale_markers(&mut ti.markers);
                for fx in &mut ti.effects {
                    let params =
                        fx.params.values_mut().chain(fx.masks.iter_mut().flat_map(|m| [&mut m.path, &mut m.feather, &mut m.opacity, &mut m.expansion]));
                    for p in params {
                        for k in &mut p.keyframes {
                            scale(&mut k.time);
                        }
                    }
                }
                let speed = ti.speed.abs();
                if ti.frame_hold.is_none() && speed > 0.0 && ti.source_out() > limit && limit > ti.source_in {
                    ti.duration = Tick((((limit - ti.source_in).0 as f64) / speed).floor() as i64);
                }
            }
        }
    }

    /// Size in pixels of what an item shows: a media clip's picture, a sequence's frame, an
    /// adjustment layer or graphic; a subclip has the size of its parent. `None` without
    /// picture. This is the size the renderer centres a clip's "auto" anchor in.
    pub fn source_size(&self, item: ItemId) -> Option<(u32, u32)> {
        let mut id = item;
        // subclips of subclips: bounded, a damaged project could point a subclip at itself
        for _ in 0..16 {
            return match &self.item(id)?.kind {
                ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)),
                ItemKind::Sequence(q) => Some((q.settings.width, q.settings.height)),
                ItemKind::AdjustmentLayer { width, height, .. } | ItemKind::Graphic { width, height, .. } => Some((*width, *height)),
                ItemKind::Subclip { parent, .. } => {
                    id = *parent;
                    continue;
                }
            };
        }
        None
    }

    /// Pixel aspect ratio of what an item shows, in lowest terms: a media clip's
    /// ([`MediaClip::pixel_aspect`], Interpret Footage first), a nested sequence's, a subclip its
    /// parent's. `None` for items drawn in the pixels of whatever sequence they are in (graphics,
    /// adjustment layers) and for an item that doesn't exist.
    pub fn source_par(&self, item: ItemId) -> Option<(u32, u32)> {
        let mut id = item;
        // subclips of subclips: bounded, as in `source_size`
        for _ in 0..16 {
            return match &self.item(id)?.kind {
                ItemKind::Media(m) => Some(m.pixel_aspect()),
                ItemKind::Sequence(q) => Some(sane_par(q.settings.par)),
                ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => None,
                ItemKind::Subclip { parent, .. } => {
                    id = *parent;
                    continue;
                }
            };
        }
        None
    }

    /// [`Project::source_size`] in the pixels of a frame whose pixel aspect is `frame_par`
    /// ([`conformed_size`]): what Scale to Frame Size, Set to Frame Size and Fit / Fill fit into
    /// the frame. Items without a pixel aspect of their own take the frame's.
    pub fn conformed_source_size(&self, item: ItemId, frame_par: (u32, u32)) -> Option<(f64, f64)> {
        let size = self.source_size(item)?;
        Some(conformed_size(size, self.source_par(item).unwrap_or(frame_par), frame_par))
    }

    /// Resolve the "auto" (NaN) points of the clips in the sequences `sequences` selects, for
    /// clips that did not go through a placing command (an interchange import builds them with
    /// the effect defaults). Points measured in the frame use the sequence's size. `anchor` is
    /// measured in the clip's source ([`Project::source_size`], a subclip through its parent) and
    /// is resolved only when `size_is_final` says so for that source item (the media item for a
    /// subclip); otherwise it stays "auto", which the renderer centres in whatever size the media
    /// turns out to have. Points that are not NaN are never touched, so this can run again when
    /// more sizes are known.
    pub fn resolve_placed_auto_points(&mut self, sequences: impl Fn(ItemId) -> bool, size_is_final: impl Fn(&ProjectItem) -> bool) {
        let seq_ids: Vec<ItemId> = self.items.values().filter(|i| matches!(i.kind, ItemKind::Sequence(_)) && sequences(i.id)).map(|i| i.id).collect();
        for seq_id in seq_ids {
            let Some(seq) = self.sequence(seq_id) else { continue };
            // the source size of each clip that has an unresolved point, looked up before the edit
            let sources: BTreeMap<ClipId, Option<(u32, u32)>> = seq
                .all_tracks()
                .flat_map(|t| t.items.iter())
                .filter(|c| c.effects.iter().any(has_auto_point))
                .map(|c| (c.id, self.final_source_size(c.item, &size_is_final)))
                .collect();
            if sources.is_empty() {
                continue;
            }
            let Some(seq) = self.sequence_mut(seq_id) else { continue };
            let frame = (seq.settings.width, seq.settings.height);
            for clip in seq.all_tracks_mut().flat_map(|t| t.items.iter_mut()) {
                let Some(source) = sources.get(&clip.id) else { continue };
                for e in &mut clip.effects {
                    resolve_auto_points_sized(e, frame, *source);
                }
            }
        }
    }

    /// [`Project::source_size`] of `item` when the item that carries the size (the media item of
    /// a subclip) passes `size_is_final`.
    fn final_source_size(&self, item: ItemId, size_is_final: &impl Fn(&ProjectItem) -> bool) -> Option<(u32, u32)> {
        let mut id = item;
        for _ in 0..16 {
            let it = self.item(id)?;
            match &it.kind {
                ItemKind::Subclip { parent, .. } => id = *parent,
                _ => return if size_is_final(it) { self.source_size(id) } else { None },
            }
        }
        None
    }
}

fn has_auto_point(e: &EffectInstance) -> bool {
    e.params.values().any(|p| matches!(&p.value, ParamValue::Vec2(v) if v.x.is_nan() || v.y.is_nan()))
}

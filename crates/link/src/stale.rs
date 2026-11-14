//! An export must not write an old EffectCraft result in silence.
//!
//! `file.exportMedia` and `export.quick` read each linked item's result movie, the last result
//! EffectCraft rendered, not the live frames the monitor shows. After an edit in EffectCraft that
//! movie is old until the next `link.result` lands, and it stays old when that render fails.
//! [`check_export`] is the engine's [`filmcraft_engine::export_tools::set_link_export_check`] hook:
//! it refuses such an export and names the clip. `allowStaleLink: true` exports the older result.

use std::collections::HashSet;

use filmcraft_engine::Session;
use filmcraft_engine::project::{ItemId, Project};
use filmcraft_engine::time::{Tick, TimeRange};

use crate::link_ops::record_of;

/// The command parameter that exports the older result on purpose.
pub const ALLOW_PARAM: &str = "allowStaleLink";

/// How deep nested sequences are followed.
const MAX_DEPTH: usize = 16;

/// The hook: the process link manager knows which results are pending. No manager, no check.
pub fn check_export(session: &Session, sequence: ItemId, range: TimeRange) -> Result<(), String> {
    let Some(manager) = crate::manager::manager() else { return Ok(()) };
    check_with(&session.project, sequence, range, &|link_id| manager.pending_result(link_id))
}

/// The first video clip of `sequence` inside `range` whose link has a pending result, as an error.
/// `pending` answers `(comp epoch, result epoch)` for such a link. A nested sequence that overlaps
/// `range` is searched whole: its clips are not mapped through speed and trims.
pub fn check_with(project: &Project, sequence: ItemId, range: TimeRange, pending: &dyn Fn(&str) -> Option<(u64, u64)>) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut todo = vec![(sequence, Some(range), 0usize)];
    while let Some((id, window, depth)) = todo.pop() {
        if depth > MAX_DEPTH || !seen.insert(id) {
            continue;
        }
        let Some(found) = project.sequence(id) else { continue };
        for clip in found.video_tracks.iter().flat_map(|track| &track.items) {
            let end = |start: Tick, len: Tick| Tick(start.0.saturating_add(len.0));
            if window.is_some_and(|window| clip.start >= end(window.start, window.duration) || end(clip.start, clip.duration) <= window.start) {
                continue;
            }
            if project.sequence(clip.item).is_some() {
                todo.push((clip.item, None, depth + 1));
                continue;
            }
            let Ok(Some(record)) = record_of(project, clip.item) else { continue };
            if let Some((comp, result)) = pending(&record.link_id) {
                return Err(format!(
                    "the EffectCraft result of clip \"{}\" is older than its comp (edit {comp}, result {result}): EffectCraft is still rendering it, or the render failed. Export again when the new result lands, or pass `{ALLOW_PARAM}: true` to export the older result",
                    clip.name
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use filmcraft_engine::project::{SequenceSettings, TrackKind};
    use filmcraft_engine::time::FrameRate;

    use super::*;
    use crate::fixtures::linked_offline;

    const LINK: &str = "22222222-2222-4222-8222-222222222222";

    /// A sequence with the linked item on V1 at frames 24..48, and a second sequence that nests it.
    fn edit() -> (Session, ItemId, ItemId) {
        let rate = FrameRate::FPS_24;
        let mut session = Session::default();
        let item = linked_offline(&mut session, LINK, "/tmp/fc-stale-result.mov");
        let inner = session.project_mut().new_sequence("Inner", SequenceSettings::default(), 1, 0, None);
        let mut clip =
            session.project_mut().make_track_item(item, TrackKind::Video, rate.tick_of(24), TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
        clip.name = "Shot 4".to_string();
        session.project_mut().sequence_mut(inner).unwrap().video_tracks[0].items.push(clip);
        let outer = session.project_mut().new_sequence("Outer", SequenceSettings::default(), 1, 0, None);
        let span = session.project.item(inner).unwrap().duration();
        let nested = session.project_mut().make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, span), rate).unwrap();
        session.project_mut().sequence_mut(outer).unwrap().video_tracks[0].items.push(nested);
        (session, inner, outer)
    }

    #[test]
    fn a_pending_result_inside_the_range_names_the_clip_and_the_way_out() {
        let (session, inner, outer) = edit();
        let rate = FrameRate::FPS_24;
        let pending = |link: &str| (link == LINK).then_some((3, 2));
        let err = check_with(&session.project, inner, TimeRange::new(Tick::ZERO, rate.tick_of(48)), &pending).unwrap_err();
        assert!(err.contains("\"Shot 4\"") && err.contains("allowStaleLink: true") && err.contains("edit 3, result 2"), "{err}");
        // Through a nested sequence too.
        assert!(check_with(&session.project, outer, TimeRange::new(Tick::ZERO, rate.tick_of(48)), &pending).is_err());
        // Outside the range, or with a current result, nothing is refused.
        assert_eq!(check_with(&session.project, inner, TimeRange::new(Tick::ZERO, rate.tick_of(24)), &pending), Ok(()));
        assert_eq!(check_with(&session.project, inner, TimeRange::new(Tick::ZERO, rate.tick_of(48)), &|_| None), Ok(()));
    }

    #[test]
    fn a_sequence_that_nests_itself_ends() {
        let (mut session, inner, _) = edit();
        let rate = FrameRate::FPS_24;
        let back = session.project_mut().make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
        session.project_mut().sequence_mut(inner).unwrap().video_tracks[0].items.insert(0, back);
        assert_eq!(check_with(&session.project, inner, TimeRange::new(Tick::ZERO, rate.tick_of(12)), &|_| None), Ok(()));
    }
}

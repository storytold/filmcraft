//! Selecting a gap in the Timeline and closing it, as in Premiere: click the empty space between
//! two clips (or press D, Select Clip at Playhead, with the playhead in a gap) and the gap is
//! outlined; Delete / Ripple Delete then closes it, pulling the clips after it back.
//!
//! | Id | | |
//! |---|---|---|
//! | `timeline.selectGap` | Select Gap | `{track?, time?}`: the gap at `time` (default: the playhead) on `track` (default: every targeted track) |
//! | `edit.clear` / `edit.rippleDelete` | Clear / Ripple Delete | with a gap selected and no clips, close the gap |
//!
//! A gap selection belongs to the sequence it was made in and is dropped as soon as clips,
//! captions or edit points are selected, or an edit changes the gap.

use filmcraft_edit as edit;
use filmcraft_project::{ItemId, Sequence, TrackId};
use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// A selected gap: the empty time `start..end` on one track of one sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedGap {
    pub sequence: ItemId,
    pub track: TrackId,
    pub start: Tick,
    pub end: Tick,
}

impl SelectedGap {
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end)
    }
    fn to_json(self, seq: &Sequence) -> Value {
        json!({"track": self.track.0, "trackName": edit::track_label(seq, self.track), "start": self.start.0, "end": self.end.0})
    }
}

/// What D (Select Clip at Playhead) and a click look at for time `t`: just past it, so a cut
/// stored a tick or two before the playhead's frame boundary counts as before the playhead.
pub(crate) fn probe(s: &Session, t: Tick) -> Tick {
    t + s.sequence_rate().boundary_slack()
}

/// The selected gaps that still stand: in the active sequence, with no clips, captions or edit
/// points selected, and each still exactly the gap on its track.
pub fn selected_gaps(s: &Session) -> Vec<SelectedGap> {
    if !s.state.selection.is_empty() || !s.state.caption_selection.is_empty() || !s.state.edit_points.is_empty() {
        return Vec::new();
    }
    let (Some(sid), Some(seq)) = (s.state.active_sequence, s.active_sequence()) else { return Vec::new() };
    s.state
        .gap_selection
        .iter()
        .filter(|g| g.sequence == sid)
        .filter(|g| seq.track(g.track).and_then(|tr| edit::gap_at(tr, g.start)).is_some_and(|r| r == g.range()))
        .copied()
        .collect()
}

/// The gaps at `t` on `tracks` (unlocked tracks only: a locked track's contents can't be
/// selected).
fn gaps_at(seq: &Sequence, sid: ItemId, tracks: &[TrackId], t: Tick) -> Vec<SelectedGap> {
    seq.all_tracks()
        .filter(|tr| tracks.contains(&tr.id) && !tr.locked)
        .filter_map(|tr| edit::gap_at(tr, t).map(|r| SelectedGap { sequence: sid, track: tr.id, start: r.start, end: r.end() }))
        .collect()
}

/// Make `gaps` the selection (clips, captions and edit points are deselected).
fn set_gap_selection(s: &mut Session, gaps: Vec<SelectedGap>) -> Value {
    s.state.selection.clear();
    s.state.caption_selection.clear();
    s.state.edit_points.clear();
    s.state.trim_shift = Default::default();
    s.state.gap_selection = gaps;
    gaps_json(s)
}

/// The selected gaps as JSON: `{"gaps": [{track, trackName, start, end}]}`.
pub fn gaps_json(s: &Session) -> Value {
    let Some(seq) = s.active_sequence() else { return json!({"gaps": []}) };
    json!({"gaps": selected_gaps(s).iter().map(|g| g.to_json(seq)).collect::<Vec<_>>()})
}

/// Select Clip at Playhead found no clip: select the gaps under the playhead on the targeted
/// tracks instead (none: the selection is cleared).
pub(crate) fn select_gaps_at_playhead(s: &mut Session) -> Value {
    let t = probe(s, s.playhead());
    let tg = s.targeting().targeted;
    let gaps = match (s.state.active_sequence, s.active_sequence()) {
        (Some(sid), Some(seq)) => gaps_at(seq, sid, &tg, t),
        _ => Vec::new(),
    };
    set_gap_selection(s, gaps)
}

/// `timeline.selectGap`: the gap at `time` on `track` (or on every targeted track). Where there
/// is no gap (on a clip, after the last clip) the selection is cleared, as a click on empty
/// space in the Timeline does.
pub(crate) fn select_gap(s: &mut Session, p: &Value) -> Result<Value> {
    let track = crate::commands::track_p(s, p, "track", "timeline.selectGap")?;
    let t = match crate::commands::time_p(s, p, "") {
        Some(t) => t,
        None => probe(s, s.playhead()),
    };
    let tracks = match track {
        Some(t) => vec![t],
        None => s.targeting().targeted,
    };
    let sid = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let gaps = gaps_at(seq, sid, &tracks, t);
    Ok(set_gap_selection(s, gaps))
}

/// Delete or Ripple Delete with gaps selected: close them. With several tracks' gaps selected
/// (D on picture and sound), the time they share is closed on all of them at once.
pub(crate) fn close_selected(s: &mut Session) -> Result<Value> {
    let gaps = selected_gaps(s);
    let start = gaps.iter().map(|g| g.start).max().ok_or_else(|| EngineError::Other("no gap selected".into()))?;
    let end = gaps.iter().map(|g| g.end).min().unwrap_or(start);
    if end <= start {
        return Err(EngineError::Other("the selected gaps don't overlap: select the gap on one track".into()));
    }
    let range = TimeRange::from_bounds(start, end);
    let tracks: Vec<TrackId> = gaps.iter().map(|g| g.track).collect();
    s.edit_sequence("Ripple Delete", |q, _, st| {
        edit::close_gap_range(q, &tracks, range)?;
        if st.ripple_sequence_markers {
            crate::sequence_tools::ripple_markers(&mut q.markers, range.end(), -range.duration);
        }
        st.gap_selection.clear();
        Ok(())
    })?;
    Ok(json!({"closed": {"start": range.start.0, "duration": range.duration.0, "tracks": tracks.iter().map(|t| t.0).collect::<Vec<_>>()}}))
}

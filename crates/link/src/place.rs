//! Capture and restore the editor place for a Craft Link hand-off.
//!
//! The playhead tick is the value FilmCraft writes back. Seconds are for EffectCraft.
//! Selection travels as time ranges because clip ids do not survive the other app.

use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, Sequence, TrackId};
use filmcraft_engine::time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};

/// Timeline zoom and scroll for the UI to apply after [`Place::restore`].
///
/// `zoom` is timeline pixels per second. The next task writes it to `target_pps`
/// (`crates/ui-egui/src/state.rs`). `scroll_s` is the left edge in seconds. The next
/// task writes it to `target_scroll`. Capture takes this view as an argument and does
/// not read egui. `ui.inspect` (`crates/ui-egui/src/control.rs`) reports `playhead` as
/// `app.session.playhead().0` and `selection` as clip id numbers. Those ids are what
/// capture converts into time ranges.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlaceView {
    pub zoom: f64,
    pub scroll_s: f64,
}

/// One selected span. The contract maps by time, not by clip id.
///
/// `start_s` and `end_s` come from the clip's timeline ticks (`TrackItem::start` and
/// `TrackItem::end`). `track_index` is the position in [`Sequence::all_tracks`]: video
/// tracks, then audio tracks. That is the same order OpenTimelineIO uses.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SelectionRange {
    pub start_s: f64,
    pub end_s: f64,
    pub track_index: u32,
}

/// The editor place at a hand-off.
///
/// `playhead` is sequence ticks. `restore` passes it to [`Session::set_playhead`].
/// `seconds` is that tick on the active sequence frame rate. EffectCraft reads `seconds`.
/// Restore does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub playhead: Tick,
    pub seconds: f64,
    pub selection: Vec<SelectionRange>,
    pub view: PlaceView,
}

/// Why a place could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceError {
    /// No open sequence. A zero playhead would look like a real place, so capture fails.
    NoSequence,
    /// The track was not in the sequence, or its index does not fit in `u32`.
    TrackIndex,
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaceError::NoSequence => f.write_str("no active sequence"),
            PlaceError::TrackIndex => f.write_str("track index is missing or does not fit in u32"),
        }
    }
}

impl std::error::Error for PlaceError {}

impl Place {
    /// Read the live playhead and the selected clips.
    ///
    /// Fails when the session has no active sequence. `view` is stored as given.
    pub fn capture(session: &Session, view: PlaceView) -> Result<Self, PlaceError> {
        let sequence = session.active_sequence().ok_or(PlaceError::NoSequence)?;
        let rate = sequence.settings.frame_rate;
        let playhead = session.playhead();
        let seconds = seconds_from(rate, playhead);
        let selection = ranges_for(sequence, &session.state.selection)?;
        Ok(Self { playhead, seconds, selection, view })
    }

    /// Write the playhead and the selection. Return the view for the UI.
    ///
    /// Playhead ticks go to [`Session::set_playhead`]. The seconds field is ignored.
    /// Every clip whose start, end, and track match a range is selected. A range that
    /// matches two clips selects both. A range that matches none selects nothing for
    /// that range. This does not create clips and does not read egui.
    pub fn restore(&self, session: &mut Session) -> Result<PlaceView, PlaceError> {
        let selected = matching_clips(session, &self.selection)?;
        session.set_playhead(self.playhead);
        session.state.selection = selected;
        Ok(self.view)
    }
}

/// Seconds of `tick` on `rate`. The tick stays the source of truth.
///
/// `FrameRate::snap` is the same frame snap `Session::set_playhead` applies. A playhead
/// the session already snapped does not move. `Tick::seconds` then turns that frame
/// boundary into seconds for EffectCraft.
fn seconds_from(rate: FrameRate, tick: Tick) -> f64 {
    rate.snap(tick).seconds()
}

fn range_at(start: Tick, end: Tick, track_index: u32) -> SelectionRange {
    SelectionRange { start_s: start.seconds(), end_s: end.seconds(), track_index }
}

fn position_of(sequence: &Sequence, id: TrackId) -> Option<u32> {
    let position = sequence.all_tracks().position(|track| track.id == id)?;
    u32::try_from(position).ok()
}

fn ranges_for(sequence: &Sequence, ids: &[ClipId]) -> Result<Vec<SelectionRange>, PlaceError> {
    let mut ranges = Vec::new();
    for id in ids {
        // The id was selected, then the clip left the sequence. There is no range to send.
        let Some((track_id, item)) = sequence.find_item(*id) else {
            continue;
        };
        let track_index = position_of(sequence, track_id).ok_or(PlaceError::TrackIndex)?;
        ranges.push(range_at(item.start, item.end(), track_index));
    }
    Ok(ranges)
}

fn matching_clips(session: &Session, ranges: &[SelectionRange]) -> Result<Vec<ClipId>, PlaceError> {
    let sequence = session.active_sequence().ok_or(PlaceError::NoSequence)?;
    let mut selected = Vec::new();
    for range in ranges {
        for (position, track) in sequence.all_tracks().enumerate() {
            let track_index = u32::try_from(position).map_err(|_| PlaceError::TrackIndex)?;
            for item in &track.items {
                let same = range_at(item.start, item.end(), track_index) == *range;
                if same && !selected.contains(&item.id) {
                    selected.push(item.id);
                }
            }
        }
    }
    Ok(selected)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use filmcraft_engine::Session;
    use filmcraft_engine::project::{ClipId, ItemId, Label, SequenceSettings, TimeInterpolation, TrackItem};
    use filmcraft_engine::time::{FrameRate, Tick};

    use super::*;

    fn clip(id: u64, start: Tick, duration: Tick) -> TrackItem {
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: format!("clip-{id}"),
            label: Label::Iris,
            custom_label_color: None,
            start,
            duration,
            source_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: Vec::new(),
            markers: Vec::new(),
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: TimeInterpolation::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
            maintain_pitch: false,
        }
    }

    /// `Session::default`, then a sequence, the same shape engine tests use.
    fn session_with(rate: FrameRate, tracks: usize, clips: &[(usize, TrackItem)]) -> Session {
        let mut session = Session::default();
        let seq_id = session.project_mut().new_sequence("Seq", SequenceSettings { frame_rate: rate, ..Default::default() }, tracks, 0, None);
        {
            let seq = session.project_mut().sequence_mut(seq_id).expect("sequence");
            for (index, item) in clips {
                seq.video_tracks.get_mut(*index).expect("video track").items.push(item.clone());
            }
        }
        session.state.active_sequence = Some(seq_id);
        session.state.open_sequences = vec![seq_id];
        session
    }

    #[test]
    fn capture_then_restore_puts_back_playhead_selection_and_view() {
        let rate = FrameRate::FPS_24;
        let first = clip(1, rate.tick_of(0), rate.tick_of(24));
        let second = clip(2, rate.tick_of(48), rate.tick_of(24));
        let mut session = session_with(rate, 2, &[(0, first), (1, second.clone())]);
        assert_eq!(session.sequence_rate(), rate);

        let tick = rate.tick_of(10);
        assert_ne!(tick, Tick::ZERO);
        session.set_playhead(tick);
        session.state.selection = vec![second.id];
        let view = PlaceView { zoom: 80.0, scroll_s: 1.5 };

        let mut place = Place::capture(&session, view).expect("capture");
        assert_eq!(place.playhead, tick);
        assert_eq!(place.seconds, tick.seconds());
        assert_eq!(place.seconds, rate.snap(tick).seconds());
        let frames = rate.frame_at(tick);
        assert_eq!(place.seconds, frames as f64 / rate.as_f64());
        assert_eq!(place.view, view);
        assert_eq!(place.selection.len(), 1);
        let range = place.selection[0];
        assert_eq!(range.start_s, second.start.seconds());
        assert_eq!(range.end_s, second.end().seconds());
        assert_eq!(range.track_index, 1);

        // Seconds are not the source of truth. Restore must use the tick.
        place.seconds = 0.0;
        session.set_playhead(Tick::ZERO);
        session.state.selection.clear();
        let restored = place.restore(&mut session).expect("restore");
        assert_eq!(session.playhead(), tick);
        assert_eq!(session.state.selection, vec![second.id]);
        assert_eq!(restored.zoom, 80.0);
        assert_eq!(restored.scroll_s, 1.5);

        // An off-frame tick only lands on the frame boundary when set_playhead snaps it.
        let off_frame = Tick(1);
        assert_ne!(rate.snap(off_frame), off_frame);
        place.playhead = off_frame;
        place.restore(&mut session).expect("restore off-frame tick");
        assert_eq!(session.playhead(), rate.snap(off_frame));
    }

    #[test]
    fn restore_selects_nothing_when_the_range_matches_no_clip() {
        let rate = FrameRate::FPS_24;
        let first = clip(1, rate.tick_of(0), rate.tick_of(24));
        let second = clip(2, rate.tick_of(48), rate.tick_of(24));
        let mut session = session_with(rate, 2, &[(0, first.clone()), (1, second.clone())]);
        session.state.selection = vec![first.id];
        let place = Place {
            playhead: rate.tick_of(4),
            seconds: 0.0,
            selection: vec![SelectionRange { start_s: second.start.seconds(), end_s: second.end().seconds(), track_index: 9 }],
            view: PlaceView { zoom: 80.0, scroll_s: 1.5 },
        };
        place.restore(&mut session).expect("restore");
        assert!(session.state.selection.is_empty(), "a range on an empty track selects nothing");
        assert!(!session.state.selection.contains(&first.id));
        assert!(!session.state.selection.contains(&second.id));
        let items: usize = session.active_sequence().expect("sequence").all_tracks().map(|track| track.items.len()).sum();
        assert_eq!(items, 2, "restore does not create a clip");
    }

    #[test]
    fn restore_selects_both_clips_when_one_range_matches_both() {
        let rate = FrameRate::FPS_24;
        let start = rate.tick_of(12);
        let duration = rate.tick_of(24);
        let first = clip(1, start, duration);
        let second = clip(2, start, duration);
        let mut session = session_with(rate, 1, &[(0, first.clone()), (0, second.clone())]);
        session.state.selection.clear();
        let place = Place {
            playhead: Tick::ZERO,
            seconds: 0.0,
            selection: vec![SelectionRange { start_s: first.start.seconds(), end_s: first.end().seconds(), track_index: 0 }],
            view: PlaceView { zoom: 40.0, scroll_s: 0.0 },
        };
        place.restore(&mut session).expect("restore");
        assert_eq!(session.state.selection, vec![first.id, second.id], "two clips match one range, so both are selected");
        let items = session.active_sequence().expect("sequence").video_tracks.first().expect("v1").items.len();
        assert_eq!(items, 2, "restore does not create a clip");
    }

    #[test]
    fn capture_errors_when_there_is_no_active_sequence() {
        let session = Session::default();
        let view = PlaceView { zoom: 80.0, scroll_s: 1.5 };
        let err = Place::capture(&session, view).expect_err("no sequence");
        assert_eq!(err, PlaceError::NoSequence);

        let mut session = Session::default();
        session.state.selection = vec![ClipId(9)];
        let place = Place { playhead: Tick(50), seconds: 1.0, selection: Vec::new(), view };
        let err = place.restore(&mut session).expect_err("no sequence");
        assert_eq!(err, PlaceError::NoSequence);
        assert_eq!(session.state.selection, vec![ClipId(9)], "restore does not write when there is no sequence");
        assert_eq!(session.playhead(), Tick::ZERO);
    }
}

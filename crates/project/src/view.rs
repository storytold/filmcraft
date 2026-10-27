use super::*;

/// How a sequence is shown in the Timeline panel: its zoom, scroll position and track heights.
/// Each open sequence keeps its own (as Premiere's sequence tabs do).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceView {
    /// Zoom: points per second.
    pub pps: f64,
    /// Time at the left edge, in seconds.
    pub scroll: f64,
    /// Vertical scroll of the video and audio halves, in points.
    #[serde(default)]
    pub v_scroll: f32,
    #[serde(default)]
    pub a_scroll: f32,
    pub video_track_h: f32,
    pub audio_track_h: f32,
}

impl SequenceView {
    /// The view with every number brought into a usable range, or `None` when a number is not
    /// finite. Views are read from project files, so nothing in them is trusted.
    pub fn checked(self) -> Option<SequenceView> {
        let all = [self.pps, self.scroll, self.v_scroll as f64, self.a_scroll as f64, self.video_track_h as f64, self.audio_track_h as f64];
        if all.iter().any(|x| !x.is_finite()) {
            return None;
        }
        Some(SequenceView {
            pps: self.pps.clamp(1e-3, 1e5),
            scroll: self.scroll.clamp(0.0, 1e7),
            v_scroll: self.v_scroll.clamp(0.0, 1e6),
            a_scroll: self.a_scroll.clamp(0.0, 1e6),
            video_track_h: self.video_track_h.clamp(8.0, 600.0),
            audio_track_h: self.audio_track_h.clamp(8.0, 600.0),
        })
    }
}

/// What was open when a project was saved: the Timeline's sequence tabs in order, the active one,
/// how each sequence was shown and where its playhead was. Stored beside the project in its file,
/// not in it: it is not part of the edit and never an undo step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectView {
    #[serde(default)]
    pub open_sequences: Vec<ItemId>,
    #[serde(default)]
    pub active_sequence: Option<ItemId>,
    #[serde(default)]
    pub sequences: BTreeMap<ItemId, SequenceView>,
    /// Each sequence's playhead, so a sequence reopens where it was left. Files
    /// written before it existed have none, and every playhead starts at zero as it did then.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub playheads: BTreeMap<ItemId, Tick>,
}

impl ProjectView {
    /// The latest playhead a project file may ask for: ten million seconds (about 115 days), the
    /// same bound as a sequence view's scroll position. Playheads are read from project files, so
    /// nothing in them is trusted.
    pub const MAX_PLAYHEAD: Tick = Tick(10_000_000 * TICKS_PER_SECOND);
}

/// A LUT imported into the project (`lut.import`). Lumetri refers to it as `lib:<id>`.
/// Largest embedded LUT source accepted from a project or import (8 MiB).
pub const MAX_LUT_TEXT_BYTES: usize = 8 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectLut {
    pub id: String,
    pub name: String,
    /// Where it was imported from (informational).
    #[serde(default)]
    pub source_path: Option<String>,
    /// `cube` or `3dl`.
    pub format: String,
    /// The file's text.
    pub text: std::sync::Arc<str>,
}

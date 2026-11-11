use super::{BTreeMap, Deserialize, ItemId, Serialize, TICKS_PER_SECOND, Tick};

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
    /// Project-scoped Timeline settings stored beside the rest of the view. A malformed settings
    /// block falls back to defaults without discarding tabs or playheads; values are sanitized by
    /// [`TimelineUiSettings::checked`].
    #[serde(default, deserialize_with = "lenient_timeline_ui")]
    pub timeline_ui: TimelineUiSettings,
}

/// Project-specific Timeline interaction preferences saved with [`ProjectView`]. Missing or
/// malformed settings default safely; [`checked`](Self::checked) bounds numeric values and
/// normalizes minimap colours before the settings reach UI controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineUiSettings {
    /// Smooth zoom-bar animation. Defaults to off; the application animation preference also
    /// needs to be enabled for animation.
    pub smooth_animations: bool,
    /// Inverted controls: the wheel over the tracks scrolls the Timeline the other way in time,
    /// and the wheel while grabbing the zoom slider's middle zooms the other way.
    pub inverted_controls: bool,
    /// Seconds of Timeline to keep before the playhead when centering it (0.1-3600; default 10).
    pub playhead_side_seconds: f64,
    /// Whether `playhead_side_seconds` is the Custom choice rather than a preset.
    pub playhead_side_custom: bool,
    /// Shift-drag speed as a multiplier of the Auto sensitivity for the current view (0.25 = four
    /// times slower, 2 = twice as fast).
    pub shift_drag_speed: f64,
    /// Whether `shift_drag_speed` is the Custom choice rather than a preset.
    pub shift_drag_custom: bool,
    /// Zooming via the zoom bar's edges keeps the playhead exactly in the middle of the view
    /// once a zoom out has brought it into view (gravity), and never lets it slip off while the
    /// user keeps nudging the edges. Escaped only by dragging the slider's middle manually.
    pub playhead_centering: bool,
    /// Hide Slider: the zoom scroll bar is gone and the tracks fill its space. The hide dialog's
    /// Unhide button and the Timeline context menu can bring it back.
    pub zoom_bar_hidden: bool,
    /// Alternate Slider Position: the zoom bar sits at the top edge, just above the ruler,
    /// instead of at the bottom edge.
    pub alternate_slider_position: bool,
    /// Show clip indicators on the zoom scroll bar (the Slider Minimap).
    pub slider_minimap: bool,
    /// Show Whole Timeline (the minimap's own menu): the minimap is locked to the full Timeline
    /// extent (the sequence plus its ten minute tail) so the viewport marker can travel past the
    /// last clip. Off: the map stops at the end of the sequence.
    pub show_whole_timeline: bool,
    /// Fill Clips (the minimap's own menu): minimap clips are filled in. Off: outlines only.
    pub fill_clips: bool,
    /// Position Indicator Color (the minimap's own menu): the "#rrggbb" colour of the "you are
    /// here" line on the zoom scroll bar.
    pub position_indicator_color: String,
    /// Prioritise Audio (the minimap's own menu): where an audio clip and a video clip sit at the
    /// same place, the audio shows on top of the video. Off: the video covers the audio.
    pub prioritise_audio: bool,
    /// Video clip indicator colour (`#rrggbb`) in the minimap.
    pub video_indicator_color: String,
    /// Audio clip indicator colour (`#rrggbb`) in the minimap.
    pub audio_indicator_color: String,
    /// Offline media indicator colour (`#rrggbb`) in the minimap.
    pub offline_indicator_color: String,
    /// Maximum Zoom Level: the smallest number of seconds the view may show (0.1 = Closest,
    /// 0.2 = Close, 0.4 = Default, 0.8 = Far; Custom 0.05-3).
    pub max_zoom_seconds: f64,
    /// Whether `max_zoom_seconds` is the Custom choice rather than a preset.
    pub max_zoom_custom: bool,
    /// The last Custom values of each dialog slider, restored when Custom is chosen again.
    /// Files written before they existed start at the one-time Custom defaults.
    #[serde(default = "default_playhead_custom")]
    pub playhead_side_last_custom: f64,
    #[serde(default = "default_speed_custom")]
    pub shift_drag_last_custom: f64,
    #[serde(default = "default_max_zoom_custom")]
    pub max_zoom_last_custom: f64,
    /// Hide Slider (track sliders): the side track sliders are gone and the tracks fill their
    /// column. The hide dialog's Unhide button and the Timeline context menu can bring them back.
    pub track_slider_hidden: bool,
    /// Smooth animation while moving a track slider.
    pub track_slider_smooth: bool,
    /// Place the track sliders on the opposite side of their tracks.
    pub track_slider_alternate_position: bool,
    /// Snap a track-slider drag to the nearest track boundary when released.
    pub track_slider_snap: bool,
    /// Decouple Edges: dragging one end circle of a track slider moves only that end -- the
    /// other end stays exactly where it is instead of mirroring about the thumb's middle.
    pub track_slider_decouple: bool,
    /// Position Indicator (the minimap's own menu): the "you are here" line on the zoom scroll
    /// bar. Off: hidden.
    pub show_position_indicator: bool,
    /// Track sliders' Alternate Drag Speed: the Shift-drag sensitivity (0.5 = Slow, 1.0 =
    /// Default, the default; Custom 0.1-10).
    pub track_slider_shift_drag_speed: f64,
    /// The user has typed a Custom Alternate Drag Speed: the tick slider stays on Custom even
    /// when the number matches a preset (the main slider's `shift_drag_custom` on its end).
    pub track_slider_shift_drag_custom: bool,
    /// The last Custom Alternate Drag Speed the user typed, restored when Custom is picked
    /// again (`shift_drag_last_custom` the same way).
    pub track_slider_shift_drag_last_custom: f64,
    /// Controls (the flyouts): the input the user chose for each action, as its display text
    /// ("Ctrl + K", "Right Click"). Empty means the built-in gesture (Double Click / Triple
    /// Click / Shift + Drag / Right Click).
    /// Control-menu binding for centring the playhead; empty keeps the built-in gesture.
    pub control_center_on_playhead: String,
    /// Control-menu binding for expanding the zoom slider; empty keeps the built-in gesture.
    pub control_explode_slider: String,
    /// Control-menu binding for alternate drag speed; empty keeps the built-in gesture.
    pub control_alternate_drag_speed: String,
    /// Control-menu binding for opening slider settings; empty keeps the built-in gesture.
    pub control_settings: String,
}

fn default_playhead_custom() -> f64 {
    15.0
}

fn default_speed_custom() -> f64 {
    1.0
}

fn default_max_zoom_custom() -> f64 {
    0.4
}

impl Default for TimelineUiSettings {
    fn default() -> Self {
        Self {
            smooth_animations: false,
            inverted_controls: false,
            playhead_side_seconds: 10.0,
            playhead_side_custom: false,
            shift_drag_speed: 1.0,
            shift_drag_custom: false,
            playhead_centering: true,
            zoom_bar_hidden: false,
            alternate_slider_position: false,
            slider_minimap: true,
            show_whole_timeline: true,
            fill_clips: false,
            position_indicator_color: "#5895ec".to_string(),
            prioritise_audio: false,
            video_indicator_color: "#64b5f6".to_string(),
            audio_indicator_color: "#2e7d32".to_string(),
            offline_indicator_color: "#d32f2f".to_string(),
            max_zoom_seconds: 0.4,
            max_zoom_custom: false,
            playhead_side_last_custom: 15.0,
            shift_drag_last_custom: 1.0,
            max_zoom_last_custom: 0.4,
            track_slider_hidden: false,
            track_slider_smooth: false,
            track_slider_alternate_position: false,
            track_slider_snap: true,
            track_slider_decouple: true,
            show_position_indicator: true,
            track_slider_shift_drag_speed: 1.0,
            track_slider_shift_drag_custom: false,
            track_slider_shift_drag_last_custom: 1.0,
            control_center_on_playhead: String::new(),
            control_explode_slider: String::new(),
            control_alternate_drag_speed: String::new(),
            control_settings: String::new(),
        }
    }
}

impl TimelineUiSettings {
    /// Sanitize settings read from a project file before they reach UI geometry or controls.
    ///
    /// Numeric limits and defaults are: playhead side 0.1-3600 seconds (10); main Shift-drag
    /// speed 0.1-50x (1); main saved Custom speed 0.1-50x (1); Maximum Zoom Level and its saved
    /// Custom value 0.05-3 seconds (0.4); track-slider Shift-drag speed and its saved Custom value
    /// 0.1-10x (1). Non-finite values use the listed defaults, and a non-positive zoom value is
    /// treated as unset (0.4 seconds). Minimap colours accept `#rgb` or `#rrggbb`, normalize to
    /// lowercase six-digit form, and otherwise use their field defaults. Invalid control-binding
    /// strings are cleared.
    pub fn checked(mut self) -> Self {
        self.playhead_side_seconds = if self.playhead_side_seconds.is_finite() { self.playhead_side_seconds.clamp(0.1, 3600.0) } else { 10.0 };
        self.shift_drag_speed = if self.shift_drag_speed.is_finite() { self.shift_drag_speed.clamp(0.1, 50.0) } else { 1.0 };
        self.playhead_side_last_custom = if self.playhead_side_last_custom.is_finite() { self.playhead_side_last_custom.clamp(0.1, 3600.0) } else { 15.0 };
        self.shift_drag_last_custom = if self.shift_drag_last_custom.is_finite() { self.shift_drag_last_custom.clamp(0.1, 50.0) } else { 1.0 };
        // a non-positive Maximum Zoom Level is "unset": the documented 0.4 s default, never the
        // clamp floor (0.05), or the fallback could no longer apply
        self.max_zoom_seconds = if self.max_zoom_seconds.is_finite() && self.max_zoom_seconds > 0.0 { self.max_zoom_seconds.clamp(0.05, 3.0) } else { 0.4 };
        self.max_zoom_last_custom =
            if self.max_zoom_last_custom.is_finite() && self.max_zoom_last_custom > 0.0 { self.max_zoom_last_custom.clamp(0.05, 3.0) } else { 0.4 };
        self.track_slider_shift_drag_speed =
            if self.track_slider_shift_drag_speed.is_finite() { self.track_slider_shift_drag_speed.clamp(0.1, 10.0) } else { 1.0 };
        // the value Custom restores must be bounded like the active one
        self.track_slider_shift_drag_last_custom =
            if self.track_slider_shift_drag_last_custom.is_finite() { self.track_slider_shift_drag_last_custom.clamp(0.1, 10.0) } else { 1.0 };
        self.position_indicator_color = normalize_hex_color(&self.position_indicator_color).unwrap_or_else(|| "#5895ec".to_string());
        self.video_indicator_color = normalize_hex_color(&self.video_indicator_color).unwrap_or_else(|| "#64b5f6".to_string());
        self.audio_indicator_color = normalize_hex_color(&self.audio_indicator_color).unwrap_or_else(|| "#2e7d32".to_string());
        self.offline_indicator_color = normalize_hex_color(&self.offline_indicator_color).unwrap_or_else(|| "#d32f2f".to_string());
        for binding in
            [&mut self.control_center_on_playhead, &mut self.control_explode_slider, &mut self.control_alternate_drag_speed, &mut self.control_settings]
        {
            // Bindings are display text from project files; keep them within UI layout bounds.
            if binding.len() > 48 || binding.chars().any(char::is_control) {
                binding.clear();
            }
        }
        self
    }

    /// The Maximum Zoom Level in seconds: the smallest number of seconds the view may show,
    /// sanely clamped to 0.05-3 (hostile or unset values give the 0.4 s default).
    pub fn max_zoom_secs(&self) -> f64 {
        if self.max_zoom_seconds.is_finite() && self.max_zoom_seconds > 0.0 { self.max_zoom_seconds.clamp(0.05, 3.0) } else { 0.4 }
    }
}

impl ProjectView {
    /// The latest playhead a project file may ask for: ten million seconds (about 115 days), the
    /// same bound as a sequence view's scroll position. Playheads are read from project files, so
    /// nothing in them is trusted.
    pub const MAX_PLAYHEAD: Tick = Tick(10_000_000 * TICKS_PER_SECOND);
}

/// One bad value in `timeline_ui` must not throw away the whole saved view (the open tabs and
/// playheads beside it): a block that fails to deserialize falls back to the defaults, and
/// [`TimelineUiSettings::checked`] then bounds whatever survived.
fn lenient_timeline_ui<'de, D: serde::Deserializer<'de>>(d: D) -> Result<TimelineUiSettings, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let Some(values) = value.as_object() else {
        log::warn!("invalid timeline UI settings in project view; expected an object, using defaults");
        return Ok(TimelineUiSettings::default());
    };
    let default = TimelineUiSettings::default();
    fn read_field<T: serde::de::DeserializeOwned + Clone>(values: &serde_json::Map<String, serde_json::Value>, name: &str, default: &T) -> T {
        match values.get(name) {
            Some(value) => match serde_json::from_value::<T>(value.clone()) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!("invalid timeline UI setting `{name}`; using its default: {error}");
                    default.clone()
                }
            },
            None => default.clone(),
        }
    }
    macro_rules! read {
        ($field:ident, $ty:ty) => {
            read_field::<$ty>(values, stringify!($field), &default.$field)
        };
    }
    Ok(TimelineUiSettings {
        smooth_animations: read!(smooth_animations, bool),
        inverted_controls: read!(inverted_controls, bool),
        playhead_side_seconds: read!(playhead_side_seconds, f64),
        playhead_side_custom: read!(playhead_side_custom, bool),
        shift_drag_speed: read!(shift_drag_speed, f64),
        shift_drag_custom: read!(shift_drag_custom, bool),
        playhead_centering: read!(playhead_centering, bool),
        zoom_bar_hidden: read!(zoom_bar_hidden, bool),
        alternate_slider_position: read!(alternate_slider_position, bool),
        slider_minimap: read!(slider_minimap, bool),
        show_whole_timeline: read!(show_whole_timeline, bool),
        fill_clips: read!(fill_clips, bool),
        position_indicator_color: read!(position_indicator_color, String),
        prioritise_audio: read!(prioritise_audio, bool),
        video_indicator_color: read!(video_indicator_color, String),
        audio_indicator_color: read!(audio_indicator_color, String),
        offline_indicator_color: read!(offline_indicator_color, String),
        max_zoom_seconds: read!(max_zoom_seconds, f64),
        max_zoom_custom: read!(max_zoom_custom, bool),
        playhead_side_last_custom: read!(playhead_side_last_custom, f64),
        shift_drag_last_custom: read!(shift_drag_last_custom, f64),
        max_zoom_last_custom: read!(max_zoom_last_custom, f64),
        track_slider_hidden: read!(track_slider_hidden, bool),
        track_slider_smooth: read!(track_slider_smooth, bool),
        track_slider_alternate_position: read!(track_slider_alternate_position, bool),
        track_slider_snap: read!(track_slider_snap, bool),
        track_slider_decouple: read!(track_slider_decouple, bool),
        show_position_indicator: read!(show_position_indicator, bool),
        track_slider_shift_drag_speed: read!(track_slider_shift_drag_speed, f64),
        track_slider_shift_drag_custom: read!(track_slider_shift_drag_custom, bool),
        track_slider_shift_drag_last_custom: read!(track_slider_shift_drag_last_custom, f64),
        control_center_on_playhead: read!(control_center_on_playhead, String),
        control_explode_slider: read!(control_explode_slider, String),
        control_alternate_drag_speed: read!(control_alternate_drag_speed, String),
        control_settings: read!(control_settings, String),
    }
    .checked())
}

/// A colour string from a project file or the control channel, normalised to the canonical
/// `#rrggbb` form the indicator colours are stored in. `#rgb` shorthand expands; anything else
/// (empty, wrong length, non-hex) is rejected and the caller falls back to its default.
pub fn normalize_hex_color(value: &str) -> Option<String> {
    let hex = value.as_bytes().strip_prefix(b"#")?;
    match hex {
        [r, g, b] if r.is_ascii_hexdigit() && g.is_ascii_hexdigit() && b.is_ascii_hexdigit() => {
            let (r, g, b) = (char::from(*r).to_ascii_lowercase(), char::from(*g).to_ascii_lowercase(), char::from(*b).to_ascii_lowercase());
            Some(format!("#{r}{r}{g}{g}{b}{b}"))
        }
        [r, g, b, a, c, d] if [r, g, b, a, c, d].iter().all(|x| x.is_ascii_hexdigit()) => Some(value.to_ascii_lowercase()),
        _ => None,
    }
}

/// A LUT imported into the project (`lut.import`). Color Grading refers to it as `lib:<id>`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_timeline_field_uses_its_default_and_preserves_other_settings() {
        let value = serde_json::json!({
            "open_sequences": [],
            "timeline_ui": {"inverted_controls": true, "max_zoom_seconds": "not a number"}
        });
        let view: ProjectView = serde_json::from_value(value).unwrap();
        assert!(view.open_sequences.is_empty());
        assert!(view.timeline_ui.inverted_controls, "a valid setting survives a malformed sibling");
        assert_eq!(view.timeline_ui.max_zoom_seconds, 0.4, "the invalid field uses its default");
    }

    /// The Controls bindings start empty (the built-in gestures do the actions) and Snap to
    /// Tracks starts on: the defaults of everything the slider menus added.
    #[test]
    fn the_controls_bindings_start_empty_and_snap_to_tracks_starts_on() {
        let s = TimelineUiSettings::default().checked();
        assert!(s.control_center_on_playhead.is_empty());
        assert!(s.control_explode_slider.is_empty());
        assert!(s.control_alternate_drag_speed.is_empty());
        assert!(s.control_settings.is_empty());
        assert!(s.track_slider_snap, "Snap to Tracks is on by default");
    }

    /// Values a project file may carry (hostile or stale) come back clamped and finite, so no
    /// setting read from disk can drive UI geometry outside its range.
    #[test]
    fn checked_clamps_values_read_from_a_project_file() {
        let s = TimelineUiSettings {
            playhead_side_seconds: 0.0,
            shift_drag_speed: f64::NAN,
            max_zoom_seconds: 99.0,
            track_slider_shift_drag_speed: -3.0,
            playhead_side_last_custom: f64::INFINITY,
            max_zoom_last_custom: f64::NAN,
            shift_drag_last_custom: 1e9,
            track_slider_shift_drag_last_custom: 1e9,
            position_indicator_color: "#1234567890".to_string(),
            video_indicator_color: "x".repeat(64),
            audio_indicator_color: "#2e7d32".to_string(),
            offline_indicator_color: "z".repeat(200),
            control_center_on_playhead: "x".repeat(49),
            control_explode_slider: "x\n".to_string(),
            control_alternate_drag_speed: "y".repeat(49),
            control_settings: "z".repeat(49),
            ..TimelineUiSettings::default()
        }
        .checked();
        assert_eq!(s.playhead_side_seconds, 0.1);
        assert_eq!(s.shift_drag_speed, 1.0);
        assert_eq!(s.max_zoom_seconds, 3.0);
        assert_eq!(s.track_slider_shift_drag_speed, 0.1);
        assert_eq!(s.playhead_side_last_custom, 15.0);
        assert_eq!(s.max_zoom_last_custom, 0.4);
        assert_eq!(s.shift_drag_last_custom, 50.0);
        assert_eq!(s.track_slider_shift_drag_last_custom, 10.0);
        assert_eq!(s.position_indicator_color, "#5895ec");
        assert_eq!(s.video_indicator_color, "#64b5f6");
        assert_eq!(s.audio_indicator_color, "#2e7d32");
        assert_eq!(s.offline_indicator_color, "#d32f2f");
        assert!(s.control_center_on_playhead.is_empty());
        assert!(s.control_explode_slider.is_empty());
        assert!(s.control_alternate_drag_speed.is_empty());
        assert!(s.control_settings.is_empty());
        assert_eq!(s.max_zoom_secs(), 3.0);
    }

    /// In-range values (and short colour strings) pass through untouched, and the read-time
    /// helper falls back to the 0.4 s default for anything that cannot be a view width.
    #[test]
    fn checked_keeps_sane_values_and_max_zoom_secs_falls_back() {
        let s = TimelineUiSettings {
            playhead_side_seconds: 15.0,
            shift_drag_speed: 2.0,
            max_zoom_seconds: 0.2,
            max_zoom_last_custom: 0.05,
            position_indicator_color: "#aabbcc".to_string(),
            ..TimelineUiSettings::default()
        }
        .checked();
        assert_eq!(s.playhead_side_seconds, 15.0);
        assert_eq!(s.shift_drag_speed, 2.0);
        assert_eq!(s.max_zoom_seconds, 0.2);
        assert_eq!(s.max_zoom_last_custom, 0.05);
        assert_eq!(s.position_indicator_color, "#aabbcc");
        assert_eq!(s.max_zoom_secs(), 0.2);
        // shorthand expands to the stored form; a non-positive zoom is "unset" (0.4), never 0.05
        let s = TimelineUiSettings {
            position_indicator_color: "#abc".to_string(),
            max_zoom_seconds: 0.0,
            track_slider_shift_drag_last_custom: -3.0,
            ..TimelineUiSettings::default()
        }
        .checked();
        assert_eq!(s.position_indicator_color, "#aabbcc");
        assert_eq!(s.max_zoom_seconds, 0.4);
        assert_eq!(s.track_slider_shift_drag_last_custom, 0.1);
        let s = TimelineUiSettings { max_zoom_seconds: 0.0, ..TimelineUiSettings::default() };
        assert_eq!(s.max_zoom_secs(), 0.4);
        let s = TimelineUiSettings { max_zoom_seconds: f64::NAN, ..TimelineUiSettings::default() };
        assert_eq!(s.max_zoom_secs(), 0.4);
    }
}

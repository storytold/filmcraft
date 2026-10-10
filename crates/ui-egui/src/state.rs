//! UI state that is not project data: tools, layout, zoom/scroll, monitor settings. Serde so the
//! control channel can read and set all of it.

use serde::{Deserialize, Serialize};

use crate::dock::{DockNode, PanelKind};
use crate::icons::Icon;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    #[default]
    Selection,
    TrackSelectForward,
    TrackSelectBackward,
    Ripple,
    Rolling,
    RateStretch,
    Remix,
    Razor,
    Slip,
    Slide,
    Pen,
    Rectangle,
    Ellipse,
    Hand,
    Zoom,
    Type,
    VerticalType,
}

impl Tool {
    pub const ALL: [Tool; 17] = [
        Tool::Selection,
        Tool::TrackSelectForward,
        Tool::TrackSelectBackward,
        Tool::Ripple,
        Tool::Rolling,
        Tool::RateStretch,
        Tool::Remix,
        Tool::Razor,
        Tool::Slip,
        Tool::Slide,
        Tool::Pen,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Hand,
        Tool::Zoom,
        Tool::Type,
        Tool::VerticalType,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Tool::Selection => "Selection Tool",
            Tool::TrackSelectForward => "Track Select Forward Tool",
            Tool::TrackSelectBackward => "Track Select Backward Tool",
            Tool::Ripple => "Ripple Edit Tool",
            Tool::Rolling => "Rolling Edit Tool",
            Tool::RateStretch => "Rate Stretch Tool",
            Tool::Remix => "Remix Tool",
            Tool::Razor => "Razor Tool",
            Tool::Slip => "Slip Tool",
            Tool::Slide => "Slide Tool",
            Tool::Pen => "Pen Tool",
            Tool::Rectangle => "Rectangle Tool",
            Tool::Ellipse => "Ellipse Tool",
            Tool::Hand => "Hand Tool",
            Tool::Zoom => "Zoom Tool",
            Tool::Type => "Type Tool",
            Tool::VerticalType => "Vertical Type Tool",
        }
    }
    pub fn shortcut(self) -> &'static str {
        match self {
            Tool::Selection => "V",
            Tool::TrackSelectForward => "A",
            Tool::TrackSelectBackward => "Shift+A",
            Tool::Ripple => "B",
            Tool::Rolling => "N",
            Tool::RateStretch => "R",
            Tool::Remix => "",
            Tool::Razor => "C",
            Tool::Slip => "Y",
            Tool::Slide => "U",
            Tool::Pen => "P",
            Tool::Rectangle | Tool::Ellipse => "",
            Tool::Hand => "H",
            Tool::Zoom => "Z",
            Tool::Type => "T",
            Tool::VerticalType => "",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            Tool::Selection => Icon::Selection,
            Tool::TrackSelectForward => Icon::TrackSelectFwd,
            Tool::TrackSelectBackward => Icon::TrackSelectBack,
            Tool::Ripple => Icon::Ripple,
            Tool::Rolling => Icon::Rolling,
            Tool::RateStretch => Icon::RateStretch,
            Tool::Remix => Icon::Remix,
            Tool::Razor => Icon::Razor,
            Tool::Slip => Icon::Slip,
            Tool::Slide => Icon::Slide,
            Tool::Pen => Icon::Pen,
            Tool::Rectangle => Icon::Rectangle,
            Tool::Ellipse => Icon::Ellipse,
            Tool::Hand => Icon::Hand,
            Tool::Zoom => Icon::Zoom,
            Tool::Type | Tool::VerticalType => Icon::Type,
        }
    }
    pub fn from_name(s: &str) -> Option<Tool> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "").replace("tool", "");
        Tool::ALL
            .iter()
            .copied()
            .find(|t| format!("{t:?}").to_ascii_lowercase() == n || t.label().to_ascii_lowercase().replace([' ', '-'], "").replace("tool", "") == n)
    }
    /// Tools-panel groups (Premiere groups related tools under one button with a flyout).
    pub fn groups() -> Vec<Vec<Tool>> {
        vec![
            vec![Tool::Selection],
            vec![Tool::TrackSelectForward, Tool::TrackSelectBackward],
            vec![Tool::Ripple, Tool::Rolling, Tool::RateStretch, Tool::Remix],
            vec![Tool::Razor],
            vec![Tool::Slip, Tool::Slide],
            vec![Tool::Pen, Tool::Rectangle, Tool::Ellipse],
            vec![Tool::Hand, Tool::Zoom],
            vec![Tool::Type, Tool::VerticalType],
        ]
    }
}

/// Playback resolution (Premiere's Full / 1/2 / 1/4 / 1/8 / 1/16).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackRes {
    Full,
    #[default]
    Half,
    Quarter,
    Eighth,
    Sixteenth,
}

impl PlaybackRes {
    pub const ALL: [PlaybackRes; 5] = [PlaybackRes::Full, PlaybackRes::Half, PlaybackRes::Quarter, PlaybackRes::Eighth, PlaybackRes::Sixteenth];
    pub fn scale(self) -> f32 {
        match self {
            PlaybackRes::Full => 1.0,
            PlaybackRes::Half => 0.5,
            PlaybackRes::Quarter => 0.25,
            PlaybackRes::Eighth => 0.125,
            PlaybackRes::Sixteenth => 0.0625,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PlaybackRes::Full => "Full",
            PlaybackRes::Half => "1/2",
            PlaybackRes::Quarter => "1/4",
            PlaybackRes::Eighth => "1/8",
            PlaybackRes::Sixteenth => "1/16",
        }
    }
}

/// Header mode (Import / Edit / Export).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Import,
    #[default]
    Edit,
    Export,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineView {
    /// Pixels per second (animated toward `target_pps`).
    pub pps: f64,
    pub target_pps: f64,
    /// Left edge time in seconds (animated toward `target_scroll`).
    pub scroll: f64,
    pub target_scroll: f64,
    /// Vertical scroll of the video / audio halves.
    pub v_scroll: f32,
    pub a_scroll: f32,
    /// Fraction of the track area given to video tracks.
    pub split: f32,
    pub video_track_h: f32,
    pub audio_track_h: f32,
    pub header_w: f32,
    pub show_thumbnails: bool,
    pub show_waveforms: bool,
    /// Follow playhead during playback (page scroll).
    pub follow: bool,
    pub fit_pending: bool,
    /// The zoom a fit gave an empty sequence (which fits its 1 s minimum): the first clip to arrive
    /// fits again, unless the zoom was changed in between.
    #[serde(skip)]
    pub fit_empty: Option<f64>,
    /// Audio tracks showing track keyframes instead of clip keyframes: track id → lane
    /// (`volume`, `pan`, `mute`, `send.<i>.level`, `fx.<slot>.<param>`).
    #[serde(default)]
    pub track_lanes: std::collections::BTreeMap<u64, String>,
}

impl TimelineView {
    /// The zoom / scroll animation is still running (element rects are moving).
    pub fn animating(&self) -> bool {
        self.pps != self.target_pps || self.scroll != self.target_scroll
    }
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            pps: 40.0,
            target_pps: 40.0,
            scroll: 0.0,
            target_scroll: 0.0,
            v_scroll: 0.0,
            a_scroll: 0.0,
            split: 0.5,
            video_track_h: 60.0,
            audio_track_h: 56.0,
            header_w: 204.0,
            show_thumbnails: true,
            show_waveforms: true,
            follow: true,
            fit_pending: true,
            fit_empty: None,
            track_lanes: Default::default(),
        }
    }
}

/// Monitor display mode (View ▸ Display Mode / the monitor's wrench menu). Multi-Camera is the
/// separate `MonitorView::multicam` switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayMode {
    #[default]
    Composite,
    Alpha,
    Red,
    Green,
    Blue,
    /// Source Monitor: the clip's audio waveform.
    AudioWaveform,
    /// Program Monitor: a reference frame beside the current frame.
    Comparison,
    /// Source Monitor: the picture above the audio waveform.
    VideoAndWaveform,
}

impl DisplayMode {
    /// Shows a single colour channel (or alpha) as greyscale.
    pub fn is_channel(self) -> bool {
        matches!(self, DisplayMode::Alpha | DisplayMode::Red | DisplayMode::Green | DisplayMode::Blue)
    }
}

/// Monitor guide (frame pixels): `vertical` = a line at x = `position`.
pub type Guide = filmcraft_engine::autosave::Guide;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorView {
    /// Playback Resolution (used while playing).
    pub res: PlaybackRes,
    /// Paused Resolution (used while stopped).
    pub paused_res: PlaybackRes,
    /// High Quality Playback: play at the paused resolution when it is higher.
    pub high_quality: bool,
    /// Magnification: None = Fit, else the zoom factor (1.0 = 100%: one frame pixel per screen pixel).
    pub zoom: Option<f32>,
    /// Pan offset of a magnified picture from the centre (points).
    pub pan: [f32; 2],
    pub safe_margins: bool,
    pub show_transport: bool,
    /// Program monitor display mode Multi-Camera (angle grid + program).
    pub multicam: bool,
    pub display: DisplayMode,
    /// Comparison View: the reference frame's time (ticks); None = set on entry from the playhead.
    pub compare_ref: Option<i64>,
    pub show_rulers: bool,
    pub show_guides: bool,
    pub lock_guides: bool,
    /// Snap in Program Monitor: graphic drags snap to guides, the frame edges and centre.
    pub snap: bool,
    pub guides: Vec<Guide>,
}

impl Default for MonitorView {
    fn default() -> Self {
        Self {
            res: PlaybackRes::Half,
            paused_res: PlaybackRes::Full,
            high_quality: false,
            zoom: None,
            pan: [0.0, 0.0],
            safe_margins: false,
            show_transport: true,
            multicam: false,
            display: DisplayMode::Composite,
            compare_ref: None,
            show_rulers: false,
            show_guides: true,
            lock_guides: false,
            snap: true,
            guides: Vec::new(),
        }
    }
}

impl MonitorView {
    /// The resolution frames are rendered at: Playback Resolution while playing (or the higher
    /// of it and Paused Resolution with High Quality Playback), Paused Resolution when stopped.
    pub fn effective_res(&self, playing: bool) -> PlaybackRes {
        if !playing || (self.high_quality && self.paused_res.scale() > self.res.scale()) { self.paused_res } else { self.res }
    }
    /// The display mode in effect (None = Multi-Camera).
    pub fn display_mode(&self) -> Option<DisplayMode> {
        (!self.multicam).then_some(self.display)
    }
}

/// Window ▸ Workspaces dialogs: Save as New Workspace… and Edit Workspaces….
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceDialog {
    SaveAs {
        name: String,
    },
    /// `selected`: the workspace picked in the list; `name`: its new name being typed.
    Edit {
        selected: Option<String>,
        name: String,
    },
}

/// Guide dialogs (View ▸ Add Guide…, Guide Templates ▸ Save Guides as Template… / Manage Guides…).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GuideDialog {
    /// `source`: the dialog acts on the Source Monitor (else the Program Monitor).
    Add {
        vertical: bool,
        position: f64,
        #[serde(default)]
        source: bool,
    },
    SaveTemplate {
        name: String,
        #[serde(default)]
        source: bool,
    },
    Manage {
        selected: Option<usize>,
        #[serde(default)]
        source: bool,
    },
}

/// The colour parameter an armed eyedropper fills (`effects.setParam` arguments).
#[derive(Clone, Debug, PartialEq)]
pub struct Eyedropper {
    pub clip: u64,
    pub effect: usize,
    pub param: String,
    pub mask: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiState {
    #[serde(default)]
    pub language: crate::i18n::Language,
    pub tool: Tool,
    pub mode: Mode,
    pub workspace: String,
    pub dock: DockNode,
    /// Focused panel (blue outline, receives shortcuts).
    pub focused: PanelKind,
    pub timeline: TimelineView,
    pub program: MonitorView,
    pub source: MonitorView,
    pub project_search: String,
    pub effects_search: String,
    /// Expanded bins in the project list view.
    pub expanded_bins: Vec<u64>,
    /// Expanded folders in the Effects panel.
    pub expanded_fx: Vec<String>,
    /// Effects panel: the Lumetri Presets folder shown as a thumbnail grid (wide panel).
    #[serde(default)]
    pub lumetri_grid_folder: Option<String>,
    /// Collapsed effect sections in Effect Controls ("clip:index").
    pub collapsed_fx: Vec<String>,
    pub show_menu_bar: bool,
    /// The header bar (Home, Import, Edit, Export, workspaces). An app that embeds FilmCraft can hide it.
    #[serde(default = "shown")]
    pub show_header: bool,
    /// The status bar (hints, messages, job progress). An app that embeds FilmCraft can hide it.
    #[serde(default = "shown")]
    pub show_status_bar: bool,
    pub dark: bool,
    /// Lumetri scopes visible in the Program monitor area.
    pub show_scopes: bool,
    /// Transient status line shown in the footer.
    pub status: String,
    /// The colour parameter an armed eyedropper will fill with the next pixel clicked in the Program
    /// monitor (Esc or a click elsewhere disarms it). Never saved.
    #[serde(skip)]
    pub eyedropper: Option<Eyedropper>,
    /// Essential Sound sub-tab: "Edit" or "Browse".
    #[serde(default)]
    pub essential_sound_tab: String,
    /// Text to Speech panel: the script and voice settings being written or edited.
    #[serde(default)]
    pub tts: crate::panels::tts::TtsDraft,
    /// Export mode: settings, preset, destination, range, the Preset Manager and Quick Export.
    #[serde(default)]
    pub export: crate::panels::export_mode::ExportUi,
    /// Text panel: active tab ("Transcript" / "Captions" / "Graphics").
    #[serde(default = "captions_tab")]
    pub text_tab: String,
    /// Text panel: caption search filter.
    #[serde(default)]
    pub caption_search: String,
    /// Text panel ▸ Transcript: selected words of the sequence transcript (anchor, end; indices as
    /// `transcript.inspect` lists them).
    #[serde(default)]
    pub transcript_sel: Option<(usize, usize)>,
    /// Text panel ▸ Transcript: search text.
    #[serde(default)]
    pub transcript_search: String,
    /// Text panel ▸ Transcript: the search filter, as Premiere's filter icon offers it
    /// (`text` = Transcript text, `fillers` = Filler words, `pauses` = Pauses).
    #[serde(default = "text_filter")]
    pub transcript_filter: String,
    /// The current search result (the ▲ ▼ arrows step through them; Delete removes it).
    #[serde(default)]
    pub transcript_hit: usize,
    /// Delete / Delete all lift (leave gaps) instead of extracting (closing them).
    #[serde(default)]
    pub transcript_lift: bool,
    /// The pause clicked in the transcript (index into the transcript's pauses).
    #[serde(default)]
    pub transcript_pause: Option<usize>,
    /// The Transcribe options (shown when Some).
    #[serde(default)]
    pub transcribe_dialog: Option<TranscribeDraft>,
    /// Preferences ▸ Playback: play the rendered range when a preview render finishes.
    #[serde(default = "yes")]
    pub play_after_render: bool,
    /// Settings dialog (open when Some): page and the values being edited.
    #[serde(default)]
    pub settings: Option<crate::panels::settings::SettingsDraft>,
    /// Audio Track Mixer: effects and sends section expanded.
    #[serde(default)]
    pub mixer_fx_open: bool,
    /// Audio Track Mixer ▸ Show/Hide Tracks: strip ids (audio tracks, submixes) not shown.
    #[serde(default)]
    pub mixer_hidden: Vec<u64>,
    /// Audio Track Mixer ▸ Meter Input(s) Only: record-armed track meters show the recording input.
    #[serde(default)]
    pub mixer_meter_input_only: bool,
    /// Audio Gain dialog draft (mode, dB values).
    #[serde(default)]
    pub audio_gain: AudioGainDraft,
    /// Add Tracks dialog draft.
    #[serde(default)]
    pub add_tracks: AddTracksDraft,
    /// Delete Tracks dialog draft.
    #[serde(default)]
    pub delete_tracks: DeleteTracksDraft,
    /// Sequence Settings dialog draft.
    #[serde(default)]
    pub sequence_settings: SequenceSettingsDraft,
    /// Set Transition Duration dialog draft (double-click a transition).
    #[serde(default)]
    pub transition_duration: TransitionDurationDraft,
    /// On-monitor text editing (Type tool / double-click on a text layer).
    #[serde(default)]
    pub gfx_edit: Option<GfxEdit>,
    /// Pen tool: path points placed so far (sequence pixels).
    #[serde(default)]
    pub pen_points: Vec<[f64; 2]>,
    /// Link Media dialog (open when Some).
    #[serde(default)]
    pub link_media: Option<LinkMediaDraft>,
    /// Create Proxies dialog (open when Some).
    #[serde(default)]
    pub create_proxies: Option<ProxyDraft>,
    /// Project Manager dialog (open when Some).
    #[serde(default)]
    pub project_manager: Option<ProjectManagerDraft>,
    /// Make Offline dialog: Some(delete files?) while open.
    #[serde(default)]
    pub make_offline: Option<bool>,
    /// Open colour dialog (Interpret Footage ▸ Color Management, Sequence ▸ Color Management).
    #[serde(default)]
    pub color_dialog: Option<ColorDialog>,
    /// Free-draw (pen) mask being placed on the Program monitor (Effect Controls ▸ pen icon).
    #[serde(default)]
    pub mask_pen: Option<MaskPenDraft>,
    /// Effect presets: the preset being renamed / the Save Preset dialog (open when Some).
    #[serde(default)]
    pub save_preset: Option<SavePresetDraft>,
    /// Open Clip / Track Fx Editor windows (graphical audio-effect editors).
    #[serde(default)]
    pub audio_fx_editors: Vec<crate::panels::audio_fx_editor::FxTarget>,
    /// Synchronize / Merge Clips / Create Multi-Camera Source Sequence dialog (open when Some).
    #[serde(default)]
    pub sync_dialog: Option<SyncDraft>,
    /// Multi-Camera Record On/Off (key 0): playing in the Multi-Camera view records cuts.
    #[serde(default = "yes")]
    pub multicam_record: bool,
    /// Edit Cameras dialog (open when Some).
    #[serde(default)]
    pub edit_cameras: Option<EditCamerasDraft>,
    /// Open guide dialog (Add Guide / Save Guides as Template / Manage Guides).
    #[serde(default)]
    pub guide_dialog: Option<GuideDialog>,
    /// Open Text Properties dialog (the wrench in the Properties panel's Text section).
    #[serde(default)]
    pub text_props_dialog: Option<TextPropsDialog>,
    /// Open Window ▸ Workspaces dialog.
    #[serde(default)]
    pub workspace_dialog: Option<WorkspaceDialog>,
    /// Edit / Clip / File menu dialog (Paste Attributes, Make Subclip, Frame Hold Options, …;
    /// open when Some). See `panels::clip_dialogs`.
    #[serde(default)]
    pub clip_dialog: Option<ClipDialogDraft>,
    /// M3.11 menu items: their dialog, Dynamic Audio Waveforms, the Media Browser selection. See
    /// `panels::menu_dialogs`.
    #[serde(default)]
    pub extras: crate::panels::menu_dialogs::Extras,
    /// Essential Graphics ▸ Browse and the graphics template / Replace Fonts dialogs. See
    /// `panels::graphics_templates`.
    #[serde(default)]
    pub gfx_templates: crate::panels::graphics_templates::GfxTemplatesState,
    /// Lumetri Scopes, Timecode, Events, Progress and Reference Monitor settings. See
    /// `panels::panel_state`.
    #[serde(default)]
    pub panels: crate::panels::panel_state::PanelsState,
    /// Keyboard-only view state (maximized frame, Project panel hover scrub). See
    /// `panels::keyboard`.
    #[serde(default)]
    pub keys: crate::panels::keyboard::KeysState,
    /// Project panel: bin shown in place, bin tabs / windows, inline rename, dialogs, hover scrub
    /// (view settings and columns are engine preferences). See `panels::project`.
    #[serde(default)]
    pub project_panel: crate::panels::project::ProjectPanelUi,
    /// Media Browser view state (tree, path field, Edit Columns…). See `panels::media_browser`.
    #[serde(default)]
    pub media_browser: crate::panels::media_browser::MediaBrowserUi,
}

/// An open Edit / Clip / File menu dialog: the engine command it runs on OK and the parameters
/// being edited (the same JSON the command takes, so agents can fill it with `ui.set`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipDialogDraft {
    pub command: String,
    pub params: serde_json::Value,
    /// Extra data shown by the dialog (effect names, channel count, …); not sent.
    #[serde(default)]
    pub info: serde_json::Value,
    #[serde(default)]
    pub error: String,
}

/// A pen mask in progress: vertices placed so far, in clip pixels (`[x, y, tangent x, tangent y]`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MaskPenDraft {
    pub clip: u64,
    pub effect: usize,
    pub points: Vec<[f64; 4]>,
}

/// Save Preset dialog (Effect Controls ▸ right-click an effect ▸ Save Preset…).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavePresetDraft {
    pub clip: u64,
    pub effects: Vec<usize>,
    pub name: String,
    pub description: String,
    /// "scale" | "anchorIn" | "anchorOut" | "none"
    pub keyframes: String,
}

/// Clip ▸ Synchronize…, Merge Clips… and Create Multi-Camera Source Sequence… share one draft;
/// The Edit Cameras dialog: camera names and on/off per angle of a multi-camera source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditCamerasDraft {
    pub sequence: u64,
    pub names: Vec<String>,
    pub enabled: Vec<bool>,
}

/// `kind` says which dialog it is (`synchronize`, `merge`, `multicam`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncDraft {
    pub kind: String,
    /// Project items (Merge Clips, Create Multi-Camera).
    pub items: Vec<u64>,
    pub name: String,
    /// `in` | `out` | `timecode` | `marker` | `audio`
    pub method: String,
    pub ignore_hours: bool,
    /// Clip marker name to sync on (empty = the first marker).
    pub marker: String,
    /// Frames added to every clip but the reference.
    pub offset: i64,
    /// Synchronize: reference track (`V1`, `A2`…; empty = the lowest track).
    pub track: String,
    /// Create Multi-Camera: `camera1` | `all` | `switch`.
    pub audio: String,
    /// Create Multi-Camera: `clip` | `track` | `metadata`.
    pub camera_names: String,
    pub processed_bin: bool,
    /// Merge Clips: drop the video clip's own audio.
    pub remove_video_audio: bool,
    pub message: String,
}

impl Default for SyncDraft {
    fn default() -> Self {
        Self {
            kind: "synchronize".into(),
            items: vec![],
            name: String::new(),
            method: "audio".into(),
            ignore_hours: false,
            marker: String::new(),
            offset: 0,
            track: String::new(),
            audio: "camera1".into(),
            camera_names: "clip".into(),
            processed_bin: true,
            remove_video_audio: false,
            message: String::new(),
        }
    }
}

/// File ▸ Link Media… (shown automatically when a project opens with missing media).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkMediaDraft {
    /// Selected row (index into the missing list).
    pub row: usize,
    pub file_name: bool,
    pub extension: bool,
    pub clip_id: bool,
    pub duration: bool,
    pub media_start: bool,
    pub metadata: bool,
    pub align_timecode: bool,
    pub relink_others: bool,
    /// Search: folder and "Display only exact name matches".
    pub folder: String,
    pub exact_name: bool,
    /// Search results for the selected row: (path, ok, identity match, problems).
    pub candidates: Vec<(String, bool, Option<bool>, String)>,
    pub candidate: Option<usize>,
    /// Rows the user set offline in this session of the dialog (item ids).
    pub skipped: Vec<u64>,
    pub message: String,
}

impl Default for LinkMediaDraft {
    fn default() -> Self {
        Self {
            row: 0,
            file_name: true,
            extension: true,
            clip_id: true,
            duration: true,
            media_start: false,
            metadata: true,
            align_timecode: false,
            relink_others: true,
            folder: String::new(),
            exact_name: true,
            candidates: vec![],
            candidate: None,
            skipped: vec![],
            message: String::new(),
        }
    }
}

/// Clip ▸ Proxy ▸ Create Proxies….
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyDraft {
    pub items: Vec<u64>,
    pub preset: String,
    /// Empty = a Proxies folder next to the original media.
    pub destination: String,
}

/// File ▸ Project Manager….
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectManagerDraft {
    pub sequences: Vec<u64>,
    /// "collect" | "consolidate"
    pub mode: String,
    pub preset: String,
    pub exclude_unused: bool,
    pub handles: u32,
    pub include_proxies: bool,
    pub include_previews: bool,
    pub destination: String,
    /// Last dry-run estimate: (original bytes, resulting bytes, files).
    pub estimate: Option<(u64, u64, usize)>,
    pub message: String,
}

impl Default for ProjectManagerDraft {
    fn default() -> Self {
        Self {
            sequences: vec![],
            mode: "collect".into(),
            preset: "prores_lt".into(),
            exclude_unused: true,
            handles: 30,
            include_proxies: true,
            include_previews: false,
            destination: String::new(),
            estimate: None,
            message: String::new(),
        }
    }
}

/// Colour dialogs: drafts are applied with one engine command on OK.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ColorDialog {
    /// `clip.interpretFootage`: the media items and the chosen colour space id ("auto" = metadata).
    Interpret { items: Vec<u64>, color_space: String },
    /// `sequence.colorSettings`.
    Sequence { working_space: String, wide_gamut: bool, auto_tone_map: bool },
}

/// The Audio Gain dialog (Clip ▸ Audio Gain…, G).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioGainDraft {
    /// "set" | "adjust" | "normalizeMax" | "normalizeAll"
    pub mode: String,
    pub set_db: f64,
    pub adjust_db: f64,
    pub max_peak_db: f64,
    pub all_peaks_db: f64,
}

/// The Add Tracks dialog (Sequence ▸ Add Tracks…): for video, audio and audio submix tracks, how
/// many to add and where (`*_after`: how many tracks of the kind come before the new ones; 0 is
/// "Before First Track"), and the type of the audio and submix tracks (`standard` / `stereo`,
/// `5.1`, `adaptive`, `mono`). Premiere's defaults: one video track and one audio track after the
/// last ones, no submix track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddTracksDraft {
    pub video: u32,
    pub video_after: usize,
    pub audio: u32,
    pub audio_after: usize,
    pub audio_type: String,
    pub submix: u32,
    pub submix_after: usize,
    pub submix_type: String,
}

impl Default for AddTracksDraft {
    fn default() -> Self {
        Self { video: 1, video_after: 0, audio: 1, audio_after: 0, audio_type: "standard".into(), submix: 0, submix_after: 0, submix_type: "stereo".into() }
    }
}

/// The Set Transition Duration dialog (double-click a transition in the Timeline): which
/// transition, and its duration in frames.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransitionDurationDraft {
    pub transition: u64,
    pub frames: i64,
}

/// The Sequence Settings dialog (Sequence ▸ Sequence Settings…), filled from the active sequence
/// when it opens (`panels::sequence_settings::open`). `tab`: `general`, `color` or `vr`; the
/// timebase is `fps_num`/`fps_den`; `mix`: `Stereo`, `Mono`, `5.1` or `Adaptive`; `working_space`:
/// a [`filmcraft_color::WorkingSpace`] id. `scale_motion` is "Scale motion effects proportionally
/// when changing frame size", on by default as in Premiere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SequenceSettingsDraft {
    pub tab: String,
    /// The sequence's name (its Project panel item); a blank name keeps the current one.
    pub name: String,
    /// File ▸ New ▸ Sequence…: the dialog makes a new sequence (`file.newSequence`) with these
    /// settings and `video_tracks` / `audio_tracks` tracks, instead of changing the active one.
    pub new_sequence: bool,
    pub video_tracks: u32,
    pub audio_tracks: u32,
    pub fps_num: i64,
    pub fps_den: i64,
    pub width: u32,
    pub height: u32,
    pub scale_motion: bool,
    pub drop_frame: bool,
    pub mix: String,
    pub sample_rate: u32,
    pub max_render_quality: bool,
    pub working_space: String,
    pub wide_gamut: bool,
    pub auto_tone_map: bool,
}

impl Default for SequenceSettingsDraft {
    fn default() -> Self {
        Self {
            tab: "general".into(),
            name: String::new(),
            new_sequence: false,
            video_tracks: 3,
            audio_tracks: 3,
            fps_num: 24_000,
            fps_den: 1001,
            width: 1920,
            height: 1080,
            scale_motion: true,
            drop_frame: false,
            mix: "Stereo".into(),
            sample_rate: 48_000,
            max_render_quality: false,
            working_space: "rec709".into(),
            wide_gamut: false,
            auto_tone_map: true,
        }
    }
}

/// The Delete Tracks dialog (Sequence ▸ Delete Tracks…): per kind, whether to delete and which
/// track (`"empty"` = All Empty Tracks, or a track name such as `"V2"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeleteTracksDraft {
    pub video: bool,
    pub video_target: String,
    pub audio: bool,
    pub audio_target: String,
}

impl Default for DeleteTracksDraft {
    fn default() -> Self {
        Self { video: false, video_target: "empty".into(), audio: false, audio_target: "empty".into() }
    }
}

/// Draft of the Text Properties dialog of a text layer: its type (point text, or paragraph text
/// wrapped in a box) and text styling.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextPropsDialog {
    pub clip: u64,
    pub layer: usize,
    pub paragraph: bool,
    /// Vertical text is always point text.
    pub vertical: bool,
    pub ligatures: bool,
    /// Ligatures when the dialog opened.
    pub ligatures_was: bool,
}

/// The text layer being edited on the Program monitor: caret and selection anchor are byte
/// offsets into the layer's text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GfxEdit {
    pub clip: u64,
    pub layer: usize,
    pub caret: usize,
    pub anchor: usize,
}

fn shown() -> bool {
    true
}

fn captions_tab() -> String {
    "Captions".into()
}

fn text_filter() -> String {
    "text".into()
}

/// The Transcribe options (Premiere's "Create transcription" dialog: language, speaker labeling).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscribeDraft {
    /// ISO 639-1 code, or `auto` to detect it.
    pub language: String,
    /// Separate speakers (off: one speaker, as for a solo voice-over).
    pub speakers: bool,
}

impl Default for TranscribeDraft {
    fn default() -> Self {
        Self { language: "en".into(), speakers: false }
    }
}

fn yes() -> bool {
    true
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            language: crate::i18n::Language::default(),
            tool: Tool::Selection,
            mode: Mode::Edit,
            workspace: "Editing".into(),
            dock: crate::dock::workspace("Editing"),
            focused: PanelKind::Timeline,
            timeline: TimelineView::default(),
            program: MonitorView::default(),
            source: MonitorView::default(),
            project_search: String::new(),
            effects_search: String::new(),
            expanded_bins: vec![],
            expanded_fx: vec!["Video Transitions".into(), "Video Transitions/Dissolve".into()],
            lumetri_grid_folder: None,
            collapsed_fx: vec![],
            show_menu_bar: true,
            show_header: true,
            show_status_bar: true,
            dark: true,
            show_scopes: false,
            status: String::new(),
            eyedropper: None,
            essential_sound_tab: "Edit".into(),
            tts: Default::default(),
            export: Default::default(),
            text_tab: captions_tab(),
            caption_search: String::new(),
            transcript_sel: None,
            transcript_search: String::new(),
            transcript_filter: text_filter(),
            transcript_hit: 0,
            transcript_lift: false,
            transcript_pause: None,
            transcribe_dialog: None,
            play_after_render: true,
            mixer_fx_open: false,
            mixer_hidden: Vec::new(),
            mixer_meter_input_only: false,
            settings: None,
            audio_gain: AudioGainDraft::default(),
            add_tracks: AddTracksDraft::default(),
            delete_tracks: DeleteTracksDraft::default(),
            sequence_settings: SequenceSettingsDraft::default(),
            transition_duration: TransitionDurationDraft::default(),
            gfx_edit: None,
            pen_points: vec![],
            link_media: None,
            create_proxies: None,
            project_manager: None,
            make_offline: None,
            color_dialog: None,
            mask_pen: None,
            save_preset: None,
            audio_fx_editors: Vec::new(),
            sync_dialog: None,
            multicam_record: true,
            edit_cameras: None,
            guide_dialog: None,
            text_props_dialog: None,
            workspace_dialog: None,
            clip_dialog: None,
            extras: Default::default(),
            gfx_templates: Default::default(),
            panels: Default::default(),
            keys: Default::default(),
            project_panel: Default::default(),
            media_browser: Default::default(),
        }
    }
}

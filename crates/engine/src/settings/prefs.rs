use super::schema::{DARK_THEMES, LIGHT_THEMES, UI_SCALES};
use super::{Deserialize, FrameRate, Serialize, Tick};

// ------------------------------------------------------------------ category values

/// Settings ▸ General.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralPrefs {
    /// Interface language (persisted independently of projects): `en`, `ja`, `es`, `pt-br`, `uk`,
    /// `zh-cn`, `de`, `ru`, `fr`, `id`, `it`, `ko`, `cs`, `pl`, `sv`, `tr`, or `system` (the default: the operating system's preferred language when the
    /// interface has it, otherwise English).
    pub interface_language: String,
    /// "At Startup": `showHome` (FilmCraft: the demo project), `openMostRecent`, `emptyProject`.
    pub at_startup: String,
    /// "When Opening a Project": `showOpenDialog` | `showHome`.
    pub when_opening_project: String,
    /// Bins ▸ Double-click / + Cmd / + Opt: `openInPlace` | `openNewTab` | `openNewWindow`.
    pub bins_double_click: String,
    pub bins_cmd_double_click: String,
    pub bins_opt_double_click: String,
    /// Projects ▸ Double-click / + Opt.
    pub projects_double_click: String,
    pub projects_opt_double_click: String,
    pub show_event_indicator: bool,
    pub show_tool_tips: bool,
    pub show_rich_tool_tips: bool,
    pub show_workspace_reset_warning: bool,
    pub show_project_load_errors: bool,
    pub show_compatibility_issues: bool,
    pub show_project_bin_in_tab: bool,
    pub show_mask_tracker_preview: bool,
    /// Most recently opened / saved projects (newest first; not shown in the dialog).
    pub recent_projects: Vec<String>,
}

impl Default for GeneralPrefs {
    fn default() -> Self {
        Self {
            interface_language: "system".into(),
            at_startup: "showHome".into(),
            when_opening_project: "showOpenDialog".into(),
            bins_double_click: "openInPlace".into(),
            bins_cmd_double_click: "openNewTab".into(),
            bins_opt_double_click: "openNewWindow".into(),
            projects_double_click: "openNewTab".into(),
            projects_opt_double_click: "openNewWindow".into(),
            show_event_indicator: true,
            show_tool_tips: true,
            show_rich_tool_tips: true,
            show_workspace_reset_warning: true,
            show_project_load_errors: true,
            show_compatibility_issues: true,
            show_project_bin_in_tab: true,
            show_mask_tracker_preview: true,
            recent_projects: Vec::new(),
        }
    }
}

/// Settings ▸ Appearance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppearancePrefs {
    /// Legacy single "Color Theme": `darkest` | `dark` | `light`. Setting it (old settings files,
    /// automation, View ▸ Appearance) selects that theme and fixes the mode to its family. It is
    /// kept in line with the fields below ([`AppearancePrefs::sync_color_theme`]): the light or
    /// dark theme of a fixed mode, and the dark theme in Auto (whose shown theme depends on the
    /// system; read `appearanceMode` for that).
    pub color_theme: String,
    /// "Appearance Mode": `auto` (follow the operating system) | `dark` (default) | `light`.
    pub appearance_mode: String,
    /// The theme shown in dark mode: `darkest` (default) | `dark`.
    pub dark_theme: String,
    /// The theme shown in light mode: `light`.
    pub light_theme: String,
    /// Theme shortcuts for the header context menu.
    pub favorite_themes: Vec<String>,
    /// "Accessible color contrast": brighter secondary text and borders.
    pub accessible_contrast: bool,
    /// Highlight (accent) colour of selections, focus and primary buttons (`#rrggbb`).
    pub highlight_color: String,
    /// "UI Scale": `auto` (default: the scale the operating system reports) or a fixed
    /// percentage of [`UI_SCALES`] (`100` = one point per physical pixel).
    pub ui_scale: String,
    /// "Animate zoom, scrolling and menus" (#2325): off, the Timeline zooms and scrolls in one
    /// step and menus, collapsing sections and tooltips appear without a fade.
    pub animations: bool,
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            color_theme: "darkest".into(),
            appearance_mode: "dark".into(),
            dark_theme: "darkest".into(),
            light_theme: "light".into(),
            favorite_themes: Vec::new(),
            accessible_contrast: false,
            highlight_color: DEFAULT_HIGHLIGHT.into(),
            ui_scale: "auto".into(),
            animations: true,
        }
    }
}

impl AppearancePrefs {
    /// The fixed UI scale (physical pixels per point) "UI Scale" asks for, or `None` for `auto`
    /// (and for a value outside [`UI_SCALES`]), which keeps the operating system's scale.
    pub fn ui_scale_factor(&self) -> Option<f32> {
        // `auto` doesn't parse; every other listed value is a positive whole percentage
        UI_SCALES.iter().any(|(k, _)| *k == self.ui_scale).then(|| self.ui_scale.parse::<f32>().ok()).flatten().map(|pct| pct / 100.0)
    }

    /// The theme to show (`darkest` | `dark` | `light`): the light or dark theme of the mode. Auto
    /// follows `system_light` (the operating system's appearance) and is dark without an answer.
    pub fn shown_theme(&self, system_light: Option<bool>) -> &str {
        let light = match self.appearance_mode.as_str() {
            "light" => true,
            "auto" => system_light == Some(true),
            _ => false,
        };
        if light { &self.light_theme } else { &self.dark_theme }
    }

    /// Select `theme` (a Color Theme value) as the theme of its family and fix the mode to that
    /// family. Unknown names are ignored.
    pub fn select_theme(&mut self, theme: &str) {
        if LIGHT_THEMES.iter().any(|(v, _)| *v == theme) {
            self.light_theme = theme.into();
            self.appearance_mode = "light".into();
        } else if DARK_THEMES.iter().any(|(v, _)| *v == theme) {
            self.dark_theme = theme.into();
            self.appearance_mode = "dark".into();
        }
    }

    /// Bring the legacy `color_theme` in line with the mode and theme choices, so clients that
    /// still read it get the theme a fixed mode shows (the dark theme in Auto).
    pub fn sync_color_theme(&mut self) {
        let theme = self.shown_theme(None).to_string();
        self.color_theme = theme;
    }

    /// The mode after this one in the header button's cycle: Auto, Light, Dark, Auto…
    pub fn next_mode(&self) -> &'static str {
        match self.appearance_mode.as_str() {
            "auto" => "light",
            "light" => "dark",
            _ => "auto",
        }
    }
}

pub const DEFAULT_HIGHLIGHT: &str = "#2f6bdf";

/// Settings ▸ Audio Hardware (the cpal output on desktop).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioHardwarePrefs {
    /// "Device Class": the audio host (`CoreAudio`, `WASAPI`, `ALSA`…); empty = system default.
    pub device_class: String,
    /// "Default Input" / "Default Output": device names; empty = the system default device.
    pub default_input: String,
    pub default_output: String,
    /// "I/O Buffer Size" (samples).
    pub buffer_size: u32,
    /// "Sample Rate" (Hz).
    pub sample_rate: u32,
    /// "Attempt to force hardware to document sample rate": open the device at the sequence rate.
    pub force_document_rate: bool,
    /// Output Mapping: the device channels (0-based) the programme's left and right go to.
    pub map_left: u32,
    pub map_right: u32,
}

impl Default for AudioHardwarePrefs {
    fn default() -> Self {
        Self {
            device_class: String::new(),
            default_input: String::new(),
            default_output: String::new(),
            buffer_size: 512,
            sample_rate: 48_000,
            force_document_rate: false,
            map_left: 0,
            map_right: 1,
        }
    }
}

/// Settings ▸ Color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorPrefs {
    pub display_color_management: bool,
    pub extended_dynamic_range: bool,
    /// HDR Graphics White (nits): `100` | `203` | `300`.
    pub hdr_graphics_white: String,
}

impl Default for ColorPrefs {
    fn default() -> Self {
        Self { display_color_management: false, extended_dynamic_range: false, hdr_graphics_white: "203".into() }
    }
}

/// Settings ▸ Graphics (Text, Shapes, Closed Captions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphicsPrefs {
    pub ligatures: bool,
    pub hindi_digits: bool,
    pub smart_quotes: bool,
    /// `ltr` | `rtl`.
    pub paragraph_direction: String,
    /// `miter` | `round` | `bevel`.
    pub text_line_join: String,
    pub text_miter_limit: f64,
    /// `butt` | `round` | `square`.
    pub text_line_cap: String,
    /// `allLines` | `perLine`.
    pub background_fill: String,
    pub default_font: String,
    pub missing_font_replacement: String,
    pub emoji_font: String,
    pub shape_line_join: String,
    pub shape_miter_limit: f64,
    pub shape_line_cap: String,
    pub caption_font: String,
    pub caption_monospace_only: bool,
    pub cea708_wide: bool,
}

impl Default for GraphicsPrefs {
    fn default() -> Self {
        Self {
            ligatures: true,
            hindi_digits: false,
            smart_quotes: true,
            paragraph_direction: "ltr".into(),
            text_line_join: "miter".into(),
            text_miter_limit: 2.5,
            text_line_cap: "butt".into(),
            background_fill: "allLines".into(),
            default_font: "Inter".into(),
            missing_font_replacement: "Inter".into(),
            emoji_font: String::new(),
            shape_line_join: "miter".into(),
            shape_miter_limit: 2.5,
            shape_line_cap: "butt".into(),
            caption_font: "JetBrains Mono".into(),
            caption_monospace_only: true,
            cea708_wide: false,
        }
    }
}

/// Settings ▸ Media Analysis & Transcription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaAnalysisPrefs {
    /// "Cache analysis results for re-use": `mediaCache` | `project`.
    pub cache_results: String,
    pub analyze_imported_media: bool,
    /// "Automatically transcribe clips".
    pub auto_transcribe: bool,
    /// `allImported` (transcribe on import) | `sequenceClips` (only clips edited into sequences).
    pub auto_transcribe_scope: String,
    /// "Speaker Labeling": `on` | `off`.
    pub speaker_labeling: String,
    pub language_auto_detect: bool,
    /// "Default language" (ISO 639-1).
    pub default_language: String,
    /// Speech model (`transcript.models`).
    pub whisper_model: String,
}

impl Default for MediaAnalysisPrefs {
    fn default() -> Self {
        Self {
            cache_results: "mediaCache".into(),
            analyze_imported_media: true,
            auto_transcribe: false,
            auto_transcribe_scope: "sequenceClips".into(),
            speaker_labeling: "on".into(),
            language_auto_detect: false,
            default_language: "en".into(),
            whisper_model: filmcraft_speech::models::DEFAULT_MODEL.into(),
        }
    }
}

/// Settings ▸ Media Cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaCachePrefs {
    /// Media Cache Files location; empty = `<data dir>/Media Cache`.
    pub location: String,
    pub save_next_to_media: bool,
    /// Media Cache Database location; empty = the files location.
    pub database_location: String,
    /// `never` | `olderThan` | `exceedsSize`.
    pub management: String,
    pub older_than_days: u32,
    pub max_size_gb: u32,
}

impl Default for MediaCachePrefs {
    fn default() -> Self {
        Self {
            location: String::new(),
            save_next_to_media: false,
            database_location: String::new(),
            management: "never".into(),
            older_than_days: 90,
            max_size_gb: 92,
        }
    }
}

/// Settings ▸ Memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MemoryPrefs {
    /// "RAM reserved for other applications" (GB).
    pub ram_reserved_gb: u32,
    /// Decoded-frame cache budget of the monitors and thumbnails (MB).
    pub frame_cache_mb: u32,
}

impl Default for MemoryPrefs {
    fn default() -> Self {
        Self { ram_reserved_gb: 4, frame_cache_mb: 768 }
    }
}

/// Settings ▸ Timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimelinePrefs {
    pub video_transition_duration: f64,
    /// `frames` | `seconds`.
    pub video_transition_unit: String,
    pub audio_transition_duration: f64,
    pub audio_transition_unit: String,
    pub still_image_duration: f64,
    pub still_image_unit: String,
    /// "Timeline Playback Auto-Scrolling": `noScroll` | `pageScroll` | `smoothScroll`.
    pub auto_scroll: String,
    /// "Timeline Mouse Scrolling": `vertical` | `horizontal`.
    pub mouse_scrolling: String,
    /// Default Audio Tracks per media type: `useFile` | `mono` | `stereo` | `5.1` | `adaptive`.
    pub mono_media_tracks: String,
    pub stereo_media_tracks: String,
    pub surround_media_tracks: String,
    pub multichannel_mono_media_tracks: String,
    pub focus_timeline_on_edit: bool,
    pub snap_playhead: bool,
    pub return_to_beginning: bool,
    pub out_of_sync_unlinked: bool,
    pub play_after_rendering: bool,
    pub clip_mismatch_warning: bool,
    pub match_frame_sets_in: bool,
    pub restore_open_sequences: bool,
    pub add_tracks_automatically: bool,
    /// Files dropped onto the Timeline are placed directly on it.
    pub drop_import_to_timeline: bool,
    /// Sequence ▸ Selection Follows Playhead: moving the playhead selects the clips under it on
    /// targeted tracks (#3089: persisted, so it survives a restart like the sibling toggles).
    pub selection_follows_playhead: bool,
}

impl Default for TimelinePrefs {
    fn default() -> Self {
        Self {
            video_transition_duration: 30.0,
            video_transition_unit: "frames".into(),
            audio_transition_duration: 1.0,
            audio_transition_unit: "seconds".into(),
            still_image_duration: 5.0,
            still_image_unit: "seconds".into(),
            auto_scroll: "pageScroll".into(),
            mouse_scrolling: "vertical".into(),
            mono_media_tracks: "useFile".into(),
            stereo_media_tracks: "useFile".into(),
            surround_media_tracks: "useFile".into(),
            multichannel_mono_media_tracks: "useFile".into(),
            focus_timeline_on_edit: false,
            snap_playhead: true,
            return_to_beginning: true,
            out_of_sync_unlinked: false,
            play_after_rendering: true,
            clip_mismatch_warning: true,
            match_frame_sets_in: false,
            restore_open_sequences: true,
            add_tracks_automatically: true,
            drop_import_to_timeline: true,
            selection_follows_playhead: false,
        }
    }
}

fn duration_in(value: f64, unit: &str, rate: FrameRate) -> Tick {
    let d = if unit == "frames" { rate.tick_of(value.max(1.0).round() as i64) } else { rate.snap_nearest(Tick::from_seconds_f64(value.max(0.0))) };
    d.max(rate.frame_duration())
}

impl TimelinePrefs {
    /// "Still Image Default Duration" at a sequence rate (whole frames, at least one).
    pub fn still_duration(&self, rate: FrameRate) -> Tick {
        duration_in(self.still_image_duration, &self.still_image_unit, rate)
    }
    /// "Video Transition Default Duration".
    pub fn video_transition_duration(&self, rate: FrameRate) -> Tick {
        duration_in(self.video_transition_duration, &self.video_transition_unit, rate)
    }
    /// "Audio Transition Default Duration".
    pub fn audio_transition_duration(&self, rate: FrameRate) -> Tick {
        duration_in(self.audio_transition_duration, &self.audio_transition_unit, rate)
    }
}

/// Settings ▸ Plugins.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PluginPrefs {
    pub developer_mode: bool,
}

/// Indeterminate Media Timebase choices → frame rates.
pub fn timebase_rate(v: &str) -> FrameRate {
    match v {
        "23.976" => FrameRate::FPS_23_976,
        "24" => FrameRate::FPS_24,
        "25" => FrameRate::FPS_25,
        "30" => FrameRate::FPS_30,
        "50" => FrameRate::FPS_50,
        "59.94" | "59.94df" => FrameRate::FPS_59_94,
        "60" => FrameRate::FPS_60,
        _ => FrameRate::FPS_29_97,
    }
}

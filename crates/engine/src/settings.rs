//! **Settings** (Premiere: app menu ▸ Settings ▸ <category>; the window is titled "Preferences").
//!
//! The values live in [`crate::autosave::Preferences`] (persisted as `preferences.json` in the
//! per-user data directory, never in the project). Every value is a dotted camelCase key whose first
//! segment is the category id (`timeline.stillImageDuration`, `labels.colors.violet.name`), so the
//! `prefs.get` / `prefs.set` / `prefs.reset` commands reach all of them from the CLI, the control
//! channel and MCP, and the Settings dialog's controls carry the automation id `settings.<key>`.
//!
//! This module holds the categories' value types, the field schema the dialog is drawn from
//! ([`categories`]; also returned by `prefs.schema` so agents can discover choices and ranges),
//! validation/migration, and the engine-side helpers that make the values take effect (default
//! durations, label colours, the media cache, audio output mapping, smart quotes…).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use filmcraft_project::Label;
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

mod commands;
mod labels;
mod media_cache;
mod prefs;
mod schema;

pub use commands::commands;
pub use labels::{
    DEFAULT_LABEL_PRESET, LABEL_PRESETS_DIR, LabelColor, LabelDefaults, LabelPrefs, LabelPreset, PresetLabel, hex, is_builtin_label_preset, label_id, parse_hex,
};
pub use media_cache::{CUSTOM_CACHE_FOLDER, cache_usage, clean, enforce_policy, media_cache_dir, previews_root};
pub use prefs::{
    AppearancePrefs, AudioHardwarePrefs, ColorPrefs, DEFAULT_HIGHLIGHT, GeneralPrefs, GraphicsPrefs, MediaAnalysisPrefs, MediaCachePrefs, MemoryPrefs,
    PluginPrefs, TimelinePrefs, timebase_rate,
};
pub use schema::{
    APPEARANCE_MODES, Category, DARK_THEMES, DURATION_UNITS, DeviceList, Field, Kind, LIGHT_THEMES, Row, UI_SCALES, categories, category, field, fields,
    is_unit_key, migrate, sanitize, validate,
};

/// Version of the preferences file layout ([`crate::autosave::Preferences::version`]).
/// v1: Auto Save, Audio (mixer automation), Playback (pre/postroll), Trim, Media (proxies), guides,
/// Essential Sound presets. v2: every Settings category.
pub const PREFS_VERSION: u32 = 2;
const SPEAKERS: &[(&str, &str)] = &[("on", "Label speakers"), ("off", "Don't label speakers")];
const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ru", "Russian"),
    ("hi", "Hindi"),
];
const CACHE_MGMT: &[(&str, &str)] = &[
    ("never", "Do not delete cache files automatically"),
    ("olderThan", "Automatically delete cache files older than"),
    ("exceedsSize", "Automatically delete oldest cache files when cache exceeds"),
];
const AUTO_SCROLL: &[(&str, &str)] = &[("noScroll", "No Scroll"), ("pageScroll", "Page Scroll"), ("smoothScroll", "Smooth Scroll")];
const MOUSE_SCROLL: &[(&str, &str)] = &[("vertical", "Vertical"), ("horizontal", "Horizontal")];
const HW_DECODE: &[(&str, &str)] = &[("auto", "Auto"), ("off", "Off")];
const TRACKS: &[(&str, &str)] = &[("useFile", "Use File"), ("mono", "Mono"), ("stereo", "Stereo"), ("5.1", "5.1"), ("adaptive", "Adaptive")];
pub const DURATION_UNITS: &[(&str, &str)] = &[("frames", "Frames"), ("seconds", "Seconds")];

/// The Settings categories in Premiere's order (Adobe-cloud-only ones are left out; Plugins is a
/// stub).
pub fn categories() -> &'static [Category] {
    CATEGORIES
}

pub fn category(id: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.id == id)

static CATEGORIES: &[Category] = &[
    Category {
        id: "general",
        title: "General",
        rows: &[
            f(
                "general.interfaceLanguage",
                "Interface Language",
                Kind::Choice(&[
                    ("system", "System Language"),
                    ("en", "English"),
                    ("ja", "日本語"),
                    ("es", "Español"),
                    ("pt-br", "Português (Brasil)"),
                    ("uk", "Українська"),
                    ("zh-cn", "简体中文"),
                    ("de", "Deutsch"),
                    ("ru", "Русский"),
                    ("fr", "Français"),
                    ("id", "Bahasa Indonesia"),
                    ("it", "Italiano"),
                    ("ko", "한국어"),
                    ("ar", "العربية"),
                ]),
                true,
            ),
            f("general.atStartup", "At Startup", Kind::Choice(STARTUP), true),
            f("general.whenOpeningProject", "When Opening a Project", Kind::Choice(OPENING), false),
            Row::Group(
                "Bins",
                &[
                    f("general.binsDoubleClick", "Double-click", Kind::Choice(BIN_OPEN), false),
                    f("general.binsCmdDoubleClick", "+ Cmd", Kind::Choice(BIN_OPEN), false),
                    f("general.binsOptDoubleClick", "+ Opt", Kind::Choice(BIN_OPEN), false),
                ],
                "Projects",
                    f("general.projectsDoubleClick", "Double-click", Kind::Choice(PROJECT_OPEN), false),
                    f("general.projectsOptDoubleClick", "+ Opt", Kind::Choice(PROJECT_OPEN), false),
            b("general.showEventIndicator", "Show Event Indicator", false),
            b("general.showToolTips", "Show Tool Tips", true),
            sub("general.showRichToolTips", "Show Rich Tool Tips", Kind::Bool, "general.showToolTips", false),
            b("general.showWorkspaceResetWarning", "Show Workspace Reset Warning dialog on double click", false),
            b("general.showProjectLoadErrors", "Show Project Load Error Dialog", false),
            b("general.showCompatibilityIssues", "Show system compatibility issues at startup", false),
            b("general.showProjectBinInTab", "Show \"Project:\" and \"Bin:\" in Project panel tab", false),
            b("general.showMaskTrackerPreview", "Show Mask Tracker Preview", false),
    },
        id: "appearance",
        title: "Appearance",
            // Drawn by the dialog as the Appearance Mode selector above the light and dark theme
            // cards; the fields below keep their keys, validation and `prefs.schema` entries.
            Row::Custom("appearanceModes"),
            f("appearance.appearanceMode", "Appearance Mode", Kind::Choice(APPEARANCE_MODES), true),
            f("appearance.lightTheme", "Light Theme", Kind::Choice(LIGHT_THEMES), true),
            f("appearance.darkTheme", "Dark Theme", Kind::Choice(DARK_THEMES), true),
            f("appearance.colorTheme", "Color Theme", Kind::Choice(THEMES), true),
            b("appearance.accessibleContrast", "Accessible color contrast", true),
            f("appearance.highlightColor", "Highlight Color", Kind::Color, true),
            f("appearance.uiScale", "UI Scale", Kind::Choice(UI_SCALES), true),
        id: "audio",
        title: "Audio",
            f("audio.automatchTime", "Automatch Time", float(0.0, 30.0, 3, "seconds"), true),
            f("audio.mixdownType", "5.1 Mixdown Type", Kind::Choice(MIXDOWN), true),
            f("audio.largeVolumeAdjustment", "Large Volume Adjustment", float(0.0, 96.0, 0, "dB"), true),
            b("audio.sumToMonoInSource", "Sum multichannel outputs to mono in Source Monitor", false),
            b("audio.scrubAudio", "Play audio while scrubbing in Source and Program Monitors", false),
            b("audio.maintainPitchShuttling", "Maintain pitch while shuttling", false),
            b("audio.muteInputDuringRecording", "Mute input during timeline recording", false),
            b("audio.generateWaveformsOnImport", "Generate waveforms automatically during import", false),
            b("audio.multithreadedWaveforms", "Generate waveforms and conform audio using multithreading", false),
            b("audio.renderAudioWithVideo", "Render audio when rendering video", false),
            b("audio.autoTagAudioTypes", "Auto-tag audio types in the timeline", false),
            b("audio.alwaysOverrideAudioTags", "Always override audio tags", false),
                "Automation Keyframe Optimization",
                    b("audio.linearKeyframeThinning", "Linear keyframe thinning", true),
                    b("audio.minimumTimeIntervalThinning", "Minimum time interval thinning", true),
                    sub("audio.minimumTimeMs", "Minimum time", int(1.0, 10_000.0, "milliseconds"), "audio.minimumTimeIntervalThinning", true),
            Row::Group("Render Edit in Audition files to:", &[f("audio.editInAuditionLocation", "Location", Kind::Choice(AUDITION), false)]),
        id: "audioHardware",
        title: "Audio Hardware",
            f("audioHardware.deviceClass", "Device Class", Kind::Device(DeviceList::Hosts), true),
            f("audioHardware.defaultInput", "Default Input", Kind::Device(DeviceList::Inputs), false),
            f("audioHardware.defaultOutput", "Default Output", Kind::Device(DeviceList::Outputs), true),
            f("audioHardware.bufferSize", "I/O Buffer Size", Kind::Choice(BUFFERS), true),
            f("audioHardware.sampleRate", "Sample Rate", Kind::Choice(RATES), true),
            b("audioHardware.forceDocumentRate", "Attempt to force hardware to document sample rate", true),
                "Output Mapping",
                    f("audioHardware.mapLeft", "Left channel to device output", int(0.0, 63.0, ""), true),
                    f("audioHardware.mapRight", "Right channel to device output", int(0.0, 63.0, ""), true),
                    Row::Custom("outputMapping"),
        id: "autoSave",
        title: "Auto Save",
                "Local Projects",
                    b("autoSave.enabled", "Automatically save projects", true),
                    sub("autoSave.intervalMinutes", "Automatically Save Every", int(1.0, 1440.0, "minute(s)"), "autoSave.enabled", true),
                    sub("autoSave.maxVersions", "Maximum Project Versions", int(1.0, 1000.0, ""), "autoSave.enabled", true),
                    sub("autoSave.saveCurrentProject", "Auto Save also saves the current project(s)", Kind::Bool, "autoSave.enabled", true),
                "Crash Recovery",
                    b("autoSave.recoveryJournal", "Keep a recovery copy of unsaved changes", true),
                    sub("autoSave.recoveryIntervalSeconds", "Update it at least every", int(1.0, 600.0, "second(s)"), "autoSave.recoveryJournal", true),
                    Row::Note(
                        "Changes are copied in the background a moment after each edit. If FilmCraft quits unexpectedly, they are offered the next time it starts.",
                    Row::Custom("autoSaveStatus"),
        id: "color",
        title: "Color",
                "Display Color Settings",
                    b("color.displayColorManagement", "Display Color Management", false),
                    b("color.extendedDynamicRange", "Extended Dynamic Range Monitoring", false),
                    f("color.hdrGraphicsWhite", "HDR Graphics White (Nits)", Kind::Choice(HDR_WHITE), false),
            Row::Note("For color management parameters, go to the Settings tab in the Lumetri Color panel."),
        id: "graphics",
        title: "Graphics",
                "Text",
                    b("graphics.ligatures", "Ligatures", true),
                    b("graphics.hindiDigits", "Hindi digits", false),
                    b("graphics.smartQuotes", "Smart quotes", true),
                    f("graphics.paragraphDirection", "Default paragraph direction", Kind::Choice(DIRECTION), false),
                    f("graphics.textLineJoin", "Stroke line join", Kind::Choice(JOIN), false),
                    f("graphics.textMiterLimit", "Miter limit", float(1.0, 100.0, 1, ""), false),
                    f("graphics.textLineCap", "Stroke line cap", Kind::Choice(CAP), false),
                    f("graphics.backgroundFill", "Background fill", Kind::Choice(BG_FILL), false),
                    f("graphics.defaultFont", "Default subtitle and text font", Kind::Font, true),
                    f("graphics.missingFontReplacement", "Missing font replacement", Kind::Font, false),
                    f("graphics.emojiFont", "Default Emoji font", Kind::Font, false),
                "Shapes",
                    f("graphics.shapeLineJoin", "Stroke line join", Kind::Choice(JOIN), false),
                    f("graphics.shapeMiterLimit", "Miter limit", float(1.0, 100.0, 1, ""), false),
                    f("graphics.shapeLineCap", "Stroke line cap", Kind::Choice(CAP), false),
                "Closed Captions",
                    f("graphics.captionFont", "Default closed caption font", Kind::Font, false),
                    b("graphics.captionMonospaceOnly", "Monospace only", false),
                    b("graphics.cea708Wide", "Always import embedded CEA-708 closed captions with wide aspect.", false),
        id: "labels",
        title: "Labels",
            Row::Custom("labelColors"),
                "Label Defaults",
                    f("labels.defaults.movie", "Movie (audio and video)", Kind::Label, true),
                    f("labels.defaults.video", "Video", Kind::Label, true),
                    f("labels.defaults.audio", "Audio", Kind::Label, true),
                    f("labels.defaults.still", "Still", Kind::Label, true),
                    f("labels.defaults.sequence", "Sequence", Kind::Label, true),
                    f("labels.defaults.dynamicLink", "Dynamic Link", Kind::Label, false),
                    f("labels.defaults.bin", "Bin", Kind::Label, false),
                    f("labels.defaults.captions", "Captions", Kind::Label, false),
            Row::Custom("labelPresets"),
        id: "media",
        title: "Media",
            f("media.indeterminateTimebase", "Indeterminate Media Timebase", Kind::Choice(TIMEBASE), true),
            f("media.timecode", "Timecode", Kind::Choice(TIMECODE), false),
            f("media.frameCount", "Frame Count", Kind::Choice(FRAME_COUNT), false),
            f("media.defaultMediaScaling", "Default Media Scaling", Kind::Choice(SCALING), true),
            b("media.writeXmpId", "Write XMP ID to files on import", false),
            b("media.validateContentCredentials", "Validate Content Credentials on import", false),
            b("media.writeClipMarkersToXmp", "Write clip markers to XMP", false),
            b("media.enableXmpLinking", "Enable clip and XMP metadata linking", false),
            b("media.includeCaptionsOnImport", "Include captions on import", false),
            b("media.enableProxies", "Enable proxies", true),
            b("media.allowDuplicateMedia", "Allow duplicate media during project import", false),
            b("media.createFolderForImportedProjects", "Create folder for imported projects", false),
            b("media.autoHideDependentClips", "Automatically Hide Dependent Clips", false),
            b("media.importImageSequences", "Import numbered stills as image sequences", false),
                "Growing Files",
                    b("media.refreshGrowingFiles", "Automatically refresh growing files", true),
                    b("media.resumeGrowingPlayback", "Automatically resume playback for growing files in Source Monitor", false),
                    sub("media.growingRefreshSeconds", "Refresh growing Files Every", int(1.0, 3600.0, "seconds"), "media.refreshGrowingFiles", true),
            b("media.hardwareDecoding", "Enable hardware accelerated decoding (requires restart)", false),
            b("media.proresHardwareEncoding", "Enable ProRes hardware accelerated encoding, if available", false),
        id: "mediaAnalysis",
        title: "Media Analysis & Transcription",
            f("mediaAnalysis.cacheResults", "Cache analysis results for re-use", Kind::Choice(ANALYSIS_CACHE), false),
                "Media analysis and transcriptions will be cached in the Media Cache folder. Reanalysis won't be required if the media is imported into another project on this computer.",
            Row::Group("Media Analysis", &[b("mediaAnalysis.analyzeImportedMedia", "Analyze all imported media to search for visuals or audio", false)]),
                "Transcription",
                    b("mediaAnalysis.autoTranscribe", "Automatically transcribe clips", true),
                    sub("mediaAnalysis.autoTranscribeScope", "Transcription preferences", Kind::Choice(TRANSCRIBE_SCOPE), "mediaAnalysis.autoTranscribe", true),
                    f("mediaAnalysis.speakerLabeling", "Speaker Labeling", Kind::Choice(SPEAKERS), true),
                    b("mediaAnalysis.languageAutoDetect", "Enable language auto-detection", true),
                    f("mediaAnalysis.defaultLanguage", "Default language", Kind::Choice(LANGUAGES), true),
                    f("mediaAnalysis.whisperModel", "Speech model", Kind::Choice(MODELS), true),
        id: "mediaCache",
        title: "Media Cache",
                "Media Cache Files",
                    f("mediaCache.location", "Location", Kind::Path, true),
                    b("mediaCache.saveNextToMedia", "Save .cfa and .pek media cache files next to original media files when possible", false),
                    Row::Button { id: "mediaCache.clean", label: "Delete…", command: "mediaCache.clean" },
                    Row::Custom("mediaCacheInfo"),
            Row::Group("Media Cache Database", &[f("mediaCache.databaseLocation", "Location", Kind::Path, false)]),
                "Media Cache Management",
                    f("mediaCache.management", "Policy", Kind::Choice(CACHE_MGMT), true),
                    f("mediaCache.olderThanDays", "Delete files older than", int(1.0, 3650.0, "days"), true),
                    f("mediaCache.maxSizeGb", "Delete oldest files when the cache exceeds", int(1.0, 100_000.0, "GB"), true),
                "The media cache holds render previews of unsaved projects and other files FilmCraft can recreate. Deleting them is always safe; FilmCraft rebuilds them as needed.",
        id: "memory",
        title: "Memory",
            Row::Custom("memoryInfo"),
            f("memory.ramReservedGb", "RAM reserved for other applications", int(1.0, 1024.0, "GB"), false),
            f("memory.frameCacheMb", "Frame cache for monitors and thumbnails", int(64.0, 65_536.0, "MB"), true),
        id: "playback",
        title: "Playback",
            f("playback.prerollSeconds", "Preroll", float(0.0, 60.0, 0, "seconds"), true),
            f("playback.postrollSeconds", "Postroll", float(0.0, 60.0, 0, "seconds"), true),
            f("playback.stepManyFrames", "Step forward/back many", int(1.0, 1000.0, "frames"), true),
            b("playback.pauseEncoderQueue", "Pause Media Encoder queue during playback", false),
            b("playback.enableTransmit", "Enable Mercury Transmit", false),
            b("playback.disableVideoInBackground", "Disable video output when in the background", false),
            b("playback.draftDecode", "Draft decoding at reduced playback resolution (H.264: faster, some frames less filtered)", true),
            f("playback.hardwareDecoding", "Hardware decoding", Kind::Choice(HW_DECODE), true),
                "Hardware decoding: Auto uses the system's video decoder (VideoToolbox on macOS, Media Foundation on Windows, VA-API or NVDEC for H.264 and HEVC on Linux) for the streams it supports, and FilmCraft's own decoder for everything else or if the hardware fails. Media that is already open keeps its decoder until it is reopened.",
        id: "plugins",
        title: "Plugins",
            Row::Group("Plugins", &[b("plugins.developerMode", "Enable developer mode", false)]),
            Row::Note("No plugins are installed. Changes will take effect the next time you start FilmCraft."),
        id: "timeline",
        title: "Timeline",
            f("timeline.videoTransitionDuration", "Video Transition Default Duration", Kind::Duration { unit_key: "timeline.videoTransitionUnit" }, true),
            f("timeline.audioTransitionDuration", "Audio Transition Default Duration", Kind::Duration { unit_key: "timeline.audioTransitionUnit" }, true),
            f("timeline.stillImageDuration", "Still Image Default Duration", Kind::Duration { unit_key: "timeline.stillImageUnit" }, true),
            f("timeline.autoScroll", "Timeline Playback Auto-Scrolling", Kind::Choice(AUTO_SCROLL), true),
            f("timeline.mouseScrolling", "Timeline Mouse Scrolling", Kind::Choice(MOUSE_SCROLL), true),
                "Default Audio Tracks",
                    f("timeline.monoMediaTracks", "Mono Media", Kind::Choice(TRACKS), false),
                    f("timeline.stereoMediaTracks", "Stereo Media", Kind::Choice(TRACKS), false),
                    f("timeline.surroundMediaTracks", "5.1 Media", Kind::Choice(TRACKS), false),
                    f("timeline.multichannelMonoMediaTracks", "Multichannel Mono Media", Kind::Choice(TRACKS), false),
            b("timeline.focusTimelineOnEdit", "Set focus on the Timeline when performing Insert/Overwrite edits", false),
            b("timeline.snapPlayhead", "Snap playhead in Timeline when Snap is enabled", true),
            b("timeline.returnToBeginning", "At playback end, return to beginning when restarting playback", true),
            b("timeline.outOfSyncUnlinked", "Display out of sync indicators for unlinked clips", false),
            b("timeline.playAfterRendering", "Play after rendering previews", true),
            b("timeline.clipMismatchWarning", "Show Clip Mismatch Warning dialog", false),
            b("timeline.matchFrameSetsIn", "Match frame sets in point", false),
            b("timeline.restoreOpenSequences", "Restore open sequences when opening projects", true),
            b("timeline.addTracksAutomatically", "Add tracks automatically when editing source clips onto the Timeline", false),
            b("timeline.dropImportToTimeline", "Place files dropped onto the Timeline directly on the Timeline", true),
        id: "trim",
        title: "Trim",
            f("trim.largeTrimOffset", "Large Trim Offset", int(1.0, 1000.0, "frames"), true),
            f("trim.largeTrimOffsetAudio", "", int(1.0, 100_000.0, "Audio Time Units"), false),
            b("trim.selectionToolRollRipple", "Allow Selection tool to choose Roll and Ripple trims without modifier key", true),
            b("trim.toolChangesTrimType", "Allow current tool to change trim type of previously selected edit point", false),
            b("trim.shiftOverlappingClips", "Shift clips that overlap trim point during ripple trimming", false),
            sub("trim.rippleAddsEdits", "Ripple trim adds edits to keep both sides of trim in sync", Kind::Bool, "trim.shiftOverlappingClips", false),
            b("trim.playheadDeterminesLoop", "Playhead position determines trim monitor loop playback", true),
            b("trim.dynamicRippleUpdates", "Update timeline dynamically during ripple edits.", false),
];

/// Every field of every category (groups flattened), in dialog order.
pub fn fields() -> Vec<(&'static str, Field)> {
    fn walk(cat: &'static str, rows: &'static [Row], out: &mut Vec<(&'static str, Field)>) {
        for r in rows {
            match r {
                Row::Field(f) => out.push((cat, *f)),
                Row::Group(_, inner) => walk(cat, inner, out),
                _ => {}
    let mut out = Vec::new();
    for c in CATEGORIES {
        walk(c.id, c.rows, &mut out);
    out

pub fn field(key: &str) -> Option<Field> {
    fields().into_iter().map(|(_, f)| f).find(|f| f.key == key)

/// Check a value for `key` against the schema (choices, colours). Ranges are clamped later.
pub fn validate(key: &str, v: &Value) -> std::result::Result<(), String> {
    if key == "appearance.favoriteThemes" {
        let items = v.as_array().ok_or("favorite themes must be a list")?;
        let mut seen = Vec::new();
        for item in items {
            let id = item.as_str().ok_or("favorite theme ids must be strings")?;
            if !LIGHT_THEMES.iter().chain(DARK_THEMES).any(|(theme, _)| *theme == id) || seen.contains(&id) {
                return Err(format!("unknown or duplicate favorite theme `{id}`"));
            seen.push(id);
        return Ok(());
    let kind = match field(key) {
        Some(f) => f.kind,
        None if is_unit_key(key) => Kind::Choice(DURATION_UNITS),
        None if key.starts_with("labels.colors.") && key.ends_with(".color") => Kind::Color,
        None => return Ok(()),
    };
    match kind {
        Kind::Choice(opts) => {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return Err(format!("`{key}` must be one of {}", opts.iter().map(|o| o.0).collect::<Vec<_>>().join(", "))),
            if opts.iter().any(|o| o.0 == s) {
                Ok(())
            } else {
                Err(format!("`{key}` must be one of {}", opts.iter().map(|o| o.0).collect::<Vec<_>>().join(", ")))
        Kind::Color if v.as_str().and_then(parse_hex).is_none() => Err(format!("`{key}` must be a colour like \"#2f6bdf\"")),
        _ => Ok(()),

/// Whether `key` is the unit (`frames` / `seconds`) of a duration field.
pub fn is_unit_key(key: &str) -> bool {
    fields().iter().any(|(_, f)| matches!(f.kind, Kind::Duration { unit_key } if unit_key == key))

/// Repair a preferences value in place against the schema: unknown choices and bad colours go back
/// to their defaults, numbers are clamped to their ranges.
pub fn sanitize(v: &mut Value, defaults: &Value) {
    if let Some(slot) = v.pointer_mut("/appearance/favoriteThemes")
        && validate("appearance.favoriteThemes", slot).is_err()
    {
        *slot = json!([]);
    for (_, f) in fields() {
        if let Kind::Duration { unit_key } = f.kind {
            let ptr = format!("/{}", unit_key.replace('.', "/"));
            let def = defaults.pointer(&ptr).cloned().unwrap_or(json!("seconds"));
            if let Some(slot) = v.pointer_mut(&ptr)
                && validate(unit_key, slot).is_err()
                *slot = def;
        let path: Vec<&str> = f.key.split('.').collect();
        let Some(slot) = path.iter().try_fold(&mut *v, |v, k| v.get_mut(*k)) else { continue };
        let def = path.iter().try_fold(defaults, |v, k| v.get(*k)).cloned().unwrap_or(Value::Null);
        match f.kind {
            Kind::Choice(_) | Kind::Color if validate(f.key, slot).is_err() => {
            Kind::Int { min, max, .. } => match slot.as_f64().filter(|x| x.is_finite()) {
                Some(x) => *slot = json!(x.round().clamp(min, max) as u64),
                None => *slot = def,
            Kind::Float { min, max, .. } => match slot.as_f64().filter(|x| x.is_finite()) {
                Some(x) => *slot = json!(x.clamp(min, max)),
            Kind::Duration { unit_key } => {
                let unit = v.pointer(&ptr).or(defaults.pointer(&ptr)).and_then(Value::as_str).unwrap_or("seconds").to_string();
                let max = if unit == "frames" { 100_000.0 } else { 3600.0 };
                let min = if unit == "frames" { 1.0 } else { 0.01 };
                match slot.as_f64().filter(|x| x.is_finite()) {
                    Some(x) if unit == "frames" => *slot = json!(x.round().clamp(min, max)),

/// Upgrade an older preferences file (`version` missing = v1) to [`PREFS_VERSION`].
pub fn migrate(v: &mut Value) {
    let Some(m) = v.as_object_mut() else { return };
    let ver = m.get("version").and_then(Value::as_u64).unwrap_or(1);
    if ver < 2 {
        // v1 kept the Trim Monitor's loop choice and pre/postroll; v2 adds every other category
        // (filled in from defaults by serde). The v1 "Auto Save" dialog wrote integer seconds for
        // pre/postroll; v2 stores them as reals, which serde reads either way.
        m.insert("version".into(), json!(2));
    // Files from before appearance modes had one Color Theme: keep showing it, as a fixed Dark or
    // Light mode, instead of switching established users to another theme or to Auto.
    if let Some(a) = m.get_mut("appearance").and_then(Value::as_object_mut)
        && !a.contains_key("appearanceMode")
        && let Some(theme) = a.get("colorTheme").and_then(Value::as_str).map(str::to_string)
        let mut p = AppearancePrefs::default();
        p.select_theme(&theme);
        a.insert("appearanceMode".into(), json!(p.appearance_mode));
        a.insert("darkTheme".into(), json!(p.dark_theme));
        a.insert("lightTheme".into(), json!(p.light_theme));

impl crate::autosave::Preferences {
    /// Settings ▸ Playback ▸ Hardware decoding as the process-wide switch the hardware decoder
    /// factories consult (new decoders only).
    pub fn apply_hardware_decoding(&self) {
        filmcraft_codecs::hw::set_hardware_decoding(self.playback.hardware_decoding != "off");

    /// Values of one category as a JSON object.
    pub fn category_value(&self, id: &str) -> Option<Value> {
        self.to_value().get(id).cloned()
    /// Reset one category to its defaults (`recentProjects` survives a General reset).
    pub fn reset_category(&mut self, id: &str) -> std::result::Result<(), String> {
        if category(id).is_none() {
            return Err(format!("unknown settings category `{id}`"));
        let mut v = self.to_value();
        let d = Self::default().to_value();
        let keep = self.general.recent_projects.clone();
        v[id] = d[id].clone();
        *self = serde_json::from_value(v).map_err(|e| e.to_string())?;
        self.general.recent_projects = keep;
    /// Remember a project as the most recent one.
    pub fn note_recent(&mut self, path: &str) {
        let r = &mut self.general.recent_projects;
        r.retain(|p| p != path);
        r.insert(0, path.to_string());
        r.truncate(10);


// ------------------------------------------------------------------ helpers used by the engine

/// Typographer's quotes: `"a 'b'"` → `“a ‘b’”`, apostrophes inside words → `’`.
pub fn smart_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let opening = prev.is_none_or(|p| p.is_whitespace() || "([{-–—“‘".contains(p));
        out.push(match c {
            '"' if opening => '“',
            '"' => '”',
            '\'' if opening => '‘',
            '\'' => '’',
            c => c,
        });
        prev = Some(c);
    }
    out
}

/// Interleave a stereo mix into a device buffer of `ch` channels following Output Mapping: left
/// to device channel `map[0]`, right to `map[1]` (others silent). A mono device gets the sum.
/// Out-of-range mappings fall back to channels 0/1.
pub fn map_output(left: &[f32], right: &[f32], out: &mut [f32], ch: usize, map: [u32; 2]) {
    let ch = ch.max(1);
    let n = (out.len() / ch).min(left.len()).min(right.len());
    out.iter_mut().for_each(|s| *s = 0.0);
    if ch == 1 {
        for i in 0..n {
            out[i] = (0.5 * (left[i] + right[i])).clamp(-1.0, 1.0);
        }
        return;
    }
    let pick = |m: u32, d: usize| if (m as usize) < ch { m as usize } else { d };
    let (l, r) = (pick(map[0], 0), pick(map[1], 1));
    for i in 0..n {
        out[i * ch + l] += left[i];
        out[i * ch + r] += right[i];
        out[i * ch + l] = out[i * ch + l].clamp(-1.0, 1.0);
        out[i * ch + r] = out[i * ch + r].clamp(-1.0, 1.0);
    }
}

/// Settings ▸ Media ▸ Default Media Scaling for a clip placed in a `frame`-sized sequence. `src`
/// is the clip's size at its display aspect in sequence pixels
/// ([`filmcraft_project::conformed_size`]): `scaleToFrameSize` turns on Scale to Frame Size
/// (rasterised at frame size), `setToFrameSize` sets Motion ▸ Scale so the picture fits the frame.
pub fn apply_media_scaling(ti: &mut filmcraft_project::TrackItem, scaling: &str, frame: (u32, u32), src: (f64, f64)) {
    if !(src.0 >= 1e-9 && src.1 >= 1e-9 && src.0.is_finite() && src.1.is_finite()) {
        return;
    }
    match scaling {
        "scaleToFrameSize" => ti.scale_to_frame = true,
        "setToFrameSize" => {
            let fit = (frame.0 as f64 / src.0).min(frame.1 as f64 / src.1);
            if let Some(m) = ti.effects.iter_mut().find(|e| e.effect == "motion") {
                m.params.insert("scale".into(), filmcraft_project::Param::new(filmcraft_project::ParamValue::Float((fit * 1000.0).round() / 10.0)));
            }
        }
        _ => {}
    }
}

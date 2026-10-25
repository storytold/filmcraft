//! The FilmCraft egui frontend.
//!
//! Thin by design: all project changes go through `filmcraft_engine::Session::execute`; this crate
//! owns only presentation state ([`state::UiState`]), GPU textures, the playback clock and the
//! control-channel handlers. Swap it for another toolkit without touching the engine.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

/// A literal UI string in the language the interface is drawn in (`i18n::t`). Only literals: the
/// i18n tests read every `tl!` literal in the crate and require a Spanish translation for it.
macro_rules! tl {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
}

/// A translated template with `{name}` placeholders filled in, for text that `format!` would build.
/// After the template literal come the values: `n = count`, or just `gain` for a variable of the
/// same name. Values are formatted with `Display`; the translation may reorder placeholders.
macro_rules! tlf {
    (@v $k:ident = $v:expr) => {
        $v
    };
    (@v $k:ident) => {
        $k
    };
    ($s:literal $(, $k:ident $(= $v:expr)?)* $(,)?) => {
        $crate::i18n::fmt($crate::i18n::t($s), &[$((stringify!($k), ToString::to_string(&tlf!(@v $k $(= $v)?)).as_str())),*])
    };
}

pub mod automation;
pub mod brand;
pub mod calc;
pub mod capture;
mod cjk;
pub mod control;
pub mod crash;
pub mod credits;
pub mod dock;
mod dock_shared;
mod dock_ui;
/// Thumbnail textures kept before the least recently used are freed (each is up to 320 px wide).
const MAX_THUMB_TEXTURES: usize = 512;

pub mod frames;
pub mod graphics_notice;
pub mod header;
pub mod i18n;
pub mod icons;
mod ime_reset;
pub mod links;
pub mod menus;
pub mod native_dialogs;
pub mod panels;
pub mod perf;
#[cfg(not(target_arch = "wasm32"))]
pub mod play_ahead;
mod rtl;
pub mod scrub;
mod shuttle;
pub mod source_playback;
pub mod state;
pub mod theme;
mod theme_favorites;
pub mod updates_ui;
pub mod widgets;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use egui::ime_context::{ImeContextTracker, ImeTarget};
use egui::{TextureHandle, TextureOptions};
use filmcraft_engine::{Services, Session};
use filmcraft_time::Tick;
use serde_json::{Value, json};

pub use control::ControlRequest;
use dock::PanelKind;
use frames::{FrameKey, FrameServer, Target};
use native_dialogs::FileDialog;
use state::UiState;
use theme::{ThemeKind, Tokens};

/// Audio output provided by the platform layer (cpal on desktop, WebAudio on web).
pub trait AudioOut {
    /// Start output; `fill(buffer, channels)` is called on the audio thread with interleaved f32.
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String>;
    fn stop(&mut self);
    /// Device sample rate.
    fn sample_rate(&self) -> u32;
    /// Device output channels (what `fill` will be called with).
    fn channels(&self) -> usize {
        2
    }
    /// Frames played since `start` (the playback master clock), if the device reports it.
    fn played_frames(&self) -> Option<u64>;
    /// Called every frame while playing, before `played_frames`: restart a stream the device
    /// dropped (a driver error, an unplugged device), keeping the clock and `fill`. A stream that
    /// cannot be restarted reports `played_frames() == None`.
    fn recover(&mut self) {}
    /// Hosts and devices that can be chosen in Settings ▸ Audio Hardware.
    fn devices(&self) -> AudioDevices {
        AudioDevices::default()
    }
    /// Apply Settings ▸ Audio Hardware (device class/output, buffer size, sample rate). Takes
    /// effect at the next `start`; `document_rate` is the sequence sample rate for "Attempt to
    /// force hardware to document sample rate".
    fn configure(&mut self, _hw: &filmcraft_engine::settings::AudioHardwarePrefs, _document_rate: Option<u32>) {}
    /// A hardware note for the status bar — e.g. the default output device could not be opened
    /// and a fallback device is in use, or no output device was found — or None.
    fn note(&self) -> Option<String> {
        None
    }
}

/// What the platform audio layer can open (Settings ▸ Audio Hardware).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioDevices {
    /// Audio hosts / device classes (`CoreAudio`, `WASAPI`, `ALSA`…).
    pub hosts: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Output channels of the selected output device.
    pub output_channels: u16,
}

/// Host hooks for native file dialogs etc.
#[derive(Default)]
pub struct HostHooks {
    pub pick_files: Option<Box<dyn FnMut(&[&str]) -> Vec<String>>>,
    /// The cursor in physical screen pixels. Where the windowing layer reports no pointer while files are dragged in from the OS (Windows), a drop is placed where this says it landed.
    pub cursor_screen_position: Option<Box<dyn Fn() -> Option<(i32, i32)>>>,
    pub pick_save: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    pub pick_open_project: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Save dialog with a filter: (filter name, extensions, suggested file name) → path. The
    /// suggestion may be a full path, and the dialog then starts in its folder.
    pub pick_save_as: Option<Box<dyn FnMut(&str, &[&str], &str) -> Option<String>>>,
    /// The menu tree changed (labels, shortcuts, enablement, checkmarks): update the native menu
    /// bar. Called from the frame loop only when something differs from the last call.
    pub menu_changed: Option<Box<dyn FnMut(&[menus::MenuItem])>>,
    /// The user's preferred languages (locale tags, most preferred first) for Interface Language ▸
    /// System Language. Without it, System Language is English.
    pub system_languages: Option<Box<dyn Fn() -> Vec<String>>>,
    /// Open dialog for a JSON file (shortcut preset import): filter name, extensions → path.
    pub pick_open_file: Option<Box<dyn FnMut(&str, &[&str]) -> Option<String>>>,
    /// Folder picker (Link Media search, proxy and Project Manager destinations).
    pub pick_folder: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Folder picker starting at the current destination (Export Frame).
    pub pick_folder_at: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    /// Pick one file for a command that relinks to it (Link Media ▸ Locate…, Attach Proxies,
    /// Reconnect Full Resolution) instead of importing it, as [`Self::pick_files`] does. Native
    /// hosts return the path. A host whose picker is asynchronous (the web) returns `None` and
    /// runs `hint.command` with `hint.params` plus `"path"` itself once the user has chosen.
    pub pick_file_for_relink: Option<Box<dyn FnMut(&[&str], Option<RelinkHint>) -> Option<String>>>,
    /// Bring the window on screen for control-channel UI requests *without* taking keyboard focus
    /// (macOS: `orderFrontRegardless`). Without it the app only requests a repaint: it never
    /// activates itself for an agent, because the user's keystrokes would land here.
    pub raise_without_focus: Option<Box<dyn FnMut()>>,
    /// Open a file in its default application, or (`true`) reveal it in the file manager (Edit ▸
    /// Edit Original, Help ▸ Reveal Log Files).
    pub open_path: Option<Box<dyn FnMut(&str, bool) -> Result<(), String>>>,
    /// Show a file dialog without blocking the UI thread and answer on the returned channel with
    /// the chosen paths (none: cancelled). With it, [`FilmcraftApp::pick`] keeps the frame loop
    /// running while the dialog is open (the desktop app on Linux, #260); without it the
    /// synchronous pickers above are used.
    pub run_dialog: Option<Box<dyn FnMut(native_dialogs::FileDialog) -> Receiver<Vec<String>>>>,
    /// Open a URL in the system browser (Help menu links, About dialog, Discord button, Import
    /// screen). Without it, or when it fails, links go through `egui::Context::open_url`, which
    /// opens a new tab on the web (the native egui backend is built without its `links` feature).
    pub open_url: Option<Box<dyn FnMut(&str) -> Result<(), String>>>,
    /// The operating system's light or dark appearance when egui cannot report it (Linux desktops
    /// whose Wayland compositor sends no theme to winit). Without it, or without an answer, Auto
    /// uses `egui::Context::system_theme`.
    pub system_theme: Option<SystemThemeFn>,
    /// Files or an image on the system clipboard, for Paste in the Timeline and the Project panel
    /// (#611). Without it Paste only pastes clips copied in FilmCraft.
    pub clipboard_media: Option<Box<dyn FnMut() -> Option<ClipboardMedia>>>,
}

/// What [`HostHooks::clipboard_media`] found on the system clipboard.
pub enum ClipboardMedia {
    /// Files copied in the file manager.
    Files(Vec<String>),
    /// An image (a screenshot, an image copied in a browser): 8-bit RGBA, row by row.
    Image { width: usize, height: usize, rgba: Vec<u8> },
}

/// Reads the system appearance (Settings ▸ Appearance ▸ Appearance Mode ▸ Sync with system).
pub type SystemThemeFn = Box<dyn Fn(&egui::Context) -> Option<egui::Theme>>;

/// The command a [`HostHooks::pick_file_for_relink`] caller runs with the chosen file, for hosts
/// that can only run it later.
#[derive(Clone, Debug)]
pub struct RelinkHint {
    /// `media.relink`, `media.attachProxies` or `media.reconnectFullRes`.
    pub command: String,
    /// Its parameters, without `"path"`.
    pub params: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    About,
    Shortcuts,
    NewSequence,
    /// Settings (Preferences window; page in `UiState::settings`).
    Preferences,
    /// "Recover unsaved changes from `<time>`?" (shown at startup when a dead session left some).
    Recovery,
    /// "Are you sure you want to discard your changes?" (File ▸ Revert).
    RevertConfirm,
    /// Clip ▸ Audio Gain… (G).
    AudioGain,
    /// Sequence ▸ Delete Tracks….
    DeleteTracks,
    /// Sequence ▸ Add Tracks….
    AddTracks,
    /// Sequence ▸ Sequence Settings….
    SequenceSettings,
    /// Set Transition Duration (double-click a transition in the Timeline).
    TransitionDuration,
}

#[derive(Default)]
pub struct Playback {
    pub playing: bool,
    pub speed: f64,
    pub looping: bool,
    /// Monotonic host time (s) and timeline tick when playback (re)started.
    anchor_time: f64,
    anchor_tick: Tick,
    /// Audio frames played at anchor (when the audio clock drives).
    pub audio_clock: bool,
    /// The audio clock's last reading and when (monotonic host s) it last moved: a device that stops
    /// consuming samples hands the clock back to the wall clock (see [`AUDIO_STALL_S`]).
    audio_seen: (u64, f64),
    /// When playback lost its output (a failed open or a dead stream), when it is opened again
    /// (monotonic host s; 0 = not retrying). Pause/play proves a fresh stream works, so a stream the
    /// device keeps dropping is reopened on a timer (see [`AUDIO_RETRY_S`]).
    audio_retry_at: f64,
    /// Audio underruns for the current (or last) play (desktop: sound is mixed ahead).
    #[cfg(not(target_arch = "wasm32"))]
    pub audio_stats: std::sync::Arc<play_ahead::AudioStats>,
    /// Shown / dropped frame accounting for the current (or last) play.
    pub meter: frames::PlaybackMeter,
    /// Waiting for the first frames before starting the clock: when the wait began (monotonic host time,
    /// s; negative = not yet stamped), and whether the Program monitor has them ready.
    pub preroll: Option<f64>,
    pub preroll_ready: bool,
    /// The window was hidden (occluded/minimized) since the monitor last refreshed.
    pub hidden: bool,
    /// Forward playback stops here (Play In to Out, Play from Playhead to Out Point).
    pub stop_at: Option<Tick>,
    /// Resting playhead for an inclusive marked range; unlike stop_at, this
    /// names the last visible frame, not the following playback boundary.
    pub rest_at: Option<Tick>,
}

/// How long (s) the audio clock may stand still during playback before the wall clock takes over.
/// An output stream can open and then never call back (ALSA with a busy or misconfigured device,
/// #136); playback must not freeze on it. Devices that are slow to start (Bluetooth) stay well
/// under this.
pub const AUDIO_STALL_S: f64 = 1.0;

/// How often (s) playback re-opens the output after its stream or device failed, while playing
/// goes on silently on the wall clock. Reopening is what pause/play does by hand; doing it on a
/// timer brings the sound back when the device recovers (#2503).
pub const AUDIO_RETRY_S: f64 = 2.0;

/// Export entries that run from the menus through the save panel: command, filter label,
/// extension.
pub(crate) const EXPORT_SAVE_DIALOGS: [(&str, &str, &str); 7] = [
    ("markers.exportCsv", "Marker report (CSV)", "csv"),
    ("file.exportEdl", "EDL", "edl"),
    ("file.exportFcp7Xml", "Final Cut Pro XML", "xml"),
    ("file.exportFcpxml", "FCPXML", "fcpxml"),
    ("file.exportOtio", "OpenTimelineIO", "otio"),
    ("file.exportAle", "Avid Log Exchange", "ale"),
    ("file.exportSelectionProject", "FilmCraft Project", "fcproj"),
];

/// Incoming composition identity for the Program monitor. Interrupted compositions remain
/// Stale until the matching opaque native reset fence; output or old terminal input is not
/// acknowledgement. The retired origin and current destination live in the UI reset adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImeComposition {
    Active(ImeTarget),
    TerminalPending(ImeTarget),
    Stale,
}

pub struct FilmcraftApp {
    pub session: Session,
    pub ui: UiState,
    pub tokens: Tokens,
    pub frames: Arc<FrameServer>,
    pub playback: Playback,
    pub source_playback: source_playback::SourcePlayback,
    pub audio: Option<Box<dyn AudioOut>>,
    /// Audio during scrubbing (#211).
    pub scrub: scrub::ScrubAudio,
    pub hooks: HostHooks,
    pub dialog: Option<Dialog>,
    pub(crate) updates_rt: updates_ui::Runtime,
    pub file_dialogs: panels::file_dialogs::FileDialogState,
    /// The native file dialog the host is showing ([`Self::pick`]).
    native_dialog: Option<native_dialogs::PendingDialog>,
    pub auto: automation::Registry,
    /// IME owner/generation/cause state for the custom editors on the Program monitor. State is
    /// kept per egui viewport; a closed viewport is released in [`eframe::App::on_exit`].
    pub(crate) ime_context: ImeContextTracker,
    /// Bumped when the custom text edit target is (re)created, a sequence/document context
    /// changes, or a control request replaces `gfxEdit`. It is the replacement epoch of the
    /// otherwise stable sequence/clip/component owner.
    pub(crate) ime_replacement_epoch: u64,
    /// The (viewport, cumulative pass) whose one IME observation has already been made.
    ime_observed_pass: Option<(egui::ViewportId, u64)>,
    /// The composition in flight (including an empty preedit awaiting its terminal event), or a
    /// tombstone for a composition whose target was replaced or caret/focus was interrupted.
    pub(crate) ime_composition: Option<ImeComposition>,
    ime_reset: ime_reset::State,
    ime_composition_viewport: Option<egui::ViewportId>,
    /// The editor drawn in this pass; the final observation runs after control drain.
    pub(crate) ime_drawn_editor: Option<panels::graphics::DrawnImeEditor>,
    /// Project document generation last seen by the UI (the engine bumps it on replacement).
    ime_document_epoch: u64,
    /// Active sequence last seen by the UI; `sequence.close` need not emit `OpenSequence`.
    ime_active_sequence: Option<filmcraft_project::ItemId>,
    /// Viewports this app's custom IME producer has observed, released when the app exits.
    ime_viewports: Vec<egui::ViewportId>,
    /// Named textures (monitors, thumbnails) with the key they show.
    textures: HashMap<String, (FrameKey, TextureHandle)>,
    /// The UI pass each thumbnail texture was last used in: thumbnails beyond
    /// [`MAX_THUMB_TEXTURES`] are freed, least recently used first (#1682).
    thumb_used: HashMap<String, u64>,
    control_rx: Option<Receiver<ControlRequest>>,
    /// One explicit panel reveal; consumed by the custom renderer, never repeated per frame.
    pub(crate) pending_panel_raise: Option<PanelKind>,
    /// Explicit automation mode: native widget input and native menu commands are excluded.
    pub(crate) control_input_only: bool,
    /// Backend focus before synthetic input changes egui's effective focus for this frame.
    pub(crate) native_window_focused: Option<bool>,
    /// Modifiers owned by an in-flight control gesture, carried between its input frames.
    synthetic_modifiers: egui::Modifiers,
    /// Requests waiting for the UI to show an element: (request, give-up time).
    deferred: Vec<(ControlRequest, f64)>,
    /// Control requests that need a fresh layout/response; processed only in a real UI pass.
    ui_requests: Vec<(ControlRequest, f64)>,
    last_ui_time: f64,
    pub(crate) synthetic: Vec<egui::Event>,
    /// One synthetic step handed to `raw_input_hook` but not yet consumed by a real UI pass.
    /// Hidden `run_logic` callbacks do not process raw input: the same step is offered again until
    /// an actual pass consumes it, rather than being dropped or advancing the queue.
    synthetic_pending_step: Option<Vec<egui::Event>>,
    /// The pending step is already present in the `RawInput` the eframe integration preserves
    /// across hidden logic ticks. Appending it again on the next hook would duplicate it.
    synthetic_step_offered: bool,
    /// Same for the `ModifiersChanged`/focus boundary events, which hidden ticks would otherwise
    /// accumulate without bound.
    synthetic_modifiers_offered: bool,
    /// An image delivered by `Event::PasteImage` for the paste action this frame. Native and web
    /// hosts deliver the bytes in the event; re-reading the OS clipboard can see a different image
    /// (or none, on the web), so the menu path consumes this payload first.
    pub(crate) pasted_image: Option<std::sync::Arc<egui::ColorImage>>,
    /// Monotonic host clock for playback, deadlines and hidden work. Never `ctx.input(i.time)`.
    clock_start: web_time::Instant,
    /// The latest time egui itself reported for a layout pass. Native and web hosts supply a
    /// monotonic wall clock; deterministic harnesses supply their own stepped clock. Taking the
    /// later of this and the host clock keeps simulated tests on their clock while a hidden
    /// logic tick (whose `InputState::time` is stale) still advances by host time.
    ui_clock: f64,
    /// Non-negative `ui_clock - host_elapsed`, captured on the first real UI pass. Native eframe
    /// starts its `RawInput::time` at its own integration beginning, which can precede
    /// `clock_start`; without this offset, `max` would hold `now()` at the stale UI time during
    /// hidden logic and then jump by the startup offset when the next UI pass arrives.
    ui_clock_offset: Option<f64>,
    #[cfg(test)]
    test_now: Option<f64>,
    /// macOS: a Control+Click already translated to a secondary (context) click, so its release is
    /// translated too even if Control is let go mid-click. AppKit delivers Control+Click to the
    /// view as a *primary* press with the Control modifier (`rightMouseDown:` only fires for a
    /// right button or two-finger click), so the app does the platform's translation itself.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    mac_ctrl_click_armed: bool,
    /// The mouse press in progress (its start time) and the undo history when it began, for Escape
    /// to take back what the press changed (#580).
    value_press: Option<(f64, filmcraft_engine::UndoMark)>,
    /// Escape was pressed during the current press in pass 0; apply the rollback after the final
    /// non-discarded UI pass (pass 1+ has no key event to re-read).
    escape_cancel: bool,
    /// BS.1770 loudness of the programme as it plays, and the next sample position to feed.
    pub(crate) loudness: Option<(filmcraft_audio_dsp::LoudnessMeter, i64)>,
    /// Status message last shown and when it first appeared (messages expire after a few seconds).
    status_seen: (String, f64),
    /// Admitted `ui.screenshot` requests and their completion slots.
    pub(crate) shots: capture::Shots,
    /// Fresh raw-input surface geometry, refreshed in `raw_input_hook` for UI and logic callbacks.
    pub(crate) capture_geometry: Option<capture::CaptureGeometry>,
    pub(crate) capture_epoch: u64,
    /// A paused monitor drew a stand-in (nearest cached) picture last frame: its exact frame is
    /// still decoding.
    pub(crate) monitor_inexact: bool,
    /// Per monitor: the frames it asked for on its latest paused passes, oldest first (stand-ins
    /// while the newest renders, see `panels::monitor`).
    pub(crate) monitor_asked: HashMap<String, std::collections::VecDeque<FrameKey>>,
    /// The frame whose picture the Program monitor drew last: the frame due, or its stand-in.
    pub(crate) program_shown: Option<FrameKey>,
    /// Consecutive frames the timeline zoom / scroll has been at rest. `ui.elements` answers from
    /// the frame before the last one, so its timeline rects are final from 2 on.
    pub(crate) timeline_still: u32,
    input_waiters: Vec<Sender<Value>>,
    styled: bool,
    /// A panic in the UI pass, shown in an error window until dismissed (see [`crash`]).
    pub ui_error: Option<String>,
    /// The window runs in a fallback graphics mode (set by the desktop host): a notice with a
    /// button back to the default mode (see [`graphics_notice`]).
    pub graphics_notice: Option<graphics_notice::GraphicsNotice>,
    /// Fault injection for robustness tests: the next UI pass panics.
    #[doc(hidden)]
    pub panic_next_frame: bool,
    /// The next close request quits without asking to save (Save / Don't Save was chosen in the
    /// quit prompt, or the control channel's `app.quit`).
    pub quit_confirmed: bool,
    fonts_ready: bool,
    pub integrated_titlebar: bool,
    pub last_timeline_width: f32,
    /// The sequence whose view `ui.timeline` holds, and that view as it was last exchanged with
    /// `session.state.timeline_views` (see `sync_timeline_view`).
    timeline_view_of: Option<filmcraft_engine::project::ItemId>,
    /// Items imported by an OS file drop, to place on the Timeline at the drop point this frame.
    pending_timeline_drop: Option<(Vec<filmcraft_engine::project::ItemId>, egui::Pos2)>,
    /// Files being dragged over the window from the OS (count, pointer): the Timeline previews where they would land.
    file_drag_hover: Option<(usize, egui::Pos2)>,
    timeline_view_last: Option<filmcraft_engine::project::SequenceView>,
    pub fps: f32,
    last_time: f64,
    bindings: Vec<menus::KeyBinding>,
    /// Shortcut-set revision `bindings` were built from.
    bindings_rev: u64,
    /// The menu tree last sent to `hooks.menu_changed` (the native menu bar).
    menu_sent: Vec<menus::MenuItem>,
    /// Keyboard Shortcuts dialog state.
    pub shortcut_editor: panels::shortcuts_dialog::EditorState,
    pub toast: Option<(String, f64)>,
    pub tl: panels::timeline::TlState,
    pub(crate) transcript_cache: panels::transcript_cache::TranscriptCache,
    /// Commands from outside the UI (native menu bar), invoked on the UI thread.
    pub command_inbox: Option<Receiver<String>>,
    /// A V key press (or a Ctrl+V paste) reached the app and its release hasn't yet.
    v_down: bool,
    /// OS files/image that were already on the clipboard when the last Copy/Cut ran. Paste ignores
    /// that same media so it pastes the copied clips instead (#697).
    pub(crate) stale_clipboard_media: Option<String>,
    /// GPU compositor (when running on wgpu): device state + compositor + the egui texture it feeds.
    pub gpu: Option<GpuState>,
    /// Preview render job being watched (job id, where to start playing when it completes).
    watched_render: Option<(u64, Tick)>,
    /// Settings last applied to the UI (theme, tooltips, frame cache, audio device).
    applied_prefs: Option<filmcraft_engine::autosave::Preferences>,
    /// Settings ▸ Appearance ▸ UI Scale last applied, with the system scale it was applied for.
    applied_ui_scale: Option<(Option<f32>, f32)>,
    workspace_restored: bool,
    /// Window ▸ Workspaces: saved layouts ([`dock::WORKSPACES_FILE`] in the data directory).
    pub workspaces: dock::WorkspacePrefs,
    /// Workspace names and the current one, as last handed to the native menu.
    menu_workspaces: (Vec<String>, String),
}

pub struct GpuState {
    pub render_state: eframe::egui_wgpu::RenderState,
    pub compositor: filmcraft_gpu::GpuCompositor,
    pub texture: Option<egui::TextureId>,
    pub last_key: Option<FrameKey>,
    pub size: (u32, u32),
    /// Composite time of the last frame (ms).
    pub last_ms: f32,
    /// Set by the device's uncaptured-error handler: the GPU path failed (validation, lost
    /// device, out of memory). The app then drops it and composites on the CPU.
    pub broken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Largest texture side the device accepts.
    pub max_texture: u32,
}

/// Largest texture side a plan needs on the GPU (output and every layer).
fn plan_side(plan: &filmcraft_render::plan::FramePlan) -> usize {
    plan.max_side()
}

/// A software rasterizer (llvmpipe, SwiftShader, softpipe — VMs and containers without GPU device
/// access, missing or broken drivers) runs wgpu entirely on the CPU. Compositing through one is
/// slower than the native CPU compositor and hides the cause: GPU usage stays at 0 % while the
/// CPU saturates. Returns a description when the adapter is one.
pub fn software_rasterizer(info: &eframe::wgpu::AdapterInfo) -> Option<String> {
    rasterizer_reason(&info.name, info.device_type)
}

fn rasterizer_reason(name: &str, device_type: eframe::wgpu::DeviceType) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if device_type == eframe::wgpu::DeviceType::Cpu || ["llvmpipe", "swiftshader", "softpipe"].iter().any(|s| lower.contains(s)) {
        return Some(format!("software rasterizer ({name}, {device_type:?})"));
    }
    None
}

/// Whether the GPU compositor can run on this adapter: it renders and blends Rgba16Float
/// targets and uploads whole frames as textures. Older or OpenGL-backed GPUs (some Intel Macs,
/// VMs, Linux without Vulkan, WebGL) can't; those use the CPU compositor, which renders the same
/// frames. Returns the reason when unsupported.
pub fn gpu_compositor_unsupported(adapter: &eframe::wgpu::Adapter) -> Option<String> {
    use eframe::wgpu::{TextureFormat, TextureFormatFeatureFlags as F, TextureUsages as U};
    if let Some(why) = software_rasterizer(&adapter.get_info()) {
        return Some(why);
    }
    let f = adapter.get_texture_format_features(TextureFormat::Rgba16Float);
    if !f.allowed_usages.contains(U::RENDER_ATTACHMENT | U::TEXTURE_BINDING) {
        return Some("Rgba16Float is not renderable".into());
    }
    if !f.flags.contains(F::BLENDABLE | F::FILTERABLE) {
        return Some("Rgba16Float is not blendable/filterable".into());
    }
    let max = adapter.limits().max_texture_dimension_2d;
    if max < 4096 {
        return Some(format!("max texture size {max} < 4096"));
    }
    None
}

impl FilmcraftApp {
    /// Enable the GPU compositor on the eframe wgpu device.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        if let Some(why) = gpu_compositor_unsupported(&rs.adapter) {
            log::warn!("GPU compositor disabled ({why}); compositing on the CPU");
            // Say so instead of letting the preview quietly crawl: "GPU 0 %, CPU 100 %" with no
            // explanation read like a renderer bug when the machine simply has no GPU access.
            self.ui.status = format!("{} ({why})", tl!("GPU acceleration is unavailable; preview renders on the CPU"));
            return;
        }
        // wgpu's default reaction to a validation error or lost device is to panic, which closed
        // the app (or left the monitors blank) on GPUs the compositor doesn't suit. Log instead and
        // fall back to the CPU compositor.
        let broken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = broken.clone();
        rs.device.on_uncaptured_error(std::sync::Arc::new(move |e: eframe::wgpu::Error| {
            log::error!("GPU error, switching to the CPU compositor: {e}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }));
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| filmcraft_gpu::GpuCompositor::new(&rs.device, &rs.queue)));
        match made {
            Ok(compositor) if !broken.load(std::sync::atomic::Ordering::Relaxed) => {
                let max_texture = rs.device.limits().max_texture_dimension_2d;
                self.gpu = Some(GpuState { render_state: rs, compositor, texture: None, last_key: None, size: (0, 0), last_ms: 0.0, broken, max_texture });
            }
            _ => log::error!("GPU compositor failed to initialise; compositing on the CPU"),
        }
    }

    /// Drop the GPU compositor after a GPU error; the monitors composite on the CPU from then on.
    fn disable_gpu(&mut self, why: &str) {
        log::error!("GPU compositor disabled: {why}");
        if let Some(g) = self.gpu.take()
            && let Some(id) = g.texture
        {
            g.render_state.renderer.write().free_texture(&id);
        }
        self.ui.status = tl!("Graphics problem: switched to software compositing").into();
    }

    /// Composite a plan on the GPU and return the egui texture showing it.
    pub fn gpu_present(&mut self, key: FrameKey, plan: &frames::GpuPlan) -> Option<(egui::TextureId, (u32, u32))> {
        if self.gpu.as_ref()?.broken.load(std::sync::atomic::Ordering::Relaxed) {
            self.disable_gpu("device error");
            return None;
        }
        let g = self.gpu.as_mut()?;
        if g.last_key == Some(key)
            && let Some(t) = g.texture
        {
            return Some((t, g.size));
        }
        if plan_side(&plan.plan) > g.max_texture as usize {
            self.disable_gpu("frame larger than the GPU's maximum texture size");
            return None;
        }
        let t0 = web_time::Instant::now();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (view, size) = g.compositor.composite_prepared(&plan.plan, Some(&plan.prepared));
            (view.clone(), size)
        }));
        let Ok((view, size)) = ran else {
            self.disable_gpu("compositor panicked");
            return None;
        };
        let g = self.gpu.as_mut()?;
        g.last_ms = t0.elapsed().as_secs_f32() * 1000.0;
        let mut renderer = g.render_state.renderer.write();
        let id = match g.texture {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(&g.render_state.device, &view, eframe::wgpu::FilterMode::Linear, id);
                id
            }
            None => renderer.register_native_texture(&g.render_state.device, &view, eframe::wgpu::FilterMode::Linear),
        };
        g.texture = Some(id);
        g.last_key = Some(key);
        g.size = size;
        Some((id, size))
    }
}

impl FilmcraftApp {
    /// Build the app around one shared `FrameServer`, falling back to the default language on an unparsable preference, loading workspaces from the prefs
    /// directory and seeding IME epoch/sequence identity.
    pub fn new(mut session: Session) -> Self {
        session.shortcuts.register_external(menus::external_commands());
        let recovery = !session.recovery_candidates().is_empty();
        let frames = Arc::new(FrameServer::new(session.media.clone(), session.services.clone(), session.previews.clone(), FrameServer::default_workers()));
        let workspaces =
            session.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| dock::WorkspacePrefs::load(&d.join(dock::WORKSPACES_FILE))).unwrap_or_default();
        let language = i18n::Language::parse(&session.prefs.general.interface_language).unwrap_or_default();
        let ime_document_epoch = session.document_epoch;
        let ime_active_sequence = session.state.active_sequence;
        Self {
            session,
            ui: UiState { language, ..UiState::default() },
            tokens: Tokens::for_kind(ThemeKind::Dark),
            frames,
            playback: Playback { speed: 1.0, ..Default::default() },
            source_playback: Default::default(),
            audio: None,
            scrub: Default::default(),
            hooks: HostHooks::default(),
            // Unsaved changes left by a session that died are offered first thing.
            dialog: recovery.then_some(Dialog::Recovery),
            updates_rt: Default::default(),
            file_dialogs: Default::default(),
            native_dialog: None,
            auto: Default::default(),
            ime_context: ImeContextTracker::new(),
            ime_replacement_epoch: 0,
            ime_observed_pass: None,
            ime_composition: None,
            ime_reset: Default::default(),
            ime_composition_viewport: None,
            ime_drawn_editor: None,
            ime_document_epoch,
            ime_active_sequence,
            ime_viewports: Vec::new(),
            textures: HashMap::new(),
            thumb_used: HashMap::new(),
            control_rx: None,
            pending_panel_raise: None,
            control_input_only: false,
            native_window_focused: None,
            synthetic_modifiers: egui::Modifiers::NONE,
            deferred: Vec::new(),
            ui_requests: Vec::new(),
            last_ui_time: 0.0,
            synthetic: Vec::new(),
            synthetic_pending_step: None,
            synthetic_step_offered: false,
            synthetic_modifiers_offered: false,
            pasted_image: None,
            clock_start: web_time::Instant::now(),
            ui_clock: 0.0,
            ui_clock_offset: None,
            #[cfg(test)]
            test_now: None,
            mac_ctrl_click_armed: false,
            value_press: None,
            escape_cancel: false,
            loudness: None,
            status_seen: (String::new(), 0.0),
            shots: capture::Shots::default(),
            capture_geometry: None,
            capture_epoch: 0,
            monitor_inexact: false,
            monitor_asked: HashMap::new(),
            program_shown: None,
            timeline_still: 0,
            input_waiters: Vec::new(),
            styled: false,
            ui_error: None,
            panic_next_frame: false,
            quit_confirmed: false,
            fonts_ready: false,
            integrated_titlebar: false,
            last_timeline_width: 1000.0,
            timeline_view_of: None,
            pending_timeline_drop: None,
            file_drag_hover: None,
            timeline_view_last: None,
            fps: 60.0,
            last_time: 0.0,
            bindings: Vec::new(),
            bindings_rev: 0,
            menu_sent: Vec::new(),
            shortcut_editor: Default::default(),
            toast: None,
            graphics_notice: None,
            tl: Default::default(),
            transcript_cache: Default::default(),
            command_inbox: None,
            v_down: false,
            stale_clipboard_media: None,
            gpu: None,
            watched_render: None,
            applied_prefs: None,
            applied_ui_scale: None,
            workspace_restored: false,
            workspaces,
            menu_workspaces: Default::default(),
        }
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Start a new replacement epoch for the Program monitor's text edit target.
    ///
    /// The owner key names the sequence, clip and component. A recreated edit session on the
    /// same key (or a document/sequence replacement) must not inherit the previous IME context,
    /// so the app advances this epoch and the tracker resets the generation once.
    pub(crate) fn restart_ime_target(&mut self) {
        self.ime_replacement_epoch = self.ime_replacement_epoch.wrapping_add(1);
        self.invalidate_ime_composition();
    }

    /// Context-less commands block immediately and retire the prior request on a real change.
    /// The next Context-bearing local event boundary requests its current destination's fence.
    pub(crate) fn invalidate_ime_composition(&mut self) {
        let origin = match self.ime_composition {
            Some(ImeComposition::Active(target) | ImeComposition::TerminalPending(target))
                if self.ime_composition_viewport == Some(self.ime_reset.viewport) =>
            {
                Some(target)
            }
            _ => self.ime_reset.origin(self.ime_reset.viewport),
        };
        let Some(origin) = origin else { return };
        let destination = panels::graphics::current_reset_destination(self);
        self.ime_reset.interrupt(origin, destination);
        self.ime_composition = Some(ImeComposition::Stale);
        self.ime_composition_viewport = Some(self.ime_reset.viewport);
    }

    /// Feed the live local edit destination to the reset reducer before each raw event. When an
    /// origin exists, a changed destination retires the old composition token and marks the
    /// composition stale; an unchanged destination only re-arms a pending `NeedsRequest`. Call
    /// with the editor's actual text, never a staged engine copy.
    pub(crate) fn sync_live_ime_destination(&mut self, ctx: &egui::Context, destination: ime_reset::Destination) {
        let viewport = ctx.viewport_id();
        self.ime_reset.viewport = viewport;
        if self.ime_reset.incoming_changed(viewport, destination) {
            let origin = match self.ime_composition {
                Some(ImeComposition::Active(target) | ImeComposition::TerminalPending(target)) if self.ime_composition_viewport == Some(viewport) => {
                    Some(target)
                }
                _ => self.ime_reset.origin(viewport),
            };
            if let Some(origin) = origin {
                self.ime_reset.interrupt(origin, Some(destination));
                self.ime_composition = Some(ImeComposition::Stale);
                self.ime_composition_viewport = Some(viewport);
            }
        }
        self.ime_reset.sync(ctx, destination);
    }

    /// Retire the current composition and suspend this viewport's IME fence. UI paths that lose
    /// the editor route through this; context-less control commands call
    /// `invalidate_ime_composition` directly. Only an eligible final descriptor can re-arm the guard.
    pub(crate) fn suspend_ime_input(&mut self, ctx: &egui::Context) {
        self.ime_reset.viewport = ctx.viewport_id();
        self.invalidate_ime_composition();
        self.ime_reset.suspend(ctx.viewport_id());
    }

    /// The editor did not own this raw route. Controls stay inert after suspension; stock
    /// text/key events remain available to their widgets. Never use this before a live loop.
    fn suspended_ime_controls(&mut self, ctx: &egui::Context) {
        self.suspend_ime_input(ctx);
        ctx.input_mut(|input| input.events.retain(|event| !matches!(event, egui::Event::ImeResetCompleted { .. })));
    }

    /// An actual modal owns input: the in-app `dialog` (Recovery, Revert, About, …) or a
    /// host-shown `native_dialog` picker. `file_dialogs` is only dialog-local state (the recovery
    /// choice); it is not an open modal on its own, so it must not block an otherwise eligible
    /// editor between frames.
    pub(crate) fn ime_modal_blocks_input(&self) -> bool {
        self.dialog.is_some() || self.native_dialog_open()
    }

    /// Hosts with post-logic application work call this at their actual outer return.
    pub fn finish_ime_reset_logic(&mut self, ctx: &egui::Context) {
        self.ime_reset.viewport = ctx.viewport_id();
        self.sync_ime_target_context(ctx);
        if !ime_reset::in_ui_pass(ctx) {
            self.suspend_ime_input(ctx);
        }
    }

    /// End-of-UI-pass reconciliation. Suspend and finish as ineligible when the custom output is
    /// absent, the window is unfocused, a modal owns input, or the text edit lacks focus;
    /// otherwise sync the live destination and let `finish` bind or allocate only for actual
    /// custom eligibility.
    fn finish_ime_reset_ui(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(input.focused));
        let eligible = self.ime_reset.owns_output(ctx)
            && focused
            && !self.ime_modal_blocks_input()
            && ctx.memory(|memory| memory.has_focus(egui::Id::unique(panels::graphics::TEXT_EDIT_ID)));
        if !eligible {
            self.suspend_ime_input(ctx);
            let destination = panels::graphics::current_reset_destination(self);
            self.ime_reset.finish(ctx, destination, false);
            return;
        }
        let destination = panels::graphics::current_reset_destination(self);
        if let Some(destination) = destination {
            // Same-pass control work can change only the caret after the overlay drew. Do
            // not allocate until actual custom eligibility has been established above.
            self.sync_live_ime_destination(ctx, destination);
        }
        self.ime_reset.finish(ctx, destination, self.ime_reset.owns_output(ctx));
    }

    /// Notice a document replacement or an active-sequence change that did not emit
    /// `OpenSequence` (`file.newProject`, a project with no sequence, `sequence.close`). The
    /// edit target and any in-flight composition belong to the old context.
    fn sync_ime_target_context(&mut self, ctx: &egui::Context) {
        let document = self.session.document_epoch;
        let sequence = self.session.state.active_sequence;
        let replaced = document != self.ime_document_epoch;
        if !replaced && sequence == self.ime_active_sequence {
            return;
        }
        self.ime_document_epoch = document;
        self.ime_active_sequence = sequence;
        self.restart_ime_target();
        self.ui.gfx_edit = None;
        self.ime_drawn_editor = None;
        let text_edit = egui::Id::unique(panels::graphics::TEXT_EDIT_ID);
        ctx.memory_mut(|memory| memory.surrender_focus(text_edit));
        ctx.data_mut(|data| data.remove::<PanelKind>(text_edit.with("panel")));
    }

    /// Mark this viewport/pass as observed by the custom producer, so the end-of-pass fallback
    /// does not observe it a second time.
    pub(crate) fn note_ime_observed(&mut self, viewport: egui::ViewportId, pass_nr: u64) {
        self.ime_observed_pass = Some((viewport, pass_nr));
        if !self.ime_viewports.contains(&viewport) {
            self.ime_viewports.push(viewport);
        }
    }

    /// Observe `None` once for a pass where no visible, focused custom IME owner drew a
    /// descriptor. This is the explicit hidden/unfocused signal the tracker needs; a gap alone
    /// is a fallback, not a replacement for it.
    pub(crate) fn finish_ime_pass(&mut self, ctx: &egui::Context) {
        let viewport = ctx.viewport_id();
        let pass_nr = ctx.cumulative_pass_nr_for(viewport);
        // The Program monitor deferred its observation until all control requests for this pass
        // have run. It reports `true` even when it observed an explicit `None` for a hidden or
        // replaced editor.
        if panels::graphics::finish_graphics_ime(self, ctx) {
            return;
        }
        if self.ime_observed_pass == Some((viewport, pass_nr)) {
            return;
        }
        // Availability retirement belongs to the unconditional outer reset finalizer.
        self.ime_context.observe(viewport, None);
        self.note_ime_observed(viewport, pass_nr);
    }

    /// Isolate an automation-owned app from native keyboard, pointer, drop and menu input.
    /// Window close/quit and screenshot delivery remain available.
    pub fn with_control_input_only(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_input_only = true;
        self.with_control(rx)
    }

    pub fn services(&self) -> Arc<dyn Services> {
        self.session.services.clone()
    }

    /// Time used by logic-only ticks, playback and deadlines.
    ///
    /// The host clock is authoritative (egui's `InputState::time` is stale during
    /// `Context::run_logic`), but an egui pass's reported time can be ahead of it: deterministic
    /// harnesses step a simulated clock without sleeping. Taking the later value keeps those
    /// runs deterministic while a hidden tick still moves on the monotonic host clock.
    pub(crate) fn now(&self) -> f64 {
        #[cfg(test)]
        if let Some(now) = self.test_now {
            return now;
        }
        let host = self.clock_start.elapsed().as_secs_f64();
        let host = self.ui_clock_offset.map_or(host, |offset| host + offset);
        host.max(self.ui_clock)
    }

    /// Override `now()` with a deterministic value for tests; compiled only under `cfg(test)`.
    #[cfg(test)]
    pub(crate) fn set_now_for_test(&mut self, now: f64) {
        self.test_now = Some(now);
    }

    /// Rebuild the frame server if the session's media pool was replaced (e.g. project opened).
    fn sync_pool(&mut self) {
        if !Arc::ptr_eq(&self.frames.pool, &self.session.media) || !Arc::ptr_eq(&self.frames.previews, &self.session.previews) {
            self.frames = Arc::new(FrameServer::new(
                self.session.media.clone(),
                self.session.services.clone(),
                self.session.previews.clone(),
                FrameServer::default_workers(),
            ));
            self.textures.clear();
            self.thumb_used.clear();
            self.tl.reset_media_caches();
            // the new frame server needs the Memory settings
            self.applied_prefs = None;
        }
    }

    /// Show theme `k` with the Settings ▸ Appearance highlight colour and contrast.
    pub fn set_theme(&mut self, ctx: &egui::Context, k: ThemeKind) {
        let a = &self.session.prefs.appearance;
        let highlight = filmcraft_engine::settings::parse_hex(&a.highlight_color);
        self.tokens = Tokens::for_kind(k).with_appearance(highlight, a.accessible_contrast);
        theme::apply_visuals(ctx, &self.tokens);
        self.apply_tooltips(ctx);
        self.ui.dark = k != ThemeKind::Light;
    }

    /// The system appearance: the host's reading, else what the windowing toolkit reports.
    pub fn system_theme(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        self.hooks.system_theme.as_ref().and_then(|read| read(ctx)).or_else(|| ctx.system_theme())
    }

    /// The theme Settings ▸ Appearance selects: the light or dark theme of the Appearance Mode,
    /// Auto following the system (dark when it gives no answer).
    pub fn appearance_kind(&self, ctx: &egui::Context) -> ThemeKind {
        let light = self.system_theme(ctx).map(|t| t == egui::Theme::Light);
        ThemeKind::from_pref(self.session.prefs.appearance.shown_theme(light))
    }

    fn apply_tooltips(&self, ctx: &egui::Context) {
        // Settings ▸ General ▸ Show Tool Tips
        let delay = if self.session.prefs.general.show_tool_tips { 0.5 } else { 1.0e9 };
        ctx.global_style_mut(|s| s.interaction.tooltip_delay = delay);
    }

    /// Settings ▸ Appearance ▸ UI Scale: a fixed scale makes a point that many physical pixels,
    /// whatever scale the system reports. It is applied only when the setting or the system scale
    /// changes, so Ctrl+- / Ctrl++ still zoom the interface for the session; going back to Auto
    /// restores the system's scale.
    fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        let want = self.session.prefs.appearance.ui_scale_factor();
        let native = ctx.native_pixels_per_point().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
        if self.applied_ui_scale == Some((want, native)) {
            return;
        }
        let prev = self.applied_ui_scale.replace((want, native));
        match want {
            Some(scale) => ctx.set_zoom_factor(scale / native),
            None if prev.is_some_and(|(p, _)| p.is_some()) => ctx.set_zoom_factor(1.0),
            None => {}
        }
    }

    /// View ▸ Reset Interface Zoom: the scale Settings ▸ Appearance ▸ UI Scale asks for (the
    /// system's with Auto), undoing any session zoom (#2253).
    /// Returns the zoom factor set (egui reports it from the next pass on).
    pub(crate) fn reset_ui_zoom(&mut self, ctx: &egui::Context) -> f32 {
        let native = ctx.native_pixels_per_point().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
        let zoom = self.session.prefs.appearance.ui_scale_factor().map_or(1.0, |scale| scale / native);
        ctx.set_zoom_factor(zoom);
        zoom
    }

    /// Make the UI follow the settings after they change (theme, tooltips, frame cache budget,
    /// play after rendering, audio device).
    pub fn apply_prefs(&mut self, ctx: &egui::Context) {
        self.apply_ui_scale(ctx);
        // Appearance Mode ▸ Sync with system: follow the system while running. The host reports
        // changes as they happen (no polling) and wakes the UI only when the value changes.
        if self.applied_prefs.is_some() {
            let k = self.appearance_kind(ctx);
            if k != self.tokens.kind {
                self.set_theme(ctx, k);
            }
        }
        // checked every frame: a theme or font change installs the style again
        let animation = if self.session.prefs.appearance.animations { theme::ANIMATION_TIME } else { 0.0 };
        if ctx.global_style().animation_time != animation {
            ctx.global_style_mut(|s| s.animation_time = animation);
        }
        if self.applied_prefs.as_ref() == Some(&self.session.prefs) {
            return;
        }
        let p = self.session.prefs.clone();
        let prev = self.applied_prefs.take();
        if prev.as_ref().is_none_or(|q| q.appearance != p.appearance || q.general.show_tool_tips != p.general.show_tool_tips) {
            let k = self.appearance_kind(ctx);
            self.set_theme(ctx, k);
        }
        if prev.as_ref().is_none_or(|q| q.general.interface_language != p.general.interface_language) {
            let language = match i18n::Language::parse(&p.general.interface_language) {
                Some(l) => l,
                None if p.general.interface_language == "system" => {
                    i18n::Language::from_locales(&self.hooks.system_languages.as_ref().map(|f| f()).unwrap_or_default())
                }
                None => i18n::Language::default(),
            };
            if language == i18n::Language::Ja && !i18n::install_japanese_font(ctx) {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Japanese font is installed on this system; the interface stays in English").into();
            } else if language == i18n::Language::ZhCn && !i18n::chinese_font_available() {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Chinese font is installed on this system; the interface stays in English").into();
            } else if language == i18n::Language::Ko && !i18n::korean_font_available() {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Korean font is installed on this system; the interface stays in English").into();
            } else {
                self.ui.language = language;
            }
            i18n::set_current(self.ui.language);
            ctx.set_language(Some(self.ui.language.bcp47().to_owned()));
        }
        self.frames.set_cache_budget(p.memory.frame_cache_mb as usize * (1 << 20));
        self.ui.play_after_render = p.timeline.play_after_rendering;
        let hardware_changed = prev.as_ref().is_none_or(|q| q.audio_hardware != p.audio_hardware);
        let pitch_changed = prev.as_ref().is_some_and(|q| q.audio.maintain_pitch_shuttling != p.audio.maintain_pitch_shuttling);
        if hardware_changed {
            if let Some(input) = self.session.voiceover.input.as_mut() {
                input.configure_host(&p.audio_hardware.device_class);
            }
            let rate = self.session.active_sequence().map(|q| q.settings.sample_rate);
            if let Some(a) = self.audio.as_mut() {
                a.stop();
                a.configure(&p.audio_hardware, rate);
            }
        }
        if (hardware_changed || pitch_changed) && self.playback.playing && self.playback.preroll.is_none() {
            self.playback.anchor_tick = self.session.playhead();
            let now = self.now();
            self.playback.anchor_time = now;
            self.playback.audio_seen = (0, now);
            self.start_audio(now);
        }
        if let Some(note) = self.audio.as_ref().and_then(|a| a.note()) {
            self.ui.status = note;
        }
        if prev.is_none() && !self.workspace_restored {
            // reopen the workspace in use when the app last closed
            self.workspace_restored = true;
            if let Some(w) = dock::find(&self.workspaces, &self.workspaces.current) {
                self.set_workspace(&w);
            }
        }
        self.applied_prefs = Some(p);
    }

    /// Every sequence has its own Timeline view (zoom, scroll, track heights), as Premiere's
    /// sequence tabs do. `ui.timeline` holds the active sequence's; the views of all sequences are
    /// in `session.state.timeline_views`, which is saved with the project. Each frame this
    /// - gives `ui.timeline` the view of a sequence that has just become active (a sequence shown
    ///   for the first time is fitted, with default track heights),
    /// - writes a change made in the panel to the session,
    /// - and takes over a view that was changed in the session (by a command or the control
    ///   channel).
    fn sync_timeline_view(&mut self) {
        let Some(active) = self.session.state.active_sequence else {
            self.timeline_view_of = None;
            return;
        };
        let stored = self.session.state.timeline_views.get(&active).copied();
        let v = &mut self.ui.timeline;
        let shown = filmcraft_engine::project::SequenceView {
            pps: v.target_pps,
            scroll: v.target_scroll,
            v_scroll: v.v_scroll,
            a_scroll: v.a_scroll,
            video_track_h: v.video_track_h,
            audio_track_h: v.audio_track_h,
        };
        let show = |v: &mut state::TimelineView, s: filmcraft_engine::project::SequenceView| {
            (v.pps, v.target_pps, v.scroll, v.target_scroll) = (s.pps, s.pps, s.scroll, s.scroll);
            (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (s.v_scroll, s.a_scroll, s.video_track_h, s.audio_track_h);
            (v.fit_pending, v.fit_empty) = (false, None);
        };
        if self.timeline_view_of != Some(active) {
            self.timeline_view_of = Some(active);
            match stored.and_then(|s| s.checked()) {
                Some(s) => {
                    show(v, s);
                    self.timeline_view_last = Some(s);
                }
                None => {
                    let d = state::TimelineView::default();
                    (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (0.0, 0.0, d.video_track_h, d.audio_track_h);
                    (v.fit_pending, v.fit_empty) = (true, None);
                    self.timeline_view_last = None;
                }
            }
        } else if v.fit_pending {
            // the panel has not fitted the sequence yet: nothing to keep
        } else if self.timeline_view_last != Some(shown) {
            self.session.state.timeline_views.insert(active, shown);
            self.timeline_view_last = Some(shown);
        } else if let Some(s) = stored.filter(|s| *s != shown).and_then(|s| s.checked()) {
            show(v, s);
            self.timeline_view_last = Some(s);
            self.session.state.timeline_views.insert(active, s);
        }
    }

    fn workspaces_path(&self) -> Option<std::path::PathBuf> {
        self.session.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join(dock::WORKSPACES_FILE))
    }

    /// Replace the saved workspaces and write them to the data directory.
    pub fn set_workspaces(&mut self, w: dock::WorkspacePrefs) -> Result<(), String> {
        self.workspaces = w;
        match self.workspaces_path() {
            Some(p) => self.workspaces.save(&p).map_err(|e| format!("saving workspaces: {e}")),
            None => Ok(()),
        }
    }

    pub fn set_workspace(&mut self, name: &str) {
        self.ui.workspace = name.to_string();
        let layout = dock::saved_docking(&self.workspaces, name);
        self.ui.dock = layout.root;
        self.ui.floating = layout.floating;
        self.pending_panel_raise = None;
        self.ui.keys.maximized = None;
        if self.workspaces.current != name {
            let mut next = self.workspaces.clone();
            next.current = name.to_string();
            if let Err(e) = self.set_workspaces(next) {
                self.ui.status = e;
            }
        }
        if name == "Color" {
            self.ui.show_scopes = false;
        }
    }

    pub fn show_panel(&mut self, p: PanelKind) {
        // closed while floating: it floats again where it was (#1985); taken before the Timeline's
        // own restore below docks it
        let open = self.ui.dock.contains(p) || self.ui.floating.iter().any(|group| group.panels.contains(&p));
        let refloat = match self.ui.closed_floating.iter().position(|(k, _)| *k == p) {
            Some(i) if !open => Some(self.ui.closed_floating.remove(i).1),
            _ => None,
        };
        if p == PanelKind::Timeline && !self.ui.floating.iter().any(|group| group.panels.contains(&p)) {
            self.ui.dock.restore_timeline();
        }
        if !self.ui.dock.contains(p) && !self.ui.floating.iter().any(|group| group.panels.contains(&p)) {
            let near = match p {
                // a side panel beside Properties (or Essential Sound), never over the Program monitor
                PanelKind::TextToSpeech => [PanelKind::Properties, PanelKind::EssentialSound, PanelKind::EssentialGraphics]
                    .into_iter()
                    .find(|k| self.ui.dock.contains(*k))
                    .unwrap_or(PanelKind::Program),
                PanelKind::LumetriColor | PanelKind::EssentialGraphics | PanelKind::EssentialSound | PanelKind::Properties => PanelKind::Program,
                PanelKind::Source
                | PanelKind::EffectControls
                | PanelKind::AudioClipMixer
                | PanelKind::Metadata
                | PanelKind::LumetriScopes
                | PanelKind::AudioTrackMixer
                | PanelKind::Text
                | PanelKind::ReferenceMonitor
                | PanelKind::Timecode => PanelKind::Source,
                _ => PanelKind::Project,
            };
            self.ui.dock.open_near(p, near);
        }
        if let Some(rect) = refloat.filter(|r| r.iter().all(|v| v.is_finite())) {
            let rect = [rect[0], rect[1], rect[2].max(160.0), rect[3].max(120.0)];
            if let Err(e) = self.apply_dock(craft_ui::docking::Action::Float { panel: p, rect }) {
                // refused (it is the only docked panel left): it stays docked, and its floating
                // place is kept rather than lost
                log::warn!("could not float {} again: {e}", p.id());
                self.ui.closed_floating.push((p, rect));
            }
        }
        let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: p });
        self.ui.focused = p;
    }

    /// Reveal in Project: bring the Project panel forward, showing the bin that holds `item` with
    /// the search cleared, so the (already selected) item is on show.
    fn reveal_in_project(&mut self, item: filmcraft_project::ItemId) {
        // the bins on the way down to the item (bounded: a project file can nest bins arbitrarily deep)
        fn path(b: &filmcraft_project::Bin, item: filmcraft_project::ItemId, depth: usize, out: &mut Vec<u64>) -> bool {
            if depth > 256 {
                return false;
            }
            for c in &b.children {
                match c {
                    filmcraft_project::BinEntry::Item(i) if *i == item => return true,
                    filmcraft_project::BinEntry::Bin(inner) => {
                        out.push(inner.id.0);
                        if path(inner, item, depth + 1, out) {
                            return true;
                        }
                        out.pop();
                    }
                    _ => {}
                }
            }
            false
        }
        let mut bins = Vec::new();
        let found = path(&self.session.project.root, item, 0, &mut bins);
        // the list shows the whole tree: open the bins on the way. Icons and freeform show one bin
        // at a time: go into the one that holds the item.
        let list = self.session.prefs.project_panel.view.mode == filmcraft_engine::project_panel::ViewMode::List;
        self.ui.project_panel.bin = if list || !found { None } else { bins.last().copied() };
        if list {
            for b in bins {
                if !self.ui.expanded_bins.contains(&b) {
                    self.ui.expanded_bins.push(b);
                }
            }
        }
        self.ui.project_panel.active_tab = None;
        self.ui.project_panel.selected_bin = None;
        self.ui.project_search.clear();
        self.show_panel(PanelKind::Project);
    }

    pub fn status(&mut self, s: impl Into<String>) {
        self.ui.status = s.into();
    }

    // ---------------------------------------------------------------- playback

    pub fn toggle_play(&mut self, speed: f64) {
        if self.playback.playing {
            self.stop();
        } else {
            self.play(speed);
        }
    }

    pub fn play(&mut self, speed: f64) {
        self.stop_source();
        if self.session.active_sequence().is_none() {
            return;
        }
        // restart from the end → from the start (Settings ▸ Timeline ▸ "At playback end, return to
        // beginning when restarting playback")
        let dur = self.session.active_sequence().map(|q| q.duration()).unwrap_or_default();
        // (not while a voice-over take starts: it was put exactly where the take must begin, e.g. on empty timeline past the end)
        if speed > 0.0
            && self.session.prefs.timeline.return_to_beginning
            && !self.session.voiceover.recording()
            && self.session.playhead() >= dur - self.session.sequence_rate().frame_duration()
        {
            self.session.set_playhead(Tick::ZERO);
        }
        // A loop restart keeps counting into the same meter.
        if !self.playback.playing || self.playback.speed != speed {
            self.playback.meter.start(speed);
        }
        self.playback.playing = true;
        self.playback.speed = speed;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.anchor_time = -1.0; // set when the preroll ends
        self.playback.preroll = Some(-1.0);
        self.playback.preroll_ready = false;
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.playback.audio_clock = false;
        self.playback.audio_retry_at = 0.0;
    }

    /// Start the clock (and audio) once the first frames are ready or the preroll timed out.
    fn end_preroll(&mut self, now: f64) {
        self.playback.preroll = None;
        self.playback.anchor_time = now;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.audio_seen = (0, now);
        self.start_audio(now);
        // Audio Track Mixer: an automation pass runs while playing forward in real time
        if (self.playback.speed - 1.0).abs() < 1e-9 && !self.session.mixrec.active() {
            let t = self.session.playhead();
            let _ = self.session.execute("mixer.recordStart", json!({"time": t.0}));
        }
        // Multi-Camera view: playing records live cuts (keys 1–9 / clicking angles)
        panels::multicam::on_play(self);
        // voice-over: the capture starts with the audio clock
        panels::voiceover::on_play(self);
    }

    /// Play the last Text to Speech preview (`Session::tts_preview`) on the audio output, stopping
    /// timeline playback first. Starting playback again takes the output back.
    pub fn play_tts_preview(&mut self) -> Result<f64, String> {
        let audio = self.session.tts_preview.clone().ok_or("nothing to preview")?;
        if self.playback.playing {
            self.stop();
        }
        let a = self.audio.as_mut().ok_or("no audio output (check Settings > Audio Hardware)")?;
        let sr = a.sample_rate().max(1);
        a.stop();
        let step = f64::from(audio.sample_rate.max(1)) / f64::from(sr);
        let seconds = audio.seconds();
        let mut pos = 0.0f64;
        let fill = Box::new(move |buf: &mut [f32], ch: usize| {
            for frame in buf.chunks_mut(ch.max(1)) {
                let i = pos as usize;
                let f = (pos - i as f64) as f32;
                let x0 = audio.samples.get(i).copied().unwrap_or(0.0);
                let x1 = audio.samples.get(i + 1).copied().unwrap_or(0.0);
                frame.fill(x0 + (x1 - x0) * f);
                pos += step;
            }
        });
        a.start(fill)?;
        self.playback.audio_clock = false;
        Ok(seconds)
    }

    pub fn stop(&mut self) {
        self.playback.playing = false;
        self.playback.stop_at = None;
        self.playback.rest_at = None;
        self.playback.preroll = None;
        self.playback.meter.finish();
        self.frames.stop_prefetch();
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.scrub.device_stopped();
        self.playback.audio_clock = false;
        self.playback.audio_retry_at = 0.0;
        if self.session.mixrec.active() {
            let t = self.session.playhead();
            if let Err(e) = self.session.execute("mixer.recordStop", json!({"time": t.0})) {
                self.ui.status = e.to_string();
            }
        }
        panels::multicam::on_stop(self);
        panels::voiceover::on_stop(self);
    }

    fn start_audio(&mut self, now: f64) {
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let start_tick = self.session.playhead();
        let Some(a) = self.audio.as_mut() else { return };
        let sr = a.sample_rate();
        // Output frames still follow the accelerated timeline. The pitch preference selects
        // either the original rate conversion or pitch-corrected program audio.
        let Some((mix_rate, mix)) = shuttle::mix(&self.session, seq_id, sr, self.playback.speed) else {
            a.stop();
            self.playback.audio_retry_at = 0.0;
            return;
        };
        let cursor = start_tick.to_units_floor(i64::from(mix_rate));
        // the old stream stops before the new mixer resets the underrun counters
        a.stop();
        // Desktop: mix ahead on a thread so the device callback never waits on decoding.
        #[cfg(not(target_arch = "wasm32"))]
        let fill = play_ahead::spawn(mix, cursor, sr, a.channels(), self.playback.audio_stats.clone());
        #[cfg(target_arch = "wasm32")]
        let fill = {
            let (mut mix, mut cursor) = (mix, cursor);
            Box::new(move |buf: &mut [f32], ch: usize| {
                mix(cursor, buf, ch);
                cursor += (buf.len() / ch.max(1)) as i64;
            })
        };
        match a.start(fill) {
            Ok(_) => {
                self.playback.audio_clock = true;
                self.playback.audio_retry_at = 0.0;
                if let Some(note) = self.audio.as_ref().and_then(|audio| audio.note()) {
                    self.ui.status = note;
                }
            }
            Err(e) => {
                log::warn!("audio output unavailable: {e}");
                self.playback.audio_clock = false;
                self.playback.audio_retry_at = now + AUDIO_RETRY_S;
            }
        }
    }

    /// Advance the Program playhead only while playing; owns preroll completion, audio-clock anchoring, output retry and the fallback to wall time when audio
    /// is lost or stalls.
    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playback.playing {
            return;
        }
        let now = self.now();
        if let Some(since) = self.playback.preroll {
            let since = if since < 0.0 { now } else { since };
            self.playback.preroll = Some(since);
            if self.playback.preroll_ready || now - since >= frames::PREROLL_TIMEOUT_S {
                self.end_preroll(now);
            } else {
                ctx.request_repaint();
                return;
            }
        }
        if self.playback.anchor_time < 0.0 {
            self.playback.anchor_time = now;
        }
        if !self.playback.audio_clock && self.playback.audio_retry_at > 0.0 && now >= self.playback.audio_retry_at {
            // The output failed mid-play: open it again where the wall clock got to (what
            // pause/play does by hand), so a recovering device brings the sound back.
            self.playback.audio_retry_at = now + AUDIO_RETRY_S;
            self.playback.anchor_tick = self.session.playhead();
            self.playback.anchor_time = now;
            self.playback.audio_seen = (0, now);
            self.start_audio(now);
        }
        let rate = self.session.sequence_rate();
        if self.playback.audio_clock
            && let Some(audio) = self.audio.as_mut()
        {
            audio.recover();
        }
        let reading = if self.playback.audio_clock { self.audio.as_ref().and_then(|a| a.played_frames().map(|f| (f, a.sample_rate()))) } else { None };
        if self.playback.audio_clock && reading.is_none() {
            // A device error relinquishes its clock immediately. Resume from the displayed
            // frame, rather than jumping to the old wall-clock anchor after buffered playback.
            self.playback.anchor_tick = self.session.playhead();
            self.playback.anchor_time = now;
            self.playback.audio_clock = false;
            self.playback.audio_retry_at = now + AUDIO_RETRY_S;
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }
            log::warn!("audio output lost its playback clock; playing without sound");
            self.ui.status = tl!("Audio output failed: playing without sound (check Settings > Audio Hardware)").into();
        }
        if let Some((f, sr)) = reading {
            if f != self.playback.audio_seen.0 {
                self.playback.audio_seen = (f, now);
            } else if now - self.playback.audio_seen.1 >= AUDIO_STALL_S {
                // the device stopped consuming samples: continue from where the audio got to on
                // the wall clock, without sound
                let played = if sr > 0 { f as f64 / sr as f64 } else { 0.0 };
                self.playback.anchor_tick += Tick::from_seconds_f64(played * self.playback.speed);
                self.playback.anchor_time = now;
                self.playback.audio_clock = false;
                if let Some(a) = self.audio.as_mut() {
                    a.stop();
                }
                log::warn!("audio output stalled (no samples consumed for {AUDIO_STALL_S} s); playing without sound");
                self.ui.status = tl!("Audio output is not responding: playing without sound (check Settings > Audio Hardware)").into();
            }
        }
        let elapsed = match reading {
            Some((f, sr)) if self.playback.audio_clock && sr > 0 => f as f64 / sr as f64,
            _ => now - self.playback.anchor_time,
        };
        let t = self.playback.anchor_tick + Tick::from_seconds_f64(elapsed * self.playback.speed);
        let seq = self.session.active_sequence();
        let dur = seq.map(|q| q.duration()).unwrap_or_default();
        let (lo, hi) = if self.playback.looping {
            (seq.and_then(|q| q.mark_in).unwrap_or(Tick::ZERO), seq.and_then(|q| q.mark_out).map(|o| o + rate.frame_duration()).unwrap_or(dur))
        } else {
            (Tick::ZERO, dur)
        };
        if let Some(end) = self.playback.stop_at.filter(|e| t >= *e && self.playback.speed > 0.0 && !self.playback.looping) {
            self.session.set_playhead(self.playback.rest_at.unwrap_or(end));
            self.stop();
        } else if t >= hi && self.playback.speed > 0.0 && (self.playback.looping || !self.session.voiceover.recording()) {
            // a voice-over take keeps playing (and recording) into the empty timeline past the end
            if self.playback.looping {
                self.session.set_playhead(lo);
                self.play(self.playback.speed);
            } else {
                self.session.set_playhead(hi - rate.frame_duration());
                self.stop();
            }
        } else if t <= Tick::ZERO && self.playback.speed < 0.0 {
            self.session.set_playhead(Tick::ZERO);
            self.stop();
        } else {
            self.session.set_playhead(t);
        }
        ctx.request_repaint();
    }

    // ---------------------------------------------------------------- textures

    /// Upload a rendered frame into a named texture (only when the key changed), opaque: the
    /// picture over black.
    pub fn texture_for(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        self.upload_frame(ctx, name, key, img, false)
    }

    /// Like [`Self::texture_for`], keeping the frame's alpha, so what is painted under the
    /// texture (the monitors' Transparency Grid) shows through its transparent areas.
    pub fn texture_with_alpha(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        self.upload_frame(ctx, name, key, img, true)
    }

    /// Fetch or create the named texture and upload only when `key` changed; `alpha` selects
    /// transparent or opaque conversion. Returns the texture's stable id.
    fn upload_frame(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba, alpha: bool) -> egui::TextureId {
        let image = || img.color_image(alpha);
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                tex.set(image(), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let tex = ctx.load_texture(name, image(), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    /// Like [`Self::texture_for`], uploading `map(img)` (computed only when the key changed).
    pub fn texture_for_mapped(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        key: FrameKey,
        img: &frames::Rgba,
        map: impl FnOnce(&frames::Rgba) -> frames::Rgba,
    ) -> egui::TextureId {
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                let m = map(img);
                tex.set(egui::ColorImage::from_rgba_unmultiplied([m.w, m.h], &m.px), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let m = map(img);
        let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([m.w, m.h], &m.px), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    pub fn texture_existing(&self, name: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        self.textures.get(name).map(|(_, t)| (t.id(), t.size_vec2()))
    }

    /// The frame a named texture shows.
    pub fn texture_key(&self, name: &str) -> Option<FrameKey> {
        self.textures.get(name).map(|(k, _)| *k)
    }

    /// The frame whose picture the Program monitor drew last: the frame due at the playhead, or
    /// the stand-in shown while that one renders (`perf.stats`, tests).
    pub fn program_picture(&self) -> Option<FrameKey> {
        self.program_shown
    }

    /// Get a thumbnail texture for an item at a media time (requested at low priority).
    /// Cache revision of a project item's own frames: media changes only when its file does
    /// (relink, Make Offline, proxies on/off), so its frames survive unrelated edits; other items
    /// (sequences) follow the project revision.
    pub fn item_revision(&self, item: filmcraft_project::ItemId) -> u64 {
        let p = &self.session.project;
        let target = match p.item(item).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Subclip { parent, .. }) => *parent,
            _ => item,
        };
        match p.item(target).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Media(m)) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                filmcraft_engine::media_pool::media_key(m).hash(&mut h);
                (m.proxy.is_some() && self.session.media.use_proxies()).hash(&mut h);
                h.finish()
            }
            _ => self.session.revision,
        }
    }

    pub fn thumbnail(&mut self, ctx: &egui::Context, item: filmcraft_project::ItemId, t: Tick, width: u32) -> Option<(egui::TextureId, egui::Vec2)> {
        let pi = self.session.project.item(item)?;
        let src_w = match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref()?.width,
            filmcraft_project::ItemKind::Sequence(s) => s.settings.width,
            _ => return None,
        };
        let rate = pi.frame_rate();
        let frame = rate.frame_at(t);
        let rev = self.item_revision(item);
        let key = FrameKey { target: Target::Item(item), frame, size: width, revision: rev, draft: false };
        let name = format!("thumb-{}-{}-{}", item.0, frame, width);
        if let Some(img) = self.frames.get(&key) {
            let id = self.texture_for(ctx, &name, key, &img);
            self.thumb_touched(ctx, &name);
            return Some((id, egui::vec2(img.w as f32, img.h as f32)));
        }
        let scale = width as f32 / src_w.max(1) as f32;
        let project = self.session.project.clone();
        self.frames.request(key, rate.tick_of(frame), scale, &project, 50);
        let shown = self.texture_existing(&name);
        if shown.is_some() {
            self.thumb_touched(ctx, &name);
        }
        shown
    }

    /// Note that thumbnail texture `name` is used in this pass, and free the least recently used
    /// thumbnails once there are more than [`MAX_THUMB_TEXTURES`]: every frame shown in a timeline
    /// tile or a hover scrub got a texture of its own that was never freed (#1682).
    fn thumb_touched(&mut self, ctx: &egui::Context, name: &str) {
        let pass = ctx.cumulative_pass_nr();
        if self.thumb_used.insert(name.to_string(), pass).is_some() || self.thumb_used.len() <= MAX_THUMB_TEXTURES {
            return;
        }
        // down to three quarters, never a thumbnail already drawn in this pass
        let mut old: Vec<(u64, String)> = self.thumb_used.iter().filter(|(_, p)| **p < pass).map(|(n, p)| (*p, n.clone())).collect();
        old.sort_unstable();
        let excess = self.thumb_used.len().saturating_sub(MAX_THUMB_TEXTURES * 3 / 4);
        for (_, n) in old.into_iter().take(excess) {
            self.thumb_used.remove(&n);
            self.textures.remove(&n);
        }
    }

    /// The number of thumbnail textures held (tests, diagnostics).
    pub fn thumbnail_textures(&self) -> usize {
        self.textures.keys().filter(|n| n.starts_with("thumb-")).count()
    }

    // ---------------------------------------------------------------- files

    /// Destination of imports started from the currently shown Project panel view.
    pub fn import_bin(&self) -> filmcraft_project::BinId {
        panels::project::view_of(self, panels::project::shown_inst(self)).bin
    }

    pub fn file_dialog(&mut self, id: &str, params: &Value) -> Result<Value, String> {
        match id {
            "file.import" => {
                let exts: Vec<&str> = filmcraft_media::VIDEO_EXTENSIONS
                    .iter()
                    .chain(filmcraft_media::AUDIO_EXTENSIONS)
                    .chain(filmcraft_media::STILL_EXTENSIONS)
                    .chain(&["srt", "vtt", "scc", "edl", "xml", "fcpxml", "otio", "aaf", "omf"])
                    .copied()
                    .collect();
                let bin = params.get("bin").cloned();
                self.pick(FileDialog::open_files(&exts), move |app, paths| {
                    if paths.is_empty() {
                        return Ok(Value::Null);
                    }
                    let bin = bin.unwrap_or_else(|| json!(app.import_bin().0));
                    let r = app.session.execute("file.import", json!({"paths": paths, "bin": bin})).map_err(|e| e.to_string());
                    if let Ok(v) = &r
                        && let Some(errs) = v.get("errors").and_then(Value::as_array)
                        && !errs.is_empty()
                    {
                        app.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
                    }
                    r
                })
            }
            // File ▸ Import with Image Sequence: choose the first numbered still
            "file.importImageSequence" => self.pick(FileDialog::open_files(filmcraft_media::STILL_EXTENSIONS), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                let r = app.session.execute("file.importImageSequence", json!({"path": path})).map_err(|e| e.to_string());
                if let Err(e) = &r {
                    app.ui.status = e.clone();
                }
                r
            }),
            "file.saveAs" | "file.save" | "file.saveCopy" => {
                let suggested =
                    if id == "file.saveCopy" { format!("{} copy.fcproj", self.session.project.name) } else { format!("{}.fcproj", self.session.project.name) };
                let cmd = if id == "file.saveCopy" { "file.saveCopy" } else { "file.saveAs" };
                self.pick(FileDialog::SaveProject { name: suggested }, move |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    app.session.execute(cmd, json!({"path": path})).map_err(|e| e.to_string())
                })
            }
            "file.open" => self.pick(FileDialog::OpenProject, |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())
            }),
            "graphics.newFromFile" => {
                let exts: Vec<&str> = filmcraft_media::STILL_EXTENSIONS.iter().chain(filmcraft_media::VIDEO_EXTENSIONS).copied().collect();
                self.pick(FileDialog::open_files(&exts), |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    app.session.execute("graphics.newFromFile", json!({"path": path})).map_err(|e| e.to_string())
                })
            }
            "captions.import" => self.pick(FileDialog::open_files(&["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp", "xml"]), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.session.execute("captions.import", json!({"path": path})).map_err(|e| e.to_string())
            }),
            "captions.export" => {
                let name = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default();
                let suggested = format!("{}.srt", name.replace(' ', "_"));
                let dialog = FileDialog::save_as(
                    "Captions (SRT, WebVTT, SCC, MCC, EBU STL, TTML, DFXP)",
                    &["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp"],
                    &suggested,
                );
                let mut p = params.clone();
                self.pick(dialog, move |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    p["path"] = json!(path);
                    app.session.execute("captions.export", p).map_err(|e| e.to_string())
                })
            }
            _ => match EXPORT_SAVE_DIALOGS.iter().find(|(c, ..)| *c == id) {
                Some(&(_, filter, ext)) => self.export_save_dialog(id, filter, ext, params),
                None => Err(format!("no dialog for {id}")),
            },
        }
    }

    /// File ▸ Export ▸ EDL…, Final Cut Pro XML… etc. from the menus (#382): ask where to save, then
    /// run the command with that `path` and the rest of `params`.
    fn export_save_dialog(&mut self, id: &str, filter: &str, ext: &str, params: &Value) -> Result<Value, String> {
        filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&self.session))?;
        if !FileDialog::save_as(filter, &[ext], "").available(&self.hooks) {
            return Err("no save dialog available: run the command with a `path`".into());
        }
        // Timelines are named after the sequence; a selection must not suggest the open project's file.
        let sequence = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone());
        let stem = match id {
            "file.exportAle" => self.session.project.name.clone(),
            "file.exportSelectionProject" => format!("{} selection", self.session.project.name),
            _ => sequence.unwrap_or_else(|| self.session.project.name.clone()),
        };
        let stem: String = stem.chars().map(|c| if matches!(c, '/' | '\\') || c.is_control() { '-' } else { c }).collect();
        let stem = if stem.trim().is_empty() { "Untitled".to_string() } else { stem };
        let mut p = params.as_object().cloned().unwrap_or_default();
        let id = id.to_string();
        self.pick(FileDialog::save_as(filter, &[ext], &format!("{stem}.{ext}")), move |app, paths| {
            let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
            p.insert("path".into(), json!(path));
            let r = app.session.execute(&id, Value::Object(p)).map_err(|e| e.to_string());
            app.ui.status = match &r {
                Ok(_) => tlf!("Exported {path}", path),
                Err(e) => e.clone(),
            };
            r
        })
    }

    /// Import dropped files. With Timeline ▸ drop imports to the timeline, the new items are also
    /// queued to be placed at the pointer (see `timeline::interact`).
    fn handle_drops(&mut self, ctx: &egui::Context) {
        // While files hover, the OS sends no cursor events: poll the cursor each frame to draw the Timeline preview.
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        self.file_drag_hover = if hovering > 0 && self.session.prefs.timeline.drop_import_to_timeline {
            ctx.request_repaint();
            self.drop_pointer(ctx).map(|p| (hovering, p))
        } else {
            None
        };
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let mut paths = Vec::new();
        for f in dropped {
            let p = f.path();
            if p.exists() {
                paths.push(p.to_string_lossy().to_string());
            }
        }
        if !paths.is_empty() {
            let pointer = self.drop_pointer(ctx);
            let imported = self.session.execute("file.import", json!({"paths": paths, "bin": self.import_bin().0}));
            if let Ok(v) = imported
                && let Some(pos) = pointer
                && self.session.prefs.timeline.drop_import_to_timeline
            {
                let items: Vec<filmcraft_engine::project::ItemId> =
                    v["items"].as_array().map(|a| a.iter().filter_map(|i| i.as_u64()).map(filmcraft_engine::project::ItemId).collect()).unwrap_or_default();
                if !items.is_empty() {
                    self.pending_timeline_drop = Some((items, pos));
                }
            }
        }
    }

    /// Where files dragged in from the OS are over the window. The OS reports no cursor motion during such a drag
    /// (winit, Windows), so ask it through the host hook, else use the last pointer position egui saw.
    fn drop_pointer(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        let os_pointer = self
            .hooks
            .cursor_screen_position
            .as_ref()
            .and_then(|f| f())
            .and_then(|px| ctx.input(|i| i.viewport().inner_rect.map(|r| screen_px_to_ui(px, i.pixels_per_point, r.min))));
        os_pointer.or_else(|| ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())))
    }

    // ---------------------------------------------------------------- input

    /// Sends the menu tree to the native menu bar when it changed, so its items show the same
    /// labels, shortcuts and enablement as the in-window menus (Undo with nothing to undo is
    /// disabled). Without a native menu bar this does nothing.
    fn sync_native_menu(&mut self) {
        if self.hooks.menu_changed.is_none() {
            return;
        }
        let items = menus::menu_items(self);
        if items == self.menu_sent {
            return;
        }
        if let Some(hook) = self.hooks.menu_changed.as_mut() {
            hook(&items);
        }
        self.menu_sent = items;
    }

    /// Whether keyboard focus is on a dock divider or a floating panel's resize grip, as drawn last pass.
    fn divider_focused(&self, ctx: &egui::Context) -> bool {
        let Some(focused) = ctx.memory(|m| m.focused()) else { return false };
        self.auto
            .all()
            .iter()
            .any(|e| e.response_id == Some(focused) && (e.id.starts_with("dock.gutter.") || (e.id.starts_with("panel.float.") && e.id.ends_with(".resize"))))
    }

    /// Per-pass shortcut handling: reset the pasted image, rebuild bindings only when the shortcut revision or workspace set changes, and leave unmodified keys
    /// to a focused text field except dock dividers.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // A paste image payload belongs to the paste event in this pass only.
        self.pasted_image = None;
        let workspaces = (dock::names(&self.workspaces), self.ui.workspace.clone());
        if self.bindings_rev != self.session.shortcuts.revision || workspaces != self.menu_workspaces {
            self.menu_workspaces = workspaces;
            self.bindings = menus::bindings(self);
            self.bindings_rev = self.session.shortcuts.revision;
        }
        // A pane divider or floating-panel grip keeps focus after a pointer drag so arrows can
        // nudge it, but it takes no other key: Space and the other shortcuts keep working (#1482).
        let divider_focused = self.divider_focused(ctx);
        if (ctx.egui_wants_keyboard_input() && !divider_focused) || self.dialog == Some(Dialog::Shortcuts) {
            return;
        }
        // A focused text field takes every unmodified key, including the ones our shortest
        // shortcuts use, so an input method can compose there (Vietnamese Telex, Korean, …; #243).
        // `egui_wants_keyboard_input` above is not enough: it is read before this pass lays the UI
        // out, so it still reports the previous pass, and it is true for any focused widget, which
        // would leave the editor without its single-key tool shortcuts while a slider has focus.
        let text_focused = ctx.text_edit_focused();
        // Esc cancels a dynamic trim in progress
        if self.session.trim_play.dynamic.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            let _ = self.session.execute("trim.cancelDynamic", json!({}));
        }
        panels::masks::pen_take_back(self, ctx, text_focused);
        // Panel shortcuts of the focused panel override application shortcuts on the same chord.
        // egui ignores extra Shift and Alt when matching, so the most specific chords go first
        // across both scopes (the sort is stable, so the panel still wins a tie): otherwise the
        // Timeline's `Delete` (Clear) swallows the app-wide `Shift+Delete` (Ripple Delete) (#680).
        let focused = self.ui.focused.title();
        let mut fire = Vec::new();
        ctx.input_mut(|i| {
            let modifiers = i.modifiers;
            // With no text on the system clipboard, egui-winit drops the Ctrl+V press but still
            // passes its release: put the press back, so Paste can paste files or an image (#611).
            let mut swallowed = None;
            for e in &i.events {
                match e {
                    egui::Event::Key { key: egui::Key::V, pressed, modifiers: m, .. } => {
                        if !pressed && !self.v_down {
                            swallowed = Some(*m);
                        }
                        self.v_down = *pressed;
                    }
                    egui::Event::PasteImage(image) => {
                        // Keep the delivered pixels; re-reading the OS clipboard later is racy and
                        // the web host has no clipboard-media hook at all.
                        self.pasted_image = Some(image.clone());
                        if modifiers.command {
                            self.v_down = true;
                        }
                    }
                    egui::Event::Paste(_) if modifiers.command => self.v_down = true,
                    _ => {}
                }
            }
            if let Some(m) = swallowed {
                i.events.push(egui::Event::Key {
                    key: egui::Key::V,
                    physical_key: Some(egui::Key::V),
                    pressed: true,
                    repeat: false,
                    modifiers: m | egui::Modifiers::COMMAND,
                });
            }
            clipboard_events_as_keys(&mut i.events, modifiers);
            let panel = self.bindings.iter().filter(|b| b.3.as_deref() == Some(focused));
            let app_wide = self.bindings.iter().filter(|b| b.3.is_none());
            let mut candidates: Vec<&menus::KeyBinding> = panel.chain(app_wide).collect();
            candidates.sort_by_key(|b| std::cmp::Reverse(menus::specificity(b.0)));
            for (m, k, id, _) in candidates {
                // With a text field focused, an unmodified key belongs to the field (and to the input
                // method composing in it), not to a tool shortcut: `J`, `Z`, `R`, `A`… are the keys
                // Vietnamese Telex combines into diacritics (#243). Chords with a modifier stay:
                // Ctrl+S and friends are not something a text field or an IME consumes.
                if lets_text_field_keep(*m, text_focused) {
                    continue;
                }
                // The focused divider consumes the arrows itself (egui matches them with any Shift/Alt).
                if divider_focused && matches!(k, egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown) {
                    continue;
                }
                if i.consume_key(*m, *k) {
                    fire.push(id.clone());
                }
            }
        });
        for mut id in fire {
            // Select All / Deselect All act on the Project panel's items when it has focus (#168).
            // Select All takes only what the shown bin lists, not the whole project (#456).
            if self.ui.focused == PanelKind::Project && id == "edit.selectAll" {
                let (cmd, params) = panels::project::select_all_command(self);
                if let Err(e) = menus::invoke(self, ctx, &cmd, params) {
                    self.ui.status = e;
                }
                continue;
            }
            if self.ui.focused == PanelKind::Project && id == "edit.deselectAll" {
                id = "project.deselectAll".into();
            }
            // Clear in the Project panel removes the selected bin when no item is selected (#587).
            let clear = if self.ui.focused == PanelKind::Project && id == "project.delete" {
                self.ui.project_panel.rename = None;
                crate::panels::project_views::clear_params(&self.session.state.project_selection, self.ui.project_panel.selected_bin)
            } else {
                None
            };
            // Mark In/Out in the Source monitor when it has focus.
            let params = if let Some(p) = clear {
                p
            } else if self.ui.focused == PanelKind::Source
                && (matches!(id.as_str(), "markers.markIn" | "markers.markOut" | "markers.clearInOut")
                    || id.starts_with("markers.markSplit")
                    || id.starts_with("markers.goToSplit"))
            {
                json!({"target": "source"})
            } else {
                json!({})
            };
            if let Err(e) = menus::invoke(self, ctx, &id, params) {
                self.ui.status = e;
            }
        }
        // Only the paste action of this pass may consume the event's image.
        self.pasted_image = None;
    }

    // ---------------------------------------------------------------- control channel

    /// Make the UI pass run for a control request without stealing the user's keyboard focus.
    pub(crate) fn raise_for_control(&mut self, ctx: &egui::Context) {
        if let Some(raise) = self.hooks.raise_without_focus.as_mut() {
            raise();
        }
        ctx.request_repaint();
    }

    /// Drain queued control requests without blocking: UI requests wait for a layout pass with a 3 s deadline (except screenshots), non-UI requests run now,
    /// and hidden windows are raised and repainted.
    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        let now = self.now();
        let mut reqs: Vec<(ControlRequest, f64)> = std::mem::take(&mut self.deferred);
        while let Ok(req) = rx.try_recv() {
            // UI requests need rendered frames: raise the window if `ui` hasn't run recently
            // (occluded macOS windows stop running `ui`).
            if req.method.starts_with("ui.") && now - self.last_ui_time > 0.25 {
                self.raise_for_control(ctx);
                // A hidden native window still needs a real UI pass for widget state/geometry.
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            }
            reqs.push((req, now + 3.0));
        }
        for (req, deadline) in reqs {
            if req.method.starts_with("ui.") && req.method != "ui.screenshot" {
                self.ui_requests.push((req, deadline));
                ctx.request_repaint();
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                continue;
            }
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Retry(msg) => {
                    if now < deadline {
                        self.deferred.push((req, deadline));
                        ctx.request_repaint();
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::Screenshot { path, crop } => {
                    capture::admit(self, ctx, path, crop, reply);
                }
            }
        }
        self.control_rx = Some(rx);
        self.ui_requests.retain(|(req, deadline)| {
            if now > *deadline {
                let _ = req.reply.send(json!({"ok": false, "error": "no UI pass ran before the control request deadline"}));
                false
            } else {
                true
            }
        });
    }

    /// Process control requests that need the current layout/response. Called at the end of a
    /// final, non-discarded UI pass; `ui.click`/`ui.drag` queue synthetic input for the next pass.
    fn drain_ui_requests(&mut self, ctx: &egui::Context) {
        let reqs = std::mem::take(&mut self.ui_requests);
        let now = self.now();
        let mut pending = reqs.into_iter();
        while let Some((req, deadline)) = pending.next() {
            // A panel/dock/resize request changes the next layout. Anything after it in this
            // batch (ui.click, ui.elements, ui.screenshot, …) must see the new geometry, so it
            // waits for the next accepted pass rather than reading the layout drawn before it.
            let changes_layout = matches!(req.method.as_str(), "ui.set" | "ui.panel.show" | "ui.panel.close" | "ui.resize");
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Retry(msg) => {
                    if now < deadline {
                        self.ui_requests.push((req, deadline));
                        ctx.request_repaint();
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::Screenshot { path, crop } => capture::admit(self, ctx, path, crop, reply),
            }
            if changes_layout {
                let rest: Vec<_> = pending.collect();
                if !rest.is_empty() {
                    self.ui_requests.extend(rest);
                    ctx.request_repaint();
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                }
                break;
            }
        }
    }

    // ---------------------------------------------------------------- frame

    /// Settings ▸ Media ▸ Automatically refresh growing files: every *Refresh growing files every*
    /// seconds, and as soon as the window comes back to the front (after a render in another app),
    /// stamp the project's media files on a worker thread and read the changed ones again
    /// ([`filmcraft_engine::media_watch`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_media_refresh(&mut self, ctx: &egui::Context, now: f64) {
        if let Some(r) = self.session.poll_media_scan() {
            match r {
                Ok(r) => {
                    if let Some(n) = r["refreshed"].as_array().map(Vec::len).filter(|n| *n > 0) {
                        self.toast = Some((format!("Refreshed {n} media item(s) changed on disk"), now));
                    }
                }
                Err(e) => self.toast = Some((e.to_string(), now)),
            }
        }
        let media = &self.session.prefs.media;
        let (on, every) = (media.refresh_growing_files, f64::from(media.growing_refresh_seconds.max(1)));
        let (last_key, focus_key) = (egui::Id::unique("mediaWatch.lastScan"), egui::Id::unique("mediaWatch.focused"));
        let focused = self.native_window_focused.unwrap_or(false);
        let was_focused: bool = ctx.data(|d| d.get_temp(focus_key)).unwrap_or(focused);
        ctx.data_mut(|d| d.insert_temp(focus_key, focused));
        if !on || filmcraft_engine::media_watch::media_files(&self.session.project, &self.session.media).is_empty() {
            return;
        }
        let last: f64 = ctx.data(|d| d.get_temp(last_key)).unwrap_or(f64::NEG_INFINITY);
        if now - last >= every || (focused && !was_focused) {
            ctx.data_mut(|d| d.insert_temp(last_key, now));
            self.session.start_media_scan();
        }
        let wait = if self.session.media_scan.is_some() { 0.1 } else { (every - (now - last)).clamp(0.1, every) };
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
    }

    /// Top-level UI pass: set the language, honor the injected panic test hook, lay out header, body and status bar, and dispatch the active mode.
    fn frame(&mut self, ui: &mut egui::Ui) {
        i18n::set_current(self.ui.language);
        if std::mem::take(&mut self.panic_next_frame) {
            crash::injected_fault("injected UI fault");
        }
        let ctx = ui.ctx().clone();
        // Text to Speech panel selection and its repaint cadence are presentation state; they do
        // not poll a background queue.
        panels::tts::follow_selection(self);
        if self.ui.tts.pending.is_some() || self.ui.tts.download_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let t = self.tokens;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, t.app_bg);
        let header_h = if self.ui.show_header { 38.0 } else { 0.0 };
        let header = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), header_h));
        if self.ui.show_header {
            header::show(self, ui, header);
        }
        let status_h = if self.ui.show_status_bar { 20.0 } else { 0.0 };
        let body = egui::Rect::from_min_max(egui::pos2(full.min.x + 1.0, header.max.y + 1.0), egui::pos2(full.max.x - 1.0, full.max.y - status_h - 2.0));
        match self.ui.mode {
            state::Mode::Edit => self.dock_area(ui, body),
            state::Mode::Import => panels::import_mode::show(self, ui, body),
            state::Mode::Export => panels::export_mode::show(self, ui, body),
        }
        // the Timeline has had its chance to place a file drop this frame; anything left stays a bin import
        self.pending_timeline_drop = None;
        panels::dialogs::show(self, &ctx);
        updates_ui::show(self, &ctx);
        if !self.ui.show_status_bar {
            return;
        }
        // Status / hint bar
        let sb = egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - status_h), full.max);
        ui.painter().rect_filled(sb, 0.0, t.header_bg);
        let now = ui.input(|i| i.time);
        if self.ui.status != self.status_seen.0 {
            self.status_seen = (self.ui.status.clone(), now);
        } else if !self.ui.status.is_empty() {
            if now - self.status_seen.1 > 8.0 {
                self.ui.status.clear();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
        }
        let hint = if !self.ui.status.is_empty() { self.ui.status.clone() } else { self.hint_text() };
        ui.painter().text(egui::pos2(sb.min.x + 10.0, sb.center().y), egui::Align2::LEFT_CENTER, hint, Tokens::ui(11.0), t.text_dim);
        let resp = ui.interact(sb, egui::Id::unique("status-bar"), egui::Sense::click());
        if resp.clicked() {
            self.ui.status.clear();
        }
        self.job_status(ui, sb, &t);
    }

    /// The running job (export / render previews), if any; plays the rendered range when a preview
    /// render completes. Runs every frame, with or without the status bar.
    fn poll_jobs(&mut self, ctx: &egui::Context) -> Option<filmcraft_engine::Job> {
        use std::sync::atomic::Ordering;
        let running = self.session.jobs.iter().rev().find(|j| !j.progress.finished.load(Ordering::Relaxed)).cloned();
        // Play after rendering previews.
        if let Some((id, from)) = self.watched_render
            && let Some(j) = self.session.jobs.iter().find(|j| j.id == id)
            && j.progress.finished.load(Ordering::Relaxed)
        {
            self.watched_render = None;
            let ok = j.progress.error.lock().map(|e| e.is_none()).unwrap_or(false);
            if ok && self.ui.play_after_render && !self.playback.playing {
                self.session.set_playhead(from);
                self.play(1.0);
            }
        }
        let job = running?;
        if job.label.starts_with("Rendering ") && !job.label.contains("audio") && self.watched_render.is_none_or(|w| w.0 != job.id) {
            let from = self.session.active_sequence().and_then(|q| q.mark_in).unwrap_or(Tick::ZERO);
            self.watched_render = Some((job.id, from));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        Some(job)
    }

    /// The running job for drawing only; polling/play-after-render stays in logic.
    fn running_job(&self) -> Option<filmcraft_engine::Job> {
        use std::sync::atomic::Ordering;
        self.session.jobs.iter().rev().find(|j| !j.progress.finished.load(Ordering::Relaxed)).cloned()
    }

    /// Right side of the status bar: the running job (export / render previews) with a progress
    /// bar and a cancel button.
    fn job_status(&mut self, ui: &mut egui::Ui, sb: egui::Rect, t: &Tokens) {
        use std::sync::atomic::Ordering;
        let Some(job) = self.running_job() else { return };
        let f = job.progress.fraction().clamp(0.0, 1.0);
        let left = job.progress.eta().map(panels::left_text).unwrap_or_default();
        let cancel = egui::Rect::from_center_size(egui::pos2(sb.max.x - 14.0, sb.center().y), egui::vec2(14.0, 14.0));
        let bar = egui::Rect::from_min_size(egui::pos2(cancel.min.x - 128.0, sb.center().y - 3.0), egui::vec2(120.0, 6.0));
        let p = ui.painter();
        p.rect_filled(bar, 3.0, t.separator);
        p.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * f, bar.height())), 3.0, t.accent);
        let verb = if job.label.starts_with("Rendering") { crate::i18n::t(&job.label).to_string() } else { tl!("Exporting").to_string() };
        p.text(
            egui::pos2(bar.min.x - 8.0, sb.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{verb}… {:.0}%{left}", f * 100.0),
            Tokens::ui(11.0),
            t.text_dim,
        );
        let resp = ui.interact(cancel, egui::Id::unique(("job-cancel", job.id)), egui::Sense::click());
        let c = if resp.hovered() { t.hot_text } else { t.text_dim };
        let k = 3.5;
        p.line_segment([cancel.center() - egui::vec2(k, k), cancel.center() + egui::vec2(k, k)], egui::Stroke::new(1.4, c));
        p.line_segment([cancel.center() + egui::vec2(-k, k), cancel.center() + egui::vec2(k, -k)], egui::Stroke::new(1.4, c));
        self.auto.add("status.job.cancel", cancel, &format!("Cancel {}", job.label));
        self.auto.add("status.job.progress", bar, &format!("{:.0}%{left}", f * 100.0));
        if resp.on_hover_text(tl!("Cancel")).clicked() {
            job.progress.cancel.store(true, Ordering::Relaxed);
            self.watched_render = None;
        }
    }

    /// Contextual hint for the status bar (Premiere shows tool/gesture hints here).
    fn hint_text(&self) -> String {
        let mac = cfg!(target_os = "macos");
        match self.ui.tool {
            state::Tool::Selection if mac => {
                tl!("Click to select, or click in empty space and drag to marquee select. Use Shift, Opt, and Cmd for other options.")
            }
            state::Tool::Selection => tl!("Click to select, or click in empty space and drag to marquee select. Use Shift, Alt, and Ctrl for other options."),
            state::Tool::TrackSelectForward => tl!("Click to select all clips to the right in all tracks. Shift-click for a single track."),
            state::Tool::TrackSelectBackward => tl!("Click to select all clips to the left in all tracks. Shift-click for a single track."),
            state::Tool::Ripple => tl!("Drag an edit point to ripple trim; later clips move to keep the gap closed."),
            state::Tool::Rolling => tl!("Drag an edit point to roll it: the out of one clip and the in of the next move together."),
            state::Tool::RateStretch => tl!("Drag an edge to change the clip's speed so it fills the new duration."),
            state::Tool::Remix => tl!("Drag the edge of a music clip to remix it to the new duration at musically matching beats."),
            state::Tool::Razor => tl!("Click to split a clip. Shift-click to split all tracks."),
            state::Tool::Slip => tl!("Drag a clip to slip its source in/out without moving it."),
            state::Tool::Slide => tl!("Drag a clip to slide it between its neighbours."),
            state::Tool::Hand => tl!("Drag to scroll the timeline."),
            state::Tool::Zoom if mac => tl!("Click to zoom in; Opt-click to zoom out."),
            state::Tool::Zoom => tl!("Click to zoom in; Alt-click to zoom out."),
            _ => "",
        }
        .to_string()
    }

    /// Remember the undo history as a mouse press begins, before any panel acts on it (a color
    /// picker sets its color on the press itself), for [`Self::cancel_value_drag`].
    fn begin_press(&mut self, ctx: &egui::Context) {
        match ctx.input(|i| i.pointer.press_start_time().filter(|_| i.pointer.any_down())) {
            None => self.value_press = None,
            Some(p) if self.value_press.is_some_and(|(q, _)| q == p) => {}
            Some(p) => self.value_press = Some((p, self.session.undo_mark())),
        }
    }

    /// Latch Escape during pass 0, where the key event is fresh.
    fn latch_escape_cancel(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.any_down() && i.key_pressed(egui::Key::Escape)) {
            self.escape_cancel = true;
        }
    }

    /// Escape while a mouse button is held takes back every edit this press made (#580): value
    /// drags (one merged step or one step per change), color picks, anything committed on the
    /// Escape frame. Called once after the final non-discarded UI pass; the latch outlives a
    /// discarded pass, and pass 1+ has no Escape key event to re-read.
    fn cancel_value_drag(&mut self, _ctx: &egui::Context) {
        if !self.escape_cancel {
            return;
        }
        self.escape_cancel = false;
        if let Some((press, mark)) = self.value_press
            && self.session.revert_to(mark) > 0
        {
            self.value_press = Some((press, self.session.undo_mark()));
        }
    }
}

impl FilmcraftApp {
    /// The "FilmCraft hit an error" window after a caught UI panic. Automation ids:
    /// `error.dismiss`, `error.save`.
    fn error_window(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.ui_error.clone() else { return };
        let mut close = false;
        egui::Window::new(tl!("FilmCraft hit an error"))
            .id(egui::Id::unique("FilmCraft hit an error"))
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(crate::widgets::dialog_center(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(tl!("Something went wrong while drawing the window. Your project is still open; save it to be safe."));
                ui.add_space(6.0);
                ui.label(egui::RichText::new(&msg).monospace().small());
                if let Some(p) = crash::log_path() {
                    ui.label(egui::RichText::new(tlf!("Details: {path}", path = p.display())).small());
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let s = ui.button(tl!("Save Project"));
                    self.auto.add("error.save", s.rect, "Save Project");
                    if s.clicked() {
                        let _ = crate::menus::invoke(self, ctx, "file.save", serde_json::json!({}));
                    }
                    let d = ui.button(tl!("Continue"));
                    self.auto.add("error.dismiss", d.rect, "Continue");
                    close |= d.clicked();
                });
            });
        if close {
            self.ui_error = None;
        }
    }
}

/// On macOS a Control+Click means "right click": AppKit delivers it to the view as a *primary*
/// press carrying the Control modifier (`rightMouseDown:` only fires for a real right button or
/// two-finger click), so the app translates it to a secondary click before egui computes pointer
/// state. `armed` pairs the press with its release, so the release is still a context click when
/// Control is let go mid-click. Called from [`eframe::App::raw_input_hook`], which runs before
/// egui's pass; only wired in on macOS, where Ctrl+Click is not a selection modifier.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn synthesize_mac_ctrl_click(armed: &mut bool, raw_input: &mut egui::RawInput) {
    for event in &mut raw_input.events {
        match event {
            egui::Event::PointerButton { button, pressed, modifiers, .. } if *button == egui::PointerButton::Primary => {
                if *pressed {
                    if modifiers.ctrl {
                        *button = egui::PointerButton::Secondary;
                        *armed = true;
                    } else {
                        // A plain primary press ends any pairing left open (eg. the pointer left
                        // the window mid-click, which egui also forgets).
                        *armed = false;
                    }
                } else if *armed {
                    *button = egui::PointerButton::Secondary;
                    *armed = false;
                }
            }
            egui::Event::PointerGone => *armed = false,
            _ => {}
        }
    }
}

/// Install once in each native/web/harness Context creator, before the first UI pass.
pub fn install_ime_reset_callbacks(ctx: &egui::Context) {
    ime_reset::install(ctx);
}

impl FilmcraftApp {
    /// Install the theme once, then keep fonts ready; a saved language without its system face
    /// falls back to English as before.
    fn logic_theme(&mut self, ctx: &egui::Context) {
        if !self.styled {
            theme::install(ctx, &self.tokens);
            // theme::install replaces the fonts: add the system Japanese font back (or fall back to
            // English when a saved Japanese setting meets a system without one)
            // (likewise Chinese without a Chinese face, Korean without a Hangul one)
            let japanese_missing = self.ui.language == i18n::Language::Ja && !i18n::install_japanese_font(ctx);
            if japanese_missing
                || (self.ui.language == i18n::Language::ZhCn && !i18n::chinese_font_available())
                || (self.ui.language == i18n::Language::Ko && !i18n::korean_font_available())
            {
                self.ui.language = i18n::Language::En;
            }
            self.styled = true;
            ctx.request_repaint();
        } else {
            self.fonts_ready = true;
        }
    }

    /// Advance the monotonic host clock and the smoothed frame rate, returning the pass time.
    /// Hidden playback is marked so a hidden window's skipped frames are not counted as drops.
    fn logic_clock(&mut self, ctx: &egui::Context) -> f64 {
        // Monotonic host time. Native and web both call logic once per layout pass and through
        // `run_logic` while hidden; `ctx.input(i.time)` is stale in that second case.
        let now = self.now();
        let dt = (now - self.last_time) as f32;
        if dt > 0.0 && dt.is_finite() {
            self.fps = self.fps * 0.9 + (1.0 / dt).min(480.0) * 0.1;
        }
        self.last_time = now;
        if self.playback.playing && ctx.input(|i| i.viewport().visible()) == Some(false) {
            // Nothing is shown while the window is hidden: not a dropped frame.
            self.playback.hidden = true;
        }
        now
    }

    /// Close-request handling and control-driven synthetic input, in the original order.
    fn logic_control(&mut self, ctx: &egui::Context) {
        // Quitting with unsaved changes asks to save them first.
        if ctx.input(|i| i.viewport().close_requested()) && panels::clip_dialogs::intercept_quit(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let had_synthetic = !self.synthetic.is_empty() || self.synthetic_pending_step.is_some();
        self.drain_control(ctx);
        if (!self.synthetic.is_empty() || self.synthetic_pending_step.is_some()) && !had_synthetic {
            // Occluded macOS windows stop running `ui`; bring the window forward (without taking
            // keyboard focus) so the input is processed.
            self.raise_for_control(ctx);
        }
        if !self.synthetic.is_empty() || self.synthetic_pending_step.is_some() {
            ctx.request_repaint();
            // The next step is consumed by a UI pass, not by another logic tick. Native eframe
            // honours this; the pinned web runner logs it as unhandled (a recorded fork gap).
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
    }

    /// Background queues and idempotent deadlines that are safe on every logic call.
    fn logic_background(&mut self, ctx: &egui::Context, now: f64) {
        self.frames.set_context(ctx);
        self.frames.pump(std::time::Duration::from_millis(if self.playback.playing { 24 } else { 40 }));
        self.session.poll_persistence();
        panels::trim_monitor::advance_at(self, now);
        if self.session.trim_play.active() {
            ctx.request_repaint();
        }
        if self.session.persistence.is_some() && self.session.is_dirty() {
            // Keep polling the auto-save worker (status, "also save the project" results) while hidden.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        self.sync_pool();
        updates_ui::tick(self, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_media_refresh(ctx, now);
    }

    /// Apply engine events that change the active sequence/view, IME target, toast or project.
    fn logic_session_events(&mut self, now: f64) {
        for ev in self.session.drain_events() {
            match ev {
                filmcraft_engine::Event::OpenSequence(_) => {
                    // A sequence switch (including opening/reverting/recovering a project, which
                    // emits this event for the newly active sequence) is a new IME owner context.
                    self.restart_ime_target();
                    // show the sequence with its own view (or fitted, the first time)
                    self.timeline_view_of = None;
                    if !self.ui.floating.iter().any(|group| group.panels.contains(&PanelKind::Timeline)) {
                        self.ui.dock.restore_timeline();
                    }
                    let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: PanelKind::Timeline });
                }
                filmcraft_engine::Event::OpenSource(_) => {
                    let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: PanelKind::Source });
                }
                filmcraft_engine::Event::RevealInProject(item) => self.reveal_in_project(item),
                filmcraft_engine::Event::Toast { message, .. } => self.toast = Some((message, now)),
                filmcraft_engine::Event::ProjectChanged { .. } => {}
            }
        }
    }

    /// Drain native menu/control command ids, preserving the control-only Quit exception.
    fn logic_command_inbox(&mut self, ctx: &egui::Context) {
        if let Some(rx) = self.command_inbox.take() {
            while let Ok(id) = rx.try_recv() {
                // control-only mode ignores native menu commands, but Quit stays available (#1943)
                if self.control_input_only && id != "app.quit" {
                    continue;
                }
                if let Err(e) = menus::invoke(self, ctx, &id, json!({})) {
                    self.ui.status = e;
                }
            }
            self.command_inbox = Some(rx);
        }
    }

    /// Run one logic pass in order: retain the later UI clock, poll the native dialog, apply the
    /// logic subsystems, advance playback, poll jobs, then drain completed screenshots and ask
    /// for hidden paints. Safe on hidden logic-only callbacks.
    pub fn logic_inner(&mut self, ctx: &egui::Context) {
        // Remember the pass's clock before anything reads `now()`. A hidden logic-only tick keeps
        // the last UI time (which is stale there), so `now()` never runs behind the host clock.
        self.ui_clock = self.ui_clock.max(ctx.input(|i| i.time));
        // a native file dialog's answer first, so the panels draw what it chose (#260)
        self.poll_native_dialog(ctx);
        self.logic_theme(ctx);
        let now = self.logic_clock(ctx);
        self.logic_control(ctx);
        self.logic_background(ctx, now);
        self.logic_session_events(now);
        self.logic_command_inbox(ctx);
        self.advance_playback(ctx);
        self.advance_source_playback(ctx);
        self.poll_jobs(ctx);
        capture::drain(self, ctx);
        capture::request_hidden_paint(self, ctx);
    }

    /// Draw one real UI pass. Establish the UI clock offset once, run first-pass input handling,
    /// draw panels under a panic guard, drain UI requests and observe the IME editor only after
    /// this pass's commands ran. Screenshots are offered to `capture::issue`, which issues only
    /// on an accepted pass.
    fn ui_inner(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.ui_clock_offset.is_none() {
            let egui_time = ui.ctx().input(|input| input.time);
            if egui_time.is_finite() {
                let host = self.clock_start.elapsed().as_secs_f64();
                // Never move host time backwards when a harness clock starts behind it; the host
                // clock stays authoritative in that case.
                self.ui_clock_offset = Some((egui_time - host).max(0.0));
            }
        }
        if !self.fonts_ready {
            // This pass did not consume the pending synthetic input; the next hook must offer it
            // again (the UI path does not preserve raw input across this early return).
            self.synthetic_step_offered = false;
            self.synthetic_modifiers_offered = false;
            self.finish_ime_pass(ui.ctx());
            ui.ctx().request_repaint();
            return;
        }
        frame.set_ui_language(Some(self.ui.language.bcp47()));
        let ctx = ui.ctx().clone();
        // A project/sequence replacement may have happened in the previous pass's control drain.
        // Drop the old edit and IME composition before shortcuts or the frame see them.
        self.sync_ime_target_context(&ctx);
        // Each pass draws at most one editor; a stale entry from a previous pass must not be
        // observed as this pass's editor.
        self.ime_drawn_editor = None;
        // Only an actual pass counts toward screenshot settlement; hidden logic ticks do not draw.
        self.timeline_still = if self.ui.timeline.animating() { 0 } else { self.timeline_still.saturating_add(1) };
        // This is a real UI pass: the one synthetic step offered to raw_input_hook has been fed to
        // egui now. A hidden logic tick never reaches this point and keeps it pending.
        self.synthetic_pending_step = None;
        self.synthetic_step_offered = false;
        self.synthetic_modifiers_offered = false;
        let first_pass = ctx.current_pass_index() == 0;
        if first_pass {
            // Setup and fresh-input handling happen where `RawInput::events` are available. Pass 1+
            // has no new events and must not re-run a click, shortcut or drop.
            self.auto.begin_frame();
            self.apply_prefs(&ctx);
            self.sync_timeline_view();
            panels::metadata::follow_selection(self);
            self.handle_drops(&ctx);
            self.begin_press(&ctx);
            self.latch_escape_cancel(&ctx);
            self.handle_shortcuts(&ctx);
            self.sync_native_menu();
            self.scrub_audio(&ctx);
        } else {
            // Pass 1+ redraws the same frame; drop the discarded pass's registrations so lookups
            // and `ui.elements` see one entry per widget, from the layout that is accepted.
            self.auto.elements.clear();
        }
        // A panic in one panel must not close the app (losing unsaved work) or leave a blank
        // window: catch it, keep the session, and say what happened.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.frame(ui))).is_err() {
            self.ui_error = Some(crash::take_last().unwrap_or_else(|| "unknown error".into()));
            self.playback.playing = false;
        }
        if !ctx.will_discard() {
            self.cancel_value_drag(&ctx);
        }
        self.error_window(ui.ctx());
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| graphics_notice::show(self, ui.ctx()))).is_err() {
            // a broken notice must not take the session down: drop it
            self.graphics_notice = None;
        }
        self.last_ui_time = self.now();
        if !self.synthetic.is_empty() || self.synthetic_pending_step.is_some() {
            ctx.request_repaint();
        }
        if self.frames.queue_len() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if !ctx.will_discard() {
            self.drain_ui_requests(&ctx);
        }
        // Observe only after this pass's control requests ran: `ui.menu.invoke`/`engine.execute`
        // may have edited the text or replaced the target. Every other pass (hidden editor, focus
        // in another panel, no editor at all) reports an explicit `None`.
        self.finish_ime_pass(&ctx);
        // Ack `ui.*` waiters once the action had a final, non-discarded UI pass to apply in. Holding
        // them through a discard avoids reporting success for a pass egui threw away.
        if !ctx.will_discard() && self.synthetic.is_empty() && self.synthetic_pending_step.is_none() && !self.input_waiters.is_empty() {
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        // Only a real, accepted UI pass may issue a capture: it observes the final layout for this
        // pass and `screenshot` itself forces the paint needed when the window is hidden.
        capture::issue(self, &ctx);
    }
}

impl eframe::App for FilmcraftApp {
    /// Snapshot this callback's fresh surface geometry, optionally synthesize macOS ctrl-click
    /// and agent input, and offer at most one queued synthetic step. A step already preserved by
    /// hidden logic callbacks is not appended twice; a real UI pass clears it.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.native_window_focused = Some(raw_input.focused);
        capture::note_raw_geometry(self, ctx, raw_input);
        #[cfg(target_os = "macos")]
        if !self.control_input_only {
            synthesize_mac_ctrl_click(&mut self.mac_ctrl_click_armed, raw_input);
        }
        if self.control_input_only {
            // Screenshot results arrive through `ScreenshotCallback`, not raw input. Preserve all
            // viewport lifecycle/geometry metadata and drop native input only.
            raw_input.events.clear();
            raw_input.hovered_files.clear();
            raw_input.dropped_files.clear();
        }
        if self.control_input_only || !self.synthetic.is_empty() || self.synthetic_pending_step.is_some() {
            // Native modifiers cannot replace those requested by a control gesture. A queued
            // reset runs in the frame after release, so clicked() still sees Shift/Alt.
            if !self.synthetic_modifiers_offered {
                raw_input.events.push(egui::Event::ModifiersChanged(self.synthetic_modifiers));
                self.synthetic_modifiers_offered = true;
            }
            // Widget keyboard handling requires effective egui focus. Control input must work
            // in a background window without activating it or taking the user's OS keyboard.
            raw_input.focused = true;
        }
        if self.synthetic_pending_step.is_none() && !self.synthetic.is_empty() {
            // Pointer events go one per frame so egui sees press → moves → release as a real drag
            // (all in one frame reads as a click); key/text runs go together up to a key release.
            let pointer = |e: &egui::Event| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. });
            let n = if pointer(&self.synthetic[0]) {
                1
            } else {
                self.synthetic
                    .iter()
                    .position(|e| pointer(e) || matches!(e, egui::Event::Key { pressed: false, .. }))
                    .map_or(self.synthetic.len(), |i| if pointer(&self.synthetic[i]) { i.max(1) } else { i + 1 })
            };
            let step: Vec<egui::Event> = self.synthetic.drain(..n).collect();
            for event in &step {
                if let egui::Event::ModifiersChanged(modifiers) = event {
                    self.synthetic_modifiers = *modifiers;
                }
            }
            self.synthetic_pending_step = Some(step);
            self.synthetic_step_offered = false;
        }
        if let Some(step) = &self.synthetic_pending_step {
            // `raw_input_hook` also runs before hidden `run_logic` ticks, whose raw input egui does
            // not feed to widgets. Keep the exact same one-step batch pending and offer it again
            // on the next callback; a real UI pass clears it at `ui()` start. Never coalesce a
            // press with its release and, crucially, never append a step that the integration
            // already preserved in its pending raw input: the UI pass would then see two copies.
            if !self.synthetic_step_offered {
                raw_input.events.extend(step.iter().cloned());
                self.synthetic_step_offered = true;
            }
        }
    }

    /// Eframe's hidden logic callback: run the logic pass, sync the IME target context, then
    /// suspend reset state because a logic-only callback is never a UI pass.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.logic_inner(ctx);
        self.finish_ime_reset_logic(ctx);
    }

    /// Cancel outstanding screenshots with one error reply each, end per-viewport IME tracker and
    /// reset state, and shut the session down so its recovery journal is flushed or removed.
    fn on_exit(&mut self) {
        // Late screenshot callbacks now find a dropped slot and write nothing; clients get one error.
        capture::cancel_all(self);
        for viewport in self.ime_viewports.drain(..) {
            self.ime_context.end_viewport(viewport);
            self.ime_reset.end_viewport(viewport);
        }
        self.ime_observed_pass = None;
        // Flush the recovery journal and stop the auto-save worker; a session with nothing unsaved
        // removes its journal, one with unsaved changes keeps it for the next launch.
        self.session.shutdown();
    }

    /// Eframe's UI entry: begin this pass's IME reset state, suspend custom controls when no
    /// eligible editor owns input, draw `ui_inner`, then run end-of-pass IME reconciliation.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ime_reset.begin_pass(&ctx);
        self.ime_drawn_editor = None;
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(input.focused));
        let another_field = ctx.memory(|memory| memory.focused().is_some_and(|id| id != egui::Id::unique(panels::graphics::TEXT_EDIT_ID)));
        if !self.fonts_ready || self.ui.gfx_edit.is_none() || self.ime_modal_blocks_input() || !focused || another_field {
            self.suspended_ime_controls(&ctx);
        }
        self.ui_inner(ui, frame);
        self.finish_ime_reset_ui(&ctx);
    }
}

/// Effective sample rate for shuttle playback. The audio device still runs at `device_rate`,
/// so at 2x its output samples represent twice the timeline duration. Reverse and
/// extreme shuttle speeds remain silent rather than asking the mixer for invalid rates.
fn shuttle_mix_rate(device_rate: u32, speed: f64) -> Option<u32> {
    if device_rate == 0 || !speed.is_finite() || !(0.5..=4.0).contains(&speed) {
        return None;
    }
    let rate = (f64::from(device_rate) / speed).round();
    if !(1.0..=f64::from(u32::MAX)).contains(&rate) {
        return None;
    }
    Some(rate as u32)
}

/// The program mix that playback plays: `mix(device_frame, interleaved, channels)` renders the
/// active sequence at device rate `sr` from `device_frame`, with the live project snapshot (edits
/// made while playing are heard), Output Mapping, the 5.1 mixdown and voice-over cues.
pub fn playback_mix(session: &Session, seq_id: filmcraft_project::ItemId, sr: u32) -> impl FnMut(i64, &mut [f32], usize) + Send + 'static {
    let project = session.project.clone();
    let provider = session.media.provider(project.clone(), session.services.clone());
    let previews = session.previews.clone();
    // Settings ▸ Audio Hardware ▸ Output Mapping
    let map = [session.prefs.audio_hardware.map_left, session.prefs.audio_hardware.map_right];
    // Preferences ▸ Audio ▸ 5.1 Mixdown Type: how a 5.1 Mix plays on a stereo device
    let mixdown = filmcraft_audio_dsp::channels::Mixdown::from_id(&session.prefs.audio.mixdown_type).unwrap_or_default();
    let cues = panels::voiceover::cues(session, sr);
    previews.live.publish_project(project.clone());
    let mut resampler = None;
    move |cursor: i64, buf: &mut [f32], ch: usize| {
        // the newest project snapshot: mixer moves and other edits are heard while playing
        let project = previews.live.project().filter(|p| p.sequence(seq_id).is_some()).unwrap_or_else(|| project.clone());
        let Some(seq) = project.sequence(seq_id) else {
            buf.fill(0.0);
            return;
        };
        let n = buf.len() / ch.max(1);
        let seq_sr = seq.settings.sample_rate;
        // a 5.1 Mix plays as six channels (L, R, C, LFE, Ls, Rs) on a device with at least six
        use filmcraft_audio_dsp::channels::Layout;
        let layout = if ch >= 6 && seq.settings.audio_master == filmcraft_project::AudioChannels::Surround51 { Layout::Surround51 } else { Layout::Stereo };
        let mix = if seq_sr == sr {
            previews.mix_layout(&project, seq_id, cursor, n, &provider, layout, mixdown)
        } else {
            // Mix at the sequence rate as one continuous stream, interpolated to the device rate
            // (started afresh when the rate or the channel layout changes while playing).
            let key = (seq_sr, layout);
            if resampler.as_ref().is_none_or(|(k, _)| *k != key) {
                resampler = Some((key, filmcraft_audio_dsp::resample::StreamResampler::new(seq_sr, sr)));
            }
            let Some((_, r)) = resampler.as_mut() else {
                buf.fill(0.0);
                return;
            };
            let channels = r.process(cursor, n, |s0, m| previews.mix_layout(&project, seq_id, s0, m, &provider, layout, mixdown).channels);
            filmcraft_frame::AudioBuffer { sample_rate: sr, channels }
        };
        if layout == Layout::Surround51 {
            buf.fill(0.0);
            for (i, frame) in buf.chunks_mut(ch).enumerate() {
                for (c, x) in frame.iter_mut().take(6).enumerate() {
                    *x = mix.channels[c][i];
                }
            }
        } else {
            filmcraft_engine::settings::map_output(&mix.channels[0], &mix.channels[1.min(mix.channels.len() - 1)], buf, ch, map);
        }
        panels::voiceover::mix_cues(buf, ch, cursor, &cues);
    }
}

/// The windowing layer (egui-winit, and egui's web backend) reports the clipboard shortcuts as
/// `Event::Copy` / `Event::Cut` / `Event::Paste` instead of key presses: Ctrl+C/X/V on Windows and
/// Linux, plus Ctrl+Insert, Shift+Insert and Shift+Delete on Windows. The shortcut bindings only
/// match key presses, so Copy, Cut, Paste, Paste Insert, Paste Attributes and (on Windows) Ripple
/// Delete never fired from the keyboard (#199). macOS was unaffected because its native menu bar
/// takes the key equivalents first.
///
/// Add the key press each event stands for, with the modifiers held, so the bindings see it. The
/// original event stays for anything else that reads it. Only called when no text field has
/// keyboard focus; text fields keep handling the clipboard themselves.
fn clipboard_events_as_keys(events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) {
    use egui::{Event, Key};
    let keys: Vec<Key> = events
        .iter()
        .filter_map(|e| match e {
            Event::Copy if modifiers.command => Some(Key::C),
            Event::Copy => Some(Key::Copy),
            Event::Cut if modifiers.command => Some(Key::X),
            Event::Cut if modifiers.shift => Some(Key::Delete),
            Event::Cut => Some(Key::Cut),
            Event::Paste(_) | Event::PasteImage(_) if modifiers.command => Some(Key::V),
            Event::Paste(_) | Event::PasteImage(_) if modifiers.shift => Some(Key::Insert),
            Event::Paste(_) | Event::PasteImage(_) => Some(Key::Paste),
            _ => None,
        })
        .collect();
    events.extend(keys.into_iter().map(|key| Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }));
}

#[cfg(test)]
mod clipboard_key_tests {
    use egui::{Event, Key, Modifiers};

    fn keys(events: Vec<Event>, m: Modifiers) -> Vec<(Key, Modifiers)> {
        let mut events = events;
        super::clipboard_events_as_keys(&mut events, m);
        events.into_iter().filter_map(|e| if let Event::Key { key, modifiers, pressed: true, .. } = e { Some((key, modifiers)) } else { None }).collect()
    }

    /// Clipboard Copy/Cut/Paste events become C/X/V key presses so they match the shortcut bindings.
    #[test]
    fn ctrl_c_x_v_become_key_presses() {
        let c = Modifiers::COMMAND;
        assert_eq!(keys(vec![Event::Copy], c), vec![(Key::C, c)]);
        assert_eq!(keys(vec![Event::Cut], c), vec![(Key::X, c)]);
        assert_eq!(keys(vec![Event::Paste("x".into())], c), vec![(Key::V, c)]);
        let image = || Event::PasteImage(std::sync::Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::RED)));
        assert_eq!(keys(vec![image()], c), vec![(Key::V, c)]);
        assert_eq!(keys(vec![image()], Modifiers::SHIFT), vec![(Key::Insert, Modifiers::SHIFT)]);
        assert_eq!(keys(vec![image()], Modifiers::NONE), vec![(Key::Paste, Modifiers::NONE)]);
    }

    #[test]
    fn extra_modifiers_are_kept() {
        // Paste Insert (Ctrl+Shift+V) and Paste Attributes (Ctrl+Alt+V) also arrive as Paste
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(keys(vec![Event::Paste("x".into())], cs), vec![(Key::V, cs)]);
        let ca = Modifiers::COMMAND | Modifiers::ALT;
        assert_eq!(keys(vec![Event::Paste("x".into())], ca), vec![(Key::V, ca)]);
    }

    #[test]
    fn windows_insert_and_delete_variants() {
        // Shift+Delete (Ripple Delete) arrives as Cut on Windows; Shift+Insert as Paste
        assert_eq!(keys(vec![Event::Cut], Modifiers::SHIFT), vec![(Key::Delete, Modifiers::SHIFT)]);
        assert_eq!(keys(vec![Event::Paste("x".into())], Modifiers::SHIFT), vec![(Key::Insert, Modifiers::SHIFT)]);
    }

    #[test]
    fn other_events_are_left_alone() {
        let mut events = vec![Event::Text("a".into()), Event::Copy];
        super::clipboard_events_as_keys(&mut events, Modifiers::COMMAND);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], Event::Text(_)));
        assert!(matches!(events[1], Event::Copy));
    }
}

#[cfg(test)]
mod mac_ctrl_click_tests {
    use egui::{Event, Modifiers, PointerButton, RawInput};

    fn click(button: PointerButton, pressed: bool, ctrl: bool) -> Event {
        Event::PointerButton { pos: egui::pos2(40.0, 12.0), button, pressed, modifiers: if ctrl { Modifiers::CTRL } else { Modifiers::NONE } }
    }

    fn buttons(raw: &mut RawInput) -> Vec<(PointerButton, bool)> {
        raw.events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { button, pressed, .. } => Some((*button, *pressed)),
                _ => None,
            })
            .collect()
    }

    /// AppKit delivers Control+Click as a primary press with the Control modifier; it must reach
    /// egui as a secondary click so `secondary_clicked()` (the context menus) fires on macOS.
    #[test]
    fn ctrl_click_is_a_secondary_click() {
        let mut raw = RawInput { events: vec![click(PointerButton::Primary, true, true), click(PointerButton::Primary, false, true)], ..Default::default() };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        assert!(!armed);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Secondary, true), (PointerButton::Secondary, false)]);
    }

    /// End to end: the rewritten events are what egui's pointer state sees, so the context
    /// menus (`secondary_clicked()`) fire on a Control+Click exactly like on a real right click.
    #[test]
    fn synthesized_click_fires_secondary_clicked() {
        let ctx = egui::Context::default();
        let mut raw = RawInput { events: vec![click(PointerButton::Primary, true, true), click(PointerButton::Primary, false, true)], ..Default::default() };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        let mut secondary = false;
        // No renderer here: drop the pass output, clearing its texture deltas so epaint's drop
        // guard does not panic on the font texture.
        let mut out = ctx.run_ui(raw, |ui| secondary = ui.ctx().input(|i| i.pointer.secondary_clicked()));
        out.textures_delta.clear();
        assert!(secondary);
    }

    /// Releasing Control before the mouse button still closes the click as a context click; the
    /// mouse release alone must not become a stray primary click.
    #[test]
    fn release_is_paired_to_the_ctrl_press() {
        let mut raw = RawInput {
            events: vec![
                click(PointerButton::Primary, true, true),
                click(PointerButton::Primary, false, false), // Control already released
            ],
            ..Default::default()
        };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        assert!(!armed);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Secondary, true), (PointerButton::Secondary, false)]);
    }

    /// A plain click keeps its meaning (on macOS Cmd is the selection modifier, not Control).
    #[test]
    fn plain_click_is_untouched() {
        let mut raw = RawInput { events: vec![click(PointerButton::Primary, true, false), click(PointerButton::Primary, false, false)], ..Default::default() };
        super::synthesize_mac_ctrl_click(&mut false, &mut raw);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Primary, true), (PointerButton::Primary, false)]);
    }

    /// A half-open pairing (pointer left the window mid-click) must not turn the next plain click
    /// into a context click.
    #[test]
    fn new_plain_press_clears_a_stale_pairing() {
        let mut raw = RawInput {
            events: vec![
                click(PointerButton::Primary, true, true),
                Event::PointerGone,
                click(PointerButton::Primary, true, false),
                click(PointerButton::Primary, false, false),
            ],
            ..Default::default()
        };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        assert!(!armed);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Secondary, true), (PointerButton::Primary, true), (PointerButton::Primary, false),]);
    }
}

#[cfg(test)]
mod gpu_fallback_tests {
    use filmcraft_render::plan::{FramePlan, PlanLayer};

    /// Software rasterizers must not be treated as a GPU compositor: the preview crawled while
    /// "GPU" usage read 0 % because wgpu had silently fallen back to llvmpipe.
    #[test]
    fn software_rasterizers_are_detected() {
        use eframe::wgpu::DeviceType as D;
        let r = super::rasterizer_reason;
        assert!(r("llvmpipe (LLVM 23.1.1, 256 bits)", D::Cpu).is_some());
        assert!(r("NVIDIA GeForce RTX 4080", D::Cpu).is_some());
        assert!(r("SwiftShader Device", D::Other).is_some());
        assert!(r("softpipe", D::Cpu).is_some());
        assert!(r("AMD Radeon RX 5700 XT", D::DiscreteGpu).is_none());
        assert!(r("Apple M3 Pro", D::IntegratedGpu).is_none());
    }

    #[test]
    fn plan_side_covers_output_and_every_layer() {
        let frame = |w: u32, h: u32| std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba_f32(w, h, vec![0.0; (w * h * 4) as usize]));
        let layer = |w, h| PlanLayer {
            frame: frame(w, h),
            matrix: filmcraft_geom::Affine::IDENTITY,
            opacity: 1.0,
            blend: filmcraft_render::Blend::Normal,
            fx: None,
            input: None,
        };
        // a 4K source in an HD sequence still needs a 3840-wide texture on the GPU
        let p = FramePlan::Layers { width: 1920, height: 1080, layers: vec![layer(64, 64), layer(3840, 2160)] };
        assert_eq!(super::plan_side(&p), 3840);
        let p = FramePlan::Layers { width: 1920, height: 1080, layers: vec![] };
        assert_eq!(super::plan_side(&p), 1920);
        let p = FramePlan::Image(filmcraft_render::Image::new(800, 4000));
        assert_eq!(super::plan_side(&p), 4000);
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    /// The egui clock starts at the integration's beginning, which can precede `clock_start`.
    /// Applying the captured offset to host time keeps hidden logic moving instead of freezing at
    /// the last UI time and then jumping when the next UI pass arrives.
    #[test]
    fn now_applies_the_ui_clock_offset_and_keeps_the_test_override_first() {
        let mut app = FilmcraftApp::new(Session::default());
        app.ui_clock = 123.0;
        app.ui_clock_offset = Some(1_000.0);
        assert!(app.now() >= 1_000.0, "the captured offset moves the host clock to egui time");
        app.ui_clock = 2_000.0;
        assert_eq!(app.now(), 2_000.0, "an egui clock ahead of host+offset still wins");
        app.set_now_for_test(7.0);
        assert_eq!(app.now(), 7.0, "the deterministic test override keeps precedence");
    }
}

#[cfg(test)]
mod audio_recovery_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Device {
        ready: bool,
        starts: Arc<AtomicUsize>,
        reading: Option<u64>,
    }

    impl AudioOut for Device {
        fn sample_rate(&self) -> u32 {
            48000
        }
        fn channels(&self) -> usize {
            2
        }
        fn configure(&mut self, hardware: &filmcraft_engine::settings::AudioHardwarePrefs, _: Option<u32>) {
            self.ready = hardware.default_output == "available";
        }
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            self.starts.fetch_add(1, Ordering::Relaxed);
            if self.ready { Ok(48000) } else { Err("device unavailable".into()) }
        }
        fn stop(&mut self) {}
        fn played_frames(&self) -> Option<u64> {
            self.reading
        }
    }

    #[test]
    fn forward_shuttle_playback_keeps_audio_enabled() {
        assert_eq!(shuttle_mix_rate(48_000, 1.0), Some(48_000));
        assert_eq!(shuttle_mix_rate(48_000, 2.0), Some(24_000));
        assert_eq!(shuttle_mix_rate(48_000, 4.0), Some(12_000));
        assert_eq!(shuttle_mix_rate(48_000, -1.0), None);
        assert_eq!(shuttle_mix_rate(0, 2.0), None);
        assert_eq!(shuttle_mix_rate(48_000, f64::NAN), None);

        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        app.audio = Some(Box::new(Device { ready: true, starts: starts.clone(), reading: Some(0) }));
        app.play(2.0);
        app.end_preroll(0.0);
        assert_eq!(starts.load(Ordering::Relaxed), 1, "2x playback must start audio output");
        assert!(app.playback.audio_clock, "the device is still the master clock");
        app.stop();
    }

    /// Capture the block primed by the real shuttle playback path, without relying on a
    /// physical device or consuming the producer faster than a device would.
    #[test]
    fn maintain_pitch_shuttling_controls_the_samples_sent_to_the_device() {
        use std::sync::Mutex;
        struct Capture(Arc<Mutex<Vec<f32>>>);
        impl AudioOut for Capture {
            fn start(&mut self, mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
                let mut buf = vec![0.0; 2048];
                fill(&mut buf, 2);
                *self.0.lock().unwrap() = buf;
                Ok(48_000)
            }
            fn stop(&mut self) {}
            fn sample_rate(&self) -> u32 {
                48_000
            }
            fn played_frames(&self) -> Option<u64> {
                Some(4_800)
            }
        }
        let amplitude = |samples: &[f32], ch: usize, hz: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, frame) in samples.as_chunks::<2>().0.iter().enumerate() {
                let phase = std::f64::consts::TAU * hz * i as f64 / 48_000.0;
                re += f64::from(frame[ch]) * phase.cos();
                im += f64::from(frame[ch]) * phase.sin();
            }
            2.0 * re.hypot(im) / (samples.len() / 2) as f64
        };
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let inter: Vec<f32> =
            (0..48_000 * 8).flat_map(|i| [440.0, 660.0].map(|hz| (0.2 * (std::f64::consts::TAU * hz * i as f64 / 48_000.0).sin()) as f32)).collect();
        let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, 48_000).into();
        let item = filmcraft_engine::commands::import_bytes(&mut session, "/shuttle-tone.wav", bytes, None).unwrap();
        session.execute("timeline.place", json!({"item":item.0,"audioTrack":"A1","seconds":0.0})).unwrap();
        let mut app = FilmcraftApp::new(session);
        let samples = Arc::new(Mutex::new(Vec::new()));
        app.audio = Some(Box::new(Capture(samples.clone())));
        let mut normal = None;
        for maintain in [true, false] {
            app.session.prefs.audio.maintain_pitch_shuttling = maintain;
            for speed in [0.5, 1.0, 1.5, 2.0, 4.0] {
                app.session.set_playhead(Tick::from_seconds_f64(1.0));
                let expected = app.session.sequence_rate().snap(app.session.playhead() + Tick::from_seconds_f64(0.1 * speed));
                app.play(speed);
                app.end_preroll(0.0);
                assert!(app.playback.audio_clock);
                let buf = samples.lock().unwrap();
                for (ch, tone) in [440.0, 660.0].into_iter().enumerate() {
                    let hz = if maintain { tone } else { tone * speed };
                    let level = amplitude(&buf, ch, hz);
                    assert!(level > 0.08, "maintain={maintain}, speed={speed}, channel={ch}: {hz} Hz level={level}");
                    if speed != 1.0 {
                        let wrong = if maintain { tone * speed } else { tone };
                        assert!(amplitude(&buf, ch, wrong) < level * 0.3, "wrong pitch {wrong} Hz at {speed}x, maintain={maintain}");
                    }
                }
                if speed == 1.0 {
                    if let Some(previous) = &normal {
                        assert_eq!(&*buf, previous, "1x must bypass pitch processing");
                    } else {
                        normal = Some(buf.clone());
                    }
                }
                drop(buf);
                app.advance_playback(&egui::Context::default());
                assert_eq!(app.session.playhead(), expected, "the device clock must advance the timeline at {speed}x, snapped to a video frame");
                app.stop();
            }
        }
    }

    #[test]
    fn changing_pitch_preference_restarts_audio_at_the_current_playhead() {
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        app.audio = Some(Box::new(Device { ready: true, starts: starts.clone(), reading: Some(0) }));
        app.session.prefs.audio_hardware.default_output = "available".into();
        app.apply_prefs(&ctx);
        app.play(2.0);
        app.end_preroll(0.0);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        app.session.set_playhead(Tick::from_seconds_f64(2.0));
        let position = app.session.playhead();
        for (maintain, expected_starts) in [(false, 2), (true, 3)] {
            app.session.execute("prefs.set", json!({"key":"audio.maintainPitchShuttling","value":maintain})).unwrap();
            app.apply_prefs(&ctx);
            assert_eq!(starts.load(Ordering::Relaxed), expected_starts);
            assert!(app.playback.audio_clock);
            assert_eq!(app.playback.speed, 2.0);
            assert_eq!(app.playback.anchor_tick, app.session.playhead());
            assert_eq!(app.session.playhead(), position);
            app.apply_prefs(&ctx);
            assert_eq!(starts.load(Ordering::Relaxed), expected_starts, "unchanged settings do not restart");
        }
        for speed in [-1.0, 8.0] {
            app.play(speed);
            app.end_preroll(0.0);
            assert!(!app.playback.audio_clock, "unsupported speed {speed} remains silent");
        }
        assert_eq!(starts.load(Ordering::Relaxed), 3);
        app.stop();
    }

    #[test]
    fn changing_hardware_recovers_wall_clock_playback_without_rewinding() {
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        app.audio = Some(Box::new(Device { ready: false, starts: starts.clone(), reading: Some(0) }));
        app.apply_prefs(&ctx);
        app.play(1.0);
        app.end_preroll(0.0);
        assert!(!app.playback.audio_clock);
        app.session.set_playhead(Tick::from_seconds_f64(2.0));
        app.session.prefs.audio_hardware.default_output = "available".into();
        app.apply_prefs(&ctx);
        assert_eq!(starts.load(Ordering::Relaxed), 2);
        assert!(app.playback.audio_clock);
        assert_eq!(app.playback.anchor_tick, app.session.playhead());
        app.stop();
    }

    /// A device that loses its playback clock resumes from the displayed frame and reports a status message.
    #[test]
    fn a_failed_device_clock_resumes_from_the_displayed_frame() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = Some(Box::new(Device { ready: true, starts: Arc::new(AtomicUsize::new(0)), reading: None }));
        let displayed = app.session.sequence_rate().tick_of(48);
        app.session.set_playhead(displayed);
        app.playback.playing = true;
        app.playback.audio_clock = true;
        app.playback.anchor_tick = Tick::ZERO;
        app.playback.anchor_time = 0.0;
        app.set_now_for_test(10.0);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(!app.playback.audio_clock);
        assert_eq!(app.playback.anchor_tick, displayed);
        assert_eq!(app.session.playhead(), displayed);
        assert!(app.ui.status.contains("Audio output failed"));
        app.stop();
    }

    /// A stream the device dropped, which `recover` restarts on the same clock.
    struct Restartable {
        reading: Option<u64>,
    }

    impl AudioOut for Restartable {
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            Ok(48000)
        }
        fn stop(&mut self) {}
        fn sample_rate(&self) -> u32 {
            48000
        }
        fn played_frames(&self) -> Option<u64> {
            self.reading
        }
        fn recover(&mut self) {
            self.reading = self.reading.or(Some(48_000));
        }
    }

    /// A recovered stream keeps the device clock and the sound keeps playing.
    #[test]
    fn a_restarted_stream_keeps_the_device_clock_and_the_sound() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = Some(Box::new(Restartable { reading: None }));
        app.playback.playing = true;
        app.playback.audio_clock = true;
        app.playback.anchor_tick = Tick::ZERO;
        app.playback.anchor_time = 0.0;
        app.playback.audio_seen = (0, 10.0);
        app.set_now_for_test(10.0);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(app.playback.audio_clock, "the restarted stream is still the master clock");
        assert!(!app.ui.status.contains("Audio output failed"));
        // 48 000 frames played at 48 kHz: one second in, on a frame boundary
        let (shown, second) = (app.session.playhead(), Tick::from_seconds_f64(1.0));
        assert!(
            shown <= second && shown + app.session.sequence_rate().frame_duration() > second,
            "the playhead follows the frames the device played: {shown:?}"
        );
        app.stop();
    }

    /// A device whose stream dies mid-play (`ticking`) or whose open fails (`opens`).
    struct Reviving {
        starts: Arc<AtomicUsize>,
        opens: Arc<AtomicBool>,
        ticking: Arc<AtomicBool>,
    }

    impl AudioOut for Reviving {
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            self.starts.fetch_add(1, Ordering::Relaxed);
            if self.opens.load(Ordering::Relaxed) { Ok(48_000) } else { Err("device unavailable".into()) }
        }
        fn stop(&mut self) {}
        fn sample_rate(&self) -> u32 {
            48_000
        }
        fn played_frames(&self) -> Option<u64> {
            self.ticking.load(Ordering::Relaxed).then_some(0)
        }
    }

    /// A lost output is reopened on retry and the sound returns without pause/play.
    #[test]
    fn a_lost_output_is_reopened_and_the_sound_returns() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        let ticking = Arc::new(AtomicBool::new(true));
        app.audio = Some(Box::new(Reviving { starts: starts.clone(), opens: Arc::new(AtomicBool::new(true)), ticking: ticking.clone() }));
        app.play(1.0);
        app.end_preroll(0.0);
        assert!(app.playback.audio_clock && starts.load(Ordering::Relaxed) == 1);
        // the stream dies: the device stops reporting a playback clock
        ticking.store(false, Ordering::Relaxed);
        app.set_now_for_test(10.0);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(!app.playback.audio_clock);
        // the device recovers before the next retry: playback reopens it itself
        ticking.store(true, Ordering::Relaxed);
        app.set_now_for_test(10.0 + AUDIO_RETRY_S + 0.5);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0 + AUDIO_RETRY_S + 0.5), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(starts.load(Ordering::Relaxed), 2, "the output is reopened without pause/play");
        assert!(app.playback.audio_clock, "the fresh stream is the master clock again");
        app.stop();
    }

    /// A device that becomes available mid-playback starts sounding on the next retry.
    #[test]
    fn a_device_that_appears_mid_playback_starts_sounding() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicBool::new(false));
        app.audio = Some(Box::new(Reviving { starts: starts.clone(), opens: opens.clone(), ticking: Arc::new(AtomicBool::new(true)) }));
        app.play(1.0);
        app.end_preroll(0.0);
        assert!(!app.playback.audio_clock, "no device yet: playing on the wall clock");
        // headphones plug in while playing: the next retry opens them
        opens.store(true, Ordering::Relaxed);
        app.set_now_for_test(AUDIO_RETRY_S + 0.5);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(AUDIO_RETRY_S + 0.5), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(starts.load(Ordering::Relaxed), 2, "the output is retried without pause/play");
        assert!(app.playback.audio_clock);
        app.stop();
    }
}

/// Whether a shortcut chord must be left alone because a text field is focused.
///
/// A focused text field owns every unmodified key: an input method composes there from plain
/// letters (Vietnamese Telex builds `á` from `a` + `s`, `ả` from `a` + `r`, `ạ` from `a` + `j`),
/// and our single-key tool shortcuts would swallow them before the field sees them (#243). Chords
/// with a modifier are not what a text field or an IME consumes, so they keep working.
fn lets_text_field_keep(modifiers: egui::Modifiers, text_focused: bool) -> bool {
    text_focused && menus::specificity(modifiers) == 0
}

#[cfg(test)]
mod text_focus_shortcut_tests {
    use super::lets_text_field_keep;
    use egui::{Key, Modifiers};

    /// Vietnamese Telex composes diacritics from unmodified letters (`a` + `s` → `á`, `a` + `r` →
    /// `ả`, `a` + `j` → `ạ`), and the editor binds `J`, `Z`, `R`, `A`, `C`, `T`, `K`, `L`, `B`, `N`,
    /// `P`, `V`, `Y`, `U`, `H` as single-key tool shortcuts. While a text field is focused those
    /// keys must reach the field, or no diacritic can ever be typed (#243).
    #[test]
    fn an_unmodified_key_reaches_a_focused_text_field() {
        assert!(lets_text_field_keep(Modifiers::NONE, true), "the IME needs the plain letter keys");
    }

    /// Without a text field the editor keeps its single-key tool shortcuts.
    #[test]
    fn an_unmodified_key_still_fires_without_a_text_field() {
        assert!(!lets_text_field_keep(Modifiers::NONE, false));
    }

    /// A modifier chord is not what a text field or an IME consumes, so it keeps firing either way.
    #[test]
    fn a_modified_chord_still_fires_while_text_is_focused() {
        for m in [Modifiers::COMMAND, Modifiers::CTRL, Modifiers::SHIFT, Modifiers::ALT, Modifiers::COMMAND | Modifiers::SHIFT] {
            assert!(!lets_text_field_keep(m, true), "{m:?} is not an IME key");
        }
    }

    /// The keys a Vietnamese Telex IME needs must be exactly the ones we now hand to the field.
    #[test]
    fn the_telex_trigger_keys_are_left_to_the_field() {
        // s=acute, f=grave, r=hook above, x=tilde, j=dot below, w=horn, z=remove diacritic
        for key in [Key::S, Key::F, Key::R, Key::X, Key::J, Key::W, Key::Z] {
            assert!(lets_text_field_keep(Modifiers::NONE, true), "{key:?} must reach the IME");
        }
        // The letters the editor binds as tools are the ones Telex composes with, so they must be
        // left to the field too: `A` (Track Select Forward), `R` (Rate Stretch), `J` (Shuttle Left),
        // `Z` (Zoom), `C` (Razor), `T` (Type).
        for key in [Key::A, Key::R, Key::J, Key::Z, Key::C, Key::T] {
            assert!(lets_text_field_keep(Modifiers::NONE, true), "{key:?} is a Telex key the tools stole");
        }
    }
}

#[cfg(test)]
mod voiceover_playback_tests {
    use super::*;

    /// A voice-over take into an empty timeline past the end keeps playing and recording.
    #[test]
    fn a_voice_over_take_records_into_the_empty_timeline_past_the_end() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        session.execute("audio.voiceover.settings", json!({"prerollSeconds": 0.0})).unwrap();
        session.prefs.timeline.return_to_beginning = true;
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = None;
        let dur = app.session.active_sequence().map(|q| q.duration()).unwrap();
        let from = app.session.sequence_rate().snap(dur + Tick::from_seconds_f64(60.0));
        app.session.set_playhead(from);
        panels::voiceover::toggle(&mut app, None).unwrap();
        // play() keeps the record point instead of returning to the beginning
        assert!(app.playback.playing);
        assert_eq!(app.session.playhead(), from);
        app.end_preroll(0.0);
        // and one second later the take is still playing and recording, not stopped at the end
        app.set_now_for_test(1.0);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(1.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(app.playback.playing);
        assert!(app.session.voiceover.recording());
        assert!(app.session.playhead() > from, "{:?} <= {from:?}", app.session.playhead());
        app.session.execute("audio.voiceover.stop", json!({"discard": true})).unwrap();
        app.stop();
    }
}

/// A physical-pixel screen position as a point in the window's UI space, given the window's content origin in points.
///
/// Assumes the whole virtual screen uses the window's `pixels_per_point`: with monitors of mixed
/// DPI the point can be off when the cursor and the window's origin are on different monitors.
fn screen_px_to_ui(px: (i32, i32), pixels_per_point: f32, content_min: egui::Pos2) -> egui::Pos2 {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    egui::pos2(px.0 as f32 / ppp - content_min.x, px.1 as f32 / ppp - content_min.y)
}

#[cfg(test)]
mod screen_px_tests {
    use super::screen_px_to_ui;

    #[test]
    fn maps_screen_pixels_into_the_window() {
        let p = screen_px_to_ui((1300, 900), 2.0, egui::pos2(100.0, 50.0));
        assert_eq!((p.x, p.y), (550.0, 400.0));
    }

    #[test]
    fn a_hostile_scale_does_not_divide_by_zero() {
        for ppp in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let p = screen_px_to_ui((10, 20), ppp, egui::pos2(0.0, 0.0));
            assert!(p.x.is_finite() && p.y.is_finite());
        }
    }
}

#[cfg(test)]
mod native_menu_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn enabled(items: &[menus::MenuItem], id: &str) -> Option<bool> {
        items.iter().find(|it| it.id == id).map(|it| it.enabled)
    }

    /// The native menu bar (macOS) gets the menu tree whenever it changes, so Edit ▸ Undo and Redo
    /// follow the history instead of staying enabled (#902).
    #[test]
    fn native_menu_follows_undo_and_redo_availability() {
        let sent: Rc<RefCell<Vec<Vec<menus::MenuItem>>>> = Rc::default();
        let mut app = FilmcraftApp::new(Session::default());
        let sink = sent.clone();
        app.hooks.menu_changed = Some(Box::new(move |items: &[menus::MenuItem]| sink.borrow_mut().push(items.to_vec())));

        app.sync_native_menu();
        let last = |sent: &Rc<RefCell<Vec<Vec<menus::MenuItem>>>>, id: &str| sent.borrow().last().and_then(|items| enabled(items, id));
        assert_eq!(last(&sent, "edit.undo"), Some(false), "nothing to undo in an empty project");
        assert_eq!(last(&sent, "edit.redo"), Some(false), "nothing to redo in an empty project");
        assert_eq!(last(&sent, "file.newProject"), Some(true));

        app.sync_native_menu();
        assert_eq!(sent.borrow().len(), 1, "an unchanged menu is not sent again");

        app.session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        app.sync_native_menu();
        assert_eq!(last(&sent, "edit.undo"), Some(true), "an edit can be undone");
        assert_eq!(last(&sent, "edit.redo"), Some(false));

        app.session.execute("edit.undo", json!({})).unwrap();
        app.sync_native_menu();
        assert_eq!(last(&sent, "edit.undo"), Some(false));
        assert_eq!(last(&sent, "edit.redo"), Some(true), "the undone edit can be redone");
    }
}

#[cfg(test)]
mod lifecycle_synthetic_tests {
    use egui::{Event, Modifiers, PointerButton, pos2};
    use filmcraft_engine::Session;

    use super::FilmcraftApp;

    /// Extract the pointer-moved positions from a raw event batch, for synthetic-input tests.
    fn pointer_events(events: &[Event]) -> Vec<egui::Pos2> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::PointerMoved(pos) => Some(*pos),
                _ => None,
            })
            .collect()
    }

    /// `raw_input_hook` runs before hidden logic ticks too, but those raw events are not fed to
    /// widgets. The same one-step batch must remain pending, instead of advancing to the next
    /// step or being silently lost; a real UI pass consumes it.
    #[test]
    fn one_synthetic_step_survives_hidden_logic_callbacks() {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(Session::default());
        app.synthetic = vec![
            Event::PointerMoved(pos2(10.0, 10.0)),
            Event::PointerMoved(pos2(20.0, 20.0)),
            Event::PointerButton { pos: pos2(20.0, 20.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
            Event::PointerButton { pos: pos2(20.0, 20.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
        ];
        let mut first = egui::RawInput::default();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut first);
        assert_eq!(pointer_events(&first.events), vec![pos2(10.0, 10.0)]);
        assert_eq!(app.synthetic.len(), 3, "only one step was popped");
        assert_eq!(app.synthetic_pending_step.as_ref().map(Vec::len), Some(1));

        // The integration keeps the raw input that did not reach widgets (native
        // `pending_raw_input`, web `input.raw`) and replays it through the hook.
        let mut hidden = first.clone();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut hidden);
        assert_eq!(pointer_events(&hidden.events), vec![pos2(10.0, 10.0)], "the preserved step is not appended twice");
        assert_eq!(app.synthetic.len(), 3, "a hidden callback must not advance the queue");

        // A real UI pass consumes the pending step (App::ui clears it at its start).
        app.synthetic_pending_step = None;
        app.synthetic_step_offered = false;
        app.synthetic_modifiers_offered = false;
        let mut next = egui::RawInput::default();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut next);
        assert_eq!(pointer_events(&next.events), vec![pos2(20.0, 20.0)]);
        assert_eq!(app.synthetic.len(), 2);
    }

    /// eframe's hidden `run_logic` path saves the raw input (including our step) and hands it to
    /// `raw_input_hook` again on the next hidden callback. The hook must not append the same step
    /// a second time, or the eventual UI pass inserts text / clicks / runs a command more than once.
    #[test]
    fn hidden_callbacks_do_not_duplicate_a_step_preserved_in_raw_input() {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(Session::default());
        app.synthetic =
            vec![Event::Text("once".into()), Event::Key { key: egui::Key::F35, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }];
        let mut first = egui::RawInput::default();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut first);
        let texts = |events: &[Event]| events.iter().filter(|e| matches!(e, Event::Text(t) if t == "once")).count();
        assert_eq!(texts(&first.events), 1);

        // What the integration preserves as pending_raw_input, then replays through the hook on
        // the next hidden logic tick.
        let mut preserved = first.clone();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut preserved);
        assert_eq!(texts(&preserved.events), 1, "the preserved raw input must keep exactly one copy");
        assert_eq!(app.synthetic_pending_step.as_ref().map(Vec::len), Some(2), "the queue did not advance");
    }
}

//! The FilmCraft egui frontend.
//!
//! Thin by design: all project changes go through `filmcraft_engine::Session::execute`; this crate
//! owns only presentation state ([`state::UiState`]), GPU textures, the playback clock and the
//! control-channel handlers. Swap it for another toolkit without touching the engine.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

/// FilmCraft as craft-update knows it, with its 128 px icon and accent colour: the update
/// prompt shows both, and an AppImage's menu entry gets the icon.
#[cfg(not(target_arch = "wasm32"))]
pub const UPDATE_APP: craft_update::app::App = craft_update::app::App {
    icon_png: Some(include_bytes!("../../../assets/app-icon/hicolor/128x128/apps/ai.storyteller.filmcraft.png")),
    accent: Some([0x8b, 0x5c, 0xf6]),
    ..craft_update::suite::FILMCRAFT
};

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
mod dock_ui;
pub use dock_ui::area_id as dock_area_id;
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
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use egui::ime_context::ImeTarget;
use egui::{TextureHandle, TextureOptions};
use filmcraft_engine::{Services, Session};
use filmcraft_time::Tick;
use serde_json::{Value, json};

pub use control::ControlRequest;
use dock::{LayoutExt as _, PanelKind};
use frames::{FrameKey, FrameServer, Target};
use native_dialogs::FileDialog;
use state::UiState;
use theme::{ThemeKind, Tokens};

mod app;
mod audio;
mod gpu;
mod hooks;
mod raw_input;

use audio::shuttle_mix_rate;
pub use audio::{AUDIO_RETRY_S, AUDIO_STALL_S, playback_mix};
pub use gpu::{gpu_compositor_unsupported, software_rasterizer};
pub use hooks::{AudioDevices, AudioOut, ClipboardMedia, HostHooks, RelinkHint, SystemThemeFn};
pub(crate) use raw_input::rewrite_native_window_input;

pub use ime_reset::install_ime_reset_callbacks;
pub(crate) use native_dialogs::EXPORT_SAVE_DIALOGS;

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

/// Incoming composition identity for the Program monitor. Interrupted compositions remain
/// Stale until the matching opaque native reset fence; output or old terminal input is not
/// acknowledgement. The retired origin and current destination live in the UI reset adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImeComposition {
    Active(ImeTarget),
    TerminalPending(ImeTarget),
    Stale,
}

/// Why the control server is not serving although `--control` was given, for the status bar
/// (#3436). The UI translates each cause as a whole sentence; the desktop app maps its
/// `control_server::DisabledWhy` onto this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlNotice {
    /// `FILMCRAFT_CONTROL_TOKEN` is missing or malformed. (A bind failure is only logged until
    /// #3318 reworks port-collision reporting.)
    Token,
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
    /// Bumped when the custom text edit target is (re)created, a sequence/document context
    /// changes, or a control request replaces `gfxEdit`. It is the replacement epoch of the
    /// otherwise stable sequence/clip/component owner.
    pub(crate) ime_replacement_epoch: u64,
    /// The composition in flight (including an empty preedit awaiting its terminal event), or a
    /// tombstone for a composition whose target was replaced or caret/focus was interrupted.
    pub(crate) ime_composition: Option<ImeComposition>,
    ime_reset: ime_reset::State,
    ime_composition_viewport: Option<egui::ViewportId>,
    /// The input method at the Program monitor's text caret: event order, the keys it uses, the
    /// marked text drawn on the video until it commits, and the published IME descriptor.
    pub(crate) ime_composer: craft_ui::ime::Composer,
    /// The edit target `ime_composer` was made for; another target gets a new composer, so a
    /// new IME context.
    pub(crate) ime_composer_target: Option<ImeTarget>,
    /// The editor drawn in this pass; the end-of-pass check runs after control drain.
    pub(crate) ime_drawn_editor: Option<panels::graphics::DrawnImeEditor>,
    /// Project document generation last seen by the UI (the engine bumps it on replacement).
    ime_document_epoch: u64,
    /// Active sequence last seen by the UI; `sequence.close` need not emit `OpenSequence`.
    ime_active_sequence: Option<filmcraft_project::ItemId>,
    /// Viewports the Program monitor's editor published in, released when the app exits.
    ime_viewports: Vec<egui::ViewportId>,
    /// Named textures (monitors, thumbnails) with the key they show.
    textures: HashMap<String, (FrameKey, TextureHandle)>,
    /// The UI pass each thumbnail texture was last used in: thumbnails beyond
    /// [`MAX_THUMB_TEXTURES`] are freed, least recently used first (#1682).
    thumb_used: HashMap<String, u64>,
    control_rx: Option<Receiver<ControlRequest>>,
    /// Why the control server is not serving although `--control` was given (a malformed
    /// `FILMCRAFT_CONTROL_TOKEN`, a port that cannot be bound): shown whenever the status bar is
    /// idle (#3436). It is also in `filmcraft.log`; `None` when the server is running.
    pub(crate) control_notice: Option<ControlNotice>,
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
    /// [`Self::mac_ctrl_click_armed`] for each dock panel's own OS window, shared with the dock
    /// area's input hook.
    pub(crate) native_ctrl_click_armed: Arc<Mutex<HashMap<egui::ViewportId, bool>>>,
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
    /// Items imported by an OS file drop, to place on the Timeline at the drop point this frame
    /// (in the coordinates of the viewport the files were dropped on).
    pending_timeline_drop: Option<(Vec<filmcraft_engine::project::ItemId>, egui::Pos2, egui::ViewportId)>,
    /// Files being dragged over a window from the OS (count, pointer, viewport): the Timeline previews where they would land.
    file_drag_hover: Option<(usize, egui::Pos2, egui::ViewportId)>,
    /// Files dragged over or dropped on dock panels in their own OS windows last frame; the
    /// dock area reports them after the panels have drawn, so they are imported next frame.
    window_files: Vec<craft_ui::docking::WindowFiles<PanelKind>>,
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
    /// Set by device error/loss callbacks; preview drops the compositor and uses CPU rendering.
    pub broken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Largest texture side the device accepts.
    pub max_texture: u32,
}

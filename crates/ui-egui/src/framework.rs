//! FilmCraft hosts the craft runtime from its own eframe `App` through the `craft_ui::shell::embed` hooks
//! (craft-libs docs/craft-framework.md, section 2.2): [`install`] once in the app creator, [`before_input`] in
//! `raw_input_hook`, [`close_path`] last in `logic` and [`before_ui`] first in `ui`.
//!
//! The runtime takes Tab, F6, Escape and the zoom keys from egui for the whole window. The app's own egui code reads
//! an Escape that no widget of its own had focus for through [`escape_pressed`] and [`take_window_cancel`], and
//! registers its timeline and monitors as the window's canvas with [`host_canvas`], which keeps Tab and the edit
//! chords in egui for it.

use std::sync::{Arc, LazyLock};

#[cfg(not(target_arch = "wasm32"))]
use craft_host::target::Backend;
use craft_host::target::ShellInfo;
use craft_host::{AppInfo, Host};
use craft_keymap::Overrides;
use craft_theme::Mode;
use craft_ui::InstallSpec;
use craft_ui::fonts::{I18nScripts, UiFonts};
use craft_ui::runtime::commands::{CommandInput, CommandSet as _, NavCommand};
#[cfg(not(target_arch = "wasm32"))]
use craft_ui::runtime::env::SystemReader;
use craft_ui::runtime::env::{RootEnv, Scale};
use craft_ui::runtime::focus::{EditSet, KeepKeys, KeyUse};
use craft_ui::runtime::frame::{FrameInput, WindowInput};
use craft_ui::runtime::keys::{Intent, with_window_intents};
use craft_ui::shell::embed::{self, CloseDecision};
#[cfg(not(target_arch = "wasm32"))]
use craft_window::system::SystemWindows;
use craft_window::system::{NoWindows, WindowSystem};
use egui::{Context, Id, Key, Modifiers, Ui};

use crate::FilmcraftApp;

/// The app's name and version for the runtime's dialogs and the dock's words. FilmCraft's own catalogs are not
/// craft-i18n's, so the host carries none and the framework's words are English.
static HOST: LazyLock<Arc<Host>> =
    LazyLock::new(|| Arc::new(Host::builder(AppInfo { id: "filmcraft", name: "FilmCraft", version: env!("CARGO_PKG_VERSION") }).build()));

/// F6 and Shift+F6 stay keys of the app's own shortcut table (the keyboard editor offers them), so the runtime's
/// pane moves are unbound. One `Arc` for the app's life: the runtime rebuilds its keymap when the `Arc` changes.
static OVERRIDES: LazyLock<Arc<Overrides>> = LazyLock::new(|| {
    let mut overrides = Overrides::new();
    overrides.insert(NavCommand::NextPane.id(), Vec::new());
    overrides.insert(NavCommand::PreviousPane.id(), Vec::new());
    Arc::new(overrides)
});

/// Installs the runtime on the desktop app's context, with the platform's window services and the OS appearance
/// reader, then the suite's interface fonts; the app's own theme replaces them through `craft_ui::fonts::load` on
/// its first logic pass. Call it once from the app creator.
#[cfg(not(target_arch = "wasm32"))]
pub fn install(ctx: &Context, app_id: &str) -> Result<(), craft_ui::InstallError> {
    let wake = ctx.clone();
    let spec = InstallSpec::new(ShellInfo::default().backend(Backend::from_env()), Arc::new(SystemWindows))
        .system(SystemReader::start(move || wake.request_repaint()))
        .app_id(app_id);
    embed::install(ctx, spec, &UiFonts::new(&I18nScripts)).map(drop)
}

/// Installs the runtime with no window services: the web shell, and every test harness that runs the app's UI.
/// A context that has the runtime already keeps it.
pub fn install_headless(ctx: &Context) {
    install_headless_with(ctx, Arc::new(NoWindows));
}

/// [`install_headless`] with the window system `system`: a harness that scripts the OS (the desktop pointer of a drag
/// that crosses into a panel's own OS window) installs a `craft_window::system::FakeSystem`.
pub fn install_headless_with(ctx: &Context, system: Arc<dyn WindowSystem>) {
    match embed::install(ctx, InstallSpec::new(ShellInfo::default(), system), &UiFonts::new(&I18nScripts)) {
        Ok(_) | Err(craft_ui::InstallError::AlreadyInstalled) => {}
        Err(error) => log::warn!("the UI runtime could not be installed: {error}"),
    }
}

/// From `raw_input_hook`, before each frame: the app's theme mode and interface scale, its host and the bindings it
/// keeps for itself. The interface scale is egui's zoom factor as the app last set it (Settings ▸ Appearance ▸ UI
/// Scale, View ▸ Reset Interface Zoom, [`zoom_keys`]); the runtime writes it back unchanged. With
/// `--control-input-only` the runtime drops the window system's input of the OS windows it hosts (a floated panel);
/// the app's `raw_input_hook` drops the main window's.
pub fn before_input(ctx: &Context, app: &FilmcraftApp) {
    let mode = if app.ui.dark { Mode::Dark } else { Mode::Light };
    let root = RootEnv::new().mode(mode).scale(Scale::new(ctx.zoom_factor()));
    let window_input = if app.control_input_only { WindowInput::Dropped } else { WindowInput::Live };
    embed::before_input(ctx, |_output| {
        FrameInput::new(root).commands(CommandInput::new().overrides(Arc::clone(&OVERRIDES))).host(Arc::clone(&HOST)).window_input(window_input)
    });
}

/// How many events of one frame's raw input came after its first pointer press: the runtime gives a pass at most one
/// press, as its last pointer event, and holds those events for the front of the next frame's input.
#[derive(Clone, Copy, Default)]
struct PressSuffix {
    frame: u64,
    len: usize,
}

/// The records of the last two frames that ran the input hook, oldest first: the frame after a press reads the
/// press frame's record after its own hook has written the newer one.
#[derive(Clone, Copy, Default)]
struct PressSuffixes([Option<PressSuffix>; 2]);

fn press_suffix_id() -> Id {
    Id::unique("filmcraft-press-suffix")
}

/// Last in `raw_input_hook`: records how many of the frame's events follow its first pointer press, so
/// [`held_press_suffix`] can tell them, at the front of the next frame, from events that arrived in that frame.
/// A second hook call in the same frame (eframe's hidden logic ticks) keeps the longer count.
pub fn note_press_suffix(ctx: &Context, events: &[egui::Event]) {
    let len = events.iter().position(|event| matches!(event, egui::Event::PointerButton { pressed: true, .. })).map_or(0, |at| events.len() - at - 1);
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|data| {
        let records = data.get_temp_mut_or_default::<PressSuffixes>(press_suffix_id());
        match &mut records.0[1] {
            Some(current) if current.frame == frame => current.len = current.len.max(len),
            _ => records.0 = [records.0[1], Some(PressSuffix { frame, len })],
        }
    });
}

/// The Ctrl+wheel events of one frame's raw input, as they arrived: the runtime's input hook folds a smooth one into
/// `Event::Zoom` off macOS (craft-libs `ui-contract/src/runtime/egui/input.rs`, `fold_legacy_pinch`), at
/// `exp(scroll_zoom_speed × points)`, and the app reads Ctrl+wheel as its own (Timeline sideways, monitor zoom at
/// its own rate). Fallback until the pin carries the framework fix; it goes at that repin.
#[derive(Clone, Default)]
struct CtrlWheels {
    frame: u64,
    wheels: Vec<egui::Event>,
}

fn ctrl_wheels_id() -> Id {
    Id::unique("filmcraft-ctrl-wheels")
}

/// Last in `raw_input_hook`: records the frame's Ctrl+wheel events (synthetic ones included) before the runtime's
/// input hook can fold them into a zoom, for [`folded_ctrl_wheels`].
pub fn note_ctrl_wheels(ctx: &Context, events: &[egui::Event]) {
    let frame = ctx.cumulative_frame_nr();
    let wheels: Vec<egui::Event> = events
        .iter()
        .filter(|event| matches!(event, egui::Event::MouseWheel { modifiers, .. } if modifiers.ctrl && !modifiers.alt && !modifiers.shift))
        .cloned()
        .collect();
    ctx.data_mut(|data| {
        // a second hook call in the same frame (eframe's hidden logic ticks) sees the same input again
        let record = data.get_temp_mut_or_default::<CtrlWheels>(ctrl_wheels_id());
        if record.frame != frame || wheels.len() > record.wheels.len() {
            *record = CtrlWheels { frame, wheels };
        }
    });
}

/// The Ctrl+wheel events of this frame that the runtime turned into `Event::Zoom`: those the input hook saw that
/// are no longer among the pass's `events`. Empty when `events` carry no zoom, so a device's own pinch stays a
/// pinch (the runtime folds nothing when a zoom arrived as one).
pub fn folded_ctrl_wheels(ctx: &Context, events: &[egui::Event]) -> Vec<egui::Event> {
    if !events.iter().any(|event| matches!(event, egui::Event::Zoom(_))) {
        return Vec::new();
    }
    let frame = ctx.cumulative_frame_nr();
    ctx.data(|data| data.get_temp::<CtrlWheels>(ctrl_wheels_id()))
        .filter(|record| record.frame == frame)
        .map(|record| record.wheels.into_iter().filter(|wheel| !events.contains(wheel)).collect())
        .unwrap_or_default()
}

fn raw_escape_id() -> Id {
    Id::unique("filmcraft-raw-escape")
}

/// Last in `raw_input_hook`: records whether the frame's raw input carries an Escape press, before the runtime's key
/// stage routes it, for [`escape_arrived`]. An Escape over a press the runtime counts as captured (one that went down
/// in a dock group) cancels that press and reaches neither egui's input nor the window's intents, but the app's own
/// value drag under the same press still takes its edits back on it, as before the runtime (#580).
pub fn note_escape(ctx: &Context, events: &[egui::Event]) {
    if events.iter().any(|event| matches!(event, egui::Event::Key { key: Key::Escape, pressed: true, repeat: false, .. })) {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|data| data.insert_temp(raw_escape_id(), frame));
    }
}

/// Whether this frame's raw input carried an Escape press ([`note_escape`]), whoever the runtime routed it to.
pub fn escape_arrived(ctx: &Context) -> bool {
    let frame = ctx.cumulative_frame_nr();
    ctx.data(|data| data.get_temp::<u64>(raw_escape_id())) == Some(frame)
}

/// How many events at the front of this pass's input are the held suffix of the previous frame's press: they came
/// in the press's batch, after it. Zero when the previous frame had no press, or nothing after it.
pub fn held_press_suffix(ctx: &Context) -> usize {
    let frame = ctx.cumulative_frame_nr();
    ctx.data(|data| data.get_temp::<PressSuffixes>(press_suffix_id()))
        .and_then(|records| records.0.into_iter().flatten().find(|suffix| frame == suffix.frame.wrapping_add(1)))
        .map_or(0, |suffix| suffix.len)
}

/// First in `ui`, each pass: hands the runtime the root's native window (eframe's `Frame`).
#[cfg(not(target_arch = "wasm32"))]
pub fn before_ui(ctx: &Context, frame: &eframe::Frame) {
    embed::before_ui(ctx, frame);
}

/// Last in `logic`: the root window's close path. Quitting with unsaved changes asks to save them first, and the
/// window stays open while the question shows.
pub fn close_path(ctx: &Context, app: &mut FilmcraftApp) {
    embed::logic(ctx, || if crate::panels::clip_dialogs::intercept_quit(app) { CloseDecision::Hold } else { CloseDecision::Close });
}

/// Registers the app's own egui view `id`, drawn in this pass, as the window's document canvas. Tab and the edit
/// chords stay in egui's input for it, as before the runtime. Call it each pass the view shows.
pub fn host_canvas(ctx: &Context, id: Id) {
    craft_ui::runtime::focus::host_canvas(ctx, id, KeyUse::CANVAS.with_keep(KeepKeys::Tab).with_edits(EditSet::ALL));
}

/// Whether Escape was pressed in this pass: in egui's input while one of the app's widgets has focus, else as the
/// window's cancel that no framework component took. Reads without taking, so every reader of the pass sees it:
/// `Intents::take` inside the offer would also take the Escape lent from the focused stop, even when the offer
/// returns `false`, and the next reader would miss it.
pub fn escape_pressed(ctx: &Context) -> bool {
    if ctx.input(|input| input.key_pressed(Key::Escape)) {
        return true;
    }
    let mut cancel = false;
    with_window_intents(ctx, false, |intents| {
        cancel = intents.untaken(&Intent::Cancel);
        false
    });
    cancel
}

/// A value drag that Escape cancels (#580), for a slider's `resp`: the press keeps `before` under `id`, and once an
/// Escape arrives during the drag `value` holds `before` until the release. Whether the drag was escaped. The runtime
/// can take an Escape (as the cancel of a press in a dock group) without ending egui's physical drag, so the
/// cancellation is remembered until the pointer is actually released.
pub fn escape_cancels_drag(ctx: &Context, resp: &egui::Response, id: Id, before: f32, value: &mut f32) -> bool {
    let escape_id = id.with("escaped");
    if resp.drag_started() {
        ctx.data_mut(|d| {
            d.insert_temp(id, before);
            d.remove::<bool>(escape_id);
        });
    }
    let in_drag = resp.dragged() || resp.drag_stopped();
    if in_drag && (escape_pressed(ctx) || escape_arrived(ctx)) {
        ctx.data_mut(|d| d.insert_temp(escape_id, true));
    }
    let escaped = in_drag && ctx.data(|d| d.get_temp::<bool>(escape_id)).unwrap_or(false);
    if escaped && let Some(start) = ctx.data(|d| d.get_temp::<f32>(id)) {
        *value = start;
    }
    if resp.drag_stopped() {
        ctx.data_mut(|d| {
            d.remove::<f32>(id);
            d.remove::<bool>(escape_id);
        });
    }
    escaped
}

/// Takes this pass's Escape: egui's, while one of the app's widgets has focus, else the window's cancel that no
/// framework component took. Whether there was one.
pub fn take_window_cancel(ctx: &Context) -> bool {
    ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) || with_window_intents(ctx, false, |intents| intents.take(Intent::Cancel))
}

/// Whether a widget has the keyboard, so the app's shortcuts leave the keys to it: one of the app's own egui widgets
/// has egui's focus, or a framework stop that types (a text field, a shortcut recorder) has the runtime's. The runtime
/// also keeps egui's focus on its other stops (a dock tab, a dialog button) and on the canvas registered with
/// [`host_canvas`] once a press reached them; the app's shortcuts still run then, as they did before the runtime,
/// when a click on a panel left no egui widget focused.
pub fn keyboard_taken(ctx: &Context) -> bool {
    if !ctx.memory(|memory| memory.focused().is_some()) {
        return false;
    }
    match craft_ui::runtime::focus::focused_rec(ctx) {
        Some(rec) => craft_ui::runtime::focus::takes_letters(&rec.stop.keys),
        None => true,
    }
}

/// Whether a field other than `except` has the keyboard: [`keyboard_taken`], where the app's checks that "a field is
/// being edited" read egui's focus. The runtime keeps egui's focus on its own stops (a dock tab, the canvas a click
/// reached) where the app had none before, and those are no field.
pub fn field_focused(ctx: &Context, except: Option<Id>) -> bool {
    match ctx.memory(|memory| memory.focused()) {
        None => false,
        Some(id) if Some(id) == except => false,
        Some(_) => keyboard_taken(ctx),
    }
}

/// Gives `ui` the app's own egui style, for panel bodies and the group view: the pin lays them out in the dock's style.
pub fn app_style(ui: &mut Ui) {
    ui.set_style(ui.ctx().global_style());
}

/// Cmd+Plus (or Cmd+=), Cmd+Minus and Cmd+0 zoom the interface, at the end of the app's UI so its own shortcuts
/// take those chords first. The runtime turns egui's own keyboard zoom off; these are its steps.
pub fn zoom_keys(ctx: &Context) {
    use egui::gui_zoom::kb_shortcuts::{ZOOM_IN, ZOOM_IN_SECONDARY, ZOOM_OUT, ZOOM_RESET};
    if ctx.input_mut(|input| input.consume_shortcut(&ZOOM_RESET)) {
        ctx.set_zoom_factor(1.0);
        return;
    }
    if ctx.input_mut(|input| input.consume_shortcut(&ZOOM_IN) || input.consume_shortcut(&ZOOM_IN_SECONDARY)) {
        egui::gui_zoom::zoom_in(ctx);
    }
    if ctx.input_mut(|input| input.consume_shortcut(&ZOOM_OUT)) {
        egui::gui_zoom::zoom_out(ctx);
    }
}

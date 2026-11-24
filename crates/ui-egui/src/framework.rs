//! FilmCraft hosts the craft runtime from its own eframe `App` through the `craft_ui::shell::embed` hooks
//! (craft-libs docs/craft-framework.md, section 2.2): [`install`] once in the app creator, [`before_input`] in
//! `raw_input_hook`, [`close_path`] last in `logic` and [`before_ui`] first in `ui`.
//!
//! The runtime takes Tab, F6, Escape and the zoom keys from egui for the whole window. The app's own egui code reads
//! an Escape that no widget of its own had focus for through [`escape_pressed`] and [`take_window_cancel`], and
//! registers its timeline and monitors as the window's canvas with [`host_canvas`], which keeps Tab and the edit
//! chords in egui for it.

use std::sync::{Arc, LazyLock};

use craft_host::target::{Backend, ShellInfo};
use craft_host::{AppInfo, Host};
use craft_keymap::Overrides;
use craft_theme::Mode;
use craft_ui::InstallSpec;
use craft_ui::fonts::{I18nScripts, UiFonts};
use craft_ui::runtime::commands::{CommandInput, CommandSet as _, NavCommand};
use craft_ui::runtime::env::{RootEnv, Scale, SystemReader};
use craft_ui::runtime::focus::{EditSet, KeepKeys, KeyUse};
use craft_ui::runtime::frame::{FrameInput, WindowInput};
use craft_ui::runtime::keys::{Intent, with_window_intents};
use craft_ui::shell::embed::{self, CloseDecision};
use craft_window::system::{NoWindows, SystemWindows, WindowSystem};
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
    let system: Arc<dyn WindowSystem> = Arc::new(NoWindows);
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

/// How many events of one pass's raw input came after its first pointer press: the runtime gives a pass at most one
/// press, as its last pointer event, and holds those events for the front of the next frame's input.
#[derive(Clone, Copy)]
struct PressSuffix {
    frame: u64,
    len: usize,
}

fn press_suffix_id() -> Id {
    Id::new("filmcraft-press-suffix")
}

/// Last in `raw_input_hook`: records how many of the pass's events follow its first pointer press, so
/// [`held_press_suffix`] can tell them, at the front of the next frame, from events that arrived in that frame.
pub fn note_press_suffix(ctx: &Context, events: &[egui::Event]) {
    let len = events.iter().position(|event| matches!(event, egui::Event::PointerButton { pressed: true, .. })).map_or(0, |at| events.len() - at - 1);
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|data| data.insert_temp(press_suffix_id(), PressSuffix { frame, len }));
}

/// How many events at the front of this pass's input are the held suffix of the previous pass's press: they came
/// in the press's batch, after it. Zero when the previous pass had no press, or nothing after it.
pub fn held_press_suffix(ctx: &Context) -> usize {
    let frame = ctx.cumulative_frame_nr();
    ctx.data(|data| data.get_temp::<PressSuffix>(press_suffix_id())).filter(|suffix| frame == suffix.frame.wrapping_add(1)).map_or(0, |suffix| suffix.len)
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

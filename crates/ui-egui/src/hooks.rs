use super::*;

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

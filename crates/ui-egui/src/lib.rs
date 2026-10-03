//! The FilmCraft egui frontend.
//!
//! Thin by design: all project changes go through `filmcraft_engine::Session::execute`; this crate
//! owns only presentation state ([`state::UiState`]), GPU textures, the playback clock and the
//! control-channel handlers. Swap it for another toolkit without touching the engine.

pub mod automation;
pub mod brand;
pub mod control;
pub mod dock;
pub mod frames;
pub mod header;
pub mod icons;
pub mod links;
pub mod menus;
pub mod panels;
pub mod perf;
pub mod state;
pub mod theme;
pub mod widgets;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use egui::{TextureHandle, TextureOptions};
use filmcraft_engine::{Services, Session};
use filmcraft_time::Tick;
use serde_json::{Value, json};

pub use control::ControlRequest;
use dock::PanelKind;
use frames::{FrameKey, FrameServer, Target};
use state::UiState;
use theme::{ThemeKind, Tokens};

/// Audio output provided by the platform layer (cpal on desktop, WebAudio on web).
pub trait AudioOut {
    /// Start output; `fill(buffer, channels)` is called on the audio thread with interleaved f32.
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String>;
    fn stop(&mut self);
    /// Device sample rate.
    fn sample_rate(&self) -> u32;
    /// Frames played since `start` (the playback master clock), if the device reports it.
    fn played_frames(&self) -> Option<u64>;
    /// Hosts and devices that can be chosen in Settings ▸ Audio Hardware.
    fn devices(&self) -> AudioDevices {
        AudioDevices::default()
    }
    /// Apply Settings ▸ Audio Hardware (device class/output, buffer size, sample rate). Takes
    /// effect at the next `start`; `document_rate` is the sequence sample rate for "Attempt to
    /// force hardware to document sample rate".
    fn configure(&mut self, _hw: &filmcraft_engine::settings::AudioHardwarePrefs, _document_rate: Option<u32>) {}
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
    pub pick_save: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    pub pick_open_project: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Save dialog with a filter: (filter name, extensions, suggested file name) → path.
    pub pick_save_as: Option<Box<dyn FnMut(&str, &[&str], &str) -> Option<String>>>,
    /// The active keyboard shortcuts changed: update native menu key equivalents.
    pub shortcuts_changed: Option<Box<dyn FnMut(&[menus::MenuItem])>>,
    /// Open dialog for a JSON file (shortcut preset import): filter name, extensions → path.
    pub pick_open_file: Option<Box<dyn FnMut(&str, &[&str]) -> Option<String>>>,
    /// Folder picker (Link Media search, proxy and Project Manager destinations).
    pub pick_folder: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Bring the window on screen for control-channel UI requests *without* taking keyboard focus
    /// (macOS: `orderFrontRegardless`). Without it the app only requests a repaint: it never
    /// activates itself for an agent, because the user's keystrokes would land here.
    pub raise_without_focus: Option<Box<dyn FnMut()>>,
    /// Open a file in its default application, or (`true`) reveal it in the file manager (Edit ▸
    /// Edit Original, Help ▸ Reveal Log Files).
    pub open_path: Option<Box<dyn FnMut(&str, bool) -> Result<(), String>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    About,
    Shortcuts,
    NewSequence,
    /// Settings (Preferences window; page in `UiState::settings`).
    Preferences,
    /// "Recover unsaved changes from <time>?" (shown at startup when a dead session left some).
    Recovery,
    /// "Are you sure you want to discard your changes?" (File ▸ Revert).
    RevertConfirm,
    /// Clip ▸ Audio Gain… (G).
    AudioGain,
    /// Sequence ▸ Delete Tracks….
    DeleteTracks,
}

#[derive(Default)]
pub struct Playback {
    pub playing: bool,
    pub speed: f64,
    pub looping: bool,
    /// Wall-clock (egui time, s) and timeline tick when playback (re)started.
    anchor_time: f64,
    anchor_tick: Tick,
    /// Audio frames played at anchor (when the audio clock drives).
    pub audio_clock: bool,
    /// Shown / dropped frame accounting for the current (or last) play.
    pub meter: frames::PlaybackMeter,
    /// Waiting for the first frames before starting the clock: when the wait began (egui time,
    /// s; negative = not yet stamped), and whether the Program monitor has them ready.
    pub preroll: Option<f64>,
    pub preroll_ready: bool,
    /// The window was hidden (occluded/minimized) since the monitor last refreshed.
    pub hidden: bool,
    /// Forward playback stops here (Play In to Out, Play from Playhead to Out Point).
    pub stop_at: Option<Tick>,
}

/// How long `ui.screenshot` waits for the window to present the frame.
const SCREENSHOT_TIMEOUT_S: f64 = 10.0;
/// How long `ui.screenshot` waits for the UI to settle (monitors showing their exact frames, the
/// timeline zoom animation finished) before capturing what is there.
const SCREENSHOT_SETTLE_MAX_S: f64 = 5.0;

pub struct FilmcraftApp {
    pub session: Session,
    pub ui: UiState,
    pub tokens: Tokens,
    pub frames: Arc<FrameServer>,
    pub playback: Playback,
    pub audio: Option<Box<dyn AudioOut>>,
    pub hooks: HostHooks,
    pub dialog: Option<Dialog>,
    pub file_dialogs: panels::file_dialogs::FileDialogState,
    pub auto: automation::Registry,
    /// Named textures (monitors, thumbnails) with the key they show.
    textures: HashMap<String, (FrameKey, TextureHandle)>,
    control_rx: Option<Receiver<ControlRequest>>,
    /// Requests waiting for the UI to show an element: (request, give-up time).
    deferred: Vec<(ControlRequest, f64)>,
    last_ui_time: f64,
    pub(crate) synthetic: Vec<egui::Event>,
    /// BS.1770 loudness of the programme as it plays, and the next sample position to feed.
    pub(crate) loudness: Option<(filmcraft_audio_dsp::LoudnessMeter, i64)>,
    /// Status message last shown and when it first appeared (messages expire after a few seconds).
    status_seen: (String, f64),
    /// Screenshots waiting for their frame: (token, path, crop, reply, give-up time).
    pending_screenshots: Vec<(u64, Option<String>, Option<[f32; 4]>, Sender<Value>, f64)>,
    queued_screenshots: Vec<(u64, f64, u32)>,
    /// A paused monitor drew a stand-in (nearest cached) picture last frame: its exact frame is
    /// still decoding.
    pub(crate) monitor_inexact: bool,
    /// Consecutive frames the timeline zoom / scroll has been at rest. `ui.elements` answers from
    /// the frame before the last one, so its timeline rects are final from 2 on.
    pub(crate) timeline_still: u32,
    input_waiters: Vec<Sender<Value>>,
    next_token: u64,
    styled: bool,
    fonts_ready: bool,
    pub integrated_titlebar: bool,
    pub last_timeline_width: f32,
    pub fps: f32,
    last_time: f64,
    bindings: Vec<menus::KeyBinding>,
    /// Shortcut-set revision `bindings` (and the native menu) were built from.
    bindings_rev: u64,
    /// Keyboard Shortcuts dialog state.
    pub shortcut_editor: panels::shortcuts_dialog::EditorState,
    pub toast: Option<(String, f64)>,
    pub tl: panels::timeline::TlState,
    /// Commands from outside the UI (native menu bar), invoked on the UI thread.
    pub command_inbox: Option<Receiver<String>>,
    /// GPU compositor (when running on wgpu): device state + compositor + the egui texture it feeds.
    pub gpu: Option<GpuState>,
    /// Preview render job being watched (job id, where to start playing when it completes).
    watched_render: Option<(u64, Tick)>,
    /// Settings last applied to the UI (theme, tooltips, frame cache, audio device).
    applied_prefs: Option<filmcraft_engine::autosave::Preferences>,
}

pub struct GpuState {
    pub render_state: eframe::egui_wgpu::RenderState,
    pub compositor: filmcraft_gpu::GpuCompositor,
    pub texture: Option<egui::TextureId>,
    pub last_key: Option<FrameKey>,
    pub size: (u32, u32),
    /// Composite time of the last frame (ms).
    pub last_ms: f32,
}

impl FilmcraftApp {
    /// Enable the GPU compositor on the eframe wgpu device.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        let compositor = filmcraft_gpu::GpuCompositor::new(&rs.device, &rs.queue);
        self.gpu = Some(GpuState { render_state: rs, compositor, texture: None, last_key: None, size: (0, 0), last_ms: 0.0 });
    }

    /// Composite a plan on the GPU and return the egui texture showing it.
    pub fn gpu_present(&mut self, key: FrameKey, plan: &frames::GpuPlan) -> Option<(egui::TextureId, (u32, u32))> {
        let g = self.gpu.as_mut()?;
        if g.last_key == Some(key)
            && let Some(t) = g.texture
        {
            return Some((t, g.size));
        }
        let t0 = web_time::Instant::now();
        let (view, size) = g.compositor.composite_prepared(&plan.plan, Some(&plan.prepared));
        let view = view.clone();
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
    pub fn new(mut session: Session) -> Self {
        session.shortcuts.register_external(menus::external_commands());
        let recovery = !session.recovery_candidates().is_empty();
        let frames = Arc::new(FrameServer::new(session.media.clone(), session.services.clone(), session.previews.clone(), FrameServer::default_workers()));
        Self {
            session,
            ui: UiState::default(),
            tokens: Tokens::for_kind(ThemeKind::Dark),
            frames,
            playback: Playback { speed: 1.0, ..Default::default() },
            audio: None,
            hooks: HostHooks::default(),
            // Unsaved changes left by a session that died are offered first thing.
            dialog: recovery.then_some(Dialog::Recovery),
            file_dialogs: Default::default(),
            auto: Default::default(),
            textures: HashMap::new(),
            control_rx: None,
            deferred: Vec::new(),
            last_ui_time: 0.0,
            synthetic: Vec::new(),
            loudness: None,
            status_seen: (String::new(), 0.0),
            pending_screenshots: Vec::new(),
            queued_screenshots: Vec::new(),
            monitor_inexact: false,
            timeline_still: 0,
            input_waiters: Vec::new(),
            next_token: 1,
            styled: false,
            fonts_ready: false,
            integrated_titlebar: false,
            last_timeline_width: 1000.0,
            fps: 60.0,
            last_time: 0.0,
            bindings: Vec::new(),
            bindings_rev: 0,
            shortcut_editor: Default::default(),
            toast: None,
            tl: Default::default(),
            command_inbox: None,
            gpu: None,
            watched_render: None,
            applied_prefs: None,
        }
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    pub fn services(&self) -> Arc<dyn Services> {
        self.session.services.clone()
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

    fn apply_tooltips(&self, ctx: &egui::Context) {
        // Settings ▸ General ▸ Show Tool Tips
        let delay = if self.session.prefs.general.show_tool_tips { 0.5 } else { 1.0e9 };
        ctx.global_style_mut(|s| s.interaction.tooltip_delay = delay);
    }

    /// Make the UI follow the settings after they change (theme, tooltips, frame cache budget,
    /// play after rendering, audio device).
    pub fn apply_prefs(&mut self, ctx: &egui::Context) {
        if self.applied_prefs.as_ref() == Some(&self.session.prefs) {
            return;
        }
        let p = self.session.prefs.clone();
        let prev = self.applied_prefs.take();
        if prev.as_ref().is_none_or(|q| q.appearance != p.appearance || q.general.show_tool_tips != p.general.show_tool_tips) {
            self.set_theme(ctx, ThemeKind::from_pref(&p.appearance.color_theme));
        }
        self.frames.set_cache_budget(p.memory.frame_cache_mb as usize * (1 << 20));
        self.ui.play_after_render = p.timeline.play_after_rendering;
        if prev.as_ref().is_none_or(|q| q.audio_hardware != p.audio_hardware) {
            let rate = self.session.active_sequence().map(|q| q.settings.sample_rate);
            let playing = self.playback.playing && self.playback.audio_clock;
            if let Some(a) = self.audio.as_mut() {
                a.stop();
                a.configure(&p.audio_hardware, rate);
            }
            if playing {
                self.start_audio();
            }
        }
        self.applied_prefs = Some(p);
    }

    pub fn set_workspace(&mut self, name: &str) {
        self.ui.workspace = name.to_string();
        self.ui.dock = dock::workspace(name);
        if name == "Color" {
            self.ui.show_scopes = false;
        }
    }

    pub fn show_panel(&mut self, p: PanelKind) {
        if !self.ui.dock.contains(p) {
            let near = match p {
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
        self.ui.dock.activate(p);
        self.ui.focused = p;
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
        if self.session.active_sequence().is_none() {
            return;
        }
        // restart from the end → from the start (Settings ▸ Timeline ▸ "At playback end, return to
        // beginning when restarting playback")
        let dur = self.session.active_sequence().map(|q| q.duration()).unwrap_or_default();
        if speed > 0.0 && self.session.prefs.timeline.return_to_beginning && self.session.playhead() >= dur - self.session.sequence_rate().frame_duration() {
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
    }

    /// Start the clock (and audio) once the first frames are ready or the preroll timed out.
    fn end_preroll(&mut self, now: f64) {
        self.playback.preroll = None;
        self.playback.anchor_time = now;
        self.playback.anchor_tick = self.session.playhead();
        self.start_audio();
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

    pub fn stop(&mut self) {
        self.playback.playing = false;
        self.playback.stop_at = None;
        self.playback.preroll = None;
        self.playback.meter.finish();
        self.frames.stop_prefetch();
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.playback.audio_clock = false;
        if self.session.mixrec.active() {
            let t = self.session.playhead();
            if let Err(e) = self.session.execute("mixer.recordStop", json!({"time": t.0})) {
                self.ui.status = e.to_string();
            }
        }
        panels::multicam::on_stop(self);
        panels::voiceover::on_stop(self);
    }

    fn start_audio(&mut self) {
        let speed = self.playback.speed;
        if (speed - 1.0).abs() > 1e-9 {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            return;
        }
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let project = self.session.project.clone();
        let provider = self.session.media.provider(project.clone(), self.session.services.clone());
        let previews = self.session.previews.clone();
        let start_tick = self.session.playhead();
        // Settings ▸ Audio Hardware ▸ Output Mapping
        let map = [self.session.prefs.audio_hardware.map_left, self.session.prefs.audio_hardware.map_right];
        // Preferences ▸ Audio ▸ 5.1 Mixdown Type: how a 5.1 Mix plays on a stereo device
        let mixdown = filmcraft_audio_dsp::channels::Mixdown::from_id(&self.session.prefs.audio.mixdown_type).unwrap_or_default();
        let Some(a) = self.audio.as_mut() else { return };
        let sr = a.sample_rate();
        let mut cursor = start_tick.to_units_floor(sr as i64);
        let cues = panels::voiceover::cues(&self.session, sr);
        previews.live.publish_project(project.clone());
        let fill = Box::new(move |buf: &mut [f32], ch: usize| {
            // the newest project snapshot: mixer moves and other edits are heard while playing
            let project = previews.live.project().filter(|p| p.sequence(seq_id).is_some()).unwrap_or_else(|| project.clone());
            let Some(seq) = project.sequence(seq_id) else { return };
            let n = buf.len() / ch.max(1);
            // Mix at the sequence rate; convert when the device rate differs (nearest sample).
            let seq_sr = seq.settings.sample_rate;
            // a 5.1 Mix plays as six channels (L, R, C, LFE, Ls, Rs) on a device with at least six
            use filmcraft_audio_dsp::channels::Layout;
            let layout = if ch >= 6 && seq.settings.audio_master == filmcraft_project::AudioChannels::Surround51 { Layout::Surround51 } else { Layout::Stereo };
            let mix = if seq_sr == sr {
                previews.mix_layout(&project, seq_id, cursor, n, &provider, layout, mixdown)
            } else {
                let s0 = (cursor as i128 * seq_sr as i128 / sr as i128) as i64;
                let m = n * seq_sr as usize / sr as usize + 2;
                let b = previews.mix_layout(&project, seq_id, s0, m, &provider, layout, mixdown);
                let mut out = filmcraft_frame::AudioBuffer::silence(sr, b.channels.len(), n);
                for (o, c) in out.channels.iter_mut().zip(&b.channels) {
                    for (i, x) in o.iter_mut().enumerate() {
                        let j = (i * seq_sr as usize / sr as usize).min(m - 1);
                        *x = c[j];
                    }
                }
                out
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
            cursor += n as i64;
        });
        match a.start(fill) {
            Ok(_) => self.playback.audio_clock = true,
            Err(e) => {
                log::warn!("audio output unavailable: {e}");
                self.playback.audio_clock = false;
            }
        }
    }

    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playback.playing {
            return;
        }
        let now = ctx.input(|i| i.time);
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
        let rate = self.session.sequence_rate();
        let elapsed = if self.playback.audio_clock {
            match self.audio.as_ref().and_then(|a| a.played_frames().map(|f| (f, a.sample_rate()))) {
                Some((f, sr)) => f as f64 / sr as f64,
                None => now - self.playback.anchor_time,
            }
        } else {
            now - self.playback.anchor_time
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
            self.session.set_playhead(end);
            self.stop();
        } else if t >= hi && self.playback.speed > 0.0 {
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

    /// Upload a rendered frame into a named texture (only when the key changed).
    pub fn texture_for(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                tex.set(egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px), TextureOptions::LINEAR);
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
            return Some((id, egui::vec2(img.w as f32, img.h as f32)));
        }
        let scale = width as f32 / src_w.max(1) as f32;
        let project = self.session.project.clone();
        self.frames.request(key, rate.tick_of(frame), scale, &project, 50);
        self.texture_existing(&name)
    }

    // ---------------------------------------------------------------- files

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
                let paths = self.hooks.pick_files.as_mut().map(|f| f(&exts)).unwrap_or_default();
                if paths.is_empty() {
                    return Ok(Value::Null);
                }
                let r = self.session.execute("file.import", json!({"paths": paths})).map_err(|e| e.to_string());
                if let Ok(v) = &r
                    && let Some(errs) = v.get("errors").and_then(Value::as_array)
                    && !errs.is_empty()
                {
                    self.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
                }
                r
            }
            // File ▸ Import with Image Sequence: choose the first numbered still
            "file.importImageSequence" => {
                let paths = self.hooks.pick_files.as_mut().map(|f| f(filmcraft_media::STILL_EXTENSIONS)).unwrap_or_default();
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                let r = self.session.execute("file.importImageSequence", json!({"path": path})).map_err(|e| e.to_string());
                if let Err(e) = &r {
                    self.ui.status = e.clone();
                }
                r
            }
            "file.saveAs" | "file.save" | "file.saveCopy" => {
                let suggested =
                    if id == "file.saveCopy" { format!("{} copy.fcproj", self.session.project.name) } else { format!("{}.fcproj", self.session.project.name) };
                let Some(path) = self.hooks.pick_save.as_mut().and_then(|f| f(&suggested)) else { return Ok(Value::Null) };
                let cmd = if id == "file.saveCopy" { "file.saveCopy" } else { "file.saveAs" };
                self.session.execute(cmd, json!({"path": path})).map_err(|e| e.to_string())
            }
            "file.open" => {
                let Some(path) = self.hooks.pick_open_project.as_mut().and_then(|f| f()) else { return Ok(Value::Null) };
                self.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())
            }
            "graphics.newFromFile" => {
                let exts: Vec<&str> = filmcraft_media::STILL_EXTENSIONS.iter().chain(filmcraft_media::VIDEO_EXTENSIONS).copied().collect();
                let paths = self.hooks.pick_files.as_mut().map(|f| f(&exts)).unwrap_or_default();
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                self.session.execute("graphics.newFromFile", json!({"path": path})).map_err(|e| e.to_string())
            }
            "captions.import" => {
                let paths = self.hooks.pick_files.as_mut().map(|f| f(&["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp", "xml"])).unwrap_or_default();
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                self.session.execute("captions.import", json!({"path": path})).map_err(|e| e.to_string())
            }
            "captions.export" => {
                let name = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default();
                let suggested = format!("{}.srt", name.replace(' ', "_"));
                let Some(path) =
                    self.hooks.pick_save_as.as_mut().and_then(|f| {
                        f("Captions (SRT, WebVTT, SCC, MCC, EBU STL, TTML, DFXP)", &["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp"], &suggested)
                    })
                else {
                    return Ok(Value::Null);
                };
                let mut p = params.clone();
                p["path"] = json!(path);
                self.session.execute("captions.export", p).map_err(|e| e.to_string())
            }
            _ => Err(format!("no dialog for {id}")),
        }
    }

    /// Import dropped files.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let mut paths = Vec::new();
        for f in dropped {
            let p = f.path();
            if p.exists() {
                paths.push(p.to_string_lossy().to_string());
            }
        }
        if !paths.is_empty() {
            let _ = self.session.execute("file.import", json!({"paths": paths}));
        }
    }

    // ---------------------------------------------------------------- input

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.bindings_rev != self.session.shortcuts.revision {
            self.bindings = menus::bindings(self);
            self.bindings_rev = self.session.shortcuts.revision;
            if let Some(hook) = self.hooks.shortcuts_changed.as_mut() {
                let items = menus::menu_items_for(&self.session);
                hook(&items);
            }
        }
        if ctx.egui_wants_keyboard_input() || self.dialog == Some(Dialog::Shortcuts) {
            return;
        }
        // Esc cancels a dynamic trim in progress
        if self.session.trim_play.dynamic.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            let _ = self.session.execute("trim.cancelDynamic", json!({}));
        }
        // Panel shortcuts of the focused panel first: they override application shortcuts.
        let focused = self.ui.focused.title();
        let mut fire = Vec::new();
        ctx.input_mut(|i| {
            let panel = self.bindings.iter().filter(|b| b.3.as_deref() == Some(focused));
            let app_wide = self.bindings.iter().filter(|b| b.3.is_none());
            for (m, k, id, _) in panel.chain(app_wide) {
                if i.consume_key(*m, *k) {
                    fire.push(id.clone());
                }
            }
        });
        for id in fire {
            // Mark In/Out in the Source monitor when it has focus.
            let params = if self.ui.focused == PanelKind::Source
                && (matches!(id.as_str(), "markers.markIn" | "markers.markOut") || id.starts_with("markers.markSplit") || id.starts_with("markers.goToSplit"))
            {
                json!({"target": "source"})
            } else {
                json!({})
            };
            if let Err(e) = menus::invoke(self, ctx, &id, params) {
                self.ui.status = e;
            }
        }
    }

    // ---------------------------------------------------------------- control channel

    /// Make the UI pass run for a control request without stealing the user's keyboard focus.
    pub(crate) fn raise_for_control(&mut self, ctx: &egui::Context) {
        if let Some(raise) = self.hooks.raise_without_focus.as_mut() {
            raise();
        }
        ctx.request_repaint();
    }

    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        let now = ctx.input(|i| i.time);
        let mut reqs: Vec<(ControlRequest, f64)> = std::mem::take(&mut self.deferred);
        while let Ok(req) = rx.try_recv() {
            // UI requests need rendered frames: raise the window if `ui` hasn't run recently
            // (occluded macOS windows stop running `ui`).
            if req.method.starts_with("ui.") && now - self.last_ui_time > 0.25 {
                self.raise_for_control(ctx);
            }
            reqs.push((req, now + 3.0));
        }
        for (req, deadline) in reqs {
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
                    let token = self.next_token;
                    self.next_token += 1;
                    let settle = ctx.input(|i| i.time) + 0.25;
                    self.queued_screenshots.push((token, settle, 0));
                    self.pending_screenshots.push((token, path, crop, reply, settle + SCREENSHOT_TIMEOUT_S));
                }
            }
        }
        self.control_rx = Some(rx);
    }

    /// Capture once the UI shows the current state (`settled`): after a seek the monitor shows the
    /// nearest cached picture until the exact frame is decoded, and an agent must not be handed
    /// the stand-in.
    fn issue_screenshots(&mut self, ctx: &egui::Context, settled: bool) {
        let now = ctx.input(|i| i.time);
        let mut any = false;
        self.queued_screenshots.retain_mut(|(token, at, frames)| {
            *frames += 1;
            any = true;
            if now >= *at && *frames >= 3 && (settled || now >= *at + SCREENSHOT_SETTLE_MAX_S) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(*token)));
                false
            } else {
                true
            }
        });
        // A hidden window (or a sleeping display) never presents, so its screenshot never
        // arrives: give up instead of waiting (and repainting) forever.
        self.pending_screenshots.retain(|(.., reply, deadline)| {
            if now > *deadline {
                let _ = reply.send(json!({"ok": false, "error": "no frame was presented (window hidden or display asleep)"}));
                false
            } else {
                true
            }
        });
        if any {
            ctx.request_repaint();
        } else if !self.pending_screenshots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let events: Vec<_> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in events {
            if let Some(i) = self.pending_screenshots.iter().position(|(t, ..)| *t == token) {
                let (_, path, crop, reply, _) = self.pending_screenshots.remove(i);
                let r = control::save_screenshot(ctx, &image, path.as_deref(), crop);
                let _ = reply.send(r);
            }
        }
    }

    // ---------------------------------------------------------------- frame

    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.auto.begin_frame();
        self.frames.set_context(&ctx);
        self.session.poll_persistence();
        panels::trim_monitor::advance(self, &ctx);
        if self.session.persistence.is_some() && self.session.is_dirty() {
            // Keep polling the auto-save worker (status, "also save the project" results).
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        self.sync_pool();
        self.apply_prefs(&ctx);
        for ev in self.session.drain_events() {
            match ev {
                filmcraft_engine::Event::OpenSequence(_) => {
                    self.ui.timeline.fit_pending = true;
                    self.ui.dock.activate(PanelKind::Timeline);
                }
                filmcraft_engine::Event::OpenSource(_) => {
                    self.ui.dock.activate(PanelKind::Source);
                }
                filmcraft_engine::Event::Toast { message, .. } => self.toast = Some((message, ctx.input(|i| i.time))),
                filmcraft_engine::Event::ProjectChanged { .. } => {}
            }
        }
        self.handle_drops(&ctx);
        if let Some(rx) = self.command_inbox.take() {
            while let Ok(id) = rx.try_recv() {
                if let Err(e) = menus::invoke(self, &ctx, &id, json!({})) {
                    self.ui.status = e;
                }
            }
            self.command_inbox = Some(rx);
        }
        self.handle_shortcuts(&ctx);
        self.advance_playback(&ctx);
        let t = self.tokens;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, t.app_bg);
        let header_h = 38.0;
        let header = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), header_h));
        header::show(self, ui, header);
        let status_h = 20.0;
        let body = egui::Rect::from_min_max(egui::pos2(full.min.x + 1.0, header.max.y + 1.0), egui::pos2(full.max.x - 1.0, full.max.y - status_h - 2.0));
        match self.ui.mode {
            state::Mode::Edit => self.dock_area(ui, body),
            state::Mode::Import => panels::import_mode::show(self, ui, body),
            state::Mode::Export => panels::export_mode::show(self, ui, body),
        }
        panels::dialogs::show(self, &ctx);
        // Status / hint bar
        let sb = egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - status_h), full.max);
        ui.painter().rect_filled(sb, 0.0, egui::Color32::from_rgb(0x1c, 0x1c, 0x1c));
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
        let resp = ui.interact(sb, egui::Id::new("status-bar"), egui::Sense::click());
        if resp.clicked() {
            self.ui.status.clear();
        }
        self.job_status(ui, sb, &t);
    }

    /// Right side of the status bar: the running job (export / render previews) with a progress
    /// bar and a cancel button; plays the rendered range when a preview render completes.
    fn job_status(&mut self, ui: &mut egui::Ui, sb: egui::Rect, t: &Tokens) {
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
        let Some(job) = running else { return };
        if job.label.starts_with("Rendering ") && !job.label.contains("audio") && self.watched_render.is_none_or(|w| w.0 != job.id) {
            let from = self.session.active_sequence().and_then(|q| q.mark_in).unwrap_or(Tick::ZERO);
            self.watched_render = Some((job.id, from));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
        let f = job.progress.fraction().clamp(0.0, 1.0);
        let cancel = egui::Rect::from_center_size(egui::pos2(sb.max.x - 14.0, sb.center().y), egui::vec2(14.0, 14.0));
        let bar = egui::Rect::from_min_size(egui::pos2(cancel.min.x - 128.0, sb.center().y - 3.0), egui::vec2(120.0, 6.0));
        let p = ui.painter();
        p.rect_filled(bar, 3.0, t.separator);
        p.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * f, bar.height())), 3.0, t.accent);
        let verb = if job.label.starts_with("Rendering") { job.label.clone() } else { "Exporting".to_string() };
        p.text(egui::pos2(bar.min.x - 8.0, sb.center().y), egui::Align2::RIGHT_CENTER, format!("{verb}… {:.0}%", f * 100.0), Tokens::ui(11.0), t.text_dim);
        let resp = ui.interact(cancel, egui::Id::new(("job-cancel", job.id)), egui::Sense::click());
        let c = if resp.hovered() { t.hot_text } else { t.text_dim };
        let k = 3.5;
        p.line_segment([cancel.center() - egui::vec2(k, k), cancel.center() + egui::vec2(k, k)], egui::Stroke::new(1.4, c));
        p.line_segment([cancel.center() + egui::vec2(-k, k), cancel.center() + egui::vec2(k, -k)], egui::Stroke::new(1.4, c));
        self.auto.add("status.job.cancel", cancel, &format!("Cancel {}", job.label));
        self.auto.add("status.job.progress", bar, &format!("{:.0}%", f * 100.0));
        if resp.on_hover_text("Cancel").clicked() {
            job.progress.cancel.store(true, Ordering::Relaxed);
            self.watched_render = None;
        }
    }

    /// Contextual hint for the status bar (Premiere shows tool/gesture hints here).
    fn hint_text(&self) -> String {
        match self.ui.tool {
            state::Tool::Selection => "Click to select, or click in empty space and drag to marquee select. Use Shift, Opt, and Cmd for other options.",
            state::Tool::TrackSelectForward => "Click to select all clips to the right in all tracks. Shift-click for a single track.",
            state::Tool::TrackSelectBackward => "Click to select all clips to the left in all tracks. Shift-click for a single track.",
            state::Tool::Ripple => "Drag an edit point to ripple trim; later clips move to keep the gap closed.",
            state::Tool::Rolling => "Drag an edit point to roll it: the out of one clip and the in of the next move together.",
            state::Tool::RateStretch => "Drag an edge to change the clip's speed so it fills the new duration.",
            state::Tool::Remix => "Drag the edge of a music clip to remix it to the new duration at musically matching beats.",
            state::Tool::Razor => "Click to split a clip. Shift-click to split all tracks.",
            state::Tool::Slip => "Drag a clip to slip its source in/out without moving it.",
            state::Tool::Slide => "Drag a clip to slide it between its neighbours.",
            state::Tool::Hand => "Drag to scroll the timeline.",
            state::Tool::Zoom => "Click to zoom in; Opt-click to zoom out.",
            _ => "",
        }
        .to_string()
    }

    fn dock_area(&mut self, ui: &mut egui::Ui, body: egui::Rect) {
        let t = self.tokens;
        // Maximize or Restore Frame (` / Shift+`): the maximized panel fills the dock area.
        let maximized = self.ui.keys.maximized.filter(|p| self.ui.dock.contains(*p));
        let mut dock = match maximized {
            Some(p) => dock::DockNode::Tabs { panels: vec![p], active: 0 },
            None => std::mem::replace(&mut self.ui.dock, dock::DockNode::Tabs { panels: vec![], active: 0 }),
        };
        let mut groups = Vec::new();
        dock::layout(ui, &mut dock, body, &t, "", &mut groups, &mut self.auto);
        let mut actions = Vec::new();
        for g in &groups {
            actions.extend(dock::draw_group_chrome(ui, g, self.ui.focused, &t, &mut self.auto));
        }
        if maximized.is_none() {
            self.ui.dock = dock;
        }
        for g in &groups {
            let Some(p) = g.panels.get(g.active).copied() else { continue };
            self.auto.add(&format!("panel.{}", p.id()), g.content, p.title());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(g.content).id_salt(("panel", p.id())));
            child.set_clip_rect(g.content);
            panels::show(self, &mut child, p, g.content);
        }
        for a in actions {
            match a {
                dock::DockAction::Activate(p) => {
                    self.ui.dock.activate(p);
                }
                dock::DockAction::Focus(p) => self.ui.focused = p,
                dock::DockAction::Close(p) => self.ui.dock.close(p),
                dock::DockAction::PanelMenu(p, pos) => {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("panel-menu"), (p, pos)));
                }
            }
        }
        panels::panel_menu_popup(self, ui);
    }
}

impl eframe::App for FilmcraftApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if !self.synthetic.is_empty() {
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
            raw_input.events.extend(self.synthetic.drain(..n));
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.styled {
            theme::install(ctx, &self.tokens);
            self.styled = true;
            ctx.request_repaint();
        } else {
            self.fonts_ready = true;
        }
        let now = ctx.input(|i| i.time);
        let dt = (now - self.last_time) as f32;
        if dt > 0.0 {
            self.fps = self.fps * 0.9 + (1.0 / dt).min(480.0) * 0.1;
        }
        self.last_time = now;
        if self.playback.playing && ctx.input(|i| i.viewport().visible()) == Some(false) {
            // Nothing is shown while the window is hidden: not a dropped frame.
            self.playback.hidden = true;
        }
        self.timeline_still = if self.ui.timeline.animating() { 0 } else { self.timeline_still.saturating_add(1) };
        let had_synthetic = !self.synthetic.is_empty();
        self.drain_control(ctx);
        if !self.synthetic.is_empty() && !had_synthetic {
            // Occluded macOS windows stop running `ui`; bring the window forward (without taking
            // keyboard focus) so the input is processed.
            self.raise_for_control(ctx);
        }
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        let settled = !std::mem::take(&mut self.monitor_inexact) && self.timeline_still > 0;
        self.issue_screenshots(ctx, settled);
        self.collect_screenshots(ctx);
    }

    fn on_exit(&mut self) {
        // Flush the recovery journal and stop the auto-save worker; a session with nothing unsaved
        // removes its journal, one with unsaved changes keeps it for the next launch.
        self.session.shutdown();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            ui.ctx().request_repaint();
            return;
        }
        // No frame worker threads on the web: render queued frames here, within a time budget
        // that leaves room for the UI pass (a no-op where workers run).
        self.frames.pump(std::time::Duration::from_millis(if self.playback.playing { 24 } else { 40 }));
        self.frame(ui);
        let ctx = ui.ctx().clone();
        self.last_ui_time = ctx.input(|i| i.time);
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            ctx.request_repaint();
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        if self.frames.queue_len() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

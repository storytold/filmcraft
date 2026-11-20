//! Repeatable playback samples and the most recent quick export on this device.

use std::time::{Duration, Instant};

use filmcraft_engine::perf;
use filmcraft_project::ItemId;
use filmcraft_ui_egui::{FilmcraftApp, state::PlaybackRes};

const WINDOW: Duration = Duration::from_secs(10);
const PREROLL_LIMIT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Performance {
    open: bool,
    sample: Option<Sample>,
    result: Option<PlaybackResult>,
    status: Option<&'static str>,
}

struct Sample {
    document: u64,
    sequence: ItemId,
    size: (u32, u32),
    started: Instant,
    playback_start: Option<Instant>,
    hardware_frames: u64,
    fallbacks: u64,
    quality: PlaybackRes,
    quality_changed: bool,
    proxies: bool,
}

struct PlaybackResult {
    sample: Sample,
    elapsed: Duration,
    shown: u64,
    dropped: u64,
    hardware_frames: u64,
    fallbacks: u64,
}

pub(super) struct ExportMeasurement {
    pub size: (u32, u32),
    pub frames: u64,
    pub seconds: f64,
    pub encoder: String,
    pub realtime: f64,
    pub warnings: Vec<String>,
}

impl Performance {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp, export: Option<&ExportMeasurement>) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Performance").open(&mut open).default_width(420.0).show(ctx, |ui| {
            ui.heading("Playback sample");
            if self.sample.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Sampling up to 10 seconds");
                    if ui.button("Stop sample").clicked() {
                        app.stop();
                    }
                });
            } else {
                let ready = !app.transport_running() && app.session.active_sequence().is_some_and(|seq| seq.duration().0 > 0);
                if ui.add_enabled(ready, egui::Button::new("Sample playback")).clicked() {
                    self.start(app);
                }
            }
            if let Some(status) = self.status {
                ui.label(status);
            }
            if let Some(result) = &self.result {
                let sample = &result.sample;
                let (width, height) = sample.size;
                let quality = if sample.quality_changed { "changed during playback" } else { sample.quality.label() };
                ui.label(format!("{width} × {height} sequence · Preview {quality} · Proxies {}", if sample.proxies { "on" } else { "off" }));
                ui.label(format!("{:.1} s · {} displayed · {} dropped", result.elapsed.as_secs_f64(), result.shown, result.dropped));
                ui.label(format!("Hardware-decoded frames {} · Decoder fallbacks {}", result.hardware_frames, result.fallbacks));
            }
            ui.separator();
            ui.heading("Last quick export");
            if let Some(export) = export {
                let (width, height) = export.size;
                ui.label(format!("{width} × {height} · {} frames · {}", export.frames, export.encoder));
                ui.label(format!("Rendered in {:.2} s · {:.2}× real time", export.seconds, export.realtime));
                for warning in &export.warnings {
                    ui.label(warning);
                }
            } else {
                ui.label("Run Export MP4 to measure an export.");
            }
        });
        self.open = open;
    }

    fn start(&mut self, app: &mut FilmcraftApp) {
        let Some(sequence) = app.session.state.active_sequence else { return };
        let Some(seq) = app.session.active_sequence() else { return };
        let (hardware_frames, fallbacks) = hardware_counts();
        self.sample = Some(Sample {
            document: app.session.document_epoch,
            sequence,
            size: (seq.settings.width, seq.settings.height),
            started: Instant::now(),
            playback_start: None,
            hardware_frames,
            fallbacks,
            quality: app.ui.program.res,
            quality_changed: false,
            proxies: app.session.media.use_proxies(),
        });
        self.status = None;
        self.result = None;
        app.play(1.0);
    }

    pub fn poll(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp) {
        let Some(mut sample) = self.sample.take() else { return };
        if sample.document != app.session.document_epoch
            || Some(sample.sequence) != app.session.state.active_sequence
            || app.playback.speed != 1.0
            || sample.proxies != app.session.media.use_proxies()
        {
            self.status = Some("The timeline or playback settings changed. Start a new sample.");
            return;
        }
        sample.quality_changed |= sample.quality != app.ui.program.res;
        if sample.playback_start.is_none() {
            if !app.playback.playing || sample.started.elapsed() >= PREROLL_LIMIT {
                if app.playback.playing {
                    app.stop();
                }
                self.status = Some("Playback ended before a sample could be collected.");
                return;
            }
            if app.playback.preroll.is_none() {
                sample.playback_start = Some(Instant::now());
            }
        }
        if let Some(start) = sample.playback_start {
            let elapsed = start.elapsed();
            if elapsed >= WINDOW || !app.playback.playing {
                if app.playback.playing {
                    app.stop();
                }
                let (shown, dropped) = app.playback.meter.counts();
                let (hardware_frames, fallbacks) = hardware_counts();
                self.result = Some(PlaybackResult {
                    elapsed,
                    shown,
                    dropped,
                    hardware_frames: hardware_frames.saturating_sub(sample.hardware_frames),
                    fallbacks: fallbacks.saturating_sub(sample.fallbacks),
                    sample,
                });
                return;
            }
        }
        self.sample = Some(sample);
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn hardware_counts() -> (u64, u64) {
    let stats = perf::decode_json();
    let hardware = &stats["hardware"];
    (hardware["frames"].as_u64().unwrap_or(0), hardware["fallbacks"].as_u64().unwrap_or(0))
}

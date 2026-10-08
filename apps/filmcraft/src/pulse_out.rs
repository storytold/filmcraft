//! Linux audio output selection. The primary backend speaks the PulseAudio native protocol
//! (PipeWire's `pipewire-pulse` or PulseAudio itself) through `filmcraft_platform::pulse`,
//! pure Rust over a Unix socket; see that module for why cpal's ALSA path is not enough. cpal
//! remains the fallback when no sound-server socket is reachable, and can be forced by picking
//! "ALSA" as the Device Class in Settings ▸ Audio Hardware.

use std::sync::Mutex;
use std::time::Duration;

use filmcraft_engine::settings::AudioHardwarePrefs;
use filmcraft_platform::pulse;
use filmcraft_ui_egui::{AudioDevices, AudioOut};

use crate::audio::CpalOut;

/// The host name this backend adds to Settings ▸ Audio Hardware ▸ Device Class.
const HOST_NAME: &str = "PipeWire / PulseAudio";
/// Device listings wait at most this long for the sound server.
const LIST_TIMEOUT: Duration = Duration::from_secs(1);

/// `AudioOut` over [`pulse::Playback`].
pub struct PulseOut {
    playback: Option<pulse::Playback>,
    rate: u32,
    hw: AudioHardwarePrefs,
    document_rate: Option<u32>,
    /// Sinks seen by the last `devices()` call: maps the display name back to the sink name.
    sinks: Mutex<Vec<pulse::Sink>>,
}

impl PulseOut {
    pub fn new() -> Self {
        Self { playback: None, rate: 48_000, hw: AudioHardwarePrefs::default(), document_rate: None, sinks: Mutex::new(Vec::new()) }
    }

    /// The rate playback mixes at: the settings' rate, or the sequence's when forced. The
    /// server resamples, so the asked-for rate is always granted.
    fn desired_rate(&self) -> u32 {
        let want = if self.hw.force_document_rate { self.document_rate.unwrap_or(self.hw.sample_rate) } else { self.hw.sample_rate };
        if want == 0 { 48_000 } else { want }
    }

    /// The server-internal sink name for the chosen output (`None` follows the default sink).
    fn target(&self) -> Option<String> {
        if self.hw.default_output.is_empty() {
            return None;
        }
        let find =
            |sinks: &[pulse::Sink]| sinks.iter().find(|s| s.description == self.hw.default_output || s.name == self.hw.default_output).map(|s| s.name.clone());
        let cached = self.sinks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        find(&cached).or_else(|| find(&pulse::list_sinks(LIST_TIMEOUT).unwrap_or_default()))
    }
}

impl AudioOut for PulseOut {
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.stop();
        self.rate = self.desired_rate();
        let playback = pulse::Playback::start(self.rate, 2, self.target(), self.hw.buffer_size.max(64), fill)?;
        self.playback = Some(playback);
        Ok(self.rate)
    }
    fn stop(&mut self) {
        self.playback = None;
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> usize {
        2
    }
    fn played_frames(&self) -> Option<u64> {
        self.playback.as_ref().and_then(pulse::Playback::played_frames)
    }
    fn devices(&self) -> AudioDevices {
        let sinks = pulse::list_sinks(LIST_TIMEOUT).unwrap_or_default();
        let outputs = sinks.iter().map(|s| s.description.clone()).collect();
        *self.sinks.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = sinks;
        AudioDevices { hosts: vec![HOST_NAME.into()], inputs: Vec::new(), outputs, output_channels: 2 }
    }
    fn configure(&mut self, hw: &AudioHardwarePrefs, document_rate: Option<u32>) {
        self.hw = hw.clone();
        self.document_rate = document_rate;
        self.rate = self.desired_rate();
    }
}

/// The desktop audio output: the sound-server backend when its socket is reachable and chosen
/// (or by default), cpal otherwise.
pub struct SystemOut {
    pulse: PulseOut,
    cpal: CpalOut,
    hw: AudioHardwarePrefs,
    pulse_available: bool,
    /// Which backend `start` last used, so the clock and device queries stay consistent.
    active_pulse: bool,
}

impl SystemOut {
    pub fn new() -> Self {
        let pulse_available = pulse::available();
        Self { pulse: PulseOut::new(), cpal: CpalOut::new(), hw: AudioHardwarePrefs::default(), pulse_available, active_pulse: pulse_available }
    }

    fn use_pulse(&self) -> bool {
        // "PipeWire" and "PulseAudio" are accepted as synonyms, so a Device Class saved under
        // an earlier or shorter name keeps selecting this backend.
        self.pulse_available && matches!(self.hw.device_class.as_str(), "" | HOST_NAME | "PipeWire" | "PulseAudio")
    }

    fn active(&self) -> &dyn AudioOut {
        if self.active_pulse { &self.pulse } else { &self.cpal }
    }
}

impl AudioOut for SystemOut {
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.pulse.stop();
        self.cpal.stop();
        self.active_pulse = self.use_pulse();
        if self.active_pulse { self.pulse.start(fill) } else { self.cpal.start(fill) }
    }
    fn stop(&mut self) {
        self.pulse.stop();
        self.cpal.stop();
    }
    fn sample_rate(&self) -> u32 {
        self.active().sample_rate()
    }
    fn channels(&self) -> usize {
        self.active().channels()
    }
    fn played_frames(&self) -> Option<u64> {
        self.active().played_frames()
    }
    fn devices(&self) -> AudioDevices {
        let mut devices = if self.use_pulse() { self.pulse.devices() } else { self.cpal.devices() };
        let mut hosts = Vec::new();
        if self.pulse_available {
            hosts.push(HOST_NAME.to_string());
        }
        hosts.extend(self.cpal.devices().hosts);
        devices.hosts = hosts;
        devices
    }
    fn configure(&mut self, hw: &AudioHardwarePrefs, document_rate: Option<u32>) {
        self.hw = hw.clone();
        self.pulse.configure(hw, document_rate);
        self.cpal.configure(hw, document_rate);
    }
}

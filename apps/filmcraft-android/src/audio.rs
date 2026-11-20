//! Audio output for Android: the playback master clock, through cpal (AAudio).
//!
//! The editor mixes on the audio thread through the callback it hands to [`AudioOut::start`]; this
//! module opens the default output device and plays it. Without it the interface plays and shows
//! meters but nothing is heard.
//!
//! A stream the system drops (headphones unplugged, a route change) is restarted by
//! [`AudioOut::recover`], keeping the played-frame clock and the mixer, a few times at most.
//!
//! cpal's Android backend needs the Android context that `android-activity` registers, so a stream
//! can only be opened once the activity is running. The module itself is plain cpal and is built
//! on every target so it is type-checked and unit-tested with the workspace.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use filmcraft_ui_egui::AudioOut;

/// The playback mixer callback handed to [`AudioOut::start`].
type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// Sample rate and channel count assumed when no output device can be queried.
const FALLBACK_RATE: u32 = 48_000;
const FALLBACK_CHANNELS: u16 = 2;
/// A driver buffer larger than this (in samples) is refused instead of allocated.
const MAX_BUFFER: usize = 1_048_576;
/// Restarts allowed for one playback.
const MAX_RESTARTS: u32 = 3;

pub struct AndroidAudio {
    stream: Option<cpal::Stream>,
    /// The running playback's mixer, shared with the stream so a restarted stream continues it.
    fill: Option<Arc<Mutex<Fill>>>,
    played: Arc<AtomicU64>,
    health: Arc<Health>,
    restarts: u32,
    rate: u32,
    channels: u16,
    note: Option<String>,
}

/// What the audio thread reports to the UI thread.
#[derive(Default)]
struct Health {
    /// The device went away: restart the stream.
    lost: AtomicBool,
    /// Unrecoverable: the mixer panicked, a buffer was refused, or restarts ran out.
    failed: AtomicBool,
}

impl Default for AndroidAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl AndroidAudio {
    /// Audio output on the system default device. The device's sample rate and channels are read
    /// now, since the mixer must be set up for them before [`AudioOut::start`].
    pub fn new() -> Self {
        let mut out = Self {
            stream: None,
            fill: None,
            played: Arc::new(AtomicU64::new(0)),
            health: Arc::new(Health::default()),
            restarts: 0,
            rate: FALLBACK_RATE,
            channels: FALLBACK_CHANNELS,
            note: None,
        };
        match default_output() {
            Ok((_, config)) => {
                out.rate = config.sample_rate().0.max(1);
                out.channels = config.channels().max(1);
            }
            Err(e) => {
                log::warn!("no audio output: {e}");
                out.note = Some("No audio output device found: playing without sound".to_string());
            }
        }
        out
    }

    /// Open the default output device and play `fill` on it. `expect` is the (rate, channels) a
    /// restarted stream must keep: the mixer was set up for them.
    fn open(&mut self, fill: Arc<Mutex<Fill>>, expect: Option<(u32, u16)>) -> Result<(), String> {
        let (device, supported) = default_output()?;
        let (rate, channels) = (supported.sample_rate().0, supported.channels());
        if rate == 0 || channels == 0 {
            return Err("the output device reports an invalid sample rate or channel count".into());
        }
        if let Some(expect) = expect
            && expect != (rate, channels)
        {
            return Err(format!("the output device now runs at {rate} Hz with {channels} channels"));
        }
        let config: cpal::StreamConfig = supported.clone().into();
        let (played, health) = (self.played.clone(), self.health.clone());
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, fill, played, health),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, fill, played, health),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, fill, played, health),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, fill, played, health),
            other => return Err(format!("unsupported sample format {other:?}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        self.rate = rate;
        self.channels = channels;
        self.stream = Some(stream);
        Ok(())
    }
}

/// The system default output device and its default configuration.
fn default_output() -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let device = cpal::default_host().default_output_device().ok_or("no output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    Ok((device, config))
}

/// A stream that converts the mixer's f32 samples to the device's sample type `T`.
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    fill: Arc<Mutex<Fill>>,
    played: Arc<AtomicU64>,
    health: Arc<Health>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let error_health = health.clone();
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |buf: &mut [T], _| {
                // a pathological driver buffer must not panic the audio thread
                if buf.len() > MAX_BUFFER || scratch.try_reserve(buf.len().saturating_sub(scratch.len())).is_err() {
                    buf.fill(T::EQUILIBRIUM);
                    health.failed.store(true, Ordering::SeqCst);
                    return;
                }
                scratch.resize(buf.len(), 0.0);
                render(&fill, &mut scratch, channels, &played, &health);
                convert_samples(&scratch, buf);
            },
            move |e| stream_error(&error_health, &e),
            None,
        )
        .map_err(|e| e.to_string())
}

/// One device callback: run the shared mixer into `buf` and advance the clock. Never panics: a
/// panicking mixer plays silence and fails the stream, and a mixer held by another stream (never
/// expected: a restart drops the old stream first) plays silence without advancing the clock.
fn render(fill: &Mutex<Fill>, buf: &mut [f32], channels: usize, played: &AtomicU64, health: &Health) {
    let mut guard = match fill.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            buf.fill(0.0);
            return;
        }
    };
    if health.failed.load(Ordering::SeqCst) || catch_unwind(AssertUnwindSafe(|| (*guard)(buf, channels))).is_err() {
        buf.fill(0.0);
        health.failed.store(true, Ordering::SeqCst);
        return;
    }
    played.fetch_add((buf.len() / channels.max(1)) as u64, Ordering::SeqCst);
}

fn stream_error(health: &Health, e: &cpal::StreamError) {
    log::warn!("audio stream error: {e}");
    if matches!(e, cpal::StreamError::DeviceNotAvailable) {
        health.lost.store(true, Ordering::SeqCst);
    }
}

/// Convert mixer samples to the device's type. Integer types represent [-1, 1): clip, and treat
/// NaN and infinities as silence.
fn convert_samples<T: cpal::Sample + cpal::FromSample<f32>>(input: &[f32], output: &mut [T]) {
    let upper = f32::from_bits(1.0_f32.to_bits() - 1);
    for (sample, converted) in input.iter().zip(output) {
        *converted = T::from_sample(if sample.is_finite() { sample.clamp(-1.0, upper) } else { 0.0 });
    }
}

impl AudioOut for AndroidAudio {
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.stop();
        self.played.store(0, Ordering::SeqCst);
        self.health = Arc::new(Health::default());
        self.restarts = 0;
        let fill = Arc::new(Mutex::new(fill));
        self.open(fill.clone(), None)?;
        self.fill = Some(fill);
        Ok(self.rate)
    }

    fn stop(&mut self) {
        self.stream = None;
        self.fill = None;
    }

    fn recover(&mut self) {
        if self.stream.is_none() || self.health.failed.load(Ordering::SeqCst) || !self.health.lost.swap(false, Ordering::SeqCst) {
            return;
        }
        let Some(fill) = self.fill.clone() else { return };
        self.restarts += 1;
        if self.restarts > MAX_RESTARTS {
            log::warn!("the audio output keeps failing; playing without sound");
            self.health.failed.store(true, Ordering::SeqCst);
            return;
        }
        log::warn!("the audio output stream lost its device; restarting it");
        // the old stream (and its audio thread) goes first: only one stream ever runs the mixer
        self.stream = None;
        if let Err(e) = self.open(fill, Some((self.rate, self.channels))) {
            log::warn!("the audio output stream could not be restarted: {e}");
            self.health.failed.store(true, Ordering::SeqCst);
        }
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> usize {
        usize::from(self.channels)
    }

    fn played_frames(&self) -> Option<u64> {
        self.stream.as_ref().filter(|_| !self.health.failed.load(Ordering::SeqCst)).map(|_| self.played.load(Ordering::SeqCst))
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_conversion_clips_and_keeps_interleaving() {
        let mut signed = [0_i16; 8];
        convert_samples(&[-2.0, 2.0, -0.5, 0.5, 0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY], &mut signed);
        assert_eq!(signed, [i16::MIN, i16::MAX, -16_384, 16_384, 0, 0, 0, 0]);
        let mut unsigned = [0_u16; 3];
        convert_samples(&[-1.0, 0.0, 1.0], &mut unsigned);
        assert_eq!(unsigned, [0, 32_768, u16::MAX]);
    }

    #[test]
    fn the_mixer_runs_and_advances_the_clock() {
        let fill: Mutex<Fill> = Mutex::new(Box::new(|buf, channels| {
            assert_eq!(channels, 2);
            buf.fill(0.25);
        }));
        let (played, health) = (AtomicU64::new(0), Health::default());
        let mut buf = [0.0_f32; 8];
        render(&fill, &mut buf, 2, &played, &health);
        assert_eq!(buf, [0.25; 8]);
        assert_eq!(played.load(Ordering::SeqCst), 4, "frames, not samples");
    }

    #[test]
    fn a_panicking_mixer_plays_silence_and_fails_the_stream() {
        let fill: Mutex<Fill> = Mutex::new(Box::new(|_, _| panic!("mixer bug")));
        let (played, health) = (AtomicU64::new(0), Health::default());
        let mut buf = [1.0_f32; 4];
        render(&fill, &mut buf, 2, &played, &health);
        assert_eq!(buf, [0.0; 4]);
        assert!(health.failed.load(Ordering::SeqCst));
        assert_eq!(played.load(Ordering::SeqCst), 0);
        // once failed, the mixer is not called again
        render(&fill, &mut buf, 2, &played, &health);
    }

    #[test]
    fn no_stream_means_no_clock() {
        let out = AndroidAudio {
            stream: None,
            fill: None,
            played: Arc::new(AtomicU64::new(0)),
            health: Arc::new(Health::default()),
            restarts: 0,
            rate: 48_000,
            channels: 2,
            note: None,
        };
        assert_eq!(out.played_frames(), None);
        assert_eq!((out.sample_rate(), out.channels()), (48_000, 2));
    }
}

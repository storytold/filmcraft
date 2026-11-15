//! cpal audio input for voice-over recording ([`filmcraft_engine::voiceover::AudioInput`]).
//!
//! The stream lives on its own thread (cpal streams are not `Send` on every backend); the
//! callback de-interleaves into a shared planar buffer that the engine drains when recording stops.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use filmcraft_engine::voiceover::{AudioInput, InputFormat};

pub struct CpalIn {
    /// Host name (Settings ▸ Audio Hardware ▸ Device Class; empty = default host).
    host: String,
    buf: Arc<Mutex<Vec<Vec<f32>>>>,
    stop: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
    /// Frames of `buf` already measured by [`AudioInput::take_peaks`].
    metered: usize,
}

impl CpalIn {
    pub fn new(host: &str) -> Self {
        Self { host: host.to_string(), buf: Arc::default(), stop: None, worker: None, error: Arc::default(), metered: 0 }
    }
}

/// At most this many trailing frames are measured per meter poll (2 s at 48 kHz), so a meter that
/// was not polled for a while doesn't scan the whole take.
const PEAK_WINDOW: usize = 96_000;

/// Per-channel peak of the frames from `from` on (only the last [`PEAK_WINDOW`]), and the new
/// measured position; no peaks when nothing new arrived.
fn peaks(buf: &[Vec<f32>], from: usize) -> (Vec<f32>, usize) {
    let len = buf.first().map_or(0, Vec::len);
    let start = from.max(len.saturating_sub(PEAK_WINDOW));
    if start >= len {
        return (Vec::new(), len);
    }
    let peaks = buf.iter().map(|c| c.get(start..).unwrap_or_default().iter().fold(0f32, |m, s| m.max(s.abs()))).collect();
    (peaks, len)
}

fn host(name: &str) -> cpal::Host {
    if !name.is_empty()
        && let Some(id) = cpal::available_hosts().into_iter().find(|h| h.name() == name)
        && let Ok(h) = cpal::host_from_id(id)
    {
        return h;
    }
    cpal::default_host()
}

fn device(h: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if !name.is_empty()
        && let Ok(mut devs) = h.input_devices()
        && let Some(d) = devs.find(|d| d.name().is_ok_and(|n| n == name))
    {
        return Some(d);
    }
    h.default_input_device()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn report_error(error: &Mutex<Option<String>>, message: String) {
    let mut error = lock(error);
    if error.is_none() {
        eprintln!("filmcraft: audio input error: {message}");
        *error = Some(message);
    }
}

fn append<T: cpal::Sample>(data: &[T], channels: usize, buf: &Mutex<Vec<Vec<f32>>>, error: &Mutex<Option<String>>)
where
    f32: cpal::FromSample<T>,
{
    if lock(error).is_some() {
        return;
    }
    let mut b = lock(buf);
    if channels == 0 || channels > 256 || data.len() > 1_048_576 {
        report_error(error, "invalid input device buffer".into());
        return;
    }
    if b.len() != channels {
        *b = vec![Vec::new(); channels];
    }
    // The recorder drains at stop. Bound accumulated samples and report exhaustion instead of
    // allowing a long take or a broken/unpaced driver to exhaust the application heap.
    let samples = b.first().map_or(0, Vec::len).saturating_mul(channels).saturating_add(data.len());
    let frames = data.len() / channels;
    if samples > 268_435_456 || b.iter_mut().any(|c| c.try_reserve(frames).is_err()) {
        report_error(error, "voice-over input buffer is full; recording failed".into());
        return;
    }
    for frame in data.chunks_exact(channels) {
        for (channel, sample) in b.iter_mut().zip(frame) {
            let sample = sample.to_sample::<f32>();
            channel.push(if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 });
        }
    }
}

fn input_stream<T: cpal::SizedSample>(
    dev: &cpal::Device,
    config: &cpal::StreamConfig,
    buf: Arc<Mutex<Vec<Vec<f32>>>>,
    error: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    f32: cpal::FromSample<T>,
{
    let channels = usize::from(config.channels);
    let stream_error = error.clone();
    dev.build_input_stream(config, move |data: &[T], _| append(data, channels, &buf, &error), move |e| report_error(&stream_error, e.to_string()), None)
}

impl AudioInput for CpalIn {
    fn configure_host(&mut self, name: &str) {
        self.host = name.to_string();
    }
    fn devices(&self) -> Vec<String> {
        host(&self.host).input_devices().map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
    }
    fn channels(&self, name: &str) -> u16 {
        device(&host(&self.host), name).and_then(|d| d.default_input_config().ok()).map(|c| c.channels()).unwrap_or(0)
    }
    fn start(&mut self, name: &str, sample_rate: u32) -> Result<InputFormat, String> {
        self.stop();
        lock(&self.buf).clear();
        self.metered = 0;
        *lock(&self.error) = None;
        let (host_name, name) = (self.host.clone(), name.to_string());
        let buf = self.buf.clone();
        let error = self.error.clone();
        let (fmt_tx, fmt_rx) = mpsc::channel::<Result<InputFormat, String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        self.worker = Some(
            std::thread::Builder::new()
                .name("voice-over input".into())
                .spawn(move || {
                    let open = || -> Result<(cpal::Stream, InputFormat), String> {
                        let h = host(&host_name);
                        let dev = device(&h, &name).ok_or("no input device")?;
                        let default = dev.default_input_config().map_err(|e| e.to_string())?;
                        // Prefer f32 at the sequence rate, retaining integer-only input devices.
                        let cfg = dev
                            .supported_input_configs()
                            .ok()
                            .and_then(|it| {
                                it.filter(|c| c.channels() > 0 && (c.min_sample_rate().0..=c.max_sample_rate().0).contains(&sample_rate))
                                    .max_by_key(|c| (c.channels() == default.channels(), c.sample_format() == cpal::SampleFormat::F32))
                                    .and_then(|c| c.try_with_sample_rate(cpal::SampleRate(sample_rate)))
                            })
                            .unwrap_or(default);
                        if cfg.channels() == 0 || cfg.channels() > 256 || cfg.sample_rate().0 == 0 {
                            return Err("the input device returned an invalid format".into());
                        }
                        let fmt = InputFormat { sample_rate: cfg.sample_rate().0, channels: cfg.channels() };
                        let format = cfg.sample_format();
                        let config = cfg.into();
                        let stream = match format {
                            cpal::SampleFormat::I8 => input_stream::<i8>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::I16 => input_stream::<i16>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::I24 => input_stream::<cpal::I24>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::I32 => input_stream::<i32>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::I64 => input_stream::<i64>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::U8 => input_stream::<u8>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::U16 => input_stream::<u16>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::U32 => input_stream::<u32>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::U64 => input_stream::<u64>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::F32 => input_stream::<f32>(&dev, &config, buf.clone(), error.clone()),
                            cpal::SampleFormat::F64 => input_stream::<f64>(&dev, &config, buf.clone(), error.clone()),
                            other => return Err(format!("unsupported input sample format {other:?}")),
                        }
                        .map_err(|e| e.to_string())?;
                        stream.play().map_err(|e| e.to_string())?;
                        Ok((stream, fmt))
                    };
                    let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(open)).unwrap_or_else(|_| Err("audio input worker panicked".into()));
                    match opened {
                        Ok((stream, fmt)) => {
                            let _ = fmt_tx.send(Ok(fmt));
                            // keep the stream alive until stop (or the sender is dropped)
                            let _ = stop_rx.recv();
                            drop(stream);
                        }
                        Err(e) => {
                            let _ = fmt_tx.send(Err(e));
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        self.stop = Some(stop_tx);
        match fmt_rx.recv().map_err(|e| e.to_string()).and_then(|f| f) {
            Ok(fmt) => Ok(fmt),
            Err(e) => {
                self.stop();
                Err(e)
            }
        }
    }
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>> {
        let mut b = lock(&self.buf);
        self.metered = self.metered.saturating_sub(b.first().map_or(0, |c| frames.min(c.len())));
        b.iter_mut()
            .map(|c| {
                let n = frames.min(c.len());
                c.drain(..n).collect()
            })
            .collect()
    }
    fn take_peaks(&mut self) -> Vec<f32> {
        let (p, metered) = peaks(&lock(&self.buf), self.metered);
        self.metered = metered;
        p
    }
    fn discard(&mut self) {
        lock(&self.buf).iter_mut().for_each(Vec::clear);
        self.metered = 0;
    }
    fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }
    fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            report_error(&self.error, "audio input worker panicked".into());
        }
    }
}

impl Drop for CpalIn {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_and_unsigned_input_is_deinterleaved_to_float() {
        let buf = Mutex::new(Vec::new());
        let error = Mutex::new(None);
        append(&[i16::MIN, i16::MAX, 0, 16384], 2, &buf, &error);
        let data = lock(&buf);
        assert_eq!(data[0], [-1.0, 0.0]);
        assert!((data[1][0] - 1.0).abs() < 0.0001);
        assert_eq!(data[1][1], 0.5);
        drop(data);
        lock(&buf).clear();
        append(&[0_u16, 32768, 65535, 32768], 2, &buf, &error);
        let data = lock(&buf);
        assert_eq!(data[1], [0.0, 0.0]);
        assert_eq!(data[0][0], -1.0);
        assert!(lock(&error).is_none());
    }

    #[test]
    fn peaks_measure_only_new_frames_and_stop_when_nothing_arrives() {
        let buf = vec![vec![0.1, -0.8, 0.2], vec![0.0, 0.3, -0.4]];
        let (p, at) = peaks(&buf, 0);
        assert_eq!((p, at), (vec![0.8, 0.4], 3));
        assert_eq!(peaks(&buf, 3), (Vec::new(), 3), "nothing new: no peaks");
        assert_eq!(peaks(&buf, 2).0, [0.2, 0.4], "only frames from the measured position");
        assert_eq!(peaks(&[], 5), (Vec::new(), 0));
        let long = vec![[vec![1.0], vec![0.25; PEAK_WINDOW]].concat()];
        assert_eq!(peaks(&long, 0).0, [0.25], "only the trailing window is scanned");
    }

    #[test]
    fn invalid_input_is_reported_without_panicking_or_appending() {
        let buf = Mutex::new(Vec::new());
        let error = Mutex::new(None);
        append(&[0.0_f32], 0, &buf, &error);
        assert!(lock(&error).is_some());
        append(&[1.0_f32], 1, &buf, &error);
        assert!(lock(&buf).is_empty());
    }
}

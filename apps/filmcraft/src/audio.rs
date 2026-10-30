//! Audio Output Backend - Cross-platform audio with ALSA/PulseAudio/PipeWire support
//!
//! Fixes #3510: No audio on Kubuntu 25.10 - ALSA default device fallback
//! Also improves audio device selection and error handling across platforms

use std::sync::{Arc, Mutex};
use cpal::{Device, Host, Stream, StreamConfig, SampleFormat, SampleRate, ChannelCount, BuildStreamError, PlayStreamError};
use cpal::traits::{HostTrait, DeviceTrait, StreamTrait};
use filmcraft_audio_dsp::{AudioBuffer, Sample};
use crate::audio_mixer::AudioMixer;

/// Audio output configuration
#[derive(Debug, Clone)]
pub struct AudioOutputConfig {
    pub device_name: Option<String>,
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub channels: u16,
    pub exclusive_mode: bool,
    pub latency_mode: LatencyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyMode {
    Low,
    Balanced,
    High,
}

impl Default for AudioOutputConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: 48000,
            buffer_size: 512,
            channels: 2,
            exclusive_mode: false,
            latency_mode: LatencyMode::Balanced,
        }
    }
}

/// Audio output backend abstraction
pub struct AudioOutput {
    host: Host,
    device: Option<Device>,
    stream: Option<Stream>,
    config: AudioOutputConfig,
    mixer: Arc<Mutex<AudioMixer>>,
    is_playing: Arc<std::sync::atomic::AtomicBool>,
    error_callback: Arc<Mutex<Option<Box<dyn Fn(String) + Send + Sync>>>>,
}

impl AudioOutput {
    /// Create new audio output with automatic device detection
    pub fn new(config: AudioOutputConfig) -> Result<Self, String> {
        // Try to initialize host with platform-specific preferences
        let host = Self::init_host()?;
        
        // Find suitable output device
        let device = Self::find_output_device(&host, &config)?;
        
        let mixer = Arc::new(Mutex::new(AudioMixer::new(config.sample_rate, config.channels as usize)?));
        
        Ok(Self {
            host,
            device,
            stream: None,
            config,
            mixer,
            is_playing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            error_callback: Arc::new(Mutex::new(None)),
        })
    }

    /// Initialize CPAL host with platform-specific fallback chain
    fn init_host() -> Result<Host, String> {
        // Platform-specific host initialization order
        #[cfg(target_os = "linux")]
        {
            // On Linux, try PipeWire first (modern), then PulseAudio, then ALSA
            if let Ok(host) = cpal::host_from_id(cpal::HostId::PipeWire) {
                log::info!("Using PipeWire audio host");
                return Ok(host);
            }
            if let Ok(host) = cpal::host_from_id(cpal::HostId::PulseAudio) {
                log::info!("Using PulseAudio audio host");
                return Ok(host);
            }
            // Fallback to ALSA
            log::info!("Using ALSA audio host");
            cpal::host_from_id(cpal::HostId::Alsa).map_err(|e| format!("Failed to initialize ALSA host: {}", e))
        }
        
        #[cfg(target_os = "macos")]
        {
            // macOS uses Core Audio
            log::info!("Using CoreAudio audio host");
            cpal::default_host()
        }
        
        #[cfg(target_os = "windows")]
        {
            // Windows uses WASAPI (default)
            log::info!("Using WASAPI audio host");
            cpal::default_host()
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            log::info!("Using default audio host");
            cpal::default_host()
        }
    }

    /// Find suitable output device with fallback chain
    fn find_output_device(host: &Host, config: &AudioOutputConfig) -> Result<Device, String> {
        // 1. Try user-specified device
        if let Some(name) = &config.device_name {
            if let Some(device) = host.output_devices()?.find(|d| d.name().map_or(false, |n| n == *name)) {
                log::info!("Using specified audio device: {}", name);
                return Ok(device);
            }
            log::warn!("Specified audio device '{}' not found, falling back", name);
        }

        // 2. Try default output device
        if let Some(device) = host.default_output_device() {
            log::info!("Using default audio device: {}", device.name().unwrap_or_default());
            return Ok(device);
        }

        // 3. On Linux, try to find a working device with fallbacks
        #[cfg(target_os = "linux")]
        {
            let devices: Vec<_> = host.output_devices()?.collect();
            
            // Priority order for Linux:
            // 1. PipeWire/PulseAudio virtual devices
            // 2. USB audio devices
            // 3. Built-in audio
            // 4. HDMI/DisplayPort audio
            
            for device in &devices {
                let name = device.name().unwrap_or_default().to_lowercase();
                if name.contains("pipewire") || name.contains("pulse") || name.contains("default") {
                    log::info!("Using PipeWire/PulseAudio device: {}", device.name().unwrap_or_default());
                    return Ok(device.clone());
                }
            }
            
            for device in &devices {
                let name = device.name().unwrap_or_default().to_lowercase();
                if name.contains("usb") || name.contains("audiointerface") || name.contains("focusrite") || name.contains("scarlett") {
                    log::info!("Using USB audio device: {}", device.name().unwrap_or_default());
                    return Ok(device.clone());
                }
            }
            
            // Fallback to first available device
            if let Some(device) = devices.first() {
                log::warn!("Falling back to first available device: {}", device.name().unwrap_or_default());
                return Ok(device.clone());
            }
        }

        // 4. Generic fallback - first available device
        if let Some(device) = host.output_devices()?.next() {
            log::warn!("Using first available audio device: {}", device.name().unwrap_or_default());
            return Ok(device);
        }

        Err("No audio output device found".to_string())
    }

    /// Start audio stream
    pub fn start(&mut self) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(()); // Already started
        }

        let device = self.device.as_ref().ok_or("No audio device selected")?;
        
        // Get supported configs and pick the best one
        let supported_configs = device.supported_output_configs()
            .map_err(|e| format!("Failed to get supported configs: {}", e))?;
        
        let stream_config = self.select_best_config(supported_configs)?;
        
        log::info!("Starting audio stream: {}Hz, {}ch, {} buffer", 
            stream_config.sample_rate.0, 
            stream_config.channels, 
            stream_config.buffer_size);
        
        let mixer = self.mixer.clone();
        let is_playing = self.is_playing.clone();
        let error_cb = self.error_callback.clone();
        
        let stream = device.build_output_stream(
            &stream_config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                if is_playing.load(std::sync::atomic::Ordering::Relaxed) {
                    let mut mixer = mixer.lock().unwrap();
                    mixer.process(data);
                } else {
                    // Silence when not playing
                    data.fill(0.0);
                }
            },
            move |err| {
                if let Some(cb) = error_cb.lock().unwrap().as_ref() {
                    cb(format!("Audio stream error: {}", err));
                } else {
                    log::error!("Audio stream error: {}", err);
                }
            },
            None,
        ).map_err(|e| format!("Failed to build audio stream: {}", e))?;

        stream.play().map_err(|e| format!("Failed to start audio stream: {}", e))?;
        
        self.stream = Some(stream);
        self.is_playing.store(true, std::sync::atomic::Ordering::Relaxed);
        
        Ok(())
    }

    /// Stop audio stream
    pub fn stop(&mut self) {
        self.is_playing.store(false, std::sync::atomic::Ordering::Relaxed);
        self.stream = None;
    }

    /// Set error callback
    pub fn set_error_callback<F>(&mut self, callback: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        *self.error_callback.lock().unwrap() = Some(Box::new(callback));
    }

    /// Get mixer for audio processing
    pub fn mixer(&self) -> Arc<Mutex<AudioMixer>> {
        self.mixer.clone()
    }

    /// Select best stream configuration
    fn select_best_config(&self, configs: cpal::SupportedStreamConfigs) -> Result<StreamConfig, String> {
        let mut best_config: Option<cpal::SupportedStreamConfig> = None;
        let target_rate = self.config.sample_rate;
        let target_channels = self.config.channels;

        for config in configs {
            // Prefer matching sample rate and channel count
            let rate_match = config.min_sample_rate().0 <= target_rate && config.max_sample_rate().0 >= target_rate;
            let channel_match = config.channels() as u16 == target_channels;
            let format_match = config.sample_format() == SampleFormat::F32;

            if rate_match && channel_match && format_match {
                best_config = Some(config);
                break;
            } else if best_config.is_none() && format_match {
                best_config = Some(config);
            }
        }

        let config = best_config.ok_or("No suitable audio format found (need f32)")?;
        
        // Use target sample rate if supported, otherwise use config's default
        let sample_rate = if config.min_sample_rate().0 <= target_rate && config.max_sample_rate().0 >= target_rate {
            SampleRate(target_rate)
        } else {
            config.default_sample_rate()
        };

        Ok(config.with_sample_rate(sample_rate).into())
    }

    /// Get current device info
    pub fn device_info(&self) -> Option<String> {
        self.device.as_ref().and_then(|d| d.name().ok())
    }

    /// List available output devices
    pub fn list_devices(&self) -> Result<Vec<String>, String> {
        let devices = self.host.output_devices()
            .map_err(|e| format!("Failed to list devices: {}", e))?;
        Ok(devices.filter_map(|d| d.name().ok()).collect())
    }

    /// Check if stream is active
    pub fn is_active(&self) -> bool {
        self.stream.is_some() && self.is_playing.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Audio mixer for combining multiple audio sources
pub mod audio_mixer {
    use super::*;
    use std::collections::VecDeque;

    pub struct AudioMixer {
        sample_rate: u32,
        channels: usize,
        master_volume: f32,
        input_buffers: Vec<VecDeque<f32>>,
        max_buffer_frames: usize,
    }

    impl AudioMixer {
        pub fn new(sample_rate: u32, channels: usize) -> Result<Self, String> {
            Ok(Self {
                sample_rate,
                channels,
                master_volume: 1.0,
                input_buffers: Vec::new(),
                max_buffer_frames: 4096,
            })
        }

        pub fn process(&mut self, output: &mut [f32]) {
            let frames = output.len() / self.channels;
            
            // Clear output
            output.fill(0.0);
            
            // Mix all input buffers
            for buffer in &mut self.input_buffers {
                for i in 0..frames.min(buffer.len() / self.channels) {
                    for ch in 0..self.channels {
                        let idx = i * self.channels + ch;
                        if idx < output.len() && idx < buffer.len() {
                            output[idx] += buffer[idx] * self.master_volume;
                        }
                    }
                }
                // Remove consumed samples
                let consumed = frames * self.channels;
                if consumed < buffer.len() {
                    buffer.drain(0..consumed);
                } else {
                    buffer.clear();
                }
            }
            
            // Clamp to prevent clipping
            for sample in output.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
        }

        pub fn push_audio(&mut self, channel: usize, data: &[f32]) {
            if channel >= self.input_buffers.len() {
                self.input_buffers.resize(channel + 1, VecDeque::new());
            }
            
            let buffer = &mut self.input_buffers[channel];
            buffer.extend(data.iter().cloned());
            
            // Limit buffer size
            if buffer.len() > self.max_buffer_frames * self.channels {
                let excess = buffer.len() - self.max_buffer_frames * self.channels;
                buffer.drain(0..excess);
            }
        }

        pub fn set_master_volume(&mut self, volume: f32) {
            self.master_volume = volume.clamp(0.0, 2.0);
        }

        pub fn master_volume(&self) -> f32 {
            self.master_volume
        }
    }
}

/// Audio input for recording (voice-over, etc.)
pub struct AudioInput {
    device: Option<Device>,
    stream: Option<Stream>,
    config: AudioOutputConfig,
    callback: Arc<Mutex<Option<Box<dyn Fn(&[f32]) + Send + Sync>>>>,
}

impl AudioInput {
    pub fn new(config: AudioOutputConfig) -> Result<Self, String> {
        let host = AudioOutput::init_host()?;
        let device = Self::find_input_device(&host, &config)?;
        
        Ok(Self {
            device,
            stream: None,
            config,
            callback: Arc::new(Mutex::new(None)),
        })
    }

    fn find_input_device(host: &Host, config: &AudioOutputConfig) -> Result<Device, String> {
        if let Some(name) = &config.device_name {
            if let Some(device) = host.input_devices()?.find(|d| d.name().map_or(false, |n| n == *name)) {
                return Ok(device);
            }
        }
        
        host.default_input_device()
            .ok_or_else(|| "No default input device found".to_string())
    }

    pub fn start<F>(&mut self, callback: F) -> Result<(), String>
    where
        F: Fn(&[f32]) + Send + Sync + 'static,
    {
        *self.callback.lock().unwrap() = Some(Box::new(callback));
        
        let device = self.device.as_ref().ok_or("No input device")?;
        let configs = device.supported_input_configs()
            .map_err(|e| format!("Failed to get input configs: {}", e))?;
        
        let stream_config = self.select_best_input_config(configs)?;
        
        let cb = self.callback.clone();
        let stream = device.build_input_stream(
            &stream_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if let Some(cb) = cb.lock().unwrap().as_ref() {
                    cb(data);
                }
            },
            |err| log::error!("Audio input error: {}", err),
            None,
        ).map_err(|e| format!("Failed to build input stream: {}", e))?;

        stream.play().map_err(|e| format!("Failed to start input stream: {}", e))?;
        self.stream = Some(stream);
        
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stream = None;
    }

    fn select_best_input_config(&self, configs: cpal::SupportedStreamConfigs) -> Result<StreamConfig, String> {
        let mut best = None;
        for config in configs {
            if config.sample_format() == SampleFormat::F32 {
                best = Some(config);
                break;
            }
        }
        let config = best.ok_or("No f32 input format")?;
        Ok(config.with_sample_rate(SampleRate(self.config.sample_rate)).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_config_default() {
        let config = AudioOutputConfig::default();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
    }

    #[test]
    fn test_mixer_creation() {
        let mixer = audio_mixer::AudioMixer::new(48000, 2);
        assert!(mixer.is_ok());
    }
}
//! libopus multistream encoder; RFC 6716 audio, RFC 7845 channel mapping and pre-skip.
use std::ffi::c_void;

use filmcraft_export::{AudioCodec, AudioEncoder, ExportSettings, Format, Result};
use filmcraft_isobmff::{CodecConfig, FourCc, OpusConfig, SampleEntry};

use super::audio_error as err;

type Create = unsafe extern "C" fn(i32, i32, i32, *mut i32, *mut i32, *mut u8, i32, *mut i32) -> *mut c_void;
type Encode = unsafe extern "C" fn(*mut c_void, *const f32, i32, *mut u8, i32) -> i32;
type Ctl = unsafe extern "C" fn(*mut c_void, i32, ...) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);

struct Api {
    create: Create,
    encode: Encode,
    ctl: Ctl,
    destroy: Destroy,
    _library: libloading::Library,
}

impl Api {
    fn load() -> std::result::Result<Self, String> {
        let library = super::load("FILMCRAFT_OPUS_LIBRARY", "opus.dll")?;
        // SAFETY: signatures from the public libopus multistream API; the library outlives symbols.
        unsafe {
            Ok(Self {
                create: *library.get(b"opus_multistream_surround_encoder_create\0").map_err(|e| e.to_string())?,
                encode: *library.get(b"opus_multistream_encode_float\0").map_err(|e| e.to_string())?,
                ctl: *library.get(b"opus_multistream_encoder_ctl\0").map_err(|e| e.to_string())?,
                destroy: *library.get(b"opus_multistream_encoder_destroy\0").map_err(|e| e.to_string())?,
                _library: library,
            })
        }
    }
}

pub fn available() -> bool {
    static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| Api::load().is_ok())
}

struct Encoder {
    api: Api,
    handle: *mut c_void,
    config: OpusConfig,
    channels: usize,
    pending: Vec<f32>,
    ended: bool,
}

// SAFETY: an exclusively owned libopus encoder has no thread affinity; &mut self serializes calls.
unsafe impl Send for Encoder {}

impl Encoder {
    fn new(channels: u32, s: &ExportSettings) -> Result<Self> {
        if !matches!(channels, 1 | 2 | 6) || !(6..=510).contains(&s.audio.bitrate_kbps) {
            return Err(err("needs mono, stereo or 5.1 and 6..510 kbps"));
        }
        let api = Api::load().map_err(err)?;
        let (mut streams, mut coupled, mut code) = (0, 0, 0);
        let mut mapping = [0u8; 6];
        let family = i32::from(channels == 6);
        // SAFETY: valid 48 kHz/channel count, mapping storage for all channels and valid out-pointers.
        let handle = unsafe { (api.create)(48000, channels as i32, family, &mut streams, &mut coupled, mapping.as_mut_ptr(), 2049, &mut code) };
        if handle.is_null() {
            return Err(err(format!("create status {code}")));
        }
        let mut enc = Self { api, handle, config: OpusConfig::default(), channels: channels as usize, pending: Vec::new(), ended: false };
        let mut delay = 0i32;
        // SAFETY: these public ctl requests respectively take an int value, an int value and an
        // int out-pointer. The encoder is exclusively owned and the out-pointer remains live.
        unsafe {
            for status in [
                (enc.api.ctl)(handle, 4002, (s.audio.bitrate_kbps * 1000) as i32),
                (enc.api.ctl)(handle, 4006, 1i32),
                (enc.api.ctl)(handle, 4027, &mut delay as *mut i32),
            ] {
                if status != 0 {
                    return Err(err(format!("configure status {status}")));
                }
            }
        }
        let channel_mapping = if family == 0 {
            Vec::new()
        } else {
            let mut m = vec![streams as u8, coupled as u8];
            m.extend_from_slice(&mapping[..channels as usize]);
            m
        };
        enc.config = OpusConfig {
            output_channels: channels as u8,
            pre_skip: u16::try_from(delay).map_err(err)?,
            input_sample_rate: 48000,
            channel_mapping_family: family as u8,
            channel_mapping,
            ..Default::default()
        };
        Ok(enc)
    }

    fn packets(&mut self) -> Result<Vec<Vec<u8>>> {
        let size = self.channels * 960;
        let n = self.pending.len() / size;
        let mut out = Vec::with_capacity(n);
        for input in self.pending[..n * size].chunks_exact(size) {
            let mut packet = vec![0u8; 8192];
            // SAFETY: a live exclusively owned encoder, 960 interleaved samples per channel and
            // writable output of the advertised size. libopus consumes input before returning.
            let len = unsafe { (self.api.encode)(self.handle, input.as_ptr(), 960, packet.as_mut_ptr(), packet.len() as i32) };
            if len < 0 {
                return Err(err(format!("encode status {len}")));
            }
            packet.truncate(len as usize);
            out.push(packet);
        }
        self.pending.drain(..n * size);
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: destroy the exclusively owned handle once, before unloading its library.
        unsafe {
            (self.api.destroy)(self.handle);
        }
    }
}

impl AudioEncoder for Encoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::audio(FourCc(*b"Opus"), CodecConfig::Opus(self.config.clone()), self.channels as u32, 48000.0, 16)
    }
    fn priming(&self) -> u32 {
        u32::from(self.config.pre_skip)
    }
    fn frame_size(&self) -> u32 {
        960
    }
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        let n = planar.first().map_or(0, Vec::len);
        if self.ended || planar.len() != self.channels || n > 48000 * 10 || planar.iter().any(|p| p.len() != n) {
            return Err(err("invalid planar audio or ended encoder"));
        }
        // FilmCraft 5.1 L,R,C,LFE,Ls,Rs -> RFC 7845 family-1 L,C,R,Ls,Rs,LFE.
        let order: &[usize] = if self.channels == 6 {
            &[0, 2, 1, 4, 5, 3]
        } else if self.channels == 2 {
            &[0, 1]
        } else {
            &[0]
        };
        for i in 0..n {
            for &c in order {
                let v = planar[c][i];
                self.pending.push(if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 });
            }
        }
        self.packets()
    }
    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        if self.ended {
            return Ok(Vec::new());
        }
        self.ended = true;
        let needed = self.pending.len() + self.priming() as usize * self.channels;
        self.pending.resize(needed.div_ceil(960 * self.channels) * 960 * self.channels, 0.0);
        self.packets()
    }
}

pub fn factory(format: Format, rate: u32, channels: u32, s: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>> {
    if format != Format::Opus && s.audio_codec() != AudioCodec::Opus {
        return None;
    }
    Some(if rate != 48000 { Err(err("Opus export uses 48000 Hz")) } else { Encoder::new(channels, s).map(|e| Box::new(e) as Box<dyn AudioEncoder>) })
}

//! NVIDIA NVENC hardware encoding: H.264 (Windows and 64-bit Linux) and H.265 (HEVC) Main / Main 10
//! (Windows).
//!
//! The encoder runs on the GPU's NVENC engine through the driver's `nvEncodeAPI64.dll` on Windows
//! (on a Direct3D 11 device) or `libnvidia-encode.so.1` on Linux (on the CUDA driver's primary
//! context, `libcuda.so.1`) (API 12.1, [`ffi`]; no CUDA toolkit, no SDK to install): pictures go in as NV12 input buffers, the Annex B output
//! comes back as length-prefixed samples with the parameter sets split out for the `avcC` (H.264)
//! or the `hvcC` (HEVC, see [`hevc`]). One session, ring and NV12 path serves both codecs; the
//! codec only chooses the GUIDs, the codec configuration and how NAL units are told apart.
//! [`export`] plugs it into Export: as an alternative to the software encoder for H.264, and as the
//! only encoder of the H.265 format (which has no software encoder).
//!
//! ```text
//! RGBA (the export pipeline) ──► ABGR input buffer ──NVENC (BT.709 limited 4:2:0 on the GPU)──►
//!    Annex B ──► length-prefixed samples + avcC / hvcC
//!
//! Windows, frames the export's GPU compositor draws as they are encoded (`target`):
//! compositor (Vulkan) ──copy──► shared RGBA8 texture (NVENC's D3D11 device) ──mapped──► NVENC ──► …
//!    (no read-back into CPU memory and no upload; the same stream)
//!
//! where the driver refuses RGB input (`Nvenc::with_rgba_input` fails), as before:
//! RGBA ──► BT.709 limited 4:2:0 (the software encoder's conversion) ──► NV12 input buffer ──NVENC──► …
//!
//! HDR (PQ / HLG) HEVC Main 10:
//! encoded BT.2020 R'G'B' floats ──► 10-bit limited 4:2:0 (`filmcraft_export::rgbf_to_yuv420_10`)
//!    ──► P010 input buffer ──NVENC──► same path; VUI BT.2020 + transfer, HDR10 SEI on every IDR (PQ)
//! ```
//!
//! `unsafe` is confined to `ffi` (data), `device` (the device's lifetime) and `session` (every driver
//! call); this module is safe
//! code. Hardware H.264 encoding never replaces an export that works in software: the factory
//! declines what NVENC cannot do (no NVIDIA GPU or driver, sizes, HDR, two-pass, MXF...) and the
//! software encoder takes over. A declined HEVC export is an error that says why.

#[allow(unsafe_code)]
pub(crate) mod device;
pub mod export;
#[allow(unsafe_code)]
mod ffi;
pub mod hevc;
#[allow(unsafe_code)]
mod session;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub(crate) mod target;

#[cfg(test)]
mod abi_tests;
#[cfg(all(test, target_os = "windows"))]
mod gpu_input_tests;

use std::collections::VecDeque;
#[cfg(target_os = "windows")]
use std::sync::Arc;

use filmcraft_isobmff::HevcConfig;

pub use self::session::{Caps, Codec, Params, Sei, Signal};
use self::session::{Locked, Session, Submitted};

/// H.264 profile (`profile_idc` 66 / 77 / 100), or HEVC Main (8-bit 4:2:0) / Main 10 (10-bit 4:2:0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Baseline,
    Main,
    High,
    /// H.265 Main: chooses the HEVC codec (like `VtProfile::HevcMain` for VideoToolbox).
    HevcMain,
    /// H.265 Main 10: 10-bit 4:2:0 (P010 input), for HDR (PQ / HLG) and 10-bit SDR.
    HevcMain10,
}

impl Profile {
    /// The codec this profile belongs to.
    pub fn codec(self) -> Codec {
        match self {
            Profile::HevcMain | Profile::HevcMain10 => Codec::Hevc,
            _ => Codec::H264,
        }
    }
}

/// What to encode.
#[derive(Clone, Debug)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    /// Frame rate as numerator / denominator.
    pub fps: (u32, u32),
    pub bitrate_kbps: u32,
    pub max_bitrate_kbps: u32,
    pub cbr: bool,
    /// Frames between IDR pictures.
    pub keyint: u32,
    pub profile: Profile,
    /// Level × 10 (41 = 4.1), for either codec; `None` lets the encoder pick.
    pub level: Option<u8>,
    /// Sample aspect ratio.
    pub sar: Option<(u32, u32)>,
    /// Allow one B-frame between references when the encoder and the profile support it.
    pub bframes: bool,
}

/// One encoded picture in decoding order.
pub struct Packet {
    /// Length-prefixed (4 bytes) NAL units, without parameter sets or access unit delimiters.
    pub data: Vec<u8>,
    pub key: bool,
    /// Presentation / decoding time in frames.
    pub pts: i64,
    pub dts: i64,
}

/// Pictures in flight: input / output buffer pairs.
const RING: usize = 8;

/// An NVENC encoder (H.264, or HEVC when the profile is [`Profile::HevcMain`] or [`Profile::HevcMain10`]).
pub struct Nvenc {
    session: Session,
    codec: Codec,
    /// Empty for H.264.
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    /// The `hvcC` record (HEVC only), built from the parameter sets the encoder wrote.
    hvcc: Option<HevcConfig>,
    /// Frames the output is delayed by reordering (0 or 1).
    delay: u32,
    free: Vec<usize>,
    /// Slots submitted and not read yet, in submission order, and how many of them are ready.
    pending: VecDeque<usize>,
    ready: usize,
    emitted: i64,
    size: (u32, u32),
    /// Main 10: pictures go in as 10-bit P010 ([`Nvenc::encode_10`]), otherwise as 8-bit NV12 ([`Nvenc::encode`]).
    ten_bit: bool,
    /// Pictures go in as packed RGBA ([`Nvenc::encode_rgba`]) and the GPU converts them to 4:2:0.
    rgba_input: bool,
    /// Pictures submitted, whatever their input.
    submissions: u64,
    /// The input pictures in GPU memory ([`Nvenc::make_targets`]); its picture `i` is the
    /// session's input key `i`.
    #[cfg(target_os = "windows")]
    targets: Option<Arc<target::Pool>>,
    /// Pictures in GPU memory submitted and still held: (submission number, key, the picture,
    /// which goes back to its pool when released), in submission order.
    #[cfg(target_os = "windows")]
    gpu_inputs: VecDeque<(u64, usize, Arc<dyn filmcraft_frame::GpuTarget>)>,
}

/// The H.264 encoder (the name it had before HEVC joined it).
pub type NvencH264 = Nvenc;

/// Whether this system has an NVIDIA GPU with a driver that has NVENC.
pub fn available() -> bool {
    Session::open().is_ok()
}

/// Whether this system's NVENC can encode HEVC: a session opened and an HEVC encoder created for a
/// small picture, once (the answer is kept). What makes the H.265 export format available.
pub fn hevc_available() -> bool {
    *HEVC_AVAILABLE.get_or_init(|| {
        let probe = Config {
            width: 640,
            height: 360,
            fps: (30, 1),
            bitrate_kbps: 2000,
            max_bitrate_kbps: 3000,
            cbr: false,
            keyint: 30,
            profile: Profile::HevcMain,
            level: None,
            sar: None,
            bframes: false,
        };
        let result = Nvenc::new(&probe);
        if let Err(why) = &result {
            log::info!("no hardware HEVC encoder: {why}");
        }
        result.is_ok()
    })
}

/// The answer of [`hevc_available`], once asked.
static HEVC_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Ask [`hevc_available`] on a thread of its own (once, however often this is called), so the
/// first draw of Export's format list does not wait for the probe session. Whoever asks while it
/// runs waits for the same answer; if the thread cannot start, or the probe panics (caught), the
/// first caller asks, as before.
pub fn warm_hevc_probe() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("nvenc-hevc-probe".into()).spawn(|| {
            if std::panic::catch_unwind(hevc_available).is_err() {
                log::warn!("the hardware HEVC probe panicked; the format list will ask again");
            }
            if std::panic::catch_unwind(hevc_hdr_available).is_err() {
                log::warn!("the hardware HEVC Main 10 probe panicked; the export will ask again");
            }
        });
        if let Err(e) = spawned {
            log::info!("hardware HEVC probe thread not started: {e}");
        }
    });
}

/// Whether this system's NVENC can encode HEVC Main 10 (10-bit): HEVC works and a Main 10 session
/// opened for a small picture, once (the answer is kept). What makes HDR H.265 exports possible:
/// `filmcraft_export::hdr_available(Format::Hevc)`.
pub fn hevc_hdr_available() -> bool {
    *HEVC_HDR_AVAILABLE.get_or_init(|| {
        if !hevc_available() {
            return false;
        }
        let probe = Config {
            width: 640,
            height: 360,
            fps: (30, 1),
            bitrate_kbps: 2000,
            max_bitrate_kbps: 3000,
            cbr: false,
            keyint: 30,
            profile: Profile::HevcMain10,
            level: None,
            sar: None,
            bframes: false,
        };
        let result = Nvenc::with_signal(&probe, &Signal::default());
        if let Err(why) = &result {
            log::info!("no hardware HEVC Main 10 encoder: {why}");
        }
        result.is_ok()
    })
}

/// The answer of [`hevc_hdr_available`], once asked.
static HEVC_HDR_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether [`hevc_hdr_available`] has its answer yet (diagnostics and tests).
pub fn hevc_hdr_probed() -> bool {
    HEVC_HDR_AVAILABLE.get().is_some()
}

/// Whether [`hevc_available`] has its answer yet (diagnostics and tests).
pub fn hevc_probed() -> bool {
    HEVC_AVAILABLE.get().is_some()
}

impl Nvenc {
    /// Open an encoder with the default BT.709 signal, or say why NVENC does not take this configuration.
    pub fn new(cfg: &Config) -> Result<Self, String> {
        Self::with_signal(cfg, &Signal::default())
    }

    /// Open an encoder whose HEVC stream carries `signal` (colour description in the VUI, SEI messages
    /// on every IDR picture), or say why NVENC does not take this configuration. H.264 takes the
    /// default signal only.
    pub fn with_signal(cfg: &Config, signal: &Signal) -> Result<Self, String> {
        Self::open(cfg, signal, false)
    }

    /// Open an 8-bit encoder that takes straight RGBA8 pictures ([`Nvenc::encode_rgba`]) and converts
    /// them to 4:2:0 on the GPU, or say why not (Main 10, or a driver that refuses RGB input).
    pub fn with_rgba_input(cfg: &Config, signal: &Signal) -> Result<Self, String> {
        if cfg.profile == Profile::HevcMain10 {
            return Err("RGBA input is 8-bit only".into());
        }
        Self::open(cfg, signal, true)
    }

    fn open(cfg: &Config, signal: &Signal, rgba_input: bool) -> Result<Self, String> {
        let (w, h) = (cfg.width, cfg.height);
        if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
            return Err(format!("{w}x{h}: NVENC needs even dimensions"));
        }
        if cfg.fps.0 == 0 || cfg.fps.1 == 0 {
            return Err("frame rate".into());
        }
        let codec = cfg.profile.codec();
        if codec == Codec::H264 && *signal != Signal::default() {
            return Err("H.264 here is BT.709 without SEI messages".into());
        }
        let level = match (codec, cfg.level) {
            (Codec::Hevc, Some(l)) => Some(hevc::level_code(l).ok_or_else(|| format!("{}.{} is not an HEVC level", l / 10, l % 10))?),
            // left to itself NVENC answers a bitrate above its chosen level's Main tier limit with the
            // High tier, which many hardware decoders refuse: pick the lowest Main tier level instead
            (Codec::Hevc, None) => {
                let max_kbps = if cfg.cbr { cfg.bitrate_kbps } else { cfg.max_bitrate_kbps.max(cfg.bitrate_kbps) };
                hevc::main_tier_level(w, h, cfg.fps, max_kbps).and_then(hevc::level_code)
            }
            (_, l) => l,
        };
        let mut session = Session::open()?;
        let caps = session.caps(codec)?;
        check_caps(&caps, cfg)?;
        if w < caps.min_size.0 || h < caps.min_size.1 || w > caps.max_size.0 || h > caps.max_size.1 {
            return Err(format!("{w}x{h} is outside NVENC's {}x{} - {}x{}", caps.min_size.0, caps.min_size.1, caps.max_size.0, caps.max_size.1));
        }
        // NVENC wants a GOP longer than the B-frame pattern: HEVC with a keyframe every one or two
        // pictures is simply written without B-frames (H.264 keeps declining those, to the software encoder)
        let room_for_bframes = codec == Codec::H264 || cfg.keyint > 2;
        let bframes = u32::from(cfg.bframes && cfg.profile != Profile::Baseline && caps.max_bframes >= 1 && room_for_bframes);
        let ten_bit = cfg.profile == Profile::HevcMain10;
        let params = Params {
            codec,
            width: w,
            height: h,
            fps: cfg.fps,
            bitrate: cfg.bitrate_kbps.max(1),
            max_bitrate: cfg.max_bitrate_kbps,
            cbr: cfg.cbr,
            gop: cfg.keyint.max(1),
            profile: match cfg.profile {
                Profile::Baseline => 0,
                Profile::Main | Profile::HevcMain | Profile::HevcMain10 => 1,
                Profile::High => 2,
            },
            ten_bit,
            rgba_input,
            signal: signal.clone(),
            level,
            sar: cfg.sar,
            bframes,
        };
        session.initialize(&params, RING)?;
        let params = session.sequence_params()?;
        let (vps, sps, pps, hvcc) = match codec {
            Codec::H264 => {
                let (sps, pps) = split_parameter_sets(&params)?;
                (Vec::new(), sps, pps, None)
            }
            Codec::Hevc => {
                let (vps, sps, pps) = split_hevc_parameter_sets(&params)?;
                // validated now: a stream whose parameter sets we cannot describe is a declined export
                let record = hevc::hevc_config(&vps, &sps, &pps, (w, h), if ten_bit { 10 } else { 8 })?;
                (vps, sps, pps, Some(record))
            }
        };
        let slots = session.slots();
        Ok(Self {
            session,
            codec,
            vps,
            sps,
            pps,
            hvcc,
            delay: bframes,
            free: (0..slots).rev().collect(),
            pending: VecDeque::new(),
            ready: 0,
            emitted: 0,
            size: (w, h),
            ten_bit,
            rgba_input,
            submissions: 0,
            #[cfg(target_os = "windows")]
            targets: None,
            #[cfg(target_os = "windows")]
            gpu_inputs: VecDeque::new(),
        })
    }

    /// The codec this encoder writes.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// The sequence and picture parameter sets (NAL units without start codes).
    pub fn parameter_sets(&self) -> (&[u8], &[u8]) {
        (&self.sps, &self.pps)
    }

    /// The video parameter set (HEVC; empty for H.264).
    pub fn vps(&self) -> &[u8] {
        &self.vps
    }

    /// The `hvcC` record of an HEVC stream (`None` for H.264).
    pub fn hevc_config(&self) -> Option<&HevcConfig> {
        self.hvcc.as_ref()
    }

    /// Frames the decoding time runs behind the presentation time (B-frame reordering).
    pub fn delay(&self) -> u32 {
        self.delay
    }

    /// Whether this encoder takes 10-bit pictures ([`Nvenc::encode_10`]) instead of 8-bit ones ([`Nvenc::encode`]).
    pub fn is_ten_bit(&self) -> bool {
        self.ten_bit
    }

    /// Whether this encoder takes packed RGBA pictures ([`Nvenc::encode_rgba`]) instead of planar ones.
    pub fn takes_rgba(&self) -> bool {
        self.rgba_input
    }

    /// Encode picture `index` from straight RGBA8 (`w * 4` bytes per row, alpha ignored); NVENC
    /// converts it to 4:2:0 with the stream's matrix. The pictures that came out, as for
    /// [`Nvenc::encode`]. An error for an encoder opened without [`Nvenc::with_rgba_input`].
    pub fn encode_rgba(&mut self, rgba: &[u8], index: u64) -> Result<Vec<Packet>, String> {
        if !self.rgba_input {
            return Err("an RGBA picture for a planar-input encoder".into());
        }
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        if rgba.len() < w.saturating_mul(h).saturating_mul(4) {
            return Err("the picture is smaller than the encoder's size".into());
        }
        self.submit_picture(index, |l| fill_rgba(l, rgba, w, h))
    }

    /// Encode picture `index` from planar 8-bit 4:2:0 (`u`, `v` at half size, rows `w` and `w / 2` bytes).
    /// Returns the pictures that came out (usually the oldest; none while the ring fills). An error
    /// for a Main 10 encoder.
    pub fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], index: u64) -> Result<Vec<Packet>, String> {
        if self.ten_bit {
            return Err("an 8-bit picture for a Main 10 encoder".into());
        }
        if self.rgba_input {
            return Err("a planar picture for an RGBA-input encoder".into());
        }
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let (luma, chroma) = (w.saturating_mul(h), (w / 2).saturating_mul(h / 2));
        if y.len() < luma || u.len() < chroma || v.len() < chroma {
            return Err("the picture is smaller than the encoder's size".into());
        }
        self.submit_picture(index, |l| fill_nv12(l, y, u, v, w, h))
    }

    /// Encode picture `index` from planar 10-bit 4:2:0 (code values 0..=1023 in `u16`, `u`, `v` at
    /// half size). The pictures that came out, as for [`Nvenc::encode`]. An error for a Main encoder.
    pub fn encode_10(&mut self, y: &[u16], u: &[u16], v: &[u16], index: u64) -> Result<Vec<Packet>, String> {
        if !self.ten_bit {
            return Err("a 10-bit picture for a Main (8-bit) encoder".into());
        }
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let (luma, chroma) = (w.saturating_mul(h), (w / 2).saturating_mul(h / 2));
        if y.len() < luma || u.len() < chroma || v.len() < chroma {
            return Err("the picture is smaller than the encoder's size".into());
        }
        self.submit_picture(index, |l| fill_p010(l, y, u, v, w, h))
    }

    fn submit_picture(&mut self, index: u64, fill: impl FnOnce(Locked<'_>)) -> Result<Vec<Packet>, String> {
        let mut out = Vec::new();
        let slot = self.next_slot(&mut out)?;
        match self.session.submit(slot, index, fill) {
            Ok(st) => {
                self.submitted_to(slot, st);
                Ok(out)
            }
            Err(e) => {
                self.free.push(slot);
                Err(e)
            }
        }
    }

    /// A free buffer slot, reading the oldest picture out (into `out`) when the ring is full.
    fn next_slot(&mut self, out: &mut Vec<Packet>) -> Result<usize, String> {
        if let Some(s) = self.free.pop() {
            return Ok(s);
        }
        // the ring is full: the oldest picture must come out first
        if self.ready == 0 {
            return Err("the encoder holds more pictures than it can".into());
        }
        out.push(self.read_oldest()?);
        self.free.pop().ok_or_else(|| "no free encoder buffer".to_string())
    }

    /// A picture went in through slot `slot`.
    fn submitted_to(&mut self, slot: usize, st: Submitted) {
        self.pending.push_back(slot);
        self.submissions = self.submissions.saturating_add(1);
        if st == Submitted::Ready {
            self.ready = self.pending.len();
        }
    }

    /// Make the encoder's input pictures in GPU memory: up to `count` textures at its size on its
    /// device (fewer when video memory runs out), each registered as an input picture, for the
    /// export's compositor to draw finished frames into ([`target::take`], then
    /// [`Nvenc::encode_target`]). The same pool when asked again. An error for an encoder without
    /// RGBA input, or when the driver takes no such picture (the frames then come as RGBA bytes).
    #[cfg(target_os = "windows")]
    pub(crate) fn make_targets(&mut self, count: usize) -> Result<Arc<target::Pool>, String> {
        if !self.rgba_input {
            return Err("input pictures in GPU memory are RGBA (ABGR) pictures".into());
        }
        if let Some(pool) = &self.targets {
            return Ok(pool.clone());
        }
        let pool = target::Pool::new(self.session.d3d11(), self.size, count)?;
        for i in 0..pool.len() {
            let registered = pool.texture(i).ok_or_else(|| "no such input picture".to_string()).and_then(|t| self.session.register_texture(t));
            match registered {
                Ok(key) if key == i => {}
                Ok(_) => {
                    self.session.unregister_inputs();
                    return Err("input pictures registered out of order".into());
                }
                Err(e) => {
                    // none of them is used: the textures go with `pool`, after their registrations
                    self.session.unregister_inputs();
                    return Err(e);
                }
            }
        }
        let pool = Arc::new(pool);
        self.targets = Some(pool.clone());
        Ok(pool)
    }

    /// Encode picture `index` from `picture`, one of this encoder's input pictures in GPU memory
    /// ([`Nvenc::make_targets`]) holding the finished frame. The encoder keeps it (nothing may
    /// draw into it) until the picture is coded: it goes back to its pool once the bitstreams that
    /// could still read it are out. The pictures that came out, as for [`Nvenc::encode`].
    #[cfg(target_os = "windows")]
    pub fn encode_target(&mut self, picture: Arc<dyn filmcraft_frame::GpuTarget>, index: u64) -> Result<Vec<Packet>, String> {
        let pool = self.targets.as_ref().ok_or("this encoder has no input pictures in GPU memory")?;
        let key = picture
            .as_any()
            .downcast_ref::<target::Target>()
            .filter(|t| t.pool_id() == pool.id())
            .map(target::Target::index)
            .ok_or("an input picture of another encoder")?;
        let mut out = Vec::new();
        let slot = self.next_slot(&mut out)?;
        match self.session.submit_mapped(slot, index, key) {
            Ok(st) => {
                self.submitted_to(slot, st);
                self.gpu_inputs.push_back((self.submissions - 1, key, picture));
                Ok(out)
            }
            Err(e) => {
                self.free.push(slot);
                Err(e)
            }
        }
    }

    /// Unmap the input pictures in GPU memory the encoder is done with and give them back to their
    /// pool: all of them (`all`, at the end of the stream), or those every picture coded so far
    /// has consumed. Bitstreams come out in coding order; with `delay` B-frames a picture is coded
    /// at most `delay` places after its submission, so once `n` bitstreams are out every picture
    /// submitted before `n - delay` has been read by the encoder.
    #[cfg(target_os = "windows")]
    fn release_inputs(&mut self, all: bool) -> Result<(), String> {
        let out = u64::try_from(self.emitted).unwrap_or(0);
        while let Some(&(n, _, _)) = self.gpu_inputs.front() {
            if !all && n.saturating_add(u64::from(self.delay)) >= out {
                break;
            }
            let Some((_, key, picture)) = self.gpu_inputs.pop_front() else { break };
            // unmapped before it can be drawn into again
            let unmapped = self.session.unmap(key);
            drop(picture);
            unmapped?;
        }
        Ok(())
    }

    /// Finish the stream: every picture still inside comes out.
    pub fn flush(&mut self) -> Result<Vec<Packet>, String> {
        if !self.pending.is_empty() {
            self.session.end_of_stream()?;
            self.ready = self.pending.len();
        }
        let mut out = Vec::new();
        while self.ready > 0 {
            out.push(self.read_oldest()?);
        }
        #[cfg(target_os = "windows")]
        self.release_inputs(true)?;
        Ok(out)
    }

    fn read_oldest(&mut self) -> Result<Packet, String> {
        let slot = self.pending.pop_front().ok_or("no picture in flight")?;
        self.ready = self.ready.saturating_sub(1);
        let r = self.session.read(slot);
        self.free.push(slot);
        let o = r?;
        let data = annex_b_to_length_prefixed_for(self.codec, &o.data)?;
        let k = self.emitted;
        self.emitted = self.emitted.saturating_add(1);
        #[cfg(target_os = "windows")]
        self.release_inputs(false)?;
        let pts = i64::try_from(o.pts).unwrap_or(i64::MAX);
        Ok(Packet { data, key: o.pic_type == ffi::NV_ENC_PIC_TYPE_IDR, pts, dts: k.saturating_sub(i64::from(self.delay)) })
    }
}

/// Copy planar 4:2:0 into an NV12 input buffer.
fn fill_nv12(l: Locked<'_>, y: &[u8], u: &[u8], v: &[u8], w: usize, h: usize) {
    let pitch = l.pitch;
    let (luma, chroma) = l.data.split_at_mut((pitch * h).min(l.data.len()));
    if w == 0 || pitch == 0 {
        return;
    }
    for (row, dst) in y.chunks_exact(w).zip(luma.chunks_exact_mut(pitch)).take(h) {
        // `submit` rejects a pitch narrower than a row; a short row is skipped, never overrun
        if let Some(d) = dst.get_mut(..w) {
            d.copy_from_slice(row);
        }
    }
    let cw = w / 2;
    if cw == 0 {
        return;
    }
    for ((ur, vr), dst) in u.chunks_exact(cw).zip(v.chunks_exact(cw)).zip(chroma.chunks_exact_mut(pitch)).take(h / 2) {
        let Some(dst) = dst.get_mut(..w) else { continue };
        for (([du, dv], a), b) in dst.as_chunks_mut::<2>().0.iter_mut().zip(ur).zip(vr) {
            *du = *a;
            *dv = *b;
        }
    }
}

/// Copy straight RGBA8 rows into an ABGR input buffer (the same byte order: R, G, B, A). A row the
/// pitch cannot hold is skipped, never overrun.
fn fill_rgba(l: Locked<'_>, rgba: &[u8], w: usize, h: usize) {
    let row = w.saturating_mul(4);
    if row == 0 || l.pitch == 0 {
        return;
    }
    for (src, dst) in rgba.chunks_exact(row).zip(l.data.chunks_mut(l.pitch)).take(h) {
        if let Some(d) = dst.get_mut(..row) {
            d.copy_from_slice(src);
        }
    }
}

/// Copy planar 10-bit 4:2:0 into a P010 input buffer: each sample is a little-endian `u16` with the
/// 10-bit code in its high bits (`code << 6`), the chroma plane interleaved (Cb, Cr) after the luma rows,
/// `pitch` in bytes. Codes above 10 bits are masked; a row the pitch cannot hold is skipped, never overrun.
fn fill_p010(l: Locked<'_>, y: &[u16], u: &[u16], v: &[u16], w: usize, h: usize) {
    let pitch = l.pitch;
    let (luma, chroma) = l.data.split_at_mut(pitch.saturating_mul(h).min(l.data.len()));
    if w == 0 || pitch == 0 {
        return;
    }
    let code = |c: u16| ((c & 0x3ff) << 6).to_le_bytes();
    for (row, dst) in y.chunks_exact(w).zip(luma.chunks_exact_mut(pitch)).take(h) {
        let Some(dst) = dst.get_mut(..w.saturating_mul(2)) else { continue };
        for (d, c) in dst.as_chunks_mut::<2>().0.iter_mut().zip(row) {
            *d = code(*c);
        }
    }
    let cw = w / 2;
    if cw == 0 {
        return;
    }
    for ((ur, vr), dst) in u.chunks_exact(cw).zip(v.chunks_exact(cw)).zip(chroma.chunks_exact_mut(pitch)).take(h / 2) {
        let Some(dst) = dst.get_mut(..w.saturating_mul(2)) else { continue };
        for ((d, a), b) in dst.as_chunks_mut::<4>().0.iter_mut().zip(ur).zip(vr) {
            let ([u0, u1], [v0, v1]) = (code(*a), code(*b));
            *d = [u0, u1, v0, v1];
        }
    }
}

/// Why this GPU's encoder cannot take `cfg` (beyond its size limits), or `Ok`: Main 10 needs 10-bit support.
fn check_caps(caps: &Caps, cfg: &Config) -> Result<(), String> {
    if cfg.profile == Profile::HevcMain10 && !caps.ten_bit {
        return Err("this GPU's encoder has no 10-bit (HEVC Main 10) support".into());
    }
    Ok(())
}

/// The NAL units of an Annex B byte stream (start codes `00 00 01` / `00 00 00 01`).
pub fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while let Some(w) = data.get(i..i.saturating_add(3)) {
        if w == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |n| n.saturating_sub(3));
        // trailing zero bytes belong to the next start code (4-byte form) or are padding
        while e > s && data.get(e - 1) == Some(&0) {
            e -= 1;
        }
        if let Some(n) = data.get(s..e).filter(|n| !n.is_empty()) {
            out.push(n);
        }
    }
    out
}

/// Annex B to 4-byte length-prefixed NAL units, dropping parameter sets (they live in the `avcC`)
/// and access unit delimiters. Errors on a stream without NAL units.
pub fn annex_b_to_length_prefixed(data: &[u8]) -> Result<Vec<u8>, String> {
    annex_b_to_length_prefixed_for(Codec::H264, data)
}

/// The NAL unit type of a NAL unit (`None` when it is too short to have a header).
pub fn nal_type(codec: Codec, nal: &[u8]) -> Option<u8> {
    match codec {
        Codec::H264 => nal.first().map(|b| b & 0x1f),
        // two header bytes: forbidden_zero_bit, nal_unit_type (6), layer id (6), temporal id plus 1 (3)
        Codec::Hevc => match nal {
            [b, _, ..] => Some((b >> 1) & 0x3f),
            _ => None,
        },
    }
}

/// [`annex_b_to_length_prefixed`] for a codec: H.264 drops SPS / PPS / AUD (types 7 to 9); HEVC drops
/// VPS / SPS / PPS / AUD (32 to 35) and end-of-sequence / end-of-bitstream markers (36, 37), since
/// the file is one coded video sequence. A NAL unit too short for its header is dropped.
pub fn annex_b_to_length_prefixed_for(codec: Codec, data: &[u8]) -> Result<Vec<u8>, String> {
    let nals = annex_b_nals(data);
    if nals.is_empty() {
        return Err("the encoder produced no NAL units".into());
    }
    let mut out = Vec::with_capacity(data.len());
    for n in nals {
        let dropped = match (codec, nal_type(codec, n)) {
            (_, None) => true,
            (Codec::H264, Some(t)) => matches!(t, 7..=9),
            (Codec::Hevc, Some(t)) => matches!(t, 32..=37),
        };
        if dropped {
            continue;
        }
        let len = u32::try_from(n.len()).map_err(|_| "a NAL unit longer than 4 GiB".to_string())?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(n);
    }
    if out.is_empty() {
        return Err("the encoder produced no picture data".into());
    }
    Ok(out)
}

/// The SPS and PPS NAL units of an Annex B byte string.
pub fn split_parameter_sets(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let nals = annex_b_nals(data);
    let find = |t: u8| nals.iter().find(|n| n.first().is_some_and(|b| b & 0x1f == t)).map(|n| n.to_vec());
    match (find(7), find(8)) {
        (Some(s), Some(p)) => Ok((s, p)),
        _ => Err("the encoder returned no SPS / PPS".into()),
    }
}

/// The VPS, SPS and PPS NAL units of an Annex B byte string from an HEVC encoder.
pub fn split_hevc_parameter_sets(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
    let nals = annex_b_nals(data);
    let find = |t: u8| nals.iter().find(|n| nal_type(Codec::Hevc, n) == Some(t)).map(|n| n.to_vec());
    match (find(32), find(33), find(34)) {
        (Some(v), Some(s), Some(p)) => Ok((v, s, p)),
        _ => Err("the encoder returned no VPS / SPS / PPS".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_annex_b_with_both_start_code_forms() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9, 0];
        let n = annex_b_nals(&s);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]);
        assert_eq!(annex_b_to_length_prefixed(&s).unwrap(), [0, 0, 0, 3, 0x65, 9, 9]);
        assert_eq!(split_parameter_sets(&s).unwrap(), (vec![0x67, 1, 2], vec![0x68, 3]));
    }

    #[test]
    fn hostile_streams_are_errors() {
        for s in [&[][..], &[0, 0, 1], &[1, 2, 3], &[0, 0, 1, 0x09, 0xf0], &[0, 0, 0, 1, 0x67, 1]] {
            assert!(annex_b_to_length_prefixed(s).is_err(), "{s:?}");
        }
        assert!(split_parameter_sets(&[0, 0, 1, 0x65, 1]).is_err());
    }

    #[test]
    fn nv12_is_interleaved_chroma_after_luma_rows() {
        let (w, h, pitch) = (4usize, 2usize, 8usize);
        let y: Vec<u8> = (0..8).collect();
        let (u, v) = (vec![100, 101], vec![200, 201]);
        let mut buf = vec![0u8; pitch * h * 3 / 2];
        fill_nv12(Locked { data: &mut buf, pitch }, &y, &u, &v, w, h);
        assert_eq!(&buf[..4], &[0, 1, 2, 3]);
        assert_eq!(&buf[pitch..pitch + 4], &[4, 5, 6, 7]);
        assert_eq!(&buf[pitch * h..pitch * h + 4], &[100, 200, 101, 201]);
    }

    #[test]
    fn nv12_never_writes_past_a_narrow_pitch() {
        // a driver pitch narrower than the row (rejected by `submit`) must not panic here either
        let (w, h, pitch) = (8usize, 4usize, 4usize);
        let y = vec![7u8; w * h];
        let (u, v) = (vec![1u8; w * h / 4], vec![2u8; w * h / 4]);
        let mut buf = vec![0u8; pitch * h * 3 / 2];
        fill_nv12(Locked { data: &mut buf, pitch }, &y, &u, &v, w, h);
        // and empty or zero-sized inputs
        fill_nv12(Locked { data: &mut [], pitch: 0 }, &[], &[], &[], 0, 0);
        fill_nv12(Locked { data: &mut buf, pitch }, &[1], &[], &[], 1, 1);
    }

    #[test]
    fn p010_is_shifted_little_endian_with_interleaved_chroma_in_bytes() {
        let (w, h, pitch) = (4usize, 2usize, 12usize); // 2 bytes per sample + 4 bytes of padding
        let y: Vec<u16> = vec![0, 1, 64, 1023, 940, 512, 4, 1019];
        let (u, v) = (vec![512, 960], vec![64, 1023]);
        let mut buf = vec![0xAAu8; pitch * h * 3 / 2];
        fill_p010(Locked { data: &mut buf, pitch }, &y, &u, &v, w, h);
        let sample = |at: usize| u16::from_le_bytes([buf[at], buf[at + 1]]);
        assert_eq!((0..4).map(|x| sample(x * 2)).collect::<Vec<_>>(), vec![0, 1 << 6, 64 << 6, 1023 << 6]);
        assert_eq!((0..4).map(|x| sample(pitch + x * 2)).collect::<Vec<_>>(), vec![940 << 6, 512 << 6, 4 << 6, 1019 << 6]);
        // chroma after pitch * h bytes: Cb Cr Cb Cr
        let c = pitch * h;
        assert_eq!((0..4).map(|x| sample(c + x * 2)).collect::<Vec<_>>(), vec![512 << 6, 64 << 6, 960 << 6, 1023 << 6]);
        // the low 6 bits of every sample are zero and the padding is untouched
        assert!((0..4).all(|x| sample(x * 2) & 0x3f == 0));
        assert_eq!(&buf[8..12], &[0xAA; 4]);
        // a code beyond 10 bits is masked, not shifted into the next field
        let mut b2 = vec![0u8; pitch * h * 3 / 2];
        fill_p010(Locked { data: &mut b2, pitch }, &[0xFFFF; 8], &[0xFFFF; 2], &[0xFFFF; 2], w, h);
        assert_eq!(u16::from_le_bytes([b2[0], b2[1]]), 1023 << 6);
    }

    #[test]
    fn p010_never_writes_past_a_narrow_pitch_or_short_buffer() {
        // a pitch narrower than 2 bytes per pixel (rejected by `submit`) must not panic here either
        let (w, h) = (8usize, 4usize);
        for pitch in [0usize, 1, 7, 8, 15, 16] {
            let mut buf = vec![0u8; pitch * h * 3 / 2];
            let y = vec![1000u16; w * h];
            let (u, v) = (vec![500u16; w * h / 4], vec![600u16; w * h / 4]);
            fill_p010(Locked { data: &mut buf, pitch }, &y, &u, &v, w, h);
        }
        // empty and short inputs, a buffer shorter than the pitch says
        fill_p010(Locked { data: &mut [], pitch: 0 }, &[], &[], &[], 0, 0);
        fill_p010(Locked { data: &mut [0u8; 5], pitch: 64 }, &[1; 64], &[1; 16], &[1; 16], 8, 8);
        let mut buf = vec![0u8; 64];
        fill_p010(Locked { data: &mut buf, pitch: 16 }, &[1], &[], &[], 1, 1);
    }

    #[test]
    fn main_10_needs_a_gpu_with_10_bit_support() {
        let cfg = |profile| Config {
            width: 640,
            height: 360,
            fps: (24, 1),
            bitrate_kbps: 1000,
            max_bitrate_kbps: 1500,
            cbr: false,
            keyint: 24,
            profile,
            level: None,
            sar: None,
            bframes: false,
        };
        let caps = |ten_bit| Caps { ten_bit, max_bframes: 2, min_size: (130, 128), max_size: (8192, 8192) };
        assert!(check_caps(&caps(true), &cfg(Profile::HevcMain10)).is_ok());
        let e = check_caps(&caps(false), &cfg(Profile::HevcMain10)).unwrap_err();
        assert!(e.contains("10-bit"), "{e}");
        for p in [Profile::HevcMain, Profile::Main, Profile::High, Profile::Baseline] {
            assert!(check_caps(&caps(false), &cfg(p)).is_ok(), "{p:?} does not need 10-bit");
        }
    }

    #[test]
    fn empty_nal_units_are_skipped() {
        assert!(annex_b_nals(&[0, 0, 1, 0, 0, 1]).is_empty());
        assert!(annex_b_to_length_prefixed(&[0, 0, 1, 0, 0, 0, 1]).is_err());
        assert!(split_parameter_sets(&[0, 0, 1]).is_err());
    }

    // HEVC NAL unit headers: type in bits 1..=6 of the first byte (VPS 0x40, SPS 0x42, PPS 0x44, AUD 0x46,
    // EOS 0x48, EOB 0x4a, IDR_W_RADL 0x26, IDR_N_LP 0x28, TRAIL_R 0x02, prefix SEI 0x4e, suffix SEI 0x50)
    #[test]
    fn hevc_nal_types_come_from_the_second_header_bit_range() {
        assert_eq!(nal_type(Codec::Hevc, &[0x40, 0x01]), Some(32));
        assert_eq!(nal_type(Codec::Hevc, &[0x26, 0x01, 9]), Some(19));
        assert_eq!(nal_type(Codec::Hevc, &[0x28, 0x01]), Some(20));
        assert_eq!(nal_type(Codec::Hevc, &[0x4e, 0x01]), Some(39));
        assert_eq!(nal_type(Codec::Hevc, &[0x50, 0x01]), Some(40));
        // the H.264 reading of the same bytes is different (and wrong for HEVC)
        assert_eq!(nal_type(Codec::H264, &[0x28, 0x01]), Some(8));
        // no header, no type
        assert_eq!(nal_type(Codec::Hevc, &[]), None);
        assert_eq!(nal_type(Codec::Hevc, &[0x40]), None);
        assert_eq!(nal_type(Codec::H264, &[]), None);
    }

    #[test]
    fn hevc_samples_keep_slices_and_sei_and_lose_parameter_sets_and_delimiters() {
        let s = [
            &[0, 0, 0, 1, 0x46, 0x01, 0x50][..], // AUD
            &[0, 0, 0, 1, 0x40, 0x01, 1, 2],     // VPS
            &[0, 0, 0, 1, 0x42, 0x01, 3],        // SPS
            &[0, 0, 1, 0x44, 0x01, 4],           // PPS
            &[0, 0, 1, 0x4e, 0x01, 5, 5],        // prefix SEI
            &[0, 0, 0, 1, 0x28, 0x01, 6, 6, 6],  // IDR_N_LP: its header byte is 0x28, a PPS to an H.264 reader
            &[0, 0, 1, 0x26, 0x01, 7],           // IDR_W_RADL
            &[0, 0, 1, 0x02, 0x01, 8],           // TRAIL_R
            &[0, 0, 1, 0x50, 0x01, 9],           // suffix SEI
            &[0, 0, 1, 0x48, 0x01],              // end of sequence
            &[0, 0, 1, 0x4a, 0x01],              // end of bitstream
        ]
        .concat();
        let out = annex_b_to_length_prefixed_for(Codec::Hevc, &s).unwrap();
        let expected: Vec<u8> = [
            &[0, 0, 0, 4, 0x4e, 0x01, 5, 5][..],
            &[0, 0, 0, 5, 0x28, 0x01, 6, 6, 6],
            &[0, 0, 0, 3, 0x26, 0x01, 7],
            &[0, 0, 0, 3, 0x02, 0x01, 8],
            &[0, 0, 0, 3, 0x50, 0x01, 9],
        ]
        .concat();
        assert_eq!(out, expected);
        // the same stream read as H.264 loses and keeps the wrong NAL units
        assert_ne!(annex_b_to_length_prefixed(&s).unwrap(), expected);
    }

    #[test]
    fn hevc_parameter_sets_are_found_by_type() {
        let s = [0, 0, 0, 1, 0x40, 0x01, 1, 0, 0, 0, 1, 0x42, 0x01, 2, 0, 0, 1, 0x44, 0x01, 3, 0, 0, 1, 0x26, 0x01, 4];
        assert_eq!(split_hevc_parameter_sets(&s).unwrap(), (vec![0x40, 1, 1], vec![0x42, 1, 2], vec![0x44, 1, 3]));
        // an H.264 SPS / PPS pair has none of them; one missing is an error
        assert!(split_hevc_parameter_sets(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3]).is_err());
        assert!(split_hevc_parameter_sets(&[0, 0, 0, 1, 0x40, 0x01, 1, 0, 0, 1, 0x42, 0x01, 2]).is_err());
    }

    #[test]
    fn hostile_hevc_streams_are_errors() {
        // empty, no start code, only parameter sets / delimiters / markers, one-byte NAL units
        for s in [
            &[][..],
            &[0, 0, 1],
            &[1, 2, 3],
            &[0, 0, 1, 0x46, 0x01, 0x50],
            &[0, 0, 0, 1, 0x40, 0x01],
            &[0, 0, 1, 0x26],
            &[0, 0, 1, 0x48, 0x01],
            &[0, 0, 1, 0xff],
        ] {
            assert!(annex_b_to_length_prefixed_for(Codec::Hevc, s).is_err(), "{s:?}");
        }
        // truncated slices are kept as they are (the decoder reports them), never panic
        assert!(annex_b_to_length_prefixed_for(Codec::Hevc, &[0, 0, 1, 0x26, 0x01]).is_ok());
        assert!(split_hevc_parameter_sets(&[]).is_err());
        assert!(split_hevc_parameter_sets(&[0, 0, 1, 0x40]).is_err());
    }

    #[test]
    fn the_hevc_profile_picks_the_hevc_codec() {
        assert_eq!(Profile::HevcMain.codec(), Codec::Hevc);
        assert_eq!(Profile::HevcMain10.codec(), Codec::Hevc);
        for p in [Profile::Baseline, Profile::Main, Profile::High] {
            assert_eq!(p.codec(), Codec::H264);
        }
    }
}

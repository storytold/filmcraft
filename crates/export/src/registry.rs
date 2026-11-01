#[cfg(test)]
use super::Ordering;
use super::encoders::{aac_factory, apv_factory, dnx_factory, h264_factory, mjpeg_factory, prores_factory};
use super::format::{ExportSettings, Format};
#[cfg(test)]
use super::stats::{GPU_FALLBACKS, GPU_FRAMES};
use super::{FrameRate, ItemId, OnceLock, PoisonError, Project, Result, RwLock, SampleEntry, SourceProvider, Tick};

/// A packet produced by a video encoder.
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub key: bool,
    /// Duration in encoder timescale units.
    pub duration: u32,
    /// pts − dts in encoder timescale units.
    pub composition_offset: i32,
}

/// Input picture for encoders: straight sRGB RGBA8 (encoders convert to their own YUV), or for
/// HDR exports the encoded R'G'B' (PQ/HLG, BT.2020; 3 floats per pixel, 0..1) in `hdr`.
pub struct EncoderFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    pub hdr: Option<&'a [f32]>,
    pub index: u64,
}

/// Per-picture work an encoder hands to the export's render threads, see
/// [`VideoEncoder::prepare_ahead`].
pub type PrepareAhead = std::sync::Arc<dyn Fn(&EncoderFrame) + Send + Sync>;

pub trait VideoEncoder: Send {
    /// MP4/MOV sample entry (codec config) — may only be complete after the first frame.
    fn sample_entry(&self) -> SampleEntry;
    fn timescale(&self) -> u32;
    fn encode(&mut self, frame: &EncoderFrame) -> Result<Vec<EncodedPacket>>;
    fn flush(&mut self) -> Result<Vec<EncodedPacket>>;
    /// Media start offset for an edit list (B-frame delay), in the encoder timescale.
    fn media_start(&self) -> Option<i64> {
        None
    }
    /// First-pass statistics of a two-pass encode (H.264).
    fn pass_stats(&self) -> Option<filmcraft_h264enc::PassStats> {
        None
    }
    /// Work on a rendered picture that needs nothing of the encoder's state (H.264: the colour
    /// conversion), for the render threads to do while earlier pictures are being encoded.
    /// [`VideoEncoder::encode`] picks the result up by [`EncoderFrame::index`] and does the work
    /// itself for a picture that has none; the encoded bytes are the same either way.
    fn prepare_ahead(&self) -> Option<PrepareAhead> {
        None
    }
}

/// Audio encoder (AAC) plugged in by codec crates; PCM is built in.
pub trait AudioEncoder: Send {
    fn sample_entry(&self) -> SampleEntry;
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>>;
    fn flush(&mut self) -> Result<Vec<Vec<u8>>>;
    /// Encoder delay (priming) in samples.
    fn priming(&self) -> u32;
    /// Samples per access unit (1024 for AAC).
    fn frame_size(&self) -> u32;
    /// [`AudioEncoder::flush`] with each access unit's duration, for codecs whose last unit is
    /// shorter than [`AudioEncoder::frame_size`] (FLAC); by default every unit is a full frame.
    fn flush_timed(&mut self) -> Result<Vec<(Vec<u8>, u32)>> {
        let n = self.frame_size();
        Ok(self.flush()?.into_iter().map(|au| (au, n)).collect())
    }
    /// The sample entry once everything is encoded, when it differs from the first one (the FLAC
    /// STREAMINFO learns its frame sizes and MD5 at the end).
    fn final_sample_entry(&self) -> Option<SampleEntry> {
        None
    }
}

pub type EncoderFactory = fn(format: Format, width: u32, height: u32, rate: FrameRate, settings: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>>;

pub type AudioEncoderFactory = fn(format: Format, sample_rate: u32, channels: u32, settings: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>>;

pub(crate) fn video_factories() -> &'static RwLock<Vec<EncoderFactory>> {
    static F: OnceLock<RwLock<Vec<EncoderFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![h264_factory, prores_factory, dnx_factory, apv_factory, mjpeg_factory]))
}

pub(crate) fn audio_factories() -> &'static RwLock<Vec<AudioEncoderFactory>> {
    static F: OnceLock<RwLock<Vec<AudioEncoderFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(vec![aac_factory]))
}

/// Register a video encoder factory (tried before the built-in ones and those registered earlier).
/// Registering the same factory twice is harmless.
pub fn register_encoder(f: EncoderFactory) {
    let mut g = video_factories().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// Whether `f` is among the registered video encoder factories (startup diagnostics, tests).
pub fn encoder_registered(f: EncoderFactory) -> bool {
    video_factories().read().unwrap_or_else(PoisonError::into_inner).iter().any(|x| std::ptr::fn_addr_eq(*x, f))
}

/// Renders one export frame on something other than the CPU reference renderer — today the GPU
/// compositor (`filmcraft-gpu`), registered by the app or CLI at startup. `render` returns the
/// frame exactly as [`filmcraft_render::render_sequence`] would (same size, premultiplied
/// linear-light RGBA), or `None` to let the CPU render this frame (no adapter, a plan the GPU
/// cannot draw, any internal error): the CPU renderer is the reference and the fallback.
pub trait FrameRenderer: Send {
    fn render(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn SourceProvider,
    ) -> Option<filmcraft_render::Image>;
}

/// Builds the frame renderer for one export, or `None` when this host cannot (no adapter).
pub type FrameRendererFactory = fn() -> Option<Box<dyn FrameRenderer>>;

fn frame_renderer_factories() -> &'static RwLock<Vec<FrameRendererFactory>> {
    static F: OnceLock<RwLock<Vec<FrameRendererFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register a frame-renderer factory (tried before those registered earlier). Registering the
/// same factory twice is harmless.
pub fn register_frame_renderer(f: FrameRendererFactory) {
    let mut g = frame_renderer_factories().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// Whether `f` is among the registered frame-renderer factories (startup diagnostics, tests).
pub fn frame_renderer_registered(f: FrameRendererFactory) -> bool {
    frame_renderer_factories().read().unwrap_or_else(PoisonError::into_inner).iter().any(|x| std::ptr::fn_addr_eq(*x, f))
}

/// Clear the registered frame-renderer factories. Test-only: registration is a process-wide
/// startup step and tests that register their own must not see each other's.
#[cfg(test)]
pub(crate) fn reset_frame_renderers_for_tests() {
    frame_renderer_factories().write().unwrap_or_else(PoisonError::into_inner).clear();
    GPU_FRAMES.store(0, Ordering::Relaxed);
    GPU_FALLBACKS.store(0, Ordering::Relaxed);
}

/// Pick the first registered factory that can build a renderer for this host.
pub(crate) fn build_frame_renderer() -> Option<Box<dyn FrameRenderer>> {
    let g = frame_renderer_factories().read().unwrap_or_else(PoisonError::into_inner);
    g.iter().filter_map(|f| f()).next()
}

pub fn register_audio_encoder(f: AudioEncoderFactory) {
    audio_factories().write().unwrap_or_else(PoisonError::into_inner).insert(0, f);
}

type FormatProbe = (Format, fn() -> bool);

fn format_probes() -> &'static RwLock<Vec<FormatProbe>> {
    static P: OnceLock<RwLock<Vec<FormatProbe>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Vec::new()))
}

/// Say how to find out whether the encoder of a format without a built-in one ([`Format::has_builtin_encoder`])
/// works on this machine: `probe` runs when [`available`] asks (it should cache its answer).
/// Registering the same probe twice is harmless.
pub fn register_format_probe(format: Format, probe: fn() -> bool) {
    let mut g = format_probes().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|(f, p)| *f == format && std::ptr::fn_addr_eq(*p, probe)) {
        g.push((format, probe));
    }
}

fn hdr_probes() -> &'static RwLock<Vec<FormatProbe>> {
    static P: OnceLock<RwLock<Vec<FormatProbe>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Vec::new()))
}

/// Say how to find out whether the registered encoder of `format` can write HDR (PQ / HLG) on this
/// machine: `probe` runs when [`hdr_available`] asks (it should cache its answer). Only H.265 needs
/// one: its encoders are hardware-only and not all of them take 10-bit pictures, so an HDR sequence
/// exports as tone-mapped SDR unless a probe says otherwise. Registering the same probe twice is harmless.
pub fn register_hdr_probe(format: Format, probe: fn() -> bool) {
    let mut g = hdr_probes().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|(f, p)| *f == format && std::ptr::fn_addr_eq(*p, probe)) {
        g.push((format, probe));
    }
}

/// Whether a registered probe says the encoder of `format` writes HDR here (false without one).
pub fn hdr_available(format: Format) -> bool {
    // the probes run outside the lock: one may take a while
    let probes: Vec<fn() -> bool> = hdr_probes().read().unwrap_or_else(PoisonError::into_inner).iter().filter(|(f, _)| *f == format).map(|(_, p)| *p).collect();
    probes.into_iter().any(|p| p())
}

/// Whether a format can currently be exported: it has a built-in encoder, or a registered probe
/// says its encoder works here.
pub fn available(format: Format) -> bool {
    if !Format::ALL.contains(&format) {
        return false;
    }
    format.has_builtin_encoder() || format_probes().read().unwrap_or_else(PoisonError::into_inner).iter().any(|(f, probe)| *f == format && probe())
}

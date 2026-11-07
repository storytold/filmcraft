#[cfg(test)]
use super::Ordering;
use super::encoders::{aac_factory, apv_factory, dnx_factory, h264_factory, mjpeg_factory, prores_factory};
use super::format::{ExportSettings, Format};
#[cfg(test)]
use super::stats::{GPU_ENCODE_FALLBACKS, GPU_ENCODED, GPU_FALLBACKS, GPU_FRAMES, GPU_RESIDENT, GPU_RESIDENT_FALLBACKS};
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

/// Input picture for encoders: straight sRGB RGBA8 (encoders convert to their own YUV), or the
/// encoded R'G'B' (3 floats per pixel, 0..1) in `hdr`: PQ/HLG BT.2020 for HDR exports, the sRGB curve
/// and BT.709 for SDR exports to an encoder that codes more than 8 bits. `rgba` is then empty, or
/// with alpha the whole RGBA8 picture, for its alpha.
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

/// A frame a paired GPU renderer left on its device ([`FrameRenderer::render_resident`]): an
/// opaque handle that only the encoder backend made with it ([`GpuPair`]) understands.
pub type ResidentFrame = Box<dyn std::any::Any + Send>;

/// The planes an encoder that takes resident frames codes ([`VideoEncoder::resident_input`]):
/// limited-range Y'CbCr with this matrix, then a full-range alpha plane when `alpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneFormat {
    /// The matrix's luma coefficients for red and blue.
    pub kr: f32,
    pub kb: f32,
    /// Chroma halved horizontally (4:2:2), else 4:4:4.
    pub chroma_422: bool,
    pub alpha: bool,
    /// Bits per sample.
    pub bit_depth: u8,
}

/// What the export does per pixel to a rendered frame before the encoder codes it (the read-back
/// path does it on the CPU: `Pipeline::frame` and the encoder's colour conversion). A renderer that
/// keeps frames on the GPU ([`FrameRenderer::render_resident`]) runs these steps there; the export
/// takes that path only when it runs every one that is enabled ([`FrameRenderer::resident_supported`]),
/// decided once, before the first frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PostSettings {
    /// Size the frame is rendered at.
    pub rendered: (u32, u32),
    /// The output frame.
    pub width: u32,
    pub height: u32,
    /// Scaling: the picture's size in the output frame, resampled to it (`None`: as rendered).
    pub content: Option<(u32, u32)>,
    /// Placement: the picture's top-left in the output frame (negative: cropped).
    pub offset: (i64, i64),
    /// The video limiter: luma floor and ceiling in percent of the encoded signal.
    pub limiter: Option<(f32, f32)>,
    /// The output transfer function is HDR (PQ / HLG through the sequence's output transform);
    /// else the sRGB curve (SDR BT.709).
    pub hdr: bool,
    /// The encoded signal is handed on as floats (encoders that code more than 8 bits); else
    /// quantised to 8 bits first, as the RGBA8 picture.
    pub floats: bool,
    /// Straight colour with alpha (Include Alpha Channel); else the colour over black.
    pub alpha: bool,
    /// The encoder's matrix, chroma siting and subsampling, bit depth and alpha plane.
    pub planes: PlaneFormat,
    /// The Export effects' overlays drawn over the frame.
    pub image_overlay: bool,
    pub name_overlay: bool,
    pub timecode_overlay: bool,
}

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
    /// The planes this encoder codes from a [`ResidentFrame`], when its backend was made with the
    /// export's renderers by one [`GpuPair`] and so can take their frames; `None` (the default):
    /// every frame comes as pixels.
    fn resident_input(&self) -> Option<PlaneFormat> {
        None
    }
    /// Encode a frame the paired renderer left on the GPU (frame `index`, as
    /// [`EncoderFrame::index`]). `None`: not coded (the backend declined or failed); the export
    /// renders this frame again through the read-back path and calls [`VideoEncoder::encode`].
    fn encode_resident(&mut self, frame: &ResidentFrame, index: u64) -> Option<Vec<EncodedPacket>> {
        let _ = (frame, index);
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
/// cannot draw, any internal error): the CPU renderer is the reference and the fallback. It runs
/// under `filmcraft_media::cancel::with_gpu_frames`, so its sources may hand it pictures that are
/// still coded ([`PixelData::Coded`](filmcraft_frame::PixelData::Coded)).
pub trait FrameRenderer: Send {
    fn render(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn SourceProvider,
    ) -> Option<filmcraft_render::Image>;

    /// What the renderer's device runs. The export plans with it before taking a renderer
    /// ([`filmcraft_render::plan::is_cpu_frame`]), so a frame the device can't composite renders
    /// on the export's own thread instead of on the CPU inside the renderer.
    fn caps(&self) -> filmcraft_render::plan::DeviceCaps;

    /// Whether [`FrameRenderer::render_resident`] runs every enabled step of `post` (false, the
    /// default, for a renderer that cannot keep frames on its device).
    fn resident_supported(&self, post: &PostSettings) -> bool {
        let _ = post;
        false
    }

    /// Render a frame and run `post` on it where it was rendered, leaving the encoder's planes
    /// there: a handle for the encoder backend made with this renderer ([`GpuPair`]). `None` (the
    /// default) sends this frame through the read-back path ([`FrameRenderer::render`]).
    fn render_resident(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn SourceProvider,
        post: &PostSettings,
    ) -> Option<ResidentFrame> {
        let _ = (project, seq, t, opts, sources, post);
        None
    }
}

/// Builds the frame renderer for one export, or `None` when this host cannot (no adapter).
pub type FrameRendererFactory = fn() -> Option<Box<dyn FrameRenderer>>;

/// Frame renderers and encoder backends made on one device, so that a rendered frame can stay
/// there for the encoder (ADR 0003 Step 2): the app registers one ([`register_gpu_pair`]) next to
/// its separate factories. The export takes its encoder's backend from a pair first, and its
/// renderers too when it will use that GPU encoder ([`ExportSettings::uses_gpu_encoder`]; any
/// other GPU-rendered export takes the separate renderer factory's); it keeps frames on the
/// device only when both came from one.
pub trait GpuPair: Send + Sync {
    fn renderer(&self) -> Option<Box<dyn FrameRenderer>>;
    fn prores_backend(&self) -> Option<Box<dyn filmcraft_prores::SliceBackend>>;
    fn dnx_backend(&self) -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>>;
}

/// Makes the [`GpuPair`] of this host (every call the same device), or `None` when it cannot.
pub type GpuPairFactory = fn() -> Option<Box<dyn GpuPair>>;

fn gpu_pair_factories() -> &'static RwLock<Vec<GpuPairFactory>> {
    static F: OnceLock<RwLock<Vec<GpuPairFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register a [`GpuPair`] factory (tried before those registered earlier, and before the separate
/// renderer and backend factories). Registering the same factory twice is harmless.
pub fn register_gpu_pair(f: GpuPairFactory) {
    let mut g = gpu_pair_factories().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// What the first registered pair that `pick` gets something from gives. The factories (device
/// creation) and `pick` run outside the registry lock, and a panic counts as nothing made.
fn from_pairs<T>(pick: impl Fn(&dyn GpuPair) -> Option<T>) -> Option<T> {
    let list = gpu_pair_factories().read().unwrap_or_else(PoisonError::into_inner).clone();
    list.into_iter().find_map(|f| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f().and_then(|pair| pick(pair.as_ref())))).ok().flatten())
}

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

/// A renderer for one export: from a registered [`GpuPair`] (`true`) when `gpu_encoder` (the
/// export will code on a pair's backend, [`ExportSettings::uses_gpu_encoder`]), or else from the
/// first registered factory that can build one for this host (`false`). An export without a GPU
/// encoder never takes the pair's: it would render on the encoders' device (other limits, other
/// device caps) and its device errors would retire GPU encoding for the whole process.
pub(crate) fn build_frame_renderer(gpu_encoder: bool) -> Option<(Box<dyn FrameRenderer>, bool)> {
    if gpu_encoder && let Some(r) = from_pairs(|pair| pair.renderer()) {
        return Some((r, true));
    }
    let g = frame_renderer_factories().read().unwrap_or_else(PoisonError::into_inner);
    g.iter().filter_map(|f| f()).next().map(|r| (r, false))
}

/// Builds the ProRes slice backend (the GPU) for one export, or `None` when this host cannot.
pub type ProResBackendFactory = fn() -> Option<Box<dyn filmcraft_prores::SliceBackend>>;

/// Builds the DNxHR macroblock backend (the GPU) for one export, or `None` when this host cannot.
pub type DnxBackendFactory = fn() -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>>;

fn prores_backend_factories() -> &'static RwLock<Vec<ProResBackendFactory>> {
    static F: OnceLock<RwLock<Vec<ProResBackendFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(Vec::new()))
}

fn dnx_backend_factories() -> &'static RwLock<Vec<DnxBackendFactory>> {
    static F: OnceLock<RwLock<Vec<DnxBackendFactory>>> = OnceLock::new();
    F.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register a ProRes backend factory: under *Hardware encoding: Auto* the ProRes encoder takes a
/// backend from it (tried before those registered earlier) for the per-block work, and codes a
/// picture itself whenever the backend declines or fails. Registering the same factory twice is harmless.
pub fn register_prores_backend(f: ProResBackendFactory) {
    let mut g = prores_backend_factories().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// [`register_prores_backend`] for the DNxHR encoder.
pub fn register_dnx_backend(f: DnxBackendFactory) {
    let mut g = dnx_backend_factories().write().unwrap_or_else(PoisonError::into_inner);
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// The first of `factories` that makes a backend. Device creation is where a driver fault is
/// likeliest, so the factories run outside the registry lock and a panicking one counts as one
/// that cannot (the export then encodes on the CPU). That is not counted in
/// `encode_fallbacks`, which counts pictures given to a backend.
fn first_backend<B: ?Sized>(factories: Vec<fn() -> Option<Box<B>>>) -> Option<Box<B>> {
    factories.into_iter().find_map(|f| std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok().flatten())
}

/// A ProRes backend: a registered [`GpuPair`]'s (`true`), or else the first registered one this
/// host can make (`false`).
pub(crate) fn build_prores_backend() -> Option<(Box<dyn filmcraft_prores::SliceBackend>, bool)> {
    if let Some(b) = from_pairs(|pair| pair.prores_backend()) {
        return Some((b, true));
    }
    let list = prores_backend_factories().read().unwrap_or_else(PoisonError::into_inner).clone();
    first_backend(list).map(|b| (b, false))
}

/// [`build_prores_backend`] for DNxHR.
pub(crate) fn build_dnx_backend() -> Option<(Box<dyn filmcraft_dnx::MacroblockBackend>, bool)> {
    if let Some(b) = from_pairs(|pair| pair.dnx_backend()) {
        return Some((b, true));
    }
    let list = dnx_backend_factories().read().unwrap_or_else(PoisonError::into_inner).clone();
    first_backend(list).map(|b| (b, false))
}

/// Clear the registered encoder backends and their counters. Test-only, like
/// [`reset_frame_renderers_for_tests`].
#[cfg(test)]
pub(crate) fn reset_encoder_backends_for_tests() {
    prores_backend_factories().write().unwrap_or_else(PoisonError::into_inner).clear();
    dnx_backend_factories().write().unwrap_or_else(PoisonError::into_inner).clear();
    gpu_pair_factories().write().unwrap_or_else(PoisonError::into_inner).clear();
    GPU_ENCODED.store(0, Ordering::Relaxed);
    GPU_ENCODE_FALLBACKS.store(0, Ordering::Relaxed);
    GPU_RESIDENT.store(0, Ordering::Relaxed);
    GPU_RESIDENT_FALLBACKS.store(0, Ordering::Relaxed);
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

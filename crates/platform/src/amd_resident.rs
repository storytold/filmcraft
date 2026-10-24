//! Serial AMD resident H.264 export session.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::{Duration, Instant},
};

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_export::{
    ColorSignal, EncodedPacket, ExportError, ExportSettings, Format, ResidentAllocation, ResidentReservation, ResidentVideoSession, Result, Stage,
    VideoEncoder, note_stage, settings::Resolved, timed,
};
use filmcraft_gpu::{ExportRenderer, nv12_repack::Nv12Repack, resident::ResidentNv12Binding, yuv_output::Nv12Layout};
use filmcraft_media::{CompressedVideoSample, CompressedVideoSource};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::plan::{ResidentFrameMetadata, ResidentSourceKind, ResidentSourceRequest, materialize_frame_required_resident};

use crate::{
    vaapi::{self, ResidentDecodedFrame, ResidentFrameSink},
    vulkan_buffer_probe::{self, import_and_repack_va_nv12_frame_with_dpb, linear_nv12_object_size_bound},
    vulkan_import::render_node_id,
    vulkan_interop_device::create_amd_interop_device,
};

const MAX_RESIDENT_SOURCES: usize = 16;
const MAX_PREROLL_SAMPLES: usize = 10_000;
/// Decoders kept between frames; their surfaces stay allocated, so they are bounded and headroom-checked.
const MAX_DECODE_CURSORS: usize = 4;
const MAX_COMPRESSED_SAMPLE_BYTES: u32 = 64 * 1024 * 1024;
const RENDER_FRAME_OVERHEAD_BYTES: u64 = 64 * 1024;
const RENDER_LAYER_OVERHEAD_BYTES: u64 = 4 * 1024;
/// Rgba32Float effect working images (16 bytes per pixel), up to three per working size.
const FX_WORKING_BYTES_PER_PIXEL: u64 = 16 * 3;
const PERSISTENT_GPU_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// The strict resident encoder of the admitted codec; both take GPU-produced NV12 without readback.
enum ResidentEncoder {
    H264(vaapi::h264enc::VaH264Encoder),
    Hevc(vaapi::hevcenc::VaHevcEncoder),
}

impl ResidentEncoder {
    fn coded_size(&self) -> (u32, u32) {
        match self {
            Self::H264(encoder) => encoder.coded_size(),
            Self::Hevc(encoder) => encoder.coded_size(),
        }
    }

    fn sample_entry(&self) -> filmcraft_isobmff::SampleEntry {
        match self {
            Self::H264(encoder) => encoder.sample_entry(),
            Self::Hevc(encoder) => encoder.sample_entry(),
        }
    }

    fn timescale(&self) -> u32 {
        match self {
            Self::H264(encoder) => encoder.timescale(),
            Self::Hevc(encoder) => encoder.timescale(),
        }
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        match self {
            Self::H264(encoder) => encoder.flush(),
            Self::Hevc(encoder) => encoder.flush(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_gpu_nv12(
        &mut self,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Buffer,
        y_stride: u32,
        uv_stride: u32,
        uv_offset: u64,
        buffer_size: u64,
        timeout: Duration,
    ) -> Result<Vec<EncodedPacket>> {
        match self {
            Self::H264(encoder) => encoder.encode_gpu_nv12(adapter, device, queue, source, y_stride, uv_stride, uv_offset, buffer_size, timeout),
            Self::Hevc(encoder) => encoder.encode_gpu_nv12(adapter, device, queue, source, y_stride, uv_stride, uv_offset, buffer_size, timeout),
        }
    }
}

struct PersistentResources {
    renderer: ExportRenderer,
    encoder: ResidentEncoder,
    repacker: Nv12Repack,
    _allocation: ResidentAllocation,
}

/// A decoder kept between consecutive output frames of one source, so a sequential export feeds one
/// new sample per frame instead of re-decoding the GOP. Only streams without picture reordering.
struct DecodeCursor {
    item: ItemId,
    /// Touched since the previous frame began; cursors that were not are dropped (see `encode_frame_inner`).
    used: bool,
    decoder: vaapi::ResidentVaDecoder,
    entry: filmcraft_isobmff::SampleEntry,
    /// The last sample fed to `decoder`.
    last_fed: usize,
}

struct H264ResidentSession {
    resources: Option<PersistentResources>,
    cursors: Vec<DecodeCursor>,
    project: Arc<Project>,
    node: vaapi::DrmNodeId,
    width: u32,
    height: u32,
    coded_size: (u32, u32),
    packet_capacity: u64,
    timescale: u32,
    /// The entry at creation; the encoder's own entry (HEVC refreshes its hvcC after the first IDR) wins.
    sample_entry: filmcraft_isobmff::SampleEntry,
    signal: ColorSignal,
    signal_in_stream: bool,
    quarantined: bool,
}

impl Drop for H264ResidentSession {
    fn drop(&mut self) {
        if self.quarantined {
            if let Some(resources) = self.resources.take() {
                // A failed exact GPU/VA retirement may leave the encoder or device in use.
                std::mem::forget(resources);
            }
            std::mem::forget(std::mem::take(&mut self.cursors));
        }
    }
}

struct OutputBufferGuard {
    buffer: Option<wgpu::Buffer>,
    output_allocation: Option<ResidentAllocation>,
    work_allocation: Option<ResidentAllocation>,
    uncertain: bool,
}

impl OutputBufferGuard {
    fn retire(&mut self) {
        self.uncertain = false;
        self.buffer.take();
        self.output_allocation.take();
        self.work_allocation.take();
    }
}

impl Drop for OutputBufferGuard {
    fn drop(&mut self) {
        if self.uncertain {
            if let Some(buffer) = self.buffer.take() {
                std::mem::forget(buffer);
            }
            if let Some(allocation) = self.output_allocation.take() {
                std::mem::forget(allocation);
            }
            if let Some(allocation) = self.work_allocation.take() {
                std::mem::forget(allocation);
            }
        }
    }
}

struct PreparedSource {
    request: ResidentSourceRequest,
    source: Arc<dyn CompressedVideoSource>,
    target_index: usize,
    target: CompressedVideoSample,
    entry: filmcraft_isobmff::SampleEntry,
    info: NalStreamInfo,
    surface_bound: u64,
    color: filmcraft_color::ColorInfo,
}

struct TargetFrameSink {
    pts: i64,
    output: Option<ResidentDecodedFrame>,
}

impl ResidentFrameSink for TargetFrameSink {
    type Admission = ();

    fn wants_frame(&self, frame: vaapi::ResidentFrameMetadata) -> bool {
        filmcraft_codecs::hw::note_hw_frames(1);
        self.output.is_none() && frame.pts == self.pts
    }

    fn reserve(&mut self, frame: vaapi::ResidentFrameMetadata) -> std::result::Result<(), String> {
        if frame.pts != self.pts || self.output.is_some() {
            return Err("resident decoder admitted an unexpected target PTS".into());
        }
        Ok(())
    }

    fn submit(&mut self, (): (), frame: ResidentDecodedFrame) -> std::result::Result<(), String> {
        if frame.pts != self.pts || self.output.is_some() {
            return Err("resident decoder submitted an unexpected target PTS".into());
        }
        self.output = Some(frame);
        Ok(())
    }
}

pub(crate) fn create_session(
    node: vaapi::DrmNodeId,
    project: Arc<Project>,
    seq: ItemId,
    settings: &ExportSettings,
    resolved: &Resolved,
) -> Result<Box<dyn ResidentVideoSession>> {
    let hevc = settings.video_format() == Format::Hevc;
    if !matches!(settings.video_format(), Format::H264 | Format::Hevc)
        || settings.signal.is_hdr()
        || settings.signal != ColorSignal::default()
        || settings.format.is_mxf()
    {
        return Err(ExportError::Unsupported("AMD resident session supports SDR H.264 or HEVC in MP4 or QuickTime".into()));
    }
    if resolved.width == 0 || resolved.height == 0 || !resolved.width.is_multiple_of(2) || !resolved.height.is_multiple_of(2) {
        return Err(ExportError::Unsupported("AMD resident H.264 output dimensions must be nonzero and even".into()));
    }
    let sequence = project.sequence(seq).ok_or(ExportError::NoSequence)?;
    if resolved.width != sequence.settings.width || resolved.height != sequence.settings.height || resolved.rate != sequence.settings.frame_rate {
        return Err(ExportError::Unsupported("AMD resident resolved settings do not match sequence".into()));
    }
    // The driver rounds the coded picture up to whole macroblocks (H.264) or 64-pixel CTUs (HEVC).
    let alignment = if hevc { 64 } else { 16 };
    let coded_size = (align_up(resolved.width, alignment)?, align_up(resolved.height, alignment)?);
    let encoder_surface_bound = linear_nv12_object_size_bound(coded_size.0, coded_size.1, 1).map_err(ExportError::Unsupported)?;
    let max_packet_bytes = packet_capacity(coded_size)?;
    let encoder_surfaces = encoder_surface_bound.checked_mul(3).ok_or_else(|| ExportError::MemoryPressure("resident H.264 surface budget overflow".into()))?;
    let accum_bytes = u64::from(resolved.width)
        .checked_mul(u64::from(resolved.height))
        // The resident renderer currently allocates one RGBA16F target; reserve up to four
        // resolution-sized cached targets so future compositor reuse stays within admission.
        .and_then(|pixels| pixels.checked_mul(32))
        .ok_or_else(|| ExportError::MemoryPressure("resident compositor budget overflow".into()))?;
    let persistent_bytes = encoder_surfaces
        .checked_add(max_packet_bytes)
        .and_then(|bytes| bytes.checked_add(accum_bytes))
        .and_then(|bytes| bytes.checked_add(PERSISTENT_GPU_OVERHEAD_BYTES))
        .ok_or_else(|| ExportError::MemoryPressure("resident persistent budget overflow".into()))?;
    let host_headroom_bytes = runtime_headroom_bytes();
    let mut persistent_reservation = ResidentReservation::try_acquire(host_headroom_bytes, None, 0, persistent_bytes)?;
    let persistent_allocation = persistent_reservation.take(persistent_bytes)?;
    drop(persistent_reservation);

    let mut effective = settings.clone();
    effective.bitrate_kbps = resolved.target_kbps;
    effective.max_bitrate_kbps = Some(resolved.max_kbps);
    effective.keyframe_distance = Some(resolved.keyint);
    effective.adaptive_bitrate = None;
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .into_iter()
        .find(|adapter| adapter.get_info().vendor == 0x1002 && render_node_id(adapter).ok() == Some(node))
        .ok_or_else(|| ExportError::Unsupported("the admitted AMD Vulkan render node is unavailable".into()))?;
    let descriptor = wgpu::DeviceDescriptor::default();
    let (device, queue) = create_amd_interop_device(&adapter, &descriptor, node).map_err(ExportError::Unsupported)?;
    let renderer =
        ExportRenderer::from_required_device(instance, adapter, device.clone(), queue.clone()).map_err(|error| ExportError::Unsupported(error.to_string()))?;
    let repacker = Nv12Repack::new(&device);
    let built = if hevc {
        vaapi::hevcenc::VaHevcEncoder::resident_on_node(node, resolved.width, resolved.height, resolved.rate, &effective)
            .map(|built| built.map(ResidentEncoder::Hevc))
    } else {
        vaapi::h264enc::VaH264Encoder::resident_on_node(node, resolved.width, resolved.height, resolved.rate, &effective)
            .map(|built| built.map(ResidentEncoder::H264))
    }
    .ok_or_else(|| ExportError::Unsupported("AMD resident settings were not admitted".into()))?
    .map_err(ExportError::Unsupported)?;
    if built.coded_size() != coded_size {
        return Err(ExportError::Unsupported("resident encoder coded size differs from the admitted coded size".into()));
    }
    filmcraft_export::note_hw_encode_session();
    let mut sample_entry = built.sample_entry();
    let signal_in_stream = !settings.format.is_mp4_with(settings.multiplexer);
    settings.signal.apply_to(&mut sample_entry, signal_in_stream);
    let timescale = built.timescale();
    let encoder_packet_capacity =
        packet_capacity(built.coded_size())?.checked_add(16).ok_or_else(|| ExportError::MemoryPressure("resident packet budget overflow".into()))?;
    Ok(Box::new(H264ResidentSession {
        cursors: Vec::new(),
        resources: Some(PersistentResources { renderer, encoder: built, repacker, _allocation: persistent_allocation }),
        project,
        node,
        width: resolved.width,
        height: resolved.height,
        coded_size,
        packet_capacity: encoder_packet_capacity,
        timescale,
        sample_entry,
        signal: settings.signal,
        signal_in_stream,
        quarantined: false,
    }))
}

impl ResidentVideoSession for H264ResidentSession {
    fn sample_entry(&self) -> filmcraft_isobmff::SampleEntry {
        let Some(resources) = self.resources.as_ref() else { return self.sample_entry.clone() };
        let mut entry = resources.encoder.sample_entry();
        self.signal.apply_to(&mut entry, self.signal_in_stream);
        entry
    }

    fn timescale(&self) -> u32 {
        self.timescale
    }

    fn frame_bytes(&self, frame: &filmcraft_render::plan::PreflightedFrame) -> Result<u64> {
        if self.quarantined {
            return Err(ExportError::Encode("resident H.264 session is quarantined".into()));
        }
        let (width, height) = frame.size();
        if u32::try_from(width).ok() != Some(self.width) || u32::try_from(height).ok() != Some(self.height) {
            return Err(ExportError::Unsupported("resident preflight dimensions do not match H.264 output".into()));
        }
        let requests = frame.source_requests();
        if requests.len() > MAX_RESIDENT_SOURCES {
            return Err(ExportError::Unsupported(format!("resident frame has {} sources; limit is {MAX_RESIDENT_SOURCES}", requests.len())));
        }
        let output_layout = Nv12Layout::with_coded_size(self.width, self.height, self.coded_size.0, self.coded_size.1)
            .ok_or_else(|| ExportError::Unsupported("resident H.264 output layout is invalid".into()))?;
        let packet_bytes = self.packet_capacity.checked_mul(2).ok_or_else(|| ExportError::MemoryPressure("resident H.264 packet workspace overflow".into()))?;
        let mut total = output_layout
            .size
            .checked_add(packet_bytes)
            .and_then(|bytes| bytes.checked_add(RENDER_FRAME_OVERHEAD_BYTES))
            .ok_or_else(|| ExportError::MemoryPressure("resident frame budget overflow".into()))?;
        total = total
            .checked_add(
                u64::try_from(requests.len())
                    .ok()
                    .and_then(|count| count.checked_mul(RENDER_LAYER_OVERHEAD_BYTES))
                    .ok_or_else(|| ExportError::MemoryPressure("resident layer budget overflow".into()))?,
            )
            .ok_or_else(|| ExportError::MemoryPressure("resident frame budget overflow".into()))?;
        total = total.checked_add(fx_working_bytes(frame)?).ok_or_else(|| ExportError::MemoryPressure("resident frame budget overflow".into()))?;
        for request in requests {
            let (source_width, source_height) = request.expected_size;
            let source_bytes = match request.kind {
                ResidentSourceKind::Decoded => {
                    if source_width == 0 || source_height == 0 || !source_width.is_multiple_of(2) || !source_height.is_multiple_of(2) {
                        return Err(ExportError::Unsupported("resident source dimensions must be nonzero and even".into()));
                    }
                    let compressed_workspace_bytes = u64::from(MAX_COMPRESSED_SAMPLE_BYTES)
                        .checked_mul(4)
                        .ok_or_else(|| ExportError::MemoryPressure("resident compressed-sample workspace overflow".into()))?;
                    let surface_bound = linear_nv12_object_size_bound(source_width, source_height, 64).map_err(ExportError::Unsupported)?;
                    let decoder_bytes = surface_bound
                        .checked_mul(vulkan_buffer_probe::MAX_VA_DECODE_SURFACES)
                        .ok_or_else(|| ExportError::MemoryPressure("resident decoder DPB budget overflow".into()))?;
                    let repack_layout = Nv12Layout::new(source_width, source_height)
                        .ok_or_else(|| ExportError::Unsupported("resident source dimensions are invalid for NV12".into()))?;
                    let repack_bytes = Nv12Repack::allocation_bytes(&repack_layout).map_err(ExportError::Unsupported)?;
                    decoder_bytes
                        .checked_add(surface_bound)
                        .and_then(|bytes| bytes.checked_add(surface_bound))
                        .and_then(|bytes| bytes.checked_add(repack_bytes))
                        .and_then(|bytes| bytes.checked_add(compressed_workspace_bytes))
                        .ok_or_else(|| ExportError::MemoryPressure("resident source budget overflow".into()))?
                }
                ResidentSourceKind::Demo(_) => {
                    filmcraft_gpu::demo_source::DemoSource::allocation_bytes(source_width, source_height).map_err(ExportError::Unsupported)?
                }
            };
            total = total.checked_add(source_bytes).ok_or_else(|| ExportError::MemoryPressure("resident frame budget overflow".into()))?;
        }
        Ok(total)
    }

    fn encode_frame(
        &mut self,
        _index: u64,
        frame: filmcraft_render::plan::PreflightedFrame,
        sources: &dyn filmcraft_render::SourceProvider,
        reservation: &mut ResidentReservation,
    ) -> Result<Vec<EncodedPacket>> {
        self.quarantined = false;
        let result = self.encode_frame_inner(frame, sources, reservation);
        if result.is_err() && self.quarantined {
            self.quarantine_resources();
        }
        result
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let resources = self.resources.as_mut().ok_or_else(|| ExportError::Encode("resident H.264 session is quarantined".into()))?;
        resources.encoder.flush()
    }
}

/// Worst-case effect working-image bytes of one frame: every node with effects is charged three
/// RGBA32F images of the largest of the output and the source sizes (pooled images are shared, so
/// this over-reserves).
fn fx_working_bytes(frame: &filmcraft_render::plan::PreflightedFrame) -> Result<u64> {
    let effect_nodes =
        frame.nodes().filter(|node| node.operations().iter().any(|operation| matches!(operation, filmcraft_render::plan::PlannedGpuOp::Effect { .. }))).count();
    if effect_nodes == 0 {
        return Ok(0);
    }
    let (out_width, out_height) = frame.size();
    let overflow = || ExportError::MemoryPressure("resident effect budget overflow".into());
    let mut pixels =
        u64::try_from(out_width).ok().zip(u64::try_from(out_height).ok()).and_then(|(width, height)| width.checked_mul(height)).ok_or_else(overflow)?;
    for request in frame.source_requests() {
        let source = u64::from(request.expected_size.0).checked_mul(u64::from(request.expected_size.1)).ok_or_else(overflow)?;
        pixels = pixels.max(source);
    }
    u64::try_from(effect_nodes)
        .ok()
        .and_then(|nodes| nodes.checked_mul(pixels))
        .and_then(|total| total.checked_mul(FX_WORKING_BYTES_PER_PIXEL))
        .ok_or_else(overflow)
}

impl H264ResidentSession {
    fn encode_frame_inner(
        &mut self,
        receipt: filmcraft_render::plan::PreflightedFrame,
        sources: &dyn filmcraft_render::SourceProvider,
        reservation: &mut ResidentReservation,
    ) -> Result<Vec<EncodedPacket>> {
        let render_started = Instant::now();
        if self.resources.is_none() {
            return Err(ExportError::Encode("resident H.264 session is quarantined".into()));
        }
        // Every earlier frame's surfaces are retired, so decoders the previous frame did not touch can go.
        self.cursors.retain_mut(|cursor| std::mem::take(&mut cursor.used));
        let source_request_count = receipt.source_requests().len();
        let mut prepared = Vec::with_capacity(source_request_count);
        let mut metadata = Vec::with_capacity(source_request_count);
        let mut generated_bytes = 0_u64;
        for request in receipt.source_requests() {
            match request.kind {
                ResidentSourceKind::Decoded => {
                    let source = sources
                        .compressed_source(request.item_id)
                        .map_err(|error| ExportError::Unsupported(format!("resident H.264 input {}: {error}", request.item_id.0)))?;
                    let input = prepare_source(self.project.as_ref(), *request, source)?;
                    metadata.push(ResidentFrameMetadata {
                        id: request.id,
                        item_id: request.item_id,
                        width: request.expected_size.0,
                        height: request.expected_size.1,
                        color: input.color,
                    });
                    prepared.push(input);
                }
                ResidentSourceKind::Demo(_) => {
                    let bytes = filmcraft_gpu::demo_source::DemoSource::allocation_bytes(request.expected_size.0, request.expected_size.1)
                        .map_err(ExportError::Unsupported)?;
                    generated_bytes =
                        generated_bytes.checked_add(bytes).ok_or_else(|| ExportError::MemoryPressure("resident generated-source budget overflow".into()))?;
                    metadata.push(ResidentFrameMetadata {
                        id: request.id,
                        item_id: request.item_id,
                        width: request.expected_size.0,
                        height: request.expected_size.1,
                        color: filmcraft_color::ColorInfo::SRGB_FULL,
                    });
                }
            }
        }
        let fx_bytes = fx_working_bytes(&receipt)?;
        for node in receipt.nodes() {
            let admitted = match node.origin() {
                filmcraft_render::plan::PixelOrigin::DecodedMediaSource => {
                    node.operations().iter().all(|operation| matches!(operation, filmcraft_render::plan::PlannedGpuOp::Effect { .. }))
                }
                filmcraft_render::plan::PixelOrigin::GpuGenerated { route: "transition" } => {
                    node.operations().iter().all(|operation| matches!(operation, filmcraft_render::plan::PlannedGpuOp::Transition { .. }))
                }
                filmcraft_render::plan::PixelOrigin::GpuGenerated { route: "demo_source" } => {
                    node.operations().iter().all(|operation| matches!(operation, filmcraft_render::plan::PlannedGpuOp::Effect { .. }))
                }
                _ => false,
            };
            if !admitted {
                return Err(ExportError::Unsupported(
                    "AMD resident export requires decoded or demo GPU sources with admitted GPU effects and GPU transitions only".into(),
                ));
            }
        }
        let plan = materialize_frame_required_resident(receipt, &metadata).map_err(|error| ExportError::Unsupported(error.to_string()))?;
        if plan.layers().any(|layer| layer.blend().reads_destination()) {
            return Err(ExportError::Unsupported("AMD resident export does not support destination-reading blends".into()));
        }

        let mut resident_sources = Vec::with_capacity(prepared.len());
        for input in &prepared {
            let dpb_bytes = input
                .surface_bound
                .checked_mul(vulkan_buffer_probe::MAX_VA_DECODE_SURFACES)
                .ok_or_else(|| ExportError::MemoryPressure("resident decoder DPB budget overflow".into()))?;
            let dpb_allocation = reservation.take(dpb_bytes)?;
            let (decoded, dpb_allocation) = decode_target(self.node, input, &mut self.cursors, dpb_allocation)?;
            let resources = self.resources.as_ref().ok_or_else(|| ExportError::Encode("resident H.264 session is quarantined".into()))?;
            let (adapter, device, queue) = resources.renderer.wgpu_handles();
            let imported = timed(Stage::Convert, || {
                import_and_repack_va_nv12_frame_with_dpb(adapter, device, queue, &resources.repacker, decoded, reservation, TRANSFER_TIMEOUT, dpb_allocation)
            })
            .map_err(|error| ExportError::Encode(format!("resident VA NV12 import: {error}")))?;
            resident_sources.push(imported);
        }

        let output_layout = Nv12Layout::with_coded_size(self.width, self.height, self.coded_size.0, self.coded_size.1)
            .ok_or_else(|| ExportError::Unsupported("resident H.264 output layout is invalid".into()))?;
        let output_allocation = reservation.take(output_layout.size)?;
        let work_bytes = RENDER_FRAME_OVERHEAD_BYTES
            .checked_add(
                u64::try_from(source_request_count)
                    .ok()
                    .and_then(|count| count.checked_mul(RENDER_LAYER_OVERHEAD_BYTES))
                    .ok_or_else(|| ExportError::MemoryPressure("resident layer budget overflow".into()))?,
            )
            .and_then(|bytes| bytes.checked_add(fx_bytes))
            .and_then(|bytes| bytes.checked_add(generated_bytes))
            .ok_or_else(|| ExportError::MemoryPressure("resident render budget overflow".into()))?;
        let work_allocation = reservation.take(work_bytes)?;
        let resources = self.resources.as_ref().ok_or_else(|| ExportError::Encode("resident H.264 session is quarantined".into()))?;
        let device = resources.renderer.wgpu_handles().1.clone();
        if output_layout.size > device.limits().max_buffer_size || output_layout.size > device.limits().max_storage_buffer_binding_size {
            return Err(ExportError::Unsupported("resident output buffer exceeds Vulkan device limits".into()));
        }
        let oom_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("filmcraft-resident-h264-output"),
            size: output_layout.size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        if let Some(error) = vulkan_buffer_probe::poll_error_scope(validation_scope, &device).map_err(ExportError::Unsupported)? {
            return Err(ExportError::Encode(format!("resident output buffer validation: {error}")));
        }
        if let Some(error) = vulkan_buffer_probe::poll_error_scope(oom_scope, &device).map_err(ExportError::Unsupported)? {
            return Err(ExportError::MemoryPressure(format!("resident output buffer allocation: {error}")));
        }
        let mut output_guard = OutputBufferGuard {
            buffer: Some(output.clone()),
            output_allocation: Some(output_allocation),
            work_allocation: Some(work_allocation),
            uncertain: false,
        };

        let mut bindings = Vec::with_capacity(resident_sources.len());
        for (input, resident) in prepared.iter().zip(&resident_sources) {
            let repacked = resident.repacked().ok_or_else(|| ExportError::Encode("resident source repack was retired before composition".into()))?;
            bindings.push(ResidentNv12Binding { id: input.request.id, layout: repacked.layout, y: &repacked.y, uv: &repacked.uv });
        }
        let resources = self.resources.as_mut().ok_or_else(|| ExportError::Encode("resident H.264 session is quarantined".into()))?;
        self.quarantined = true;
        output_guard.uncertain = true;
        let render_result =
            catch_unwind(AssertUnwindSafe(|| resources.renderer.render_resident_nv12_buffer_coded(&plan, &bindings, output, Some(self.coded_size))));
        let (rendered, layout, submission) = match render_result {
            Ok(Ok(rendered)) => rendered,
            Ok(Err(error)) => {
                // The compositor may have submitted partial work without returning its submission index.
                return Err(ExportError::Unsupported(error.to_string()));
            }
            Err(_) => return Err(ExportError::Encode("resident compositor panicked; GPU ownership remains unproven".into())),
        };
        output_guard.buffer = Some(rendered);
        if layout != output_layout {
            return Err(ExportError::Encode("resident compositor returned a different checked NV12 layout".into()));
        }
        filmcraft_export::note_gpu_frame();
        let queue = resources.renderer.wgpu_handles().2.clone();
        if resident_sources.is_empty() {
            timed(Stage::Wait, || crate::vulkan_import::wait_for_submission(&device, submission.clone(), TRANSFER_TIMEOUT))
                .map_err(|error| ExportError::Encode(format!("resident render submission: {error}")))?;
        } else {
            for source in &mut resident_sources {
                timed(Stage::Wait, || source.retire_after_queue_submission(&queue, submission.clone(), TRANSFER_TIMEOUT))
                    .map_err(|error| ExportError::Encode(format!("resident source retirement: {error}")))?;
            }
        }
        drop(resident_sources);
        note_stage(Stage::Render, render_started.elapsed());

        let (adapter, device, queue) = resources.renderer.wgpu_handles();
        let source = output_guard.buffer.as_ref().ok_or_else(|| ExportError::Encode("resident output buffer was lost".into()))?;
        let _packet_workspace_allocation = reservation.take(self.packet_capacity)?;
        self.quarantined = true;
        let packets = timed(Stage::Encode, || {
            resources.encoder.encode_gpu_nv12(
                adapter,
                device,
                queue,
                source,
                layout.y_stride,
                layout.uv_stride,
                layout.uv_offset,
                layout.size,
                TRANSFER_TIMEOUT,
            )
        })?;
        let packet_bytes = packets
            .iter()
            .try_fold(0_u64, |total, packet| total.checked_add(u64::try_from(packet.data.len()).ok()?))
            .ok_or_else(|| ExportError::MemoryPressure("resident packet byte count overflow".into()))?;
        if packet_bytes > self.packet_capacity {
            return Err(ExportError::MemoryPressure("resident encoder exceeded its reserved packet capacity".into()));
        }
        output_guard.retire();
        self.quarantined = false;
        Ok(packets)
    }

    fn quarantine_resources(&mut self) {
        self.quarantined = true;
        if let Some(resources) = self.resources.take() {
            std::mem::forget(resources);
        }
        std::mem::forget(std::mem::take(&mut self.cursors));
    }
}

fn prepare_source(project: &Project, request: ResidentSourceRequest, source: Arc<dyn CompressedVideoSource>) -> Result<PreparedSource> {
    if source.sample_count() == 0 || source.timescale() == 0 {
        return Err(ExportError::Unsupported("resident H.264 source has no samples or timescale".into()));
    }
    let target_index = source
        .seek(request.source_time)
        .map_err(|error| ExportError::Unsupported(format!("resident H.264 source seek: {error}")))?
        .ok_or_else(|| ExportError::Unsupported("resident H.264 source time has no sample".into()))?;
    if target_index >= source.sample_count() {
        return Err(ExportError::Unsupported("resident H.264 source seek returned an out-of-range sample".into()));
    }
    let target = source.sample_metadata(target_index).map_err(|error| ExportError::Unsupported(error.to_string()))?;
    if target.index != target_index {
        return Err(ExportError::Unsupported("resident source metadata index mismatch".into()));
    }
    let entry = source.sample_entry(target_index).map_err(|error| ExportError::Unsupported(error.to_string()))?.clone();
    let mut info = NalStreamInfo::from_entry(&entry)
        .ok_or_else(|| ExportError::Unsupported("resident input is not H.264".into()))?
        .map_err(|error| ExportError::Unsupported(error.to_string()))?;
    if info.codec != NalCodec::H264 || info.bit_depth_luma != 8 || info.bit_depth_chroma != 8 || info.chroma_format_idc != 1 || info.interlaced {
        return Err(ExportError::Unsupported("resident H.264 input must be progressive 8-bit 4:2:0".into()));
    }
    let (bound_width, bound_height) = (align_up(request.expected_size.0, 64)?, align_up(request.expected_size.1, 64)?);
    if request.expected_size.0 == 0
        || request.expected_size.1 == 0
        || !request.expected_size.0.is_multiple_of(2)
        || !request.expected_size.1.is_multiple_of(2)
        || info.crop.2 != request.expected_size.0
        || info.crop.3 != request.expected_size.1
        || info.coded.0 > bound_width
        || info.coded.1 > bound_height
        || info.coded.0 > 16_384
        || info.coded.1 > 16_384
    {
        return Err(ExportError::Unsupported("resident H.264 coded/cropped dimensions exceed their reserved bound".into()));
    }
    let surface_bound = linear_nv12_object_size_bound(request.expected_size.0, request.expected_size.1, 64).map_err(ExportError::Unsupported)?;
    let source_color = project
        .item(request.item_id)
        .and_then(|item| item.as_media())
        .and_then(|media| media.info.video.as_ref())
        .map(|video| video.color)
        .unwrap_or(info.color);
    info.color = source_color;
    Ok(PreparedSource { request, source, target_index, target, entry, info, surface_bound, color: source_color })
}

/// Feed the sample after the cursor's last one; `None` when the decoder produced no requested picture.
/// `submitted` tells the caller whether VA work was sent (its ownership is then unproven on error).
fn feed_next(cursor: &mut DecodeCursor, input: &PreparedSource, submitted: &mut bool) -> Result<Option<ResidentDecodedFrame>> {
    let metadata = input.source.sample_metadata(input.target_index).map_err(|error| ExportError::Unsupported(error.to_string()))?;
    if metadata.size > MAX_COMPRESSED_SAMPLE_BYTES {
        return Err(ExportError::Unsupported("resident H.264 access unit exceeds 64 MiB".into()));
    }
    let sample = input.source.read_sample(input.target_index).map_err(|error| ExportError::Unsupported(error.to_string()))?;
    if u64::try_from(sample.len()).ok() != Some(u64::from(metadata.size)) {
        return Err(ExportError::Unsupported("resident H.264 sample size changed after metadata query".into()));
    }
    let mut sink = TargetFrameSink { pts: input.target.pts, output: None };
    *submitted = true;
    cursor.decoder.decode(&sample, metadata.pts, &mut sink).map_err(ExportError::Encode)?;
    cursor.last_fed = input.target_index;
    Ok(sink.output.take())
}

/// Whether a decoder's surfaces, which stay allocated between frames, fit twice into the host headroom.
fn cursor_fits(input: &PreparedSource) -> bool {
    let Some(bytes) = input.surface_bound.checked_mul(vulkan_buffer_probe::MAX_VA_DECODE_SURFACES) else { return false };
    runtime_headroom_bytes().is_some_and(|headroom| headroom >= bytes.saturating_mul(2))
}

fn decode_target(
    node: vaapi::DrmNodeId,
    input: &PreparedSource,
    cursors: &mut Vec<DecodeCursor>,
    dpb_allocation: ResidentAllocation,
) -> Result<(ResidentDecodedFrame, ResidentAllocation)> {
    let item = input.request.item_id;
    // Clips of one media item may sit at different positions in a frame (a transition), so match by position.
    let next = input.target_index;
    let matched = cursors
        .iter()
        .position(|cursor| cursor.item == item && input.info.reorder == 0 && cursor.entry == input.entry && cursor.last_fed.saturating_add(1) == next);
    if let Some(mut cursor) = matched.map(|index| cursors.swap_remove(index)) {
        let mut submitted = false;
        match catch_unwind(AssertUnwindSafe(|| feed_next(&mut cursor, input, &mut submitted))) {
            Ok(Ok(Some(frame))) => {
                cursor.used = true;
                cursors.push(cursor);
                return Ok((frame, dpb_allocation));
            }
            // Submitted work yielded no picture: its VA ownership is unproven, so leak it and decode afresh.
            Ok(Ok(None)) => std::mem::forget(cursor),
            // Failed before any VA submission: nothing is in flight, so everything drops cleanly.
            Ok(Err(error)) if !submitted => return Err(error),
            Ok(Err(error)) => {
                std::mem::forget((cursor, dpb_allocation));
                return Err(error);
            }
            Err(_) => {
                std::mem::forget((cursor, dpb_allocation));
                return Err(ExportError::Encode("resident VA decoder panicked; surface lifetime remains uncertain".into()));
            }
        }
    }
    let mut first_sync = input.target_index;
    let mut preroll = 0usize;
    loop {
        let metadata = input.source.sample_metadata(first_sync).map_err(|error| ExportError::Unsupported(error.to_string()))?;
        if metadata.is_sync {
            break;
        }
        if first_sync == 0 || preroll >= MAX_PREROLL_SAMPLES {
            return Err(ExportError::Unsupported("resident H.264 GOP preroll exceeds its bound".into()));
        }
        first_sync -= 1;
        preroll += 1;
    }
    let after_target = input.info.reorder.saturating_add(2);
    let last = input.target_index.saturating_add(after_target).min(input.source.sample_count() - 1);
    for index in first_sync..=last {
        let metadata = input.source.sample_metadata(index).map_err(|error| ExportError::Unsupported(error.to_string()))?;
        if input.source.sample_entry(index).map_err(|error| ExportError::Unsupported(error.to_string()))? != &input.entry {
            return Err(ExportError::Unsupported("resident H.264 sample description changes inside the GOP".into()));
        }
        if index != input.target_index && metadata.pts == input.target.pts {
            return Err(ExportError::Unsupported("resident H.264 target PTS is duplicated inside its preroll".into()));
        }
    }
    let mut decoder = vaapi::ResidentVaDecoder::new_linear_diagnostic(input.info.clone(), node).map_err(ExportError::Unsupported)?;
    filmcraft_codecs::hw::note_hw_session();
    let mut sink = TargetFrameSink { pts: input.target.pts, output: None };
    let mut submission_attempted = false;
    let mut last_fed = None;
    let mut flushed = false;
    let output = catch_unwind(AssertUnwindSafe(|| -> Result<ResidentDecodedFrame> {
        for index in first_sync..=last {
            let metadata = input.source.sample_metadata(index).map_err(|error| ExportError::Unsupported(error.to_string()))?;
            if metadata.size > MAX_COMPRESSED_SAMPLE_BYTES {
                return Err(ExportError::Unsupported("resident H.264 access unit exceeds 64 MiB".into()));
            }
            let sample = input.source.read_sample(index).map_err(|error| ExportError::Unsupported(error.to_string()))?;
            if u64::try_from(sample.len()).ok() != Some(u64::from(metadata.size)) {
                return Err(ExportError::Unsupported("resident H.264 sample size changed after metadata query".into()));
            }
            submission_attempted = true;
            decoder.decode(&sample, metadata.pts, &mut sink).map_err(ExportError::Encode)?;
            last_fed = Some(index);
            if sink.output.is_some() {
                break;
            }
        }
        if sink.output.is_none() {
            submission_attempted = true;
            flushed = true;
            decoder.flush(&mut sink).map_err(ExportError::Encode)?;
        }
        sink.output.take().ok_or_else(|| ExportError::Unsupported("resident H.264 preroll did not produce the requested PTS".into()))
    }));
    let frame = match output {
        Ok(Ok(frame)) => frame,
        Ok(Err(error)) => {
            if submission_attempted {
                // Decode/flush may have submitted VA work even without yielding the target lease.
                std::mem::forget((decoder, dpb_allocation));
            }
            return Err(error);
        }
        Err(_) => {
            if submission_attempted {
                std::mem::forget((decoder, dpb_allocation));
            }
            return Err(ExportError::Encode("resident VA decoder panicked; surface lifetime remains uncertain".into()));
        }
    };
    if let (0, false, Some(last_fed)) = (input.info.reorder, flushed, last_fed)
        && cursors.len() < MAX_DECODE_CURSORS
        && cursor_fits(input)
    {
        cursors.push(DecodeCursor { item, used: true, decoder, entry: input.entry.clone(), last_fed });
        return Ok((frame, dpb_allocation));
    }
    drop(decoder);
    Ok((frame, dpb_allocation))
}

fn runtime_headroom_bytes() -> Option<u64> {
    filmcraft_export::runtime_ram_headroom_kib().and_then(|kib| kib.checked_mul(1024))
}

fn align_up(value: u32, alignment: u32) -> Result<u32> {
    if value == 0 || alignment == 0 || !alignment.is_power_of_two() {
        return Err(ExportError::Unsupported("resident H.264 dimensions or alignment are invalid".into()));
    }
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| ExportError::MemoryPressure("resident H.264 coded dimension overflow".into()))
}

fn packet_capacity(coded: (u32, u32)) -> Result<u64> {
    u64::from(coded.0)
        .checked_mul(u64::from(coded.1))
        .and_then(|pixels| pixels.checked_mul(3))
        .map(|bytes| bytes / 2)
        .and_then(|bytes| bytes.checked_add(65_536))
        .ok_or_else(|| ExportError::MemoryPressure("resident H.264 packet bound overflow".into()))
}

//! Physical AMD output-copy probe: synthetic NV12 bytes are uploaded from CPU into a Vulkan buffer,
//! copied into a VA-API input surface, and encoded. The separate decoded-surface probe exercises
//! real VA decode, DMA-BUF import and GPU repack without reading or uploading source pixels.
//! These stage probes do not prove a complete exported movie. Without an AMD adapter they print SKIPPED and return.
#![cfg(target_os = "linux")]

#[path = "it/common/mod.rs"]
mod common;

use std::time::Duration;

use filmcraft_export::{BitrateMode, ExportSettings, Format, HardwareEncoding};
use filmcraft_platform::vaapi::h264enc::VaH264Encoder;
use filmcraft_time::FrameRate;

const TIMEOUT: Duration = Duration::from_secs(5);

fn await_adapter_list<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(adapters) => return adapters,
            Poll::Pending => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                assert!(!remaining.is_zero(), "AMD adapter enumeration timed out");
                std::thread::park_timeout(remaining.min(Duration::from_millis(10)));
            }
        }
    }
}

/// Open the interop device from one physical AMD Vulkan adapter and its exact DRM render node.
/// `None` when this machine has no physical AMD Vulkan adapter (the caller skips).
fn amd_interop_device() -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue, filmcraft_platform::vaapi::DrmNodeId)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = await_adapter_list(instance.enumerate_adapters(wgpu::Backends::VULKAN)).into_iter().find(|adapter| {
        let info = adapter.get_info();
        info.vendor == 0x1002 && matches!(info.device_type, wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu)
    })?;
    let node = filmcraft_platform::vulkan_import::render_node_id(&adapter).expect("AMD Vulkan adapter must expose its DRM render node");
    let (device, queue) = filmcraft_platform::vulkan_interop_device::create_amd_interop_device(&adapter, &wgpu::DeviceDescriptor::default(), node)
        .expect("selected AMD adapter must open with DMA-BUF and foreign queue-family capabilities");
    device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| panic!("GPU error: {error}")));
    Some((adapter, device, queue, node))
}

/// Synthetic coded-size NV12 fixture. CPU upload here does not exercise source decoding.
fn coded_nv12(coded: (u32, u32), y_stride: u32, uv_stride: u32, uv_offset: u64, size: u64) -> Vec<u8> {
    let (width, height) = coded;
    let mut bytes = vec![0u8; usize::try_from(size).expect("NV12 buffer size fits memory")];
    let uv_height = height.div_ceil(2);
    for row in 0..height {
        let start = row as usize * y_stride as usize;
        for column in 0..width {
            bytes[start + column as usize] = 16 + (column % 220) as u8;
        }
    }
    for row in 0..uv_height as usize {
        let start = uv_offset as usize + row * uv_stride as usize;
        for column in (0..uv_stride as usize).step_by(2) {
            bytes[start + column] = 128;
            bytes[start + column + 1] = 128;
        }
    }
    bytes
}

/// Packed layout produced by the GPU NV12 output stage: aligned rows, chroma after luma.
fn layout(coded: (u32, u32)) -> (u32, u32, u64, u64) {
    let (width, height) = coded;
    let y_stride = (width + 255) & !255;
    let uv_stride = ((width.div_ceil(2) * 2) + 255) & !255;
    let uv_offset = u64::from(y_stride) * u64::from(height);
    let size = uv_offset + u64::from(uv_stride) * u64::from(height.div_ceil(2));
    (y_stride, uv_stride, uv_offset, size)
}

fn hw_settings() -> ExportSettings {
    ExportSettings {
        format: Format::H264,
        hardware_encoding: HardwareEncoding::Auto,
        bitrate_mode: BitrateMode::Vbr1Pass,
        bitrate_kbps: 8_000,
        ..Default::default()
    }
}

/// CPU upload creates only a synthetic test frame; production output buffers are GPU-produced.
fn picture_buffer(device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resident-nv12-synthetic-fixture"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytes);
    buffer
}

#[test]
fn synthetic_gpu_nv12_output_copy_encodes_two_frames_on_amd() {
    let Some((adapter, device, queue, node)) = amd_interop_device() else {
        eprintln!("SKIPPED: no physical AMD Vulkan adapter");
        return;
    };
    eprintln!("adapter: {:?}; DRM node: {node:?}", adapter.get_info());
    let mut encoder = VaH264Encoder::resident_on_node(node, 1920, 1080, FrameRate::FPS_30, &hw_settings())
        .expect("H.264 hardware settings must select the VA-API output path")
        .expect("AMD VA-API H.264 encoder must be available on the selected DRM node");
    let coded = encoder.coded_size();
    assert_eq!(coded, (1920, 1088), "1080 lines round up to whole macroblocks");

    let (y_stride, uv_stride, uv_offset, size) = layout(coded);
    let bytes = coded_nv12(coded, y_stride, uv_stride, uv_offset, size);
    let buffer = picture_buffer(&device, &queue, &bytes);

    let first = encoder.encode_gpu_nv12(&adapter, &device, &queue, &buffer, y_stride, uv_stride, uv_offset, size, TIMEOUT).expect("first GPU picture");
    assert_eq!(first.len(), 1, "one picture, one packet");
    assert!(first[0].key, "the first picture is an IDR");
    assert!(first[0].data.len() > 4, "the driver wrote coded bytes");
    let nal_len = u32::from_be_bytes(first[0].data[..4].try_into().expect("a 32-bit NAL length prefix")) as usize;
    assert!(nal_len >= 1 && 4 + nal_len <= first[0].data.len(), "the sample is a length-prefixed NAL unit");
    assert_eq!(first[0].data[4] & 0x1f, 5, "the first slice is an IDR slice");

    let second = encoder.encode_gpu_nv12(&adapter, &device, &queue, &buffer, y_stride, uv_stride, uv_offset, size, TIMEOUT).expect("second GPU picture");
    assert_eq!(second.len(), 1, "one picture, one packet");
    assert!(!second[0].key, "the second picture follows the first");
    let nal_len = u32::from_be_bytes(second[0].data[..4].try_into().expect("a 32-bit NAL length prefix")) as usize;
    assert!(nal_len >= 1 && 4 + nal_len <= second[0].data.len(), "the second sample is a length-prefixed NAL unit");
    assert_eq!(second[0].data[4] & 0x1f, 1, "the second slice is a non-IDR slice");
}

#[test]
fn short_synthetic_nv12_buffer_is_rejected_on_amd() {
    let Some((adapter, device, queue, node)) = amd_interop_device() else {
        eprintln!("SKIPPED: no physical AMD Vulkan adapter");
        return;
    };
    eprintln!("adapter: {:?}; DRM node: {node:?}", adapter.get_info());
    let mut encoder = VaH264Encoder::resident_on_node(node, 1920, 1080, FrameRate::FPS_30, &hw_settings())
        .expect("H.264 hardware settings must select the VA-API output path")
        .expect("AMD VA-API H.264 encoder must be available on the selected DRM node");
    let coded = encoder.coded_size();
    let (y_stride, uv_stride, uv_offset, size) = layout(coded);
    let bytes = coded_nv12(coded, y_stride, uv_stride, uv_offset, size);
    let buffer = picture_buffer(&device, &queue, &bytes[..bytes.len() - 4096]);
    match encoder.encode_gpu_nv12(&adapter, &device, &queue, &buffer, y_stride, uv_stride, uv_offset, size, TIMEOUT) {
        Ok(packets) => panic!("a buffer shorter than the NV12 layout encoded {} packets", packets.len()),
        Err(error) => {
            let message = format!("{error}");
            assert!(message.contains("VA-API H.264 GPU input"), "actionable error: {message}");
        }
    }
}

#[test]
fn decoded_va_surface_repackages_on_gpu_without_cpu_pixels() {
    use filmcraft_codecs::hw::NalStreamInfo;
    use filmcraft_platform::vaapi::{ResidentDecodedFrame, ResidentFrameSink, ResidentVaDecoder};

    #[derive(Default)]
    struct Capture(Option<ResidentDecodedFrame>);
    impl ResidentFrameSink for Capture {
        type Admission = ();
        fn reserve(&mut self, _: filmcraft_platform::vaapi::ResidentFrameMetadata) -> Result<(), String> {
            if self.0.is_some() { Err("probe already owns its one decoded frame".into()) } else { Ok(()) }
        }
        fn submit(&mut self, _: (), frame: ResidentDecodedFrame) -> Result<(), String> {
            self.0 = Some(frame);
            Ok(())
        }
    }

    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = common::named(&ff, "h264_high.mp4").expect("compressed H.264 fixture");
    let stream = common::read_stream(&path);
    let info = NalStreamInfo::from_entry(&stream.entry).expect("sample entry").expect("H.264 stream");
    let (sample, pts) = stream.samples.iter().find(|(sample, _)| info.nal_types(sample).contains(&5)).expect("IDR sample");
    let Some((adapter, device, queue, node)) = amd_interop_device() else {
        eprintln!("SKIPPED: no physical AMD Vulkan adapter");
        return;
    };
    let mut decoder = ResidentVaDecoder::new_linear_diagnostic(info, node).expect("strict VA decoder");
    let mut capture = Capture::default();
    decoder.decode(sample, *pts, &mut capture).expect("decode IDR without CPU pixels");
    decoder.flush(&mut capture).expect("flush VA decoder");
    let frame = capture.0.take().expect("decoded surface lease");
    let crop = frame.crop;
    let (object_bytes, _) = frame.lease.object_layout(0).expect("DMA-BUF object");
    let layout = filmcraft_gpu::yuv_output::Nv12Layout::new(crop.2, crop.3).expect("visible NV12 layout");
    let repack_bytes = filmcraft_gpu::nv12_repack::Nv12Repack::allocation_bytes(&layout).expect("repack budget");
    let total = object_bytes.checked_mul(2).and_then(|n| n.checked_add(repack_bytes)).expect("aggregate budget");
    let headroom = filmcraft_export::runtime_ram_headroom_kib().and_then(|n| n.checked_mul(1024));
    let mut reservation = filmcraft_export::ResidentReservation::try_acquire(headroom, None, 0, total).expect("reserve before import");
    let repacker = filmcraft_gpu::nv12_repack::Nv12Repack::new(&device);
    let mut resident =
        filmcraft_platform::vulkan_buffer_probe::import_and_repack_va_nv12_frame(&adapter, &device, &queue, &repacker, frame, &mut reservation, TIMEOUT)
            .expect("import, acquire, copy, release and repack entirely on GPU");
    assert_eq!(resident.drm_node_id(), Some(node));
    let repacked = resident.repacked().expect("GPU-owned NV12 planes");
    assert_eq!(repacked.y.format(), wgpu::TextureFormat::R8Unorm);
    assert_eq!(repacked.uv.format(), wgpu::TextureFormat::Rg8Unorm);
    assert_eq!((repacked.y.width(), repacked.y.height()), (crop.2, crop.3));
    let completion = resident.submit_consumer_commands([]).expect("same-queue completion fence");
    resident.retire_after_gpu_submission(completion, TIMEOUT).expect("exact submission retirement");
}

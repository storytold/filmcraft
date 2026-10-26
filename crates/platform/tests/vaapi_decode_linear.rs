//! Ignored physical probe for exact-node H.264 decode into a single-object NV12 LINEAR VA surface.
#![cfg(target_os = "linux")]

#[path = "it/common/mod.rs"]
mod common;

use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_platform::vaapi::ffi;
use filmcraft_platform::vaapi::va::Display;
use filmcraft_platform::vaapi::{ResidentDecodedFrame, ResidentFrameSink, ResidentVaDecoder};

#[derive(Default)]
struct Capture(Vec<ResidentDecodedFrame>);

impl ResidentFrameSink for Capture {
    type Admission = ();

    fn reserve(&mut self, _: filmcraft_platform::vaapi::ResidentFrameMetadata) -> Result<(), String> {
        Ok(())
    }

    fn submit(&mut self, _: (), frame: ResidentDecodedFrame) -> Result<(), String> {
        self.0.push(frame);
        Ok(())
    }
}

#[test]
#[ignore = "requires physical VA-API H.264 decode and PRIME_2 export support"]
fn h264_decode_exports_linear_nv12_lease_on_selected_node() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = common::named(&ff, "h264_high.mp4").expect("generate the H.264 compressed-byte fixture");
    let stream = common::read_stream(&path);
    let info = NalStreamInfo::from_entry(&stream.entry).expect("parse H.264 sample entry").expect("H.264 stream info");
    let (sample, pts) = stream.samples.iter().find(|(sample, _)| info.nal_types(sample).contains(&5)).expect("fixture contains an IDR access unit");
    let display = Display::open().expect("open a VA-API render node");
    let node = display.drm_node_id().expect("read selected render-node identity");
    drop(display);

    let mut decoder = ResidentVaDecoder::new_linear_diagnostic(info, node).expect("allocate strict NV12 LINEAR decode surfaces on selected node");
    let mut capture = Capture::default();
    decoder.decode(sample, *pts, &mut capture).expect("parse compressed bytes and decode on VA");
    decoder.flush(&mut capture).expect("flush resident decoder");
    assert!(!capture.0.is_empty(), "IDR decode produced a resident lease");

    let frame = capture.0.first().expect("resident frame");
    let lease = &frame.lease;
    assert_eq!(lease.drm_node_id(), Some(node), "exported lease stays on selected DRM node");
    assert_eq!(lease.fourcc(), Some(ffi::VA_FOURCC_NV12), "export is NV12");
    assert_eq!(lease.object_count(), Some(1), "export uses one DMA-BUF object");
    let (object_size, object_modifier) = lease.object_layout(0).expect("validated single-object metadata");
    assert!(object_size > 0, "exported object has nonzero size");
    assert_eq!(object_modifier, ffi::DRM_FORMAT_MOD_LINEAR, "requested LINEAR modifier was selected");

    let (width, height) = lease.size().expect("coded surface dimensions");
    assert!(width > 0 && height > 0 && width.is_multiple_of(2) && height.is_multiple_of(2));
    assert_eq!(lease.layer_count(), Some(2), "NV12 uses two separate layers");
    let expected = [(ffi::DRM_FORMAT_R8, width, height, 1u64), (ffi::DRM_FORMAT_GR88, width / 2, height / 2, 2u64)];
    let mut ends = [0u64; 2];
    for (layer, (format, plane_width, plane_height, pixel_bytes)) in expected.into_iter().enumerate() {
        assert_eq!(lease.plane_count(layer), Some(1), "each NV12 layer has one plane");
        let plane = lease.plane(layer, 0).expect("validated exported plane");
        assert_eq!(plane.object_index, 0, "every NV12 plane shares object zero");
        assert_eq!(plane.modifier, ffi::DRM_FORMAT_MOD_LINEAR, "every plane is LINEAR");
        assert_eq!(plane.layer_format, format, "expected DRM layer format");
        assert_eq!(u64::from(plane.object_size), object_size, "plane reports the exported object size");
        let row_bytes = u64::from(plane_width).checked_mul(pixel_bytes).expect("row size");
        let offset = u64::from(plane.offset);
        let pitch = u64::from(plane.pitch);
        assert!(pitch >= row_bytes && pitch % pixel_bytes == 0 && offset % pixel_bytes == 0, "plane pitch and offset are valid");
        ends[layer] = pitch
            .checked_mul(u64::from(plane_height - 1))
            .and_then(|span| offset.checked_add(span))
            .and_then(|start| start.checked_add(row_bytes))
            .expect("plane end");
        assert!(ends[layer] <= object_size, "plane footprint stays inside the object");
    }
    let y_offset = u64::from(lease.plane(0, 0).expect("Y plane").offset);
    let uv_offset = u64::from(lease.plane(1, 0).expect("UV plane").offset);
    assert!(y_offset >= ends[1] || uv_offset >= ends[0], "NV12 planes do not overlap");
    // The capture retains the lease through metadata checks. Dropping it uses the existing
    // quarantine path because this probe performs no Vulkan submission or GPU import.
}

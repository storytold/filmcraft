use filmcraft_frame::{Chroma, GpuPixels, GpuSurface, PixelData, VideoFrame};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const FAILURE: &str = "injected device loss";

#[derive(Debug, Default)]
struct LostSurface(AtomicUsize);

impl GpuSurface for LostSurface {
    fn size(&self) -> (u32, u32) {
        (4, 4)
    }
    fn byte_len(&self) -> usize {
        4 * 4 + 2 * 2 * 2
    }
    fn download(&self) -> Result<PixelData, String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(FAILURE.into())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn id(&self) -> u64 {
        1
    }
}

fn frame() -> (VideoFrame, Arc<LostSurface>) {
    let surface = Arc::new(LostSurface::default());
    (
        VideoFrame {
            width: 4,
            height: 4,
            data: PixelData::Gpu(GpuPixels::new(surface.clone(), Chroma::C420, 8)),
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        },
        surface,
    )
}

// Debug assertion compiles against both infallible baseline and fallible candidate APIs.
fn rejects_pixels(result: impl std::fmt::Debug) {
    assert_eq!(format!("{result:?}"), format!("Err({FAILURE:?})"));
}

#[test]
fn materialization_reports_device_loss() {
    let (frame, surface) = frame();
    rejects_pixels(frame.cpu());
    rejects_pixels(frame.clone().cpu());
    assert_eq!(surface.0.load(Ordering::Relaxed), 1);
}

#[test]
fn display_reports_device_loss() {
    let (frame, surface) = frame();
    rejects_pixels(frame.to_rgba8());
    rejects_pixels(frame.luma8());
    assert_eq!(surface.0.load(Ordering::Relaxed), 1);
}

#[test]
fn compositor_conversion_reports_device_loss() {
    let (frame, surface) = frame();
    rejects_pixels(frame.to_linear_f32());
    rejects_pixels(frame.to_linear_f32_decimated(2));
    rejects_pixels(frame.to_linear_f32_region(1, None, filmcraft_frame::Region { x: 0, y: 0, w: 2, h: 2 }));
    rejects_pixels(frame.rotated(1));
    assert_eq!(surface.0.load(Ordering::Relaxed), 1);
}

#[test]
fn concurrent_consumers_share_one_failure() {
    let (frame, surface) = frame();
    let consumers = ["preview", "export", "scopes", "thumbnail"];
    let start = std::sync::Barrier::new(consumers.len());
    std::thread::scope(|scope| {
        for _consumer in consumers {
            let frame = frame.clone();
            let start = &start;
            scope.spawn(move || {
                start.wait();
                rejects_pixels(frame.cpu());
            });
        }
    });
    assert_eq!(surface.0.load(Ordering::Relaxed), 1);
}

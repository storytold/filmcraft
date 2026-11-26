use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use filmcraft_color::{ColorInfo, ColorSpace, Primaries, Transfer};
use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project};
use filmcraft_time::{FrameRate, Tick};

use crate::plan::{DeviceCaps, FramePlan, execute_cpu, plan_item};

struct PhoneSource {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
    turns: u8,
    cpu_turns: AtomicUsize,
}

impl MediaSource for PhoneSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.cpu_turns.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(self.frame.rotated(self.turns)))
    }

    fn video_frame_unturned(&self, _: FrameRequest) -> filmcraft_media::Result<(Arc<VideoFrame>, u8)> {
        Ok((self.frame.clone(), self.turns))
    }

    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<AudioBuffer> {
        Err(filmcraft_media::MediaError::NoStream("audio"))
    }
}

fn source(turns: u8, hdr: bool) -> Arc<PhoneSource> {
    let (w, h) = (16u32, 12u32);
    let color = if hdr { ColorInfo { transfer: Transfer::Hlg, primaries: Primaries::Bt2020, ..ColorInfo::REC709 } } else { ColorInfo::REC709 };
    let frame = Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv8 {
            planes: [
                Arc::new((0..w * h).map(|i| (32 + i % 180) as u8).collect()),
                Arc::new(vec![128; (w * h / 4) as usize]),
                Arc::new(vec![128; (w * h / 4) as usize]),
            ],
            chroma: Chroma::C420,
            alpha: None,
        },
        color,
        par: (1, 1),
        pts: Tick::ZERO,
    });
    let (w, h) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let info = GeneratorSource::new(Generator::BlackVideo, w, h, FrameRate::FPS_60, Tick::from_seconds_f64(1.0)).info().clone();
    Arc::new(PhoneSource { info, frame, turns, cpu_turns: AtomicUsize::new(0) })
}

fn project(src: &PhoneSource) -> (Project, filmcraft_project::ItemId) {
    let mut p = Project::new("Source preview");
    let item = p.add_item(
        "Phone",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(Generator::BlackVideo),
            info: src.info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    (p, item)
}

#[test]
fn source_preview_keeps_decoded_planes_and_turns_the_picture_on_the_gpu() {
    for turns in 0..4 {
        for hdr in [false, true] {
            let src = source(turns, hdr);
            let (p, item) = project(&src);
            let provider = |_| Some(src.clone() as filmcraft_media::SharedSource);
            let plan = plan_item(&p, item, Tick::ZERO, 1.0, DeviceCaps::NONE, &provider).unwrap();
            let FramePlan::Layers { layers, .. } = &plan else { panic!("the source should stay on the GPU") };
            assert!(Arc::ptr_eq(&layers[0].frame, &src.frame), "no full-size plane copy or conversion");
            assert_eq!(src.cpu_turns.load(Ordering::Relaxed), 0, "no CPU rotation");
            assert_eq!(layers[0].input.is_some(), hdr, "HDR retains the SDR input transform");
            let reference = crate::render_item(&p, item, Tick::ZERO, 1.0, &provider).unwrap();
            let image = execute_cpu(&plan);
            assert_eq!((image.w, image.h), (reference.w, reference.h));
            assert!(image.px.iter().zip(&reference.px).all(|(a, b)| (a - b).abs() < 1e-4), "turn {turns}, HDR {hdr}");

            let small = execute_cpu(&plan_item(&p, item, Tick::ZERO, 0.5, DeviceCaps::NONE, &provider).unwrap());
            assert!(small.w < image.w && small.h < image.h, "playback resolution reduces both dimensions");
        }
    }
}

#[test]
fn source_preview_preserves_cpu_colour_management_when_the_gpu_cannot_run_it() {
    let src = source(1, false);
    let (mut p, item) = project(&src);
    let ItemKind::Media(media) = &mut p.item_mut(item).unwrap().kind else { panic!("media") };
    media.interpret.color_space = Some(ColorSpace::AppleLog);
    let provider = |_| Some(src.clone() as filmcraft_media::SharedSource);
    let plan = plan_item(&p, item, Tick::ZERO, 0.5, DeviceCaps::NONE, &provider).unwrap();
    let FramePlan::Image(image) = plan else { panic!("log conversion must retain its CPU fallback") };
    assert!(src.cpu_turns.load(Ordering::Relaxed) > 0);
    assert_eq!(image, crate::render_item(&p, item, Tick::ZERO, 0.5, &provider).unwrap());
}

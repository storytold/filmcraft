//! HLG → SDR is one conversion for Source, Program, the layer plan and exported files.
//! Procedural 10-bit patches only; no phone recordings or reference images are shipped.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, ColorPipeline, Matrix, Primaries, Range, Transfer, WorkingSpace};
use filmcraft_export::{ExportSettings, Format, GpuRendering, Progress, export, rgbf_to_yuv420_10};
use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::{RenderOptions, SourceMap, render_item, render_sequence};
use filmcraft_time::{FrameRate, Tick, TimeRange};

const W: u32 = 256;
const H: u32 = 32;

struct HlgPatches {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
}

impl MediaSource for HlgPatches {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Ok(self.frame.clone())
    }

    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Err(filmcraft_media::MediaError::NoStream("audio"))
    }
}

fn project() -> (Project, ItemId, ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let duration = rate.tick_of(3);
    let patches = [[0.0; 3], [0.25; 3], [0.5; 3], [0.75; 3], [1.0; 3], [0.8, 0.4, 0.2], [0.2, 0.9, 0.4], [0.2, 0.4, 0.8]];
    let rgb: Vec<f32> = (0..W * H).flat_map(|i| patches[(i % W / 32) as usize]).collect();
    let (mut y, mut cb, mut cr) = (Vec::new(), Vec::new(), Vec::new());
    rgbf_to_yuv420_10(&rgb, W as usize, H as usize, filmcraft_export::Matrix::new(0.2627, 0.0593), &mut y, &mut cb, &mut cr).unwrap();
    let color = ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Hlg, primaries: Primaries::Bt2020, range: Range::Limited };
    let frame = Arc::new(VideoFrame {
        width: W,
        height: H,
        data: PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], bits: 10, chroma: Chroma::C420, alpha: None },
        color,
        par: (1, 1),
        pts: Tick::ZERO,
    });
    let generator = Generator::BlackVideo;
    let mut info = GeneratorSource::new(generator.clone(), W, H, rate, duration).info().clone();
    info.audio_streams.clear();
    let v = info.video.as_mut().unwrap();
    v.color = color;
    v.pixel_format = "YUV 4:2:0 10-bit".into();
    let mut p = Project::new("HLG SDR regression");
    let item = p.add_item(
        "HLG patches",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(generator),
            info: info.clone(),
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
    let seq = p.new_sequence("SDR", SequenceSettings { width: W, height: H, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let clip = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, duration), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
    let mut map = SourceMap::default();
    map.0.insert(item, Arc::new(HlgPatches { info, frame }));
    (p, item, seq, map)
}

fn tmp(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("fc-hlg-sdr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).to_string_lossy().into_owned()
}

#[test]
fn source_program_layer_plan_and_sdr_exports_agree() {
    let (mut p, item, seq, map) = project();
    let source = render_item(&p, item, Tick::ZERO, 1.0, &map).unwrap().over_black_rgba8();
    let program = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map).over_black_rgba8();
    assert_eq!(source, program, "Source and Program: one SDR transform");
    // HLG 50% / 75% map to ~38% / 60% sRGB (the OOTF default, matching Apple's own
    // conversion), rather than the brighter ~54% / 90% of the old 1000-nit mapping.
    assert!(source[(16 * W as usize + 80) * 4].abs_diff(98) <= 1);
    assert!(source[(16 * W as usize + 112) * 4].abs_diff(154) <= 1);
    let plan = filmcraft_render::plan::plan_frame(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    let filmcraft_render::plan::FramePlan::Layers { layers, .. } = &plan else { panic!("HLG keeps the GPU layer path") };
    assert!(layers[0].input.is_some());
    assert_eq!(filmcraft_render::plan::execute_cpu(&plan).over_black_rgba8(), program, "layer plan: same conversion");

    for working in [WorkingSpace::Rec709, WorkingSpace::Rec2100Hlg] {
        p.sequence_mut(seq).unwrap().settings.color = ColorPipeline { working, ..ColorPipeline::REC709 };
        let p = Arc::new(p.clone());
        let preview = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map).over_black_rgba8();
        assert_eq!(preview, program, "{working:?}: working → monitor maps once");
        if working.is_hdr() {
            let hdr = render_sequence(&p, seq, Tick::ZERO, RenderOptions { working_output: true, ..Default::default() }, &map);
            assert!(hdr.get(144, 16)[0] > 4.9, "HDR export keeps highlights above reference white");
        }
        let path = tmp(&format!("{}.png", working.id()));
        let settings = ExportSettings { format: Format::PngSequence, path, include_audio: false, gpu_rendering: GpuRendering::Off, ..Default::default() };
        let report = export(&p, seq, &settings, &map, &Progress::default()).unwrap();
        assert_eq!(report.frames, 3);
        let png = image::open(tmp(&format!("{}000.png", working.id()))).unwrap().into_rgba8();
        assert_eq!(png.into_raw(), preview, "{working:?}: PNG export equals preview");

        for (format, extension) in [(Format::ProRes, "mov"), (Format::H264, "mp4")] {
            let path = tmp(&format!("{}-{}.{extension}", working.id(), format.id()));
            let settings = ExportSettings {
                format,
                path: path.clone(),
                sdr: true,
                include_audio: false,
                bitrate_kbps: 20_000,
                gpu_rendering: GpuRendering::Off,
                ..Default::default()
            };
            export(&p, seq, &settings, &map, &Progress::default()).unwrap();
            let src = filmcraft_codecs::open_bytes(&path, std::fs::read(&path).unwrap().into()).unwrap();
            let frame = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
            // ProRes carries colour in its frame header; the container-only stream-info
            // probe can guess SD colour from these deliberately tiny test dimensions.
            assert_eq!(frame.color, ColorInfo::REC709, "{path}: decoded frame colour signalling");
            let decoded = frame.to_rgba8();
            // Sample the interiors of the constant patches to exclude lossy chroma edges.
            for x in (16..W as usize).step_by(32) {
                let offset = (16 * W as usize + x) * 4;
                for k in 0..3 {
                    assert!(decoded[offset + k].abs_diff(preview[offset + k]) <= 3, "{working:?} {format:?} patch {x}: decoded != preview");
                }
            }
            if let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("HLG SDR export") {
                let out = std::process::Command::new(ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-select_streams",
                        "v:0",
                        "-show_entries",
                        "stream=color_transfer,color_primaries,color_space",
                        "-of",
                        "default=nw=1",
                        &path,
                    ])
                    .output()
                    .unwrap();
                assert!(out.status.success());
                let text = String::from_utf8_lossy(&out.stdout);
                for tag in ["color_transfer=bt709", "color_primaries=bt709", "color_space=bt709"] {
                    assert!(text.contains(tag), "{path}: missing {tag}: {text}");
                }
            }
            if let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("HLG SDR export decode") {
                let out = std::process::Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i", &path, "-f", "null", "-"]).output().unwrap();
                assert!(out.status.success(), "{path}: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
    }

    // The broadcast choice (Sequence ▸ Color Settings ▸ HLG to SDR = BT.2446 A) keeps the
    // Method A conversion on the same path: 50% / 75% land at ~39% / 67% sRGB.
    p.sequence_mut(seq).unwrap().settings.color = ColorPipeline { hlg_sdr: filmcraft_color::HlgSdr::Bt2446, ..ColorPipeline::REC709 };
    let broadcast = render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &map).over_black_rgba8();
    assert!(broadcast[(16 * W as usize + 80) * 4].abs_diff(100) <= 1);
    assert!(broadcast[(16 * W as usize + 112) * 4].abs_diff(171) <= 1);
}

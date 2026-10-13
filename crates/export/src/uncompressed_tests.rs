//! Uncompressed QuickTime export (`Format::Uncompressed`): the samples the file holds are the
//! picture's own samples, so its size is the layout's, our own decoder reads the picture back, and
//! ffmpeg — an external oracle, never linked — agrees on the codec tag, the pixel format and the
//! pixels.

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::TICKS_PER_SECOND;

/// A 1 s, 24 fps, 320×180 sequence with a red matte on V1 (the same one `tests::{project, tmp}`
/// build, which are private to that module).
fn project() -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("unc");
    let g = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, 320, 180, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let red = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
    let r = FrameRate::FPS_24;
    let v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(red, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-export-unc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// One case per pixel layout: the fourcc the sample entry must carry and the codec name our
/// decoder reports for it.
const LAYOUTS: [(UncompressedPixelFormat, &str, &str, &str); 3] = [
    (UncompressedPixelFormat::Uyvy8, "2vuy", "Uncompressed 8-bit 4:2:2", "uyvy422"),
    (UncompressedPixelFormat::V210, "v210", "Uncompressed 10-bit 4:2:2", "yuv422p10le"),
    (UncompressedPixelFormat::Rgb24, "raw ", "Uncompressed RGB 24-bit", "rgb24"),
];

/// `ExportSettings` for one layout at `(w, h)` — 24 frames of the red matte, no audio.
fn settings(layout: UncompressedPixelFormat, path: &str, size: (u32, u32)) -> ExportSettings {
    ExportSettings {
        format: Format::Uncompressed,
        uncompressed_pixel_format: layout,
        frame_size: Some(size),
        include_audio: false,
        path: path.into(),
        ..Default::default()
    }
}

#[test]
fn uncompressed_quicktime_export_roundtrip() {
    let (p, seq, m) = project();
    for (layout, tag, codec, _) in LAYOUTS {
        let name = format!("unc_{}.mov", layout.label().replace([' ', ':'], "_"));
        let path = tmp(&name);
        let (w, h) = (320u32, 180u32);
        let r = export(&p, seq, &settings(layout, &path, (w, h)), &m, &Progress::default()).unwrap();
        assert_eq!(r.frames, 24);
        // one sample a frame, exactly the layout's bytes: v210 pads every row to a multiple of 48
        // pixels (320 pixels are 336 wide, 896 bytes), the others are tight
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
        let track = file.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Video).unwrap();
        assert_eq!(std::str::from_utf8(&track.entries[0].format.0).unwrap(), tag);
        assert_eq!(track.entries[0].video.as_ref().unwrap().depth, 24);
        assert_eq!(track.samples.len(), 24, "{layout:?}");
        assert_eq!(track.samples[0].size as usize, layout.frame_bytes(w, h), "{layout:?}");
        assert!(r.bytes >= (layout.frame_bytes(w, h) * 24) as u64, "{layout:?}: {} bytes", r.bytes);
        // and back: the matte in BT.709 (a 180-line picture would otherwise be guessed BT.601:
        // uncompressed video has no in-band colour description, the `colr` box is the only one)
        let src = filmcraft_codecs::open_bytes(&name, bytes).unwrap();
        let info = src.info().video.clone().unwrap();
        assert_eq!(info.codec, codec);
        let f = src.video_frame(FrameRequest::full(Tick(TICKS_PER_SECOND / 3))).unwrap();
        assert_eq!(f.color.matrix, filmcraft_color::Matrix::Bt709, "{layout:?}");
        let px = f.to_rgba8();
        assert!(px[0] > 240 && px[1] < 16 && px[2] < 16 && px[3] == 255, "{layout:?}: {:?}", &px[..4]);
    }
}

/// Export size is exact for uncompressed video, not an estimate of a bitrate: the settings
/// summary, the report and the sample entry all agree on the same bytes.
#[test]
fn uncompressed_size_is_the_pictures_own() {
    let (p, seq, m) = project();
    let layout = UncompressedPixelFormat::V210;
    let path = tmp("unc_size.mov");
    let r = export(&p, seq, &settings(layout, &path, (320, 180)), &m, &Progress::default()).unwrap();
    let samples = layout.frame_bytes(320, 180) as u64 * 24;
    assert!(r.bytes > samples && r.bytes < samples + 4096, "{} bytes for {samples} of samples", r.bytes);
    let est = settings(layout, &path, (320, 180)).estimate_bytes(320, 180, FrameRate::FPS_24, 48_000, FrameRate::FPS_24.tick_of(24));
    assert_eq!(est, samples);
}

/// ffmpeg reads the file we wrote: the codec tag and pixel format it reports, and the pixels it
/// decodes — at a width no multiple of 48 (so `v210` rows are padded, 896 bytes for 320 pixels) and
/// at one that is (so they are not).
#[test]
fn uncompressed_quicktime_export_ffmpeg_oracle() {
    let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("uncompressed quicktime") else { return };
    let Some(ffprobe) = filmcraft_testkit::ffprobe_or_skip("uncompressed quicktime") else { return };
    let (p, seq, m) = project();
    for (layout, tag, _, pix) in LAYOUTS {
        for (w, h) in [(320u32, 180u32), (384, 180)] {
            let name = format!("unc_{}_{w}x{h}.mov", layout.label().replace([' ', ':'], "_"));
            let path = tmp(&name);
            export(&p, seq, &settings(layout, &path, (w, h)), &m, &Progress::default()).unwrap();
            let out = std::process::Command::new(&ffprobe)
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_tag_string,pix_fmt,width,height", "-of", "csv=p=0", &path])
                .output()
                .unwrap();
            let line = String::from_utf8_lossy(&out.stdout);
            let fields: Vec<&str> = line.trim().split(',').collect();
            let (ws, hs) = (w.to_string(), h.to_string());
            assert_eq!(fields.first().copied(), Some(tag), "{layout:?} at {w}x{h}: ffprobe says {fields:?}");
            assert!(fields.contains(&pix), "{layout:?} at {w}x{h}: ffprobe says {fields:?}, not {pix}");
            assert!(fields.contains(&ws.as_str()) && fields.contains(&hs.as_str()), "{layout:?} at {w}x{h}: ffprobe says {fields:?}");
            // the pixels ffmpeg decodes are the pixels we decoded: nothing is compressed, so the
            // only difference is that 8-bit 4:2:2 rounds both ways through the RGB matrices
            // (a wrong layout or row stride would be off by tens)
            let frame = Tick(TICKS_PER_SECOND / 3);
            let raw = std::process::Command::new(&ffmpeg)
                .args(["-v", "error", "-i", &path, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .output()
                .unwrap();
            assert!(raw.status.success(), "{layout:?}: ffmpeg could not decode it");
            let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
            let src = filmcraft_codecs::open_bytes(&name, bytes).unwrap();
            let ours = src.video_frame(FrameRequest::full(frame)).unwrap().to_rgba8();
            assert_eq!(raw.stdout.len(), ours.len(), "{layout:?} at {w}x{h}");
            let mut worst = 0i32;
            for (i, (a, b)) in raw.stdout.iter().zip(ours.iter()).enumerate() {
                if i % 4 == 3 {
                    continue;
                }
                worst = worst.max((i32::from(*a) - i32::from(*b)).abs());
            }
            println!("{tag} at {w}x{h}: {pix}, worst pixel difference {worst}");
            assert!(worst <= 2, "{layout:?} at {w}x{h}: ffmpeg and FilmCraft differ by {worst}");
        }
    }
}

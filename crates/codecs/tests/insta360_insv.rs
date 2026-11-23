//! Insta360 INSV on real camera footage, when a file is on this machine.
//!
//! Never downloads. Point `FILMCRAFT_INSV` at an `.insv` (an Insta360 X-series
//! recording: two fisheye tracks plus the metadata trailer); without it the test
//! prints SKIPPED. The checks are the ones the format guarantees: the trailer's
//! model and calibration, the stitched equirectangular size, and a seamless
//! equator (no coverage gap where the two lenses meet).

use std::path::PathBuf;

use filmcraft_media::FrameRequest;

fn insv() -> Option<PathBuf> {
    let p = std::env::var_os("FILMCRAFT_INSV").map(PathBuf::from)?;
    p.is_file().then_some(p)
}

#[test]
fn real_insv_stitches_to_equirect() {
    let Some(path) = insv() else {
        eprintln!("SKIPPED: set FILMCRAFT_INSV to an .insv file");
        return;
    };
    let src = {
        let r = filmcraft_media::reader::FileReader::open(&path).expect("reader");
        filmcraft_media::reader::open_reader(
            &path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            std::sync::Arc::new(r),
            &filmcraft_codecs::reader_openers(),
            &filmcraft_codecs::openers(),
        )
        .expect("open")
    };
    let info = src.info();
    // The trailer's field 2 names the camera model (X4, X4 Air, …), and the
    // container string must carry it; pinning one literal model would make a
    // second corpus a false failure.
    assert!(info.container.starts_with("Insta360 INSV (Insta360 X") && info.container.ends_with(')'), "container names the model: {}", info.container);
    let v = info.video.clone().expect("video");
    // the two fisheye tracks are square circles; the stitch is 2:1 equirectangular
    assert_eq!(v.width, v.height * 2, "equirectangular is 2:1: {}x{}", v.width, v.height);
    assert!(v.frame_rate.num > 0 && v.frame_rate.den > 0);
    assert!(info.has_audio());

    // the first frame: decodes at the stitched size
    let f = src.video_frame(FrameRequest::full(Default::default())).expect("frame 0");
    assert_eq!((f.width, f.height), (v.width, v.height));
    let l = f.luma8();
    let (w, h) = (v.width as usize, v.height as usize);

    // the equator is covered end to end: no long run of black (a coverage gap or a
    // mis-set seam would leave one)
    let row = h / 2;
    let dark_run = |from: usize| -> usize {
        let mut n = 0;
        while n < w && l[row * w + (from + n) % w] < 3 {
            n += 1;
        }
        n
    };
    let longest = (0..w).map(dark_run).max().unwrap_or(0);
    assert!(longest < w / 8, "the equator has no coverage gap (longest black run {longest} of {w})");

    // both poles are covered too (the zenith and nadir come from the lens edges)
    let pole_mean = |row: usize| -> f64 { (0..w).map(|x| u64::from(l[row * w + x])).sum::<u64>() as f64 / w as f64 };
    assert!(pole_mean(0) > 1.0 && pole_mean(h - 1) > 1.0, "poles are covered: {} {}", pole_mean(0), pole_mean(h - 1));

    // a frame in the middle plays too
    let mid = filmcraft_time::Tick(info.duration.0 / 2);
    assert!(src.video_frame(FrameRequest::full(mid)).is_ok(), "mid frame");

    // the trailer's calibration is readable and usable
    let reader: filmcraft_media::SharedReader = std::sync::Arc::new(filmcraft_media::reader::FileReader::open(&path).expect("reader"));
    let meta = filmcraft_insta360::parse(&Src(reader)).expect("parse").expect("trailer");
    let [a, b] = meta.lenses.expect("calibration");
    assert!(a.xi > 1.0 && b.xi > 1.0);
    assert!(a.cx < f64::from(a.sensor_w) / 2.0 && b.cx > f64::from(b.sensor_w) / 2.0, "lens 0 left, lens 1 right");
}

/// A media reader as the trailer parser's byte source.
struct Src(filmcraft_media::SharedReader);

impl filmcraft_insta360::ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

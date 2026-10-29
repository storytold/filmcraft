//! Encode DNxHR with our encoder, decode with ffmpeg (`-xerror`) and with our decoder.

use crate::common;
use common::*;
use filmcraft_dnx::{ChromaFormat, Encoder, EncoderConfig, Profile, decode_frame};

struct Case {
    name: &'static str,
    profile: Profile,
    depth: u8,
    lavfi: &'static str,
    min_psnr: f64,
}

#[test]
fn encode_decodes_in_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let (w, h) = (1920u32, 1080u32);
    let cases = [
        Case { name: "enc_sq_mandel", profile: Profile::Sq, depth: 8, lavfi: "mandelbrot=s=1920x1080:r=25", min_psnr: 40.0 },
        Case { name: "enc_hq_mandel", profile: Profile::Hq, depth: 8, lavfi: "mandelbrot=s=1920x1080:r=25", min_psnr: 45.0 },
        Case { name: "enc_hq_testsrc", profile: Profile::Hq, depth: 8, lavfi: "testsrc2=s=1920x1080:r=25", min_psnr: 45.0 },
        Case { name: "enc_hqx10_mandel", profile: Profile::Hqx, depth: 10, lavfi: "mandelbrot=s=1920x1080:r=25", min_psnr: 45.0 },
        Case { name: "enc_hqx12_testsrc", profile: Profile::Hqx, depth: 12, lavfi: "testsrc2=s=1920x1080:r=25", min_psnr: 45.0 },
        Case { name: "enc_lb_testsrc", profile: Profile::Lb, depth: 8, lavfi: "testsrc2=s=1920x1080:r=25", min_psnr: 30.0 },
        Case { name: "enc_444_testsrc", profile: Profile::R444, depth: 10, lavfi: "testsrc2=s=1920x1080:r=25", min_psnr: 45.0 },
    ];
    for c in cases {
        let mut cfg = EncoderConfig::new(c.profile, w, h);
        cfg.bit_depth = c.depth;
        let chroma = cfg.chroma();
        let src = source_frames(&ff, SourceFrameSpec { name: c.name, lavfi: c.lavfi, frames: 2, width: w, height: h, chroma, depth: c.depth });
        let mut enc = Encoder::with_config(cfg).unwrap();
        let t = std::time::Instant::now();
        let coded: Vec<Vec<u8>> = src.iter().map(|f| enc.encode(f).unwrap()).collect();
        let ms = t.elapsed().as_secs_f64() * 1000.0 / src.len() as f64;
        for p in &coded {
            assert_eq!(p.len() as u32, enc.frame_size(), "{}: CBR frame size", c.name);
        }
        let mov = fixture_dir().join(format!("{}.out.mov", c.name));
        write_mov(&mov, b"AVdh", w as u16, h as u16, 24, &coded);
        let pix = match (chroma, c.depth) {
            (ChromaFormat::Yuv444, 10) => "yuv444p10le",
            (_, 8) => "yuv422p",
            (_, 10) => "yuv422p10le",
            _ => "yuv422p12le",
        };
        // ffmpeg reads 4:4:4 Y'CbCr DNxHR as gbrp unless every macroblock is flagged
        let ff_raw = ffmpeg_decode(&ff, &mov, pix);
        let mut agree = Stats::default();
        let mut psnr_min = f64::MAX;
        let per = frame_samples(&src[0]);
        assert_eq!(ff_raw.len(), per * coded.len(), "{}: ffmpeg frame count", c.name);
        for (i, p) in coded.iter().enumerate() {
            let f = decode_frame(p).unwrap();
            let ours = planes(&f, pix);
            agree.add(&ours, &ff_raw[i * per..(i + 1) * per]);
            let peak = ((1u32 << c.depth) - 1) as f64;
            let srcp = planes(&src[i], pix);
            psnr_min = psnr_min.min(psnr(&srcp[..f.y.len()], &ours[..f.y.len()], peak));
        }
        println!(
            "{:<18} {} {}-bit: {} bytes/frame, {:.1} ms/frame, Y PSNR {:.2} dB, vs ffmpeg max {} exact {:.2}%",
            c.name,
            c.profile.name(),
            c.depth,
            enc.frame_size(),
            ms,
            psnr_min,
            agree.max,
            agree.exact_pct()
        );
        assert!(agree.max <= 2, "{}: ffmpeg disagrees by {}", c.name, agree.max);
        assert!(psnr_min >= c.min_psnr, "{}: PSNR {psnr_min:.2}", c.name);
    }
}

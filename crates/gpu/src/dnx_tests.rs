//! The GPU DNxHD / DNxHR decoder against the CPU decoder (the reference): every sample of every
//! plane, for our own encoder's frames (always) and ffmpeg's `dnxhd` fixtures (DNxHD CIDs,
//! field-coded 1080i, thin rasters; skipped without ffmpeg).

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use filmcraft_dnx::{AlphaCoding, ChromaFormat, Encoder, EncoderConfig, Frame, Profile};
use filmcraft_frame::{Chroma, CodedFormat, CodedImage, PixelData, VideoFrame};

use crate::intra::{IntraStage, read_plane};

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let d = crate::blend_tests::device()?;
    if !crate::intra::supported(&d.0) {
        eprintln!("no compute support; skipping");
        return None;
    }
    Some(d)
}

/// What the decoder hands a GPU monitor: the bitstream, with the real CPU fallback.
fn coded(sample: &[u8]) -> Arc<CodedImage> {
    let h = filmcraft_dnx::probe(sample).unwrap();
    let chroma = match h.chroma {
        ChromaFormat::Yuv420 => Chroma::C420,
        ChromaFormat::Yuv422 => Chroma::C422,
        ChromaFormat::Yuv444 => Chroma::C444,
    };
    Arc::new(CodedImage::new(
        CodedFormat::Dnx,
        Arc::from(sample),
        chroma,
        u32::from(h.bit_depth),
        h.alpha,
        Box::new(|d: &[u8]| filmcraft_dnx::decode_frame(d).ok().map(|f| host_frame(f).data)),
    ))
}

/// A decoded frame as the decoder hands it out (R'G'B' converted to Y'CbCr).
fn host_frame(f: Frame) -> VideoFrame {
    filmcraft_codecs::video::dnx_to_video_frame(f)
}

/// The planes of `sample`'s CPU decode as the decoder hands them out (Y, Cb, Cr, then A).
fn reference(sample: &[u8], name: &str) -> (u32, u32, Vec<Vec<u16>>) {
    let f = host_frame(filmcraft_dnx::decode_frame(sample).unwrap_or_else(|e| panic!("{name}: CPU decode: {e}")));
    let planes = match f.data {
        PixelData::Yuv8 { planes, alpha, .. } => planes.iter().chain(alpha.as_ref()).map(|p| p.iter().map(|&v| u16::from(v)).collect()).collect(),
        PixelData::Yuv16 { planes, alpha, .. } => planes.iter().chain(alpha.as_ref()).map(|p| p.to_vec()).collect(),
        _ => panic!("{name}: planes"),
    };
    (f.width, f.height, planes)
}

fn check(stage: &mut IntraStage, dev: &wgpu::Device, q: &wgpu::Queue, name: &str, sample: &[u8], failures: &mut Vec<String>) {
    let (w, h, want) = reference(sample, name);
    let Some(d) = stage.decode(dev, q, &coded(sample), w, h) else {
        failures.push(format!("{name}: the GPU did not take the frame"));
        return;
    };
    let r: Vec<(usize, u16, usize)> = want
        .iter()
        .zip(&d.textures)
        .map(|(want, tex)| {
            let got = read_plane(dev, q, tex);
            assert_eq!(got.len(), want.len(), "{name}: plane size");
            let bad = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
            let max = got.iter().zip(want.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            (bad, max, want.len())
        })
        .collect();
    let summary: Vec<String> = r.iter().map(|(bad, max, n)| format!("{bad}/{n} (max {max})")).collect();
    eprintln!("{name}: {}", summary.join(", "));
    // as for ProRes: fused multiply-adds may move a sample at a rounding tie by one code
    if d.textures.len() != want.len() || r.iter().any(|(bad, max, n)| *max > 1 || *bad * 1000 > *n) {
        failures.push(format!("{name}: {} planes, {}", d.textures.len(), summary.join(", ")));
    }
}

/// A picture with detail everywhere (gradients, a checkerboard, noise) at `bits`; R'G'B' (in
/// G, B, R order) when `rgb`.
fn busy(w: u32, h: u32, bits: u8, chroma: ChromaFormat, rgb: bool) -> Frame {
    let mut f = Frame::new(w, h, chroma, bits, false);
    f.rgb = rgb;
    let cw = f.chroma_width() as usize;
    let s = u32::from(bits) - 8;
    let mut seed = 7u32;
    let mut rnd = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 22
    };
    for y in 0..h as usize {
        for x in 0..w as usize {
            let chk = if (x / 8 + y / 8) % 2 == 0 { 50 } else { 0 };
            f.y[y * w as usize + x] = (((16 + (x * 3 + y) % 150 + chk + rnd() as usize % 10).min(235) as u32) << s) as u16;
        }
        for x in 0..cw {
            f.cb[y * cw + x] = (((128 + (x as i32 * 5 - y as i32 * 3) % 75) as u32) << s) as u16;
            f.cr[y * cw + x] = (((128 - (x as i32 * 2 + y as i32 * 7) % 75) as u32) << s) as u16;
        }
    }
    f
}

fn encode(profile: Profile, w: u32, h: u32, bits: u8, rgb: bool) -> Vec<u8> {
    encode_alpha(profile, w, h, bits, rgb, None)
}

/// As [`encode`], with a matte coded as `alpha` says: soft-edged disc, ramp, noisy corner.
fn encode_alpha(profile: Profile, w: u32, h: u32, bits: u8, rgb: bool, alpha: Option<AlphaCoding>) -> Vec<u8> {
    let cfg = EncoderConfig { bit_depth: bits, alpha, ..EncoderConfig::new(profile, w, h) };
    let chroma = cfg.chroma();
    let mut f = busy(w, h, bits, chroma, rgb);
    if alpha.is_some() {
        let (wi, hi) = (i64::from(w), i64::from(h));
        let max = (1i64 << bits) - 1;
        let mut a = vec![0u16; (w * h) as usize];
        for y in 0..hi {
            for x in 0..wi {
                let d = (((x - wi / 2).pow(2) + (y - hi / 2).pow(2)) as f64).sqrt() - (wi.min(hi) / 3) as f64;
                let mut v = ((0.5 - d / 6.0).clamp(0.0, 1.0) * max as f64) as i64;
                if y < hi / 4 {
                    v = x * max / wi;
                }
                if x > wi * 7 / 8 && y > hi * 7 / 8 {
                    v = i64::from((x as u32 ^ (y as u32).wrapping_mul(40503)).wrapping_mul(2654435761) >> 20) & max;
                }
                a[(y * wi + x) as usize] = v as u16;
            }
        }
        f.alpha = Some(a);
    }
    Encoder::with_config(cfg).unwrap().encode(&f).unwrap()
}

/// `sample` with the colour flag (ACF) of the first macroblock of every third scan line flipped:
/// R'G'B' and Y'CbCr macroblocks mixed in one picture.
fn mixed(sample: &[u8]) -> Vec<u8> {
    let l = filmcraft_dnx::frame_layout(sample).unwrap();
    let mut s = sample.to_vec();
    for r in l.rows.iter().step_by(3) {
        // qsf (11 bits), then ACF
        s[r.data.0 as usize + 1] ^= 0x10;
    }
    s
}

#[test]
fn gpu_dnx_matches_the_cpu_decoder_on_our_frames() {
    let Some((dev, q)) = device() else { return };
    let mut stage = IntraStage::new(&dev);
    stage.raw_codes = true;
    let mut failures = Vec::new();
    for (profile, w, h, bits, rgb) in [
        (Profile::Hq, 1920, 1080, 8, false),
        (Profile::Sq, 1280, 720, 8, false),
        (Profile::Lb, 334, 202, 8, false),
        (Profile::Hqx, 1920, 1080, 10, false),
        (Profile::Hqx, 640, 360, 12, false),
        (Profile::Hqx, 70, 38, 10, false),
        (Profile::Hq, 16, 16, 8, false),
        (Profile::Hqx, 3840, 2160, 10, false),
        (Profile::R444, 1920, 1080, 10, false),
        (Profile::R444, 1920, 1080, 10, true),
        (Profile::R444, 333, 201, 12, true),
        (Profile::R444, 640, 360, 12, false),
        (Profile::R444, 3840, 2160, 10, true),
    ] {
        let sample = encode(profile, w, h, bits, rgb);
        let name = format!("{profile:?} {w}x{h} {bits}-bit{}", if rgb { " RGB" } else { "" });
        check(&mut stage, &dev, &q, &name, &sample, &mut failures);
        if profile == Profile::R444 {
            check(&mut stage, &dev, &q, &format!("{name} mixed"), &mixed(&sample), &mut failures);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn gpu_dnx_alpha_matches_the_cpu_decoder() {
    let Some((dev, q)) = device() else { return };
    let mut stage = IntraStage::new(&dev);
    stage.raw_codes = true;
    let mut failures = Vec::new();
    for coding in [AlphaCoding::Dct, AlphaCoding::Lossless] {
        for (profile, w, h, bits, rgb) in [
            (Profile::Hq, 1920, 1080, 8, false),
            (Profile::Lb, 334, 202, 8, false),
            (Profile::Hqx, 1920, 1080, 10, false),
            (Profile::Hqx, 70, 38, 12, false),
            (Profile::Hqx, 3840, 2160, 10, false),
            (Profile::R444, 1280, 720, 10, true),
            (Profile::R444, 333, 201, 12, false),
        ] {
            if profile == Profile::Lb && coding == AlphaCoding::Lossless {
                // (lossless alpha of a ramp does not fit LB's bit budget)
                continue;
            }
            let sample = encode_alpha(profile, w, h, bits, rgb, Some(coding));
            let name = format!("{profile:?} {w}x{h} {bits}-bit{} {coding:?} alpha", if rgb { " RGB" } else { "" });
            check(&mut stage, &dev, &q, &name, &sample, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// One ffmpeg-encoded frame (a raw VC-3 file: one coding unit, two for a field-coded frame).
fn ffmpeg_frame(ff: &Path, name: &str, lavfi: &str, enc: &[&str]) -> Vec<u8> {
    let dir = filmcraft_testkit::fixtures_dir("gpu-dnx");
    let out = dir.join(format!("{name}.dnxhd"));
    if !out.exists() {
        let tmp = filmcraft_testkit::temp_path(&out);
        let r = Command::new(ff)
            // (ffmpeg's frame-threaded encoder refuses thin rasters)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", lavfi, "-frames:v", "1", "-threads", "1"])
            .args(enc)
            .args(["-f", "dnxhd"])
            .arg(&tmp)
            .output()
            .unwrap();
        assert!(r.status.success(), "{name}: {}", String::from_utf8_lossy(&r.stderr));
        std::fs::rename(&tmp, &out).unwrap();
    }
    std::fs::read(&out).unwrap()
}

#[test]
fn gpu_dnx_matches_the_cpu_decoder_on_ffmpeg_streams() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("gpu dnx") else { return };
    let Some((dev, q)) = device() else { return };
    let mut stage = IntraStage::new(&dev);
    stage.raw_codes = true;
    let p8 = "format=yuv422p";
    let p10 = "format=yuv422p10le";
    let il = ["-flags", "+ildct"];
    let hd = |b: &'static str| vec!["-c:v", "dnxhd", "-b:v", b];
    let hr = |p: &'static str| vec!["-c:v", "dnxhd", "-profile:v", p];
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        ("hd1235_mandel", format!("mandelbrot=s=1920x1080:r=25,{p10}"), hd("185M")),
        ("hd1237_testsrc", format!("testsrc2=s=1920x1080:r=25,{p8}"), hd("120M")),
        ("hd1238_mandel", format!("mandelbrot=s=1920x1080:r=25,{p8}"), hd("185M")),
        ("hd1241_1080i", format!("testsrc2=s=1920x1080:r=25,setfield=tff,{p10}"), [&hd("185M")[..], &il].concat()),
        ("hd1242_1080i", format!("mandelbrot=s=1920x1080:r=25,setfield=tff,{p8}"), [&hd("120M")[..], &il].concat()),
        ("hd1243_1080i", format!("testsrc2=s=1920x1080:r=25,setfield=tff,{p8}"), [&hd("185M")[..], &il].concat()),
        ("hd1244_thin_1080i", format!("testsrc2=s=1440x1080:r=25,setfield=tff,{p8}"), [&hd("120M")[..], &il].concat()),
        ("hd1250_720p", format!("mandelbrot=s=1280x720:r=50,{p10}"), hd("180M")),
        ("hd1251_720p", format!("testsrc2=s=1280x720:r=50,{p8}"), hd("180M")),
        ("hd1252_720p", format!("testsrc2=s=1280x720:r=50,{p8}"), hd("120M")),
        ("hd1253_1080p36", format!("mandelbrot=s=1920x1080:r=25,{p8}"), hd("36M")),
        ("hd1258_thin_720p", format!("testsrc2=s=960x720:r=24000/1001,{p8}"), hd("42M")),
        ("hd1259_thin_1080p", format!("mandelbrot=s=1440x1080:r=24000/1001,{p8}"), hd("63M")),
        ("hr_lb_odd", format!("mandelbrot=s=1918x1078:r=25,{p8}"), hr("dnxhr_lb")),
        ("hr_sq_uhd", format!("testsrc2=s=3840x2160:r=25,{p8}"), hr("dnxhr_sq")),
        ("hr_hq_2k", format!("mandelbrot=s=2048x1080:r=25,{p8}"), hr("dnxhr_hq")),
        ("hr_hqx_uhd", format!("mandelbrot=s=3840x2160:r=25,{p10}"), hr("dnxhr_hqx")),
        ("hr_hqx_small", format!("testsrc2=s=352x288:r=25,{p10}"), hr("dnxhr_hqx")),
        // 4:4:4: Y'CbCr (ACF = 1 everywhere) and R'G'B'
        ("hr_444_yuv", "mandelbrot=s=1920x1080:r=25,format=yuv444p10le".into(), hr("dnxhr_444")),
        ("hr_444_rgb", "testsrc2=s=1920x1080:r=25,format=gbrp10le".into(), hr("dnxhr_444")),
        ("hr_444_rgb_uhd", "mandelbrot=s=3840x2160:r=25,format=gbrp10le".into(), hr("dnxhr_444")),
        ("hr_444_rgb_odd", "testsrc2=s=718x405:r=25,format=gbrp10le".into(), hr("dnxhr_444")),
    ];
    let mut failures = Vec::new();
    for (name, lavfi, enc) in cases {
        let f = ffmpeg_frame(&ff, name, &lavfi, &enc);
        check(&mut stage, &dev, &q, name, &f, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn corrupt_scan_lines_are_reported_not_trusted() {
    let Some((dev, q)) = device() else { return };
    let mut stage = IntraStage::new(&dev);
    let sample = encode(Profile::Hq, 640, 360, 8, false);
    let l = filmcraft_dnx::frame_layout(&sample).unwrap();
    // garbage over the middle of a scan line: long runs past the end of a block
    let mut bad = sample.clone();
    let r = l.rows[3].data;
    for b in &mut bad[r.0 as usize + 20..(r.0 + r.1 / 2) as usize] {
        *b = 0xff;
    }
    assert!(filmcraft_dnx::decode_frame(&bad).is_err(), "the CPU rejects it too");
    let img = coded(&bad);
    assert!(stage.decode(&dev, &q, &img, 640, 360).is_some());
    let _ = dev.poll(wgpu::PollType::wait_indefinitely());
    assert!(img.gpu_failed(), "marked for the CPU decode");
    // garbage at the end of a scan line: in its alpha
    for coding in [AlphaCoding::Dct, AlphaCoding::Lossless] {
        let sample = encode_alpha(Profile::Hq, 640, 360, 8, false, Some(coding));
        let l = filmcraft_dnx::frame_layout(&sample).unwrap();
        let mut bad = sample.clone();
        let r = l.rows[5].data;
        for b in &mut bad[(r.0 + r.1 - 24) as usize..(r.0 + r.1) as usize] {
            *b = 0xff;
        }
        assert!(filmcraft_dnx::decode_frame(&bad).is_err(), "{coding:?}: the CPU rejects it too");
        let img = coded(&bad);
        assert!(stage.decode(&dev, &q, &img, 640, 360).is_some());
        let _ = dev.poll(wgpu::PollType::wait_indefinitely());
        assert!(img.gpu_failed(), "{coding:?}: marked for the CPU decode");
    }
    // and from then on decoded on the CPU
    assert!(stage.decode(&dev, &q, &img, 640, 360).is_none());
    // a frame whose size disagrees with its header is not taken
    assert!(stage.decode(&dev, &q, &coded(&sample), 640, 368).is_none());
}

#[test]
fn what_the_gpu_does_not_take_stays_on_the_cpu() {
    let Some((dev, q)) = device() else { return };
    let mut stage = IntraStage::new(&dev);
    // macroblock-adaptive field coding signalled (CID 1260's coding)
    let mut s = encode(Profile::Hqx, 64, 32, 10, false);
    s[6] |= 0x20;
    assert!(stage.decode(&dev, &q, &coded(&s), 64, 32).is_none(), "macroblock-adaptive");
    // not a VC-3 frame at all
    let junk = Arc::new(CodedImage::new(CodedFormat::Dnx, Arc::from(&[0u8; 700][..]), Chroma::C422, 8, false, Box::new(|_| None)));
    assert!(stage.decode(&dev, &q, &junk, 64, 32).is_none());
    assert_eq!(stage.fallbacks, 2);
}

#[test]
fn coded_dnx_frames_composite_like_their_cpu_decode() {
    use crate::GpuCompositor;
    use filmcraft_geom::Affine;
    use filmcraft_render::Blend;
    use filmcraft_render::plan::{FramePlan, PlanLayer};
    let Some((dev, q)) = device() else { return };
    let mut c = GpuCompositor::new(&dev, &q);
    for (profile, w, h, bits, rgb, alpha) in [
        (Profile::Hq, 1920, 1080, 8, false, None),
        (Profile::Hqx, 640, 360, 10, false, None),
        (Profile::Hqx, 334, 202, 12, false, None),
        (Profile::R444, 640, 360, 10, true, None),
        (Profile::Hq, 640, 360, 8, false, Some(AlphaCoding::Dct)),
        (Profile::Hqx, 640, 360, 10, false, Some(AlphaCoding::Lossless)),
    ] {
        let sample = encode_alpha(profile, w, h, bits, rgb, alpha);
        let host = Arc::new(host_frame(filmcraft_dnx::decode_frame(&sample).unwrap()));
        let coded = Arc::new(VideoFrame { data: PixelData::Coded(coded(&sample)), ..(*host).clone() });
        let run = |c: &mut GpuCompositor, f: Arc<VideoFrame>| {
            let plan = FramePlan::Layers {
                width: (w / 2) as usize,
                height: (h / 2) as usize,
                layers: vec![PlanLayer::new(f, Affine::scale(0.5, 0.5), 1.0, Blend::Normal)],
            };
            c.composite(&plan);
            c.read_output().map(|o| o.2).unwrap_or_default()
        };
        let before = c.intra_decodes();
        let via_gpu = run(&mut c, coded.clone());
        let after = c.intra_decodes();
        let via_cpu = run(&mut c, host);
        assert_eq!(after.0, before.0 + 1, "{profile:?} {bits}-bit {alpha:?}: decoded on the GPU");
        let max = via_gpu.iter().zip(&via_cpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        eprintln!("{profile:?} {w}x{h} {bits}-bit {alpha:?}: at most {max} apart");
        assert!(!via_cpu.is_empty() && max <= 1, "{profile:?} {bits}-bit {alpha:?}: {max}");
        // CPU consumers see the CPU decode
        assert_eq!(coded.to_rgba8(), host_frame(filmcraft_dnx::decode_frame(&sample).unwrap()).to_rgba8());
    }
}

/// Playback cost of a 4K DNxHR frame through the compositor (`cargo test --release -p
/// filmcraft-gpu --lib dnx_gpu_vs_cpu_bench -- --ignored --nocapture`): CPU decode + upload
/// against bitstream upload + GPU decode, each until the GPU is done.
#[test]
#[ignore = "benchmark"]
fn dnx_gpu_vs_cpu_bench() {
    use crate::GpuCompositor;
    use filmcraft_geom::Affine;
    use filmcraft_render::Blend;
    use filmcraft_render::plan::{FramePlan, PlanLayer};
    use std::time::Instant;
    let Some((dev, q)) = device() else { return };
    let mut c = GpuCompositor::new(&dev, &q);
    for (profile, w, h, bits, rgb, alpha) in [
        (Profile::Hq, 3840, 2160, 8, false, None),
        (Profile::Hqx, 3840, 2160, 10, false, None),
        (Profile::Hqx, 1920, 1080, 10, false, None),
        (Profile::R444, 3840, 2160, 10, true, None),
        (Profile::Hqx, 3840, 2160, 10, false, Some(AlphaCoding::Dct)),
        (Profile::Hqx, 3840, 2160, 10, false, Some(AlphaCoding::Lossless)),
    ] {
        let sample = encode_alpha(profile, w, h, bits, rgb, alpha);
        let run = |c: &mut GpuCompositor, f: Arc<VideoFrame>| {
            let plan = FramePlan::Layers {
                width: 1920,
                height: 1080,
                layers: vec![PlanLayer::new(f, Affine::scale(1920.0 / w as f64, 1080.0 / h as f64), 1.0, Blend::Normal)],
            };
            let prepared = crate::prepare(&plan);
            c.composite_prepared(&plan, Some(&prepared));
            let _ = dev.poll(wgpu::PollType::wait_indefinitely());
        };
        let frame = |gpu: bool| {
            if gpu {
                // what the decoder hands a GPU monitor: the bitstream, no decode
                let host = VideoFrame {
                    width: w,
                    height: h,
                    data: PixelData::Rgba8(Arc::new(Vec::new())),
                    color: filmcraft_color::ColorInfo::REC709,
                    par: (1, 1),
                    pts: filmcraft_time::Tick::ZERO,
                };
                Arc::new(VideoFrame { data: PixelData::Coded(coded(&sample)), ..host })
            } else {
                // the decode is part of the CPU path's cost
                Arc::new(host_frame(filmcraft_dnx::decode_frame(&sample).unwrap()))
            }
        };
        let n = 60;
        for (label, gpu) in [("CPU decode + upload", false), ("GPU decode", true)] {
            // warm up (pipelines, pooled buffers, rayon)
            for _ in 0..3 {
                run(&mut c, frame(gpu));
            }
            let mut times: Vec<f64> = (0..n)
                .map(|_| {
                    let t = Instant::now();
                    run(&mut c, frame(gpu));
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            times.sort_by(f64::total_cmp);
            // (the median: GPU clocks change under a benchmark's bursts)
            let ms = times[n / 2];
            eprintln!(
                "{profile:?} {w}x{h} {bits}-bit{} ({:.1} MB/frame) {label:20}: {ms:6.2} ms/frame ({:5.1} fps; fastest {:.2} ms)",
                alpha.map_or(String::new(), |a| format!(" {a:?} alpha")),
                sample.len() as f64 / 1e6,
                1000.0 / ms,
                times[0]
            );
        }
    }
}

//! The GPU ProRes decoder against the CPU decoder (the reference): every sample of every plane,
//! for our own encoder's frames (always) and ffmpeg's `prores_ks` / `prores_aw` fixtures
//! (interlaced, 8-/16-bit alpha, odd sizes; skipped without ffmpeg).

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use filmcraft_frame::{Chroma, CodedFormat, CodedImage};
use filmcraft_prores::{ChromaFormat, Encoder, EncoderConfig, Frame, Profile};

use crate::intra::{IntraStage, read_plane};

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    crate::blend_tests::device()
}

fn coded(sample: &[u8]) -> Arc<CodedImage> {
    let h = filmcraft_prores::probe(sample).unwrap();
    let (chroma, bits) = match h.chroma {
        ChromaFormat::Yuv422 => (Chroma::C422, 10),
        ChromaFormat::Yuv444 => (Chroma::C444, 12),
    };
    let alpha = h.alpha != filmcraft_prores::AlphaType::None;
    Arc::new(CodedImage::new(CodedFormat::ProRes, Arc::from(sample), chroma, bits, alpha, Box::new(|_| None)))
}

/// Mismatching samples and the largest difference, plane by plane.
fn compare(stage: &mut IntraStage, dev: &wgpu::Device, q: &wgpu::Queue, name: &str, sample: &[u8]) -> Vec<(usize, u16, usize)> {
    let cpu = filmcraft_prores::decode_frame(sample).unwrap_or_else(|e| panic!("{name}: CPU decode: {e}"));
    let img = coded(sample);
    let d = stage.decode(dev, q, &img, cpu.width, cpu.height).unwrap_or_else(|| panic!("{name}: the GPU did not take the frame"));
    let mut planes: Vec<&Vec<u16>> = vec![&cpu.y, &cpu.cb, &cpu.cr];
    if let Some(a) = &cpu.alpha {
        planes.push(a);
    }
    assert_eq!(d.textures.len(), planes.len(), "{name}: planes");
    planes
        .iter()
        .zip(&d.textures)
        .map(|(want, tex)| {
            let got = read_plane(dev, q, tex);
            assert_eq!(got.len(), want.len(), "{name}: plane size");
            let bad = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
            let max = got.iter().zip(want.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            (bad, max, want.len())
        })
        .collect()
}

fn check(stage: &mut IntraStage, dev: &wgpu::Device, q: &wgpu::Queue, name: &str, sample: &[u8], failures: &mut Vec<String>) {
    let r = compare(stage, dev, q, name, sample);
    let summary: Vec<String> = r.iter().map(|(bad, max, n)| format!("{bad}/{n} (max {max})")).collect();
    eprintln!("{name}: {}", summary.join(", "));
    // the GPU's f32 arithmetic may fuse multiply-adds the CPU rounds separately: a sample whose
    // exact value lies at a rounding tie can come out one code apart, nothing more
    if r.iter().any(|(bad, max, n)| *max > 1 || *bad * 1000 > *n) {
        failures.push(format!("{name}: {}", summary.join(", ")));
    }
}

/// A picture with detail everywhere (gradients, a checkerboard, noise).
fn busy(w: u32, h: u32, chroma: ChromaFormat, alpha: bool) -> Frame {
    let mut f = Frame::new(w, h, chroma, 10, alpha);
    let cw = f.chroma_width() as usize;
    let mut seed = 7u32;
    let mut rnd = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 22
    };
    for y in 0..h as usize {
        for x in 0..w as usize {
            let chk = if (x / 8 + y / 8) % 2 == 0 { 200 } else { 0 };
            f.y[y * w as usize + x] = (64 + (x * 3 + y) % 600 + chk + rnd() as usize % 40).min(940) as u16;
            if let Some(a) = f.alpha.as_mut() {
                a[y * w as usize + x] =
                    if (x as i64 - w as i64 / 2).pow(2) + (y as i64 - h as i64 / 2).pow(2) < (h as i64 / 3).pow(2) { 1023 } else { (x % 1024) as u16 };
            }
        }
        for x in 0..cw {
            f.cb[y * cw + x] = (512 + (x as i32 * 5 - y as i32 * 3) % 300) as u16;
            f.cr[y * cw + x] = (512 - (x as i32 * 2 + y as i32 * 7) % 300) as u16;
        }
    }
    f
}

#[test]
fn gpu_prores_matches_the_cpu_decoder_on_our_frames() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    if !crate::intra::supported(&dev) {
        eprintln!("no compute support; skipping");
        return;
    }
    let mut stage = IntraStage::new(&dev);
    stage.raw_codes = true;
    let mut failures = Vec::new();
    for (profile, w, h, log2, alpha) in [
        (Profile::Hq, 1920, 1080, 3, false),
        (Profile::Proxy, 1280, 720, 3, false),
        (Profile::Lt, 333, 201, 2, false),
        (Profile::Standard, 70, 38, 1, false),
        (Profile::Hq, 64, 16, 0, false),
        (Profile::P4444, 640, 360, 3, true),
        (Profile::P4444Xq, 301, 177, 3, false),
        (Profile::Hq, 3840, 2160, 3, false),
    ] {
        let frame = busy(w, h, profile.chroma(), alpha);
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.log2_slice_mbs = log2;
        cfg.encode_alpha = alpha;
        let sample = Encoder::with_config(cfg).encode(&frame).unwrap();
        check(&mut stage, &dev, &q, &format!("{profile:?} {w}x{h} slices {}", 1 << log2), &sample, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn ffmpeg_frames(ff: &Path, name: &str, lavfi: &str, frames: u32, enc: &[&str]) -> Vec<Vec<u8>> {
    let dir = filmcraft_testkit::fixtures_dir("gpu-prores");
    let mov = dir.join(format!("{name}.mov"));
    if !mov.exists() {
        let tmp = filmcraft_testkit::temp_path(&mov);
        let n = frames.to_string();
        let out = Command::new(ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", lavfi, "-frames:v", &n])
            .args(enc)
            .arg(&tmp)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        std::fs::rename(&tmp, &mov).unwrap();
    }
    let out =
        Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-i"]).arg(&mov).args(["-map", "0:v", "-c", "copy", "-f", "data", "-"]).output().unwrap();
    assert!(out.status.success());
    let mut d = &out.stdout[..];
    let mut v = Vec::new();
    while d.len() >= 8 {
        let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
        if n < 8 || n > d.len() {
            break;
        }
        v.push(d[..n].to_vec());
        d = &d[n..];
    }
    v
}

#[test]
fn gpu_prores_matches_the_cpu_decoder_on_ffmpeg_streams() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("gpu prores") else { return };
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    if !crate::intra::supported(&dev) {
        return;
    }
    let mut stage = IntraStage::new(&dev);
    stage.raw_codes = true;
    let p422 = "format=yuv422p10le";
    let p444 = "format=yuv444p10le";
    let alpha = "format=yuva444p10le";
    let il = ["-flags", "+ildct+ilme"];
    let ks = |p: &'static str| vec!["-c:v", "prores_ks", "-profile:v", p];
    let aw = |p: &'static str| vec!["-c:v", "prores_aw", "-profile:v", p];
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        ("ks_proxy_1080", format!("testsrc2=s=1920x1080:r=25,{p422}"), ks("0")),
        ("ks_hq_mandel_1918", format!("mandelbrot=s=1918x1080:r=25,{p422}"), ks("3")),
        ("aw_hq_640", format!("testsrc2=s=640x360:r=25,{p422}"), aw("3")),
        ("ks_hq_small_slices", format!("mandelbrot=s=352x288:r=25,{p422}"), [&ks("3")[..], &["-mbs_per_slice", "2"]].concat()),
        ("ks_hq_ntsc_tff", format!("testsrc2=s=720x486:r=30000/1001,setfield=tff,{p422}"), [&ks("3")[..], &il].concat()),
        ("ks_std_ntsc_bff", format!("mandelbrot=s=720x486:r=30000/1001,setfield=bff,{p422}"), [&ks("2")[..], &il].concat()),
        ("aw_hq_1080i", format!("testsrc2=s=1920x1080:r=30000/1001,setfield=tff,{p422}"), [&aw("3")[..], &il].concat()),
        ("ks_4444xq_mandel_1918", format!("mandelbrot=s=1918x1080:r=25,{p444}"), ks("5")),
        ("ks_4444_alpha16_640", format!("testsrc2=s=640x360:r=25,{alpha}"), [&ks("4")[..], &["-alpha_bits", "16"]].concat()),
        ("ks_4444_alpha8_640", format!("testsrc2=s=640x360:r=25,{alpha}"), [&ks("4")[..], &["-alpha_bits", "8"]].concat()),
        ("ks_4444xq_alpha_odd", format!("mandelbrot=s=333x201:r=25,{alpha}"), ks("5")),
        ("aw_4444_alpha_640", format!("testsrc2=s=640x360:r=25,{alpha}"), aw("4")),
        ("ks_4444_ntsc_tff", format!("testsrc2=s=720x486:r=30000/1001,setfield=tff,{p444}"), [&ks("4")[..], &il].concat()),
    ];
    let mut failures = Vec::new();
    for (name, lavfi, enc) in cases {
        for (i, f) in ffmpeg_frames(&ff, name, &lavfi, 2, &enc).iter().enumerate() {
            check(&mut stage, &dev, &q, &format!("{name} #{i}"), f, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn corrupt_slices_are_reported_not_trusted() {
    let Some((dev, q)) = device() else { return };
    if !crate::intra::supported(&dev) {
        return;
    }
    let frame = busy(640, 360, ChromaFormat::Yuv422, false);
    let good = Encoder::new(Profile::Hq, 640, 360).encode(&frame).unwrap();
    let layout = filmcraft_prores::frame_layout(&good).unwrap();
    let mut stage = IntraStage::new(&dev);
    let mut flagged = 0;
    let mut seed = 99u32;
    for _ in 0..40 {
        let mut bad = good.clone();
        // scramble bytes inside slice data only (the layout stays valid)
        for _ in 0..16 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let s = &layout.slices[(seed >> 8) as usize % layout.slices.len()];
            let (o, n) = s.comps[(seed >> 4) as usize % 3];
            if n > 0 {
                bad[(o + (seed >> 12) % n) as usize] ^= 0x5a;
            }
        }
        let img = coded(&bad);
        // never fails, never hangs: either decoded (garbage) or handed to the CPU
        let _ = stage.decode(&dev, &q, &img, 640, 360);
        let _ = dev.poll(wgpu::PollType::wait_indefinitely());
        flagged += img.gpu_failed() as u32;
    }
    eprintln!("{flagged}/40 corrupted frames flagged");
    assert!(flagged > 0, "corruption is detected");
}

/// A frame of `sample` as the decoder hands it to a GPU monitor (coded, decoding on the CPU
/// through the real fallback) and as it hands it to everyone else (CPU-decoded planes).
fn frames(sample: &[u8]) -> (Arc<filmcraft_frame::VideoFrame>, Arc<filmcraft_frame::VideoFrame>) {
    use filmcraft_frame::{PixelData, VideoFrame};
    let cpu = filmcraft_prores::decode_frame(sample).unwrap();
    let (w, h) = (cpu.width, cpu.height);
    let chroma = if cpu.chroma == ChromaFormat::Yuv444 { Chroma::C444 } else { Chroma::C422 };
    let planes = |f: filmcraft_prores::Frame| PixelData::Yuv16 {
        planes: [Arc::new(f.y), Arc::new(f.cb), Arc::new(f.cr)],
        chroma: if f.chroma == ChromaFormat::Yuv444 { Chroma::C444 } else { Chroma::C422 },
        bits: f.bit_depth as u32,
        alpha: f.alpha.map(Arc::new),
    };
    let bits = cpu.bit_depth as u32;
    let alpha = cpu.alpha.is_some();
    let host = VideoFrame { width: w, height: h, data: planes(cpu), color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: filmcraft_time::Tick::ZERO };
    let img = CodedImage::new(
        CodedFormat::ProRes,
        Arc::from(sample),
        chroma,
        bits,
        alpha,
        Box::new(move |d: &[u8]| filmcraft_prores::decode_frame(d).ok().map(planes)),
    );
    let coded = VideoFrame { data: PixelData::Coded(Arc::new(img)), ..host.clone() };
    (Arc::new(coded), Arc::new(host))
}

#[test]
fn coded_frames_composite_like_their_cpu_decode() {
    use crate::GpuCompositor;
    use filmcraft_geom::Affine;
    use filmcraft_render::Blend;
    use filmcraft_render::plan::{FramePlan, PlanLayer};
    let Some((dev, q)) = device() else { return };
    let mut c = GpuCompositor::new(&dev, &q);
    if c.intra.is_none() {
        eprintln!("no compute support; skipping");
        return;
    }
    let mut failures = Vec::new();
    for (profile, w, h, alpha) in [(Profile::Hq, 1920, 1080, false), (Profile::P4444, 640, 360, true), (Profile::Lt, 333, 201, false)] {
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.encode_alpha = alpha;
        let sample = Encoder::with_config(cfg).encode(&busy(w, h, profile.chroma(), alpha)).unwrap();
        let (coded, host) = frames(&sample);
        // over a grey layer, so alpha shows; half size, so sampling between pixels runs too
        let grey = Arc::new(filmcraft_frame::VideoFrame::rgba_f32(w, h, vec![0.18; (w * h * 4) as usize]));
        let run = |c: &mut GpuCompositor, f: Arc<filmcraft_frame::VideoFrame>| {
            let plan = FramePlan::Layers {
                width: (w / 2) as usize,
                height: (h / 2) as usize,
                layers: vec![
                    PlanLayer::new(grey.clone(), Affine::scale(0.5, 0.5), 1.0, Blend::Normal),
                    PlanLayer::new(f, Affine::scale(0.5, 0.5), 1.0, Blend::Normal),
                ],
            };
            c.composite(&plan);
            c.read_output().map(|o| o.2).unwrap_or_default()
        };
        let before = c.intra_decodes();
        let via_gpu = run(&mut c, coded.clone());
        let after = c.intra_decodes();
        let via_cpu = run(&mut c, host);
        assert_eq!(after.0, before.0 + 1, "{profile:?}: decoded on the GPU");
        let diff = via_gpu.iter().zip(&via_cpu).filter(|(a, b)| a != b).count();
        let max = via_gpu.iter().zip(&via_cpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        eprintln!("{profile:?} {w}x{h}: {diff}/{} bytes differ, at most {max}", via_cpu.len());
        if max > 1 {
            failures.push(format!("{profile:?}: {diff} bytes differ, at most {max}"));
        }
        // a second composite of the same frame takes it from the upload cache
        let _ = run(&mut c, coded);
        assert_eq!(c.intra_decodes().0, after.0, "{profile:?}: cached");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_coded_frame_falls_back_to_its_cpu_decode() {
    use filmcraft_frame::PixelData;
    let sample = Encoder::new(Profile::Hq, 64, 32).encode(&busy(64, 32, ChromaFormat::Yuv422, false)).unwrap();
    let (coded, host) = frames(&sample);
    // CPU consumers see the CPU decode
    let cpu = coded.cpu_frame();
    let (PixelData::Yuv16 { planes: a, .. }, PixelData::Yuv16 { planes: b, .. }) = (&cpu.data, &host.data) else { panic!("planes") };
    assert_eq!(a, b);
    assert_eq!(coded.to_rgba8(), host.to_rgba8());
    // a picture the GPU found corrupt is uploaded from its CPU decode
    let Some((dev, q)) = device() else { return };
    let mut c = crate::GpuCompositor::new(&dev, &q);
    let PixelData::Coded(img) = &coded.data else { panic!("coded") };
    img.set_gpu_failed();
    let plan = filmcraft_render::plan::FramePlan::Layers {
        width: 64,
        height: 32,
        layers: vec![filmcraft_render::plan::PlanLayer::new(coded.clone(), filmcraft_geom::Affine::IDENTITY, 1.0, filmcraft_render::Blend::Normal)],
    };
    c.composite(&plan);
    assert_eq!(c.intra_decodes().0, 0);
}

/// Playback cost of a 4K ProRes frame through the compositor (`cargo test --release -p
/// filmcraft-gpu --lib prores_gpu_vs_cpu_bench -- --ignored --nocapture`): CPU decode + half-float
/// conversion + upload against bitstream upload + GPU decode, each until the GPU is done.
#[test]
#[ignore = "benchmark"]
fn prores_gpu_vs_cpu_bench() {
    use crate::GpuCompositor;
    use filmcraft_geom::Affine;
    use filmcraft_render::Blend;
    use filmcraft_render::plan::{FramePlan, PlanLayer};
    use std::time::Instant;
    let Some((dev, q)) = device() else { return };
    let mut c = GpuCompositor::new(&dev, &q);
    for (profile, w, h) in [(Profile::Hq, 3840, 2160), (Profile::P4444, 3840, 2160), (Profile::Hq, 1920, 1080)] {
        let sample = Encoder::new(profile, w, h).encode(&busy(w, h, profile.chroma(), false)).unwrap();
        let run = |c: &mut GpuCompositor, f: Arc<filmcraft_frame::VideoFrame>| {
            let plan = FramePlan::Layers {
                width: 1920,
                height: 1080,
                layers: vec![PlanLayer::new(f, Affine::scale(1920.0 / w as f64, 1080.0 / h as f64), 1.0, Blend::Normal)],
            };
            let prepared = crate::prepare(&plan);
            c.composite_prepared(&plan, Some(&prepared));
            let _ = dev.poll(wgpu::PollType::wait_indefinitely());
        };
        let n = 30;
        for (label, gpu) in [("CPU decode + upload", false), ("GPU decode", true)] {
            // warm up (pipelines, pooled buffers, rayon)
            for _ in 0..3 {
                let (coded, host) = frames(&sample);
                run(&mut c, if gpu { coded } else { host });
            }
            let t = Instant::now();
            for _ in 0..n {
                let f = if gpu {
                    // what the decoder hands a GPU monitor: the bitstream, no decode
                    Arc::new(filmcraft_frame::VideoFrame {
                        width: w,
                        height: h,
                        data: filmcraft_frame::PixelData::Coded(coded(&sample)),
                        color: filmcraft_color::ColorInfo::REC709,
                        par: (1, 1),
                        pts: filmcraft_time::Tick::ZERO,
                    })
                } else {
                    // the decode is part of the CPU path's cost
                    let cpu = filmcraft_prores::decode_frame(&sample).unwrap();
                    let chroma = if cpu.chroma == ChromaFormat::Yuv444 { Chroma::C444 } else { Chroma::C422 };
                    Arc::new(filmcraft_frame::VideoFrame {
                        width: w,
                        height: h,
                        data: filmcraft_frame::PixelData::Yuv16 {
                            planes: [Arc::new(cpu.y), Arc::new(cpu.cb), Arc::new(cpu.cr)],
                            chroma,
                            bits: cpu.bit_depth as u32,
                            alpha: None,
                        },
                        color: filmcraft_color::ColorInfo::REC709,
                        par: (1, 1),
                        pts: filmcraft_time::Tick::ZERO,
                    })
                };
                run(&mut c, f);
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            eprintln!("{profile:?} {w}x{h} ({:.1} MB/frame) {label:20}: {ms:6.2} ms/frame ({:5.1} fps)", sample.len() as f64 / 1e6, 1000.0 / ms);
        }
    }
}

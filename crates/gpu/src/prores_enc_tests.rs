//! The GPU ProRes encoder against the CPU encoder (the reference): every GPU frame parses, decodes
//! (with our decoder and with ffmpeg, when present), keeps the profile's size limits and is within
//! 0.1 dB of the CPU frame's PSNR; most frames are byte-identical to the CPU's.

use std::process::Command;

use filmcraft_prores::{
    ChromaFormat, DecodeOptions, Encoder, EncoderConfig, Frame, Interlace, Profile, SliceBackend, decode_frame, decode_frame_with, frame_layout,
    rate_control_for_tests,
};

use crate::intra_enc::fits;
use crate::prores_enc::{C_INV, C_SCAN, C_SCAN_INTERLACED, ProResGpuEncoder, RC_ROUNDS, consts};

fn encoder(test: &str) -> Option<ProResGpuEncoder> {
    let e = ProResGpuEncoder::new();
    if e.is_none() {
        eprintln!("SKIPPED {test}: no adapter");
    }
    e
}

/// Deterministic busy planes: gradient + checkerboard + xorshift noise; an alpha ramp and disc.
fn test_frame(profile: Profile, w: u32, h: u32, alpha: bool, seed: u32) -> Frame {
    let mut f = Frame::new(w, h, profile.chroma(), 10, alpha);
    let mut s = seed.max(1);
    let mut noise = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let cw = f.chroma_width() as usize;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let check = if (x / 4 + y / 4) % 2 == 0 { 60 } else { 0 };
            f.y[y * w as usize + x] = (64 + (x * 3 + y * 2) % 600 + check + noise() as usize % 40) as u16;
            if let Some(a) = f.alpha.as_mut() {
                let disc = (x as i64 - w as i64 / 2).pow(2) + (y as i64 - h as i64 / 2).pow(2) < (h as i64 / 3).pow(2);
                a[y * w as usize + x] = if disc { 1023 } else { ((x * 1023) / w.max(1) as usize) as u16 };
            }
        }
        for x in 0..cw {
            f.cb[y * cw + x] = (400 + (x + y) % 200 + noise() as usize % 30) as u16;
            f.cr[y * cw + x] = (500 + (x * 5 + y) % 100 + noise() as usize % 30) as u16;
        }
    }
    f
}

/// Full-scale noise in every plane (alpha too).
fn noise_frame(profile: Profile, w: u32, h: u32, alpha: bool) -> Frame {
    let mut f = Frame::new(w, h, profile.chroma(), 10, alpha);
    let mut s = 12345u32;
    let planes = f.y.iter_mut().chain(f.cb.iter_mut()).chain(f.cr.iter_mut()).chain(f.alpha.iter_mut().flatten());
    for v in planes {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        *v = (s % 1024) as u16;
    }
    f
}

/// PSNR of the frame's 10-bit decode against the 10-bit source, over every plane.
fn psnr(coded: &[u8], src: &Frame) -> f64 {
    let d = decode_frame_with(coded, &DecodeOptions { bit_depth: Some(10), threads: true }).unwrap();
    let pairs = [(&d.y, &src.y), (&d.cb, &src.cb), (&d.cr, &src.cr)];
    let (mut se, mut n) = (0f64, 0usize);
    for (a, b) in pairs {
        for (x, y) in a.iter().zip(b.iter()) {
            se += (f64::from(*x) - f64::from(*y)).powi(2);
            n += 1;
        }
    }
    if se == 0.0 { f64::INFINITY } else { 10.0 * (1023f64 * 1023.0 / (se / n as f64)).log10() }
}

struct Case {
    name: String,
    cfg: EncoderConfig,
    frame: Frame,
}

/// `Profile::ALL` × odd and common sizes × slice widths 1..=8 macroblocks (4444 with and
/// without alpha).
fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    for profile in Profile::ALL {
        let alphas: &[bool] = if profile.chroma() == ChromaFormat::Yuv444 { &[false, true] } else { &[false] };
        for &alpha in alphas {
            for (w, h) in [(1, 1), (17, 13), (64, 48), (1919, 1079), (1920, 1080)] {
                for log2 in 0..=3u8 {
                    let mut cfg = EncoderConfig::new(profile, w, h);
                    cfg.log2_slice_mbs = log2;
                    cfg.encode_alpha = alpha;
                    let seed = w ^ (h << 8) ^ u32::from(log2) << 20;
                    let name = format!("{profile:?}{} {w}x{h} slices {}", if alpha { "+alpha" } else { "" }, 1 << log2);
                    v.push(Case { name, cfg, frame: test_frame(profile, w, h, alpha, seed) });
                }
            }
        }
    }
    v
}

/// Encode `frame` on the CPU and with the GPU backend: (cpu, gpu, backend used).
fn both(enc: &mut ProResGpuEncoder, cfg: &EncoderConfig, frame: &Frame) -> (Vec<u8>, Vec<u8>, bool) {
    let cpu = Encoder::with_config(cfg.clone()).encode(frame).unwrap();
    let (gpu, used) = Encoder::with_config(cfg.clone()).encode_with(frame, Some(enc)).unwrap();
    (cpu, gpu, used)
}

/// The correctness bar of a GPU frame (ADR 0003): it parses, decodes, and is within 0.1 dB.
fn check_frame(name: &str, cpu: &[u8], gpu: &[u8], src: &Frame, failures: &mut Vec<String>) {
    let layout = match frame_layout(gpu) {
        Ok(l) => l,
        Err(e) => return failures.push(format!("{name}: layout: {e}")),
    };
    for sl in &layout.slices {
        let size: u32 = sl.comps.iter().map(|c| c.1).sum();
        if size > 65535 {
            failures.push(format!("{name}: slice of {size} bytes"));
        }
    }
    if let Err(e) = decode_frame(gpu) {
        return failures.push(format!("{name}: decode: {e}"));
    }
    let (pc, pg) = (psnr(cpu, src), psnr(gpu, src));
    if pg < pc - 0.1 {
        failures.push(format!("{name}: PSNR {pg:.3} dB, CPU {pc:.3} dB"));
    }
}

#[test]
fn gpu_prores_frames_parse_decode_and_match_the_cpu_quality() {
    let Some(mut enc) = encoder("gpu_prores_frames_parse_decode_and_match_the_cpu_quality") else { return };
    let mut failures = Vec::new();
    for c in cases() {
        let (cpu, gpu, used) = both(&mut enc, &c.cfg, &c.frame);
        assert!(used, "{}: the GPU did not encode the frame", c.name);
        check_frame(&c.name, &cpu, &gpu, &c.frame, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_gpu_matches_the_cpu_byte_for_byte_on_most_frames() {
    let Some(mut enc) = encoder("the_gpu_matches_the_cpu_byte_for_byte_on_most_frames") else { return };
    let (mut same, mut total, mut differ) = (0usize, 0usize, Vec::new());
    for c in cases() {
        let (cpu, gpu, used) = both(&mut enc, &c.cfg, &c.frame);
        assert!(used, "{}", c.name);
        total += 1;
        if cpu == gpu {
            same += 1;
        } else {
            differ.push(c.name);
        }
    }
    eprintln!("byte-identical: {same}/{total}; differing: {differ:?}");
    // f32 fused multiply-adds may move a coefficient that sits on a rounding tie, nothing more
    assert!(same * 10 >= total * 9, "only {same}/{total} frames byte-identical: {differ:?}");
}

#[test]
fn noise_and_black_frames_stay_within_the_profile_limits() {
    let Some(mut enc) = encoder("noise_and_black_frames_stay_within_the_profile_limits") else { return };
    let mut failures = Vec::new();
    let huge_budget = |mut cfg: EncoderConfig| {
        cfg.bits_per_mb = Some(u32::MAX / 4096);
        cfg
    };
    let cases = [
        // rate control at Q_MAX
        ("Proxy noise 1080p", EncoderConfig::new(Profile::Proxy, 1920, 1080), noise_frame(Profile::Proxy, 1920, 1080, false)),
        ("4444 noise + alpha noise", EncoderConfig::new(Profile::P4444, 640, 360), noise_frame(Profile::P4444, 640, 360, true)),
        // the 16-bit slice size guard (a budget far above any slice)
        ("Proxy noise, size guard", huge_budget(EncoderConfig::new(Profile::Proxy, 256, 64)), noise_frame(Profile::Proxy, 256, 64, false)),
        ("4444XQ noise + alpha, size guard", huge_budget(EncoderConfig::new(Profile::P4444Xq, 256, 64)), noise_frame(Profile::P4444Xq, 256, 64, true)),
        ("HQ black 1080p", EncoderConfig::new(Profile::Hq, 1920, 1080), Frame::new(1920, 1080, ChromaFormat::Yuv422, 10, false)),
        ("HQ black 17x13", EncoderConfig::new(Profile::Hq, 17, 13), Frame::new(17, 13, ChromaFormat::Yuv422, 10, false)),
        ("4444 black + alpha", EncoderConfig::new(Profile::P4444, 64, 48), Frame::new(64, 48, ChromaFormat::Yuv444, 10, true)),
    ];
    for (name, cfg, frame) in cases {
        let target = Encoder::with_config(cfg.clone()).target_frame_bytes();
        let (cpu, gpu, used) = both(&mut enc, &cfg, &frame);
        assert!(used, "{name}: the GPU did not encode the frame");
        check_frame(name, &cpu, &gpu, &frame, &mut failures);
        // over the target only where the CPU is too (Q_MAX reached)
        if gpu.len() > target.max(cpu.len()) {
            failures.push(format!("{name}: {} bytes, target {target}, CPU {}", gpu.len(), cpu.len()));
        }
        eprintln!("{name}: GPU {} bytes, CPU {} bytes, target {target}, identical {}", gpu.len(), cpu.len(), gpu == cpu);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The CPU's rate control over the bits `enc` measures (its `bits`, never its `choose`).
fn cpu_choice(enc: &mut ProResGpuEncoder, budget: u64, limits: &[u64], hint: u8) -> Option<(Vec<u8>, u8)> {
    rate_control_for_tests(budget, limits, hint, &mut |q| Some(enc.bits(q)?.iter().map(|c| c.iter().map(|&b| u64::from(b)).sum()).collect()))
}

/// Every slice's coded bits at `q`, as the rate control adds them.
fn slice_bits(enc: &mut ProResGpuEncoder, q: u8) -> Vec<u64> {
    enc.bits(q).unwrap().iter().map(|c| c.iter().map(|&b| u64::from(b)).sum()).collect()
}

#[test]
fn gpu_rate_control_picks_the_cpus_quantisers() {
    let Some(mut enc) = encoder("gpu_rate_control_picks_the_cpus_quantisers") else { return };
    let (mut failures, mut runs, mut guarded) = (Vec::new(), 0usize, 0usize);
    for c in cases() {
        let e = Encoder::with_config(c.cfg.clone());
        let jobs = e.slice_jobs();
        let n = jobs.len();
        assert!(enc.load(&c.cfg, &c.frame, &jobs), "{}: not loaded", c.name);
        // the encoder's own budget and 16-bit slice limits
        let hsize = if c.cfg.encode_alpha { 8 } else { 6 };
        let alpha: Vec<usize> = if c.cfg.encode_alpha { enc.alpha().unwrap().iter().map(Vec::len).collect() } else { vec![0; n] };
        let limits: Vec<u64> = alpha.iter().map(|a| (65535 - hsize - a) as u64 * 8).collect();
        let mbs = u64::from(c.cfg.width.div_ceil(16)) * u64::from(c.cfg.height.div_ceil(16));
        let budget = (u64::from(e.bits_per_mb()) * mbs).saturating_sub(8 * (36 + n * (2 + hsize)) as u64);
        let (_, right) = cpu_choice(&mut enc, budget, &limits, 4).unwrap();
        // a budget exactly the frame at the right quantiser
        let exact: u64 = slice_bits(&mut enc, right).iter().sum();
        // the size guard: limits at the bits five and two steps coarser (those slices are raised a
        // few quantisers), no limit on the rest; on small frames also no slice fitting at all on
        // every third slice (it ends at Q_MAX: a round of whole-frame bits per quantiser on the CPU)
        let coarser = |enc: &mut ProResGpuEncoder, by: u8| slice_bits(enc, right.saturating_add(by).min(224));
        let (five, two) = (coarser(&mut enc, 5), coarser(&mut enc, 2));
        let tight: Vec<u64> = (0..n).map(|i| [five[i], two[i], u64::MAX][i % 3]).collect();
        let none_fit: Vec<u64> = (0..n).map(|i| [0, two[i], u64::MAX][i % 3]).collect();
        let mut questions: Vec<(u8, u64, &[u64])> = vec![
            (0, budget, &limits),
            (1, budget, &limits),
            (right, budget, &limits),
            (224, budget, &limits),
            (255, budget, &limits),
            (right, exact, &limits),
            (right.saturating_sub(1).max(1), exact.saturating_sub(1), &limits),
            // nothing fits
            (right, 0, &limits),
            (224, 1, &limits),
            // everything fits at Q_MIN
            (224, u64::MAX, &limits),
            (right, budget, &tight),
            (1, budget, &tight),
            (right, exact, &tight),
        ];
        if c.cfg.width <= 64 {
            questions.extend([(right, budget, &none_fit[..]), (right, u64::MAX, &none_fit[..])]);
        }
        for (hint, budget, limits) in questions {
            let gpu = enc.choose(hint, budget, limits);
            let cpu = cpu_choice(&mut enc, budget, limits, hint);
            runs += 1;
            match (&gpu, &cpu) {
                (Some(g), Some(_)) if gpu == cpu => guarded += usize::from(g.0.iter().any(|&q| q > g.1)),
                _ => failures.push(format!("{}: hint {hint}, budget {budget}: GPU {gpu:?}, CPU {cpu:?}", c.name)),
            }
        }
    }
    assert!(failures.is_empty(), "{} of {runs} differ:\n{}", failures.len(), failures.join("\n"));
    assert!(guarded > 0, "the size guard never raised a slice");
}

#[test]
fn the_rate_control_rounds_cover_every_cpu_search() {
    // every hint against every place the frame starts to fit: the distinct quantisers the CPU
    // measures (the search and the one finer step) are at most the GPU's rounds
    let mut most = 0usize;
    for hint in 0..=255u8 {
        for first_fit in 1..=225u32 {
            let mut asked = Vec::new();
            let mut bits = |q: u8| {
                if !asked.contains(&q) {
                    asked.push(q);
                }
                Some(vec![if u32::from(q) >= first_fit { 10 } else { 1000 }])
            };
            rate_control_for_tests(100, &[u64::MAX], hint, &mut bits).unwrap();
            most = most.max(asked.len());
        }
    }
    assert!(most <= RC_ROUNDS, "the CPU measures up to {most} quantisers, the GPU runs {RC_ROUNDS} rounds");
    assert_eq!(most, RC_ROUNDS, "the bound is tight");
}

#[test]
fn oversized_frames_are_declined() {
    // the limit check, fed small limits: a 16K frame does not fit, a small one does
    let small = wgpu::Limits::downlevel_defaults();
    let coeff = |w: u64, h: u64| w.div_ceil(16) * h.div_ceil(16) * 8 * 64 * 4;
    assert!(!fits(&small, coeff(16384, 16384), 16384 * 16384 * 4, 16384, 16384));
    assert!(!fits(&small, coeff(1920, 1080), 1920 * 1080 * 4, 4096, 1080), "wider than the texture limit");
    assert!(!fits(&small, small.max_storage_buffer_binding_size + 1, 16, 64, 64), "coefficients over the binding limit");
    assert!(!fits(&small, 16, small.max_storage_buffer_binding_size + 1, 64, 64), "output over the binding limit");
    assert!(!fits(&small, 16, 16, 0, 64), "empty");
    assert!(fits(&small, coeff(1920, 1080), 1920 * 1080 * 4, 1920, 1080));

    // the real device: a frame wider than its textures may be goes to the CPU
    let Some(mut enc) = encoder("oversized_frames_are_declined") else { return };
    let w = enc.limits().max_texture_dimension_2d.saturating_add(1);
    if w > 65535 {
        eprintln!("SKIPPED oversized_frames_are_declined (device part): textures up to {} wide", w - 1);
        return;
    }
    let (h, profile) = (16, Profile::Proxy);
    let cfg = EncoderConfig::new(profile, w, h);
    let frame = test_frame(profile, w, h, false, 3);
    let jobs = Encoder::with_config(cfg.clone()).slice_jobs();
    assert!(!enc.load(&cfg, &frame, &jobs), "{w}x{h} loaded");
    let (cpu, gpu, used) = both(&mut enc, &cfg, &frame);
    assert!(!used, "{w}x{h}: the GPU took the frame");
    assert_eq!(gpu, cpu);
    // and the encoder still takes the next frame that fits
    let small_cfg = EncoderConfig::new(profile, 64, 48);
    let (_, _, used) = both(&mut enc, &small_cfg, &test_frame(profile, 64, 48, false, 4));
    assert!(used, "a declined frame must not disable the backend");
}

#[test]
fn a_second_device_error_retires_the_backend() {
    let Some(mut enc) = encoder("a_second_device_error_retires_the_backend") else { return };
    let (profile, w, h) = (Profile::Hq, 64, 48);
    let cfg = EncoderConfig::new(profile, w, h);
    let frame = test_frame(profile, w, h, false, 5);
    let jobs = Encoder::with_config(cfg.clone()).slice_jobs();
    // one error: that frame goes to the CPU, the next one is tried again
    assert!(enc.load(&cfg, &frame, &jobs));
    enc.device_error();
    assert!(enc.bits(4).is_none(), "the failed frame is gone");
    let (_, _, used) = both(&mut enc, &cfg, &frame);
    assert!(used, "after one device error");
    // a second: off for good
    enc.device_error();
    assert!(!enc.load(&cfg, &frame, &jobs));
    let (cpu, gpu, used) = both(&mut enc, &cfg, &frame);
    assert!(!used, "after two device errors");
    assert_eq!(gpu, cpu);
}

/// Interlaced frames (two field pictures, either field first) of every profile at 1080i: the GPU
/// codes them, and they meet the bar of a progressive frame.
#[test]
fn gpu_interlaced_prores_matches_the_cpu_quality() {
    let Some(mut enc) = encoder("gpu_interlaced_prores_matches_the_cpu_quality") else { return };
    let (mut failures, mut same, mut total) = (Vec::new(), 0usize, 0usize);
    for interlace in [Interlace::TopFieldFirst, Interlace::BottomFieldFirst] {
        for profile in Profile::ALL {
            let alpha = profile.chroma() == ChromaFormat::Yuv444;
            let (w, h) = (1920, 1080);
            let cfg = EncoderConfig { interlace, encode_alpha: alpha, ..EncoderConfig::new(profile, w, h) };
            let frame = test_frame(profile, w, h, alpha, 21);
            let name = format!("{interlace:?} {profile:?} {w}x{h}");
            let (cpu, gpu, used) = both(&mut enc, &cfg, &frame);
            assert!(used, "{name}: the GPU did not encode the frame");
            check_frame(&name, &cpu, &gpu, &frame, &mut failures);
            if let Ok(l) = frame_layout(&gpu) {
                assert_eq!(l.header.interlace, interlace, "{name}");
            }
            total += 1;
            same += usize::from(cpu == gpu);
        }
    }
    eprintln!("interlaced 1080: byte-identical {same}/{total}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Interlaced frames at odd sizes and every slice width (the bottom field one line shorter, edge
/// replication within each field), alpha too: as good as the CPU's, mostly byte-identical.
#[test]
fn gpu_interlaced_prores_odd_sizes_match_the_cpu() {
    let Some(mut enc) = encoder("gpu_interlaced_prores_odd_sizes_match_the_cpu") else { return };
    let (mut failures, mut same, mut total, mut differ) = (Vec::new(), 0usize, 0usize, Vec::new());
    for interlace in [Interlace::TopFieldFirst, Interlace::BottomFieldFirst] {
        for c in cases().into_iter().filter(|c| c.cfg.height >= 2) {
            let cfg = EncoderConfig { interlace, ..c.cfg };
            let name = format!("{interlace:?} {}", c.name);
            let (cpu, gpu, used) = both(&mut enc, &cfg, &c.frame);
            assert!(used, "{name}: the GPU did not encode the frame");
            check_frame(&name, &cpu, &gpu, &c.frame, &mut failures);
            total += 1;
            if cpu == gpu {
                same += 1;
            } else {
                differ.push(name);
            }
        }
        for (profile, w, h) in [(Profile::Hq, 16, 2), (Profile::Lt, 33, 3), (Profile::P4444, 70, 37)] {
            let alpha = profile.chroma() == ChromaFormat::Yuv444;
            let cfg = EncoderConfig { interlace, encode_alpha: alpha, ..EncoderConfig::new(profile, w, h) };
            let frame = test_frame(profile, w, h, alpha, 5);
            let name = format!("{interlace:?} {profile:?} {w}x{h}");
            let (cpu, gpu, used) = both(&mut enc, &cfg, &frame);
            assert!(used, "{name}: the GPU did not encode the frame");
            check_frame(&name, &cpu, &gpu, &frame, &mut failures);
            total += 1;
            if cpu == gpu {
                same += 1;
            } else {
                differ.push(name);
            }
        }
    }
    eprintln!("interlaced: byte-identical {same}/{total}; differing: {differ:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(same * 10 >= total * 9, "only {same}/{total} frames byte-identical: {differ:?}");
}

/// The input frame's own interlace flag is metadata the CPU encoder does not read: the field
/// structure is the configuration's, on the GPU as on the CPU.
#[test]
fn the_frames_interlace_flag_does_not_change_the_coding() {
    let Some(mut enc) = encoder("the_frames_interlace_flag_does_not_change_the_coding") else { return };
    let (profile, w, h) = (Profile::Hq, 64, 48);
    let cfg = EncoderConfig::new(profile, w, h);
    let mut frame = test_frame(profile, w, h, false, 6);
    frame.interlace = Interlace::TopFieldFirst;
    let (cpu, gpu, used) = both(&mut enc, &cfg, &frame);
    assert!(used);
    assert_eq!(frame_layout(&gpu).unwrap().header.interlace, Interlace::Progressive);
    assert_eq!(gpu, cpu);
}

/// The constants hold both scan orders and every quantiser's step, none over another.
#[test]
fn the_constants_keep_both_scans() {
    let c = consts();
    let words = |at: usize| c.get(at..at + 64).unwrap().to_vec();
    let scan = |s: &[u8; 64]| s.iter().map(|&v| u32::from(v)).collect::<Vec<u32>>();
    assert_eq!(words(C_SCAN), scan(&filmcraft_prores::layout::tables::SCAN_PROGRESSIVE));
    assert_eq!(words(C_SCAN_INTERLACED), scan(&filmcraft_prores::layout::tables::SCAN_INTERLACED));
    let inv = c.get(C_INV..C_SCAN_INTERLACED).unwrap();
    assert_eq!(inv.len(), 225);
    assert!(inv.iter().skip(1).all(|&w| f32::from_bits(w) > 0.0 && f32::from_bits(w) <= 0.25));
}

/// A QuickTime file of `frames` (one ProRes track).
fn mov(profile: Profile, w: u32, h: u32, frames: &[Vec<u8>]) -> Vec<u8> {
    use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let track = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(profile.fourcc()), w as u16, h as u16), 25)).unwrap();
    for f in frames {
        mux.write_sample(track, WriteSample { data: f, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    }
    mux.finish().unwrap().into_inner()
}

#[test]
fn gpu_prores_frames_decode_in_ffmpeg() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("gpu prores encode") else { return };
    let Some(mut enc) = encoder("gpu_prores_frames_decode_in_ffmpeg") else { return };
    let dir = filmcraft_testkit::fixtures_dir("gpu-prores-enc");
    let mut failures = Vec::new();
    for (profile, w, h, alpha, log2) in [
        (Profile::Hq, 1920, 1080, false, 3),
        (Profile::Proxy, 1919, 1079, false, 3),
        (Profile::Lt, 17, 13, false, 0),
        (Profile::Standard, 64, 48, false, 2),
        (Profile::P4444, 333, 201, true, 3),
        (Profile::P4444Xq, 640, 360, false, 1),
    ] {
        let name = format!("{profile:?}_{w}x{h}{}", if alpha { "_alpha" } else { "" });
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.log2_slice_mbs = log2;
        cfg.encode_alpha = alpha;
        let frames: Vec<Frame> = (0..2).map(|i| test_frame(profile, w, h, alpha, 9 + i)).collect();
        let mut e = Encoder::with_config(cfg);
        let coded: Vec<Vec<u8>> = frames
            .iter()
            .map(|f| {
                let (c, used) = e.encode_with(f, Some(&mut enc)).unwrap();
                assert!(used, "{name}");
                c
            })
            .collect();
        let path = dir.join(format!("{name}.mov"));
        std::fs::write(&path, mov(profile, w, h, &coded)).unwrap();
        let fmt = match (profile.chroma(), alpha) {
            (ChromaFormat::Yuv422, _) => "yuv422p10le",
            (ChromaFormat::Yuv444, false) => "yuv444p12le",
            (ChromaFormat::Yuv444, true) => "yuva444p12le",
        };
        let out = Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", fmt, "-"])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || !err.trim().is_empty() {
            failures.push(format!("{name}: ffmpeg: {err}"));
            continue;
        }
        // ffmpeg's decode agrees with ours to within a code (its IDCT is not ours)
        let raw: Vec<u16> = out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let mut at = 0usize;
        for (i, c) in coded.iter().enumerate() {
            let ours = decode_frame(c).unwrap();
            let planes = [Some(&ours.y), Some(&ours.cb), Some(&ours.cr), ours.alpha.as_ref()];
            for p in planes.into_iter().flatten() {
                let Some(theirs) = raw.get(at..at + p.len()) else {
                    failures.push(format!("{name} #{i}: ffmpeg returned {} samples", raw.len()));
                    break;
                };
                let max = p.iter().zip(theirs).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                if max > 1 {
                    failures.push(format!("{name} #{i}: ffmpeg's decode differs by {max}"));
                }
                at += p.len();
            }
        }
        if at != raw.len() {
            failures.push(format!("{name}: ffmpeg returned {} samples, we decode {at}", raw.len()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// GPU-coded interlaced frames, top and bottom field first: ffmpeg decodes them without an error,
/// agrees with our decoder within a code, and reports the coded field order on every frame.
#[test]
fn gpu_interlaced_prores_frames_decode_in_ffmpeg() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("gpu interlaced prores encode") else { return };
    let Some(probe) = filmcraft_testkit::ffprobe_or_skip("gpu interlaced prores field order") else { return };
    let Some(mut enc) = encoder("gpu_interlaced_prores_frames_decode_in_ffmpeg") else { return };
    let dir = filmcraft_testkit::fixtures_dir("gpu-prores-enc");
    let mut failures = Vec::new();
    for (interlace, tff) in [(Interlace::TopFieldFirst, "1"), (Interlace::BottomFieldFirst, "0")] {
        for (profile, w, h, alpha) in [(Profile::Hq, 1920, 1080, false), (Profile::Lt, 720, 487, false), (Profile::P4444, 333, 201, true)] {
            let name = format!("il_{interlace:?}_{profile:?}_{w}x{h}");
            let cfg = EncoderConfig { interlace, encode_alpha: alpha, ..EncoderConfig::new(profile, w, h) };
            let mut e = Encoder::with_config(cfg);
            let coded: Vec<Vec<u8>> = (0..2)
                .map(|i| {
                    let (c, used) = e.encode_with(&test_frame(profile, w, h, alpha, 31 + i), Some(&mut enc)).unwrap();
                    assert!(used, "{name}");
                    c
                })
                .collect();
            let path = dir.join(format!("{name}.mov"));
            std::fs::write(&path, mov(profile, w, h, &coded)).unwrap();
            let fmt = if alpha { "yuva444p12le" } else { "yuv422p10le" };
            let out = Command::new(&ff)
                .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
                .arg(&path)
                .args(["-f", "rawvideo", "-pix_fmt", fmt, "-"])
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() || !err.trim().is_empty() {
                failures.push(format!("{name}: ffmpeg: {err}"));
                continue;
            }
            let raw: Vec<u16> = out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let mut at = 0usize;
            for (i, c) in coded.iter().enumerate() {
                let ours = decode_frame(c).unwrap();
                for p in [Some(&ours.y), Some(&ours.cb), Some(&ours.cr), ours.alpha.as_ref()].into_iter().flatten() {
                    let max = raw.get(at..at + p.len()).map(|t| p.iter().zip(t).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0));
                    if max.is_none_or(|m| m > 1) {
                        failures.push(format!("{name} #{i}: ffmpeg's decode differs by {max:?}"));
                    }
                    at += p.len();
                }
            }
            // the decoder reads the field order from every frame header
            let frames = Command::new(&probe)
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=interlaced_frame,top_field_first", "-of", "csv=p=0"])
                .arg(&path)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&frames.stdout);
            let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            if lines.len() != coded.len() || lines.iter().any(|l| *l != format!("1,{tff}")) {
                failures.push(format!("{name}: ffprobe reports {lines:?}, want 1,{tff} per frame"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "benchmark"]
fn prores_gpu_encode_bench() {
    let Some(mut enc) = encoder("prores_gpu_encode_bench") else { return };
    let (w, h, profile) = (3840, 2160, Profile::Hq);
    let frames: Vec<Frame> = (0..30).map(|i| test_frame(profile, w, h, false, 100 + i)).collect();
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let mut cpu_enc = Encoder::new(profile, w, h);
    let cpu: Vec<f64> = frames
        .iter()
        .map(|f| {
            let t = std::time::Instant::now();
            cpu_enc.encode(f).unwrap();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    let mut gpu_enc = Encoder::new(profile, w, h);
    let mut used_all = true;
    let gpu: Vec<f64> = frames
        .iter()
        .map(|f| {
            let t = std::time::Instant::now();
            let (_, used) = gpu_enc.encode_with(f, Some(&mut enc)).unwrap();
            used_all &= used;
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    assert!(used_all, "the GPU declined a frame");
    eprintln!("ProRes HQ {w}x{h}, {} frames: CPU median {:.1} ms, GPU median {:.1} ms", frames.len(), median(cpu), median(gpu));
}

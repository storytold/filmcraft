//! The GPU DNxHR encoder against the CPU encoder (the reference): every GPU frame parses, decodes
//! (with our decoder and with ffmpeg, when present), has the profile's CBR size and is within
//! 0.1 dB of the CPU frame's PSNR; lossless alpha decodes exactly, DCT alpha within the same PSNR
//! rule (ffmpeg can't read VC-3 alpha, so alpha is checked with our decoder only); most frames are
//! byte-identical to the CPU's.

use std::process::Command;

use filmcraft_dnx::{
    AlphaCoding, ChromaFormat, Encoder, EncoderConfig, Frame, Interlace, MacroblockBackend, Profile, decode_frame, frame_layout, rate_control_for_tests,
};

use crate::dnx_enc::{DnxGpuEncoder, RC_ROUNDS};
use crate::intra_enc::fits;

fn encoder(test: &str) -> Option<DnxGpuEncoder> {
    let e = DnxGpuEncoder::new();
    if e.is_none() {
        eprintln!("SKIPPED {test}: no adapter");
    }
    e
}

fn chroma(profile: Profile) -> ChromaFormat {
    if profile == Profile::R444 { ChromaFormat::Yuv444 } else { ChromaFormat::Yuv422 }
}

/// Deterministic busy planes at `depth` bits (gradient + checkerboard + xorshift noise); alpha is
/// an opaque disc over horizontal bands (flat enough for lossless alpha to fit even at LB).
fn test_frame(profile: Profile, w: u32, h: u32, depth: u8, rgb: bool, alpha: bool, seed: u32) -> Frame {
    let mut f = Frame::new(w, h, chroma(profile), depth, alpha);
    f.rgb = rgb;
    let mut s = seed.max(1);
    let mut noise = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let max = (1u32 << depth) - 1;
    let sh = u32::from(depth) - 8;
    let (wu, cw) = (w as usize, f.chroma_width() as usize);
    for y in 0..h as usize {
        for x in 0..wu {
            let check = if (x / 4 + y / 4) % 2 == 0 { 15 } else { 0 };
            f.y[y * wu + x] = ((((16 + (x * 3 + y * 2) % 150 + check) as u32) << sh) + noise() % (10 << sh)).min(max) as u16;
            if let Some(a) = f.alpha.as_mut() {
                let disc = (x as i64 - wu as i64 / 2).pow(2) + (y as i64 - h as i64 / 2).pow(2) < (h as i64 / 3).pow(2);
                a[y * wu + x] = if disc { max as u16 } else { ((((y / 32) * 37) as u32) << sh).min(max) as u16 };
            }
        }
        for x in 0..cw {
            f.cb[y * cw + x] = ((((100 + (x + y) % 50) as u32) << sh) + noise() % (8 << sh)).min(max) as u16;
            f.cr[y * cw + x] = ((((120 + (x * 5 + y) % 30) as u32) << sh) + noise() % (8 << sh)).min(max) as u16;
        }
    }
    f
}

/// Full-scale noise in every plane (alpha too).
fn noise_frame(profile: Profile, w: u32, h: u32, depth: u8, alpha: bool) -> Frame {
    let mut f = Frame::new(w, h, chroma(profile), depth, alpha);
    let mut s = 12345u32;
    let planes = f.y.iter_mut().chain(f.cb.iter_mut()).chain(f.cr.iter_mut()).chain(f.alpha.iter_mut().flatten());
    for v in planes {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        *v = (s % (1 << depth)) as u16;
    }
    f
}

/// A source sample at the coded depth, as the encoder converts it.
fn coded(v: u16, from: u8, to: u8) -> u16 {
    let shift = i32::from(to) - i32::from(from);
    let v = i32::from(v);
    let c = if shift >= 0 { v << shift } else { (v + (1 << (-shift - 1))) >> -shift };
    c.min((1 << to) - 1) as u16
}

/// PSNR of `planes` of a decode against the source at the coded depth.
fn psnr_of(pairs: &[(&[u16], &[u16])], from: u8, to: u8) -> f64 {
    let (mut se, mut n) = (0f64, 0usize);
    for (dec, src) in pairs {
        for (a, b) in dec.iter().zip(src.iter()) {
            se += (f64::from(*a) - f64::from(coded(*b, from, to))).powi(2);
            n += 1;
        }
    }
    let peak = f64::from((1u32 << to) - 1);
    if se == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak / (se / n as f64)).log10() }
}

/// Colour PSNR and (with alpha) alpha PSNR of a frame's decode against the source.
fn psnr(dec: &Frame, src: &Frame) -> (f64, Option<f64>) {
    let (from, to) = (src.bit_depth, dec.bit_depth);
    let colour = psnr_of(&[(&dec.y, &src.y), (&dec.cb, &src.cb), (&dec.cr, &src.cr)], from, to);
    let alpha = match (&dec.alpha, &src.alpha) {
        (Some(a), Some(b)) => Some(psnr_of(&[(a, b)], from, to)),
        _ => None,
    };
    (colour, alpha)
}

struct Case {
    name: String,
    cfg: EncoderConfig,
    frame: Frame,
}

/// The coded colour configurations: LB, SQ, HQ (8-bit), HQX (10 and 12-bit), 444 (10 and 12-bit,
/// Y'CbCr and RGB).
fn colours() -> Vec<(Profile, u8, bool)> {
    let mut v = vec![(Profile::Lb, 8, false), (Profile::Sq, 8, false), (Profile::Hq, 8, false), (Profile::Hqx, 10, false), (Profile::Hqx, 12, false)];
    for depth in [10, 12] {
        for rgb in [false, true] {
            v.push((Profile::R444, depth, rgb));
        }
    }
    v
}

/// Every colour configuration × no / DCT / lossless alpha × odd and common sizes (4:2:2 needs an
/// even width: odd widths become one wider). The 17×13 and 64×48 frames come at another input
/// depth than the coded one (8 or 10 bits up, 16 bits down).
fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    for (profile, depth, rgb) in colours() {
        for alpha in [None, Some(AlphaCoding::Dct), Some(AlphaCoding::Lossless)] {
            for (w, h) in [(1, 1), (17, 13), (64, 48), (1919, 1079), (1920, 1080)] {
                let w = if chroma(profile) == ChromaFormat::Yuv422 && w % 2 == 1 { w + 1 } else { w };
                let input = match (w, h) {
                    (17 | 18, 13) => {
                        if depth == 8 {
                            10
                        } else {
                            8
                        }
                    }
                    (64, 48) => 16,
                    _ => depth,
                };
                let cfg = EncoderConfig { bit_depth: depth, alpha, ..EncoderConfig::new(profile, w, h) };
                let seed = w ^ (h << 8) ^ u32::from(depth) << 20;
                let name = format!("{profile:?} {depth}-bit{} {alpha:?} {w}x{h} from {input}-bit", if rgb { " RGB" } else { "" });
                v.push(Case { name, cfg, frame: test_frame(profile, w, h, input, rgb, alpha.is_some(), seed) });
            }
        }
    }
    v
}

/// Encode `frame` on the CPU and with the GPU backend: (cpu, gpu, backend used), `None` when the
/// CPU refuses the frame (lossless alpha is not rate controlled and can overflow); the GPU path must
/// then refuse it too.
fn both(enc: &mut DnxGpuEncoder, cfg: &EncoderConfig, frame: &Frame) -> Option<(Vec<u8>, Vec<u8>, bool)> {
    let cpu = Encoder::with_config(cfg.clone()).unwrap().encode(frame);
    let gpu = Encoder::with_config(cfg.clone()).unwrap().encode_with(frame, Some(enc));
    match (cpu, gpu) {
        (Ok(cpu), Ok((gpu, used))) => Some((cpu, gpu, used)),
        (Err(_), Err(_)) => None,
        (c, g) => panic!("cpu {:?}, gpu {:?}", c.map(|v| v.len()), g.map(|v| v.0.len())),
    }
}

/// The correctness bar of a GPU frame (ADR 0003): it parses, decodes, has the CBR size, and is
/// within 0.1 dB; lossless alpha is exact, DCT alpha within 0.1 dB too.
fn check_frame(name: &str, cfg: &EncoderConfig, cpu: &[u8], gpu: &[u8], src: &Frame, failures: &mut Vec<String>) {
    let size = Encoder::with_config(cfg.clone()).unwrap().frame_size() as usize;
    if gpu.len() != size {
        failures.push(format!("{name}: {} bytes, the profile's frame is {size}", gpu.len()));
    }
    if let Err(e) = frame_layout(gpu) {
        return failures.push(format!("{name}: layout: {e}"));
    }
    let dg = match decode_frame(gpu) {
        Ok(d) => d,
        Err(e) => return failures.push(format!("{name}: decode: {e}")),
    };
    let dc = decode_frame(cpu).unwrap();
    let ((pc, ac), (pg, ag)) = (psnr(&dc, src), psnr(&dg, src));
    if pg < pc - 0.1 {
        failures.push(format!("{name}: PSNR {pg:.3} dB, CPU {pc:.3} dB"));
    }
    match (cfg.alpha, ac, ag) {
        (None, _, None) => {}
        (Some(AlphaCoding::Lossless), _, Some(ag)) if ag.is_infinite() => {}
        (Some(AlphaCoding::Dct), Some(ac), Some(ag)) if ag >= ac - 0.1 => {}
        _ => failures.push(format!("{name}: alpha PSNR {ag:?} dB, CPU {ac:?} dB")),
    }
}

#[test]
fn gpu_dnx_frames_parse_decode_and_match_the_cpu_quality() {
    let Some(mut enc) = encoder("gpu_dnx_frames_parse_decode_and_match_the_cpu_quality") else { return };
    let (mut failures, mut checked, mut refused) = (Vec::new(), 0usize, Vec::new());
    for c in cases() {
        let Some((cpu, gpu, used)) = both(&mut enc, &c.cfg, &c.frame) else {
            refused.push(c.name);
            continue;
        };
        assert!(used, "{}: the GPU did not encode the frame", c.name);
        check_frame(&c.name, &c.cfg, &cpu, &gpu, &c.frame, &mut failures);
        checked += 1;
    }
    eprintln!("checked {checked} frames; refused by CPU and GPU alike: {refused:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(refused.len() * 20 <= checked, "too many refused frames: {refused:?}");
}

#[test]
fn the_gpu_matches_the_cpu_byte_for_byte_on_most_frames() {
    let Some(mut enc) = encoder("the_gpu_matches_the_cpu_byte_for_byte_on_most_frames") else { return };
    let (mut same, mut total, mut differ) = (0usize, 0usize, Vec::new());
    for c in cases() {
        let Some((cpu, gpu, used)) = both(&mut enc, &c.cfg, &c.frame) else { continue };
        assert!(used, "{}", c.name);
        total += 1;
        if cpu == gpu {
            same += 1;
        } else {
            differ.push(c.name);
        }
    }
    eprintln!("byte-identical: {same}/{total}; differing: {differ:?}");
    assert!(same * 10 >= total * 9, "only {same}/{total} frames byte-identical: {differ:?}");
}

#[test]
fn noise_and_black_frames_stay_within_the_profile_limits() {
    let Some(mut enc) = encoder("noise_and_black_frames_stay_within_the_profile_limits") else { return };
    let mut failures = Vec::new();
    let with = |profile: Profile, w: u32, h: u32, depth: u8, alpha: Option<AlphaCoding>| EncoderConfig {
        bit_depth: depth,
        alpha,
        ..EncoderConfig::new(profile, w, h)
    };
    let black = |profile: Profile, w: u32, h: u32, depth: u8, alpha: bool| Frame::new(w, h, chroma(profile), depth, alpha);
    let cases = [
        // the rate control at its coarsest quantisers
        ("LB noise 1080p", with(Profile::Lb, 1920, 1080, 8, None), noise_frame(Profile::Lb, 1920, 1080, 8, false)),
        ("LB noise + DCT alpha noise", with(Profile::Lb, 640, 360, 8, Some(AlphaCoding::Dct)), noise_frame(Profile::Lb, 640, 360, 8, true)),
        ("444 12-bit noise + DCT alpha noise", with(Profile::R444, 256, 64, 12, Some(AlphaCoding::Dct)), noise_frame(Profile::R444, 256, 64, 12, true)),
        ("HQ black 1080p", with(Profile::Hq, 1920, 1080, 8, None), black(Profile::Hq, 1920, 1080, 8, false)),
        ("HQX 12-bit black 18x13", with(Profile::Hqx, 18, 13, 12, None), black(Profile::Hqx, 18, 13, 12, false)),
        ("444 black + DCT alpha", with(Profile::R444, 64, 48, 10, Some(AlphaCoding::Dct)), black(Profile::R444, 64, 48, 10, true)),
        ("444 black + lossless alpha", with(Profile::R444, 64, 48, 10, Some(AlphaCoding::Lossless)), black(Profile::R444, 64, 48, 10, true)),
        ("LB black + lossless alpha 1080p", with(Profile::Lb, 1920, 1080, 8, Some(AlphaCoding::Lossless)), black(Profile::Lb, 1920, 1080, 8, true)),
    ];
    for (name, cfg, frame) in cases {
        let Some((cpu, gpu, used)) = both(&mut enc, &cfg, &frame) else {
            eprintln!("{name}: refused by the CPU and the GPU path alike");
            continue;
        };
        assert!(used, "{name}: the GPU did not encode the frame");
        check_frame(name, &cfg, &cpu, &gpu, &frame, &mut failures);
        eprintln!("{name}: GPU {} bytes, CPU {} bytes, identical {}", gpu.len(), cpu.len(), gpu == cpu);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // lossless noise does not fit any frame: both refuse it, and the GPU never hands one out
    let cfg = with(Profile::Lb, 256, 64, 8, Some(AlphaCoding::Lossless));
    assert!(both(&mut enc, &cfg, &noise_frame(Profile::Lb, 256, 64, 8, true)).is_none(), "lossless alpha noise fitted an LB frame");
}

/// Bits of every scan line (macroblock bits summed, padded to 4 bytes), added up.
fn padded_frame_bits(mb_bits: &[u32], nw: usize) -> u64 {
    mb_bits.chunks(nw).map(|row| row.iter().map(|&b| u64::from(b)).sum::<u64>().div_ceil(32) * 32).sum()
}

#[test]
fn gpu_rate_control_picks_the_cpus_quantisers() {
    let Some(mut enc) = encoder("gpu_rate_control_picks_the_cpus_quantisers") else { return };
    let (mut failures, mut runs, mut refined) = (Vec::new(), 0usize, 0usize);
    for c in cases() {
        assert!(enc.load(&c.cfg, &c.frame), "{}: not loaded", c.name);
        let (nw, ns) = (c.cfg.width.div_ceil(16) as usize, c.cfg.height.div_ceil(16) as usize);
        let e = Encoder::with_config(c.cfg.clone()).unwrap();
        // the encoder's own budget
        let budget = (u64::from(e.frame_size()) - u64::from(filmcraft_dnx::header::header_size_for(c.cfg.height)) - 4) * 8;
        let cpu_choice = |enc: &mut DnxGpuEncoder, hint: u32, budget: u64| rate_control_for_tests(hint, nw, ns, budget, &mut |q| enc.bits(q));
        let Some((_, right)) = cpu_choice(&mut enc, 8, budget) else {
            failures.push(format!("{}: no CPU answer", c.name));
            continue;
        };
        // a budget exactly the frame at the right quantiser
        let exact = padded_frame_bits(&enc.bits(right).unwrap(), nw);
        let questions: [(u32, u64); 10] = [
            (0, budget),
            (1, budget),
            (right, budget),
            (2047, budget),
            (u32::MAX, budget),
            (right, exact),
            (right + 1, exact.saturating_sub(1)),
            // nothing fits (the CPU answers Q_MAX without measuring it)
            (right, 0),
            (1, 1),
            // everything fits at 1
            (2047, u64::MAX),
        ];
        for (hint, budget) in questions {
            let gpu = enc.choose(hint, budget);
            let cpu = cpu_choice(&mut enc, hint, budget).map(|(qs, _)| qs);
            runs += 1;
            match &gpu {
                Some(g) if gpu == cpu => refined += usize::from(g.windows(2).any(|w| w[0] != w[1])),
                _ => failures.push(format!("{}: hint {hint}, budget {budget}: GPU {gpu:?}, CPU {cpu:?}", c.name)),
            }
        }
    }
    assert!(failures.is_empty(), "{} of {runs} differ:\n{}", failures.len(), failures.join("\n"));
    assert!(refined > 0, "no answer refined a macroblock");
}

#[test]
fn the_rate_control_rounds_cover_every_cpu_search() {
    // hints against every place the frame starts to fit: the distinct quantisers the CPU measures
    // (the search, the uniform one and the one finer step) are at most the GPU's rounds
    let hints = (0..=40u32).chain((41..2010).step_by(7)).chain(2010..=2049).chain([u32::MAX]);
    let mut most = 0usize;
    for hint in hints {
        for first_fit in 1..=2048u32 {
            let mut asked = Vec::new();
            let mut bits = |q: u32| {
                if !asked.contains(&q) {
                    asked.push(q);
                }
                Some(vec![if q >= first_fit { 10 } else { 1000 }])
            };
            rate_control_for_tests(hint, 1, 1, 100, &mut bits).unwrap();
            most = most.max(asked.len());
        }
    }
    assert!(most <= RC_ROUNDS, "the CPU measures up to {most} quantisers, the GPU runs {RC_ROUNDS} rounds");
    assert_eq!(most, RC_ROUNDS, "the bound is tight");
}

/// A device on the same adapter with small limits (a 2048 texture, 64 MiB buffers).
fn small_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = crate::adapter::native_instance();
    let adapter = crate::export_renderer::offscreen_adapter(&instance)?;
    let big = adapter.limits();
    let limits = wgpu::Limits { max_texture_dimension_2d: 2048, max_storage_buffer_binding_size: 64 << 20, max_buffer_size: 64 << 20, ..big };
    let desc = wgpu::DeviceDescriptor { label: Some("dnx-enc-small"), required_limits: limits, ..Default::default() };
    pollster::block_on(adapter.request_device(&desc)).ok()
}

#[test]
fn oversized_frames_are_declined() {
    // the limit check, fed small limits: a 16K DNxHR 444 frame with alpha does not fit, 1080p does
    let small = wgpu::Limits::downlevel_defaults();
    let coeff = |w: u64, h: u64, blocks: u64| w.div_ceil(16) * h.div_ceil(16) * blocks * 64 * 4;
    assert!(!fits(&small, coeff(16384, 16384, 16), 16384 * 16384 * 8, 16384, 16384));
    assert!(!fits(&small, coeff(8192, 4320, 16), 8192 * 4320 * 8, 8192, 4320), "8K 444 + alpha");
    assert!(!fits(&small, coeff(1920, 1080, 8), 1920 * 1080 * 4, 4096, 1080), "wider than the texture limit");
    assert!(fits(&small, coeff(1920, 1080, 8), 1920 * 1080 * 4, 1920, 1080));

    let Some(mut enc) = encoder("oversized_frames_are_declined") else { return };
    // the real device: a frame wider than its textures may be is not loaded
    let w = enc.limits().max_texture_dimension_2d.saturating_add(1);
    let cfg = EncoderConfig::new(Profile::Hq, w, 16);
    if w <= 65536 {
        let frame = test_frame(Profile::Hq, w, 16, 8, false, false, 3);
        assert!(!enc.load(&cfg, &frame), "{w}x16 loaded");
    }
    // a device with small limits: the encoder falls back to the CPU for what does not fit
    let Some((device, queue)) = small_device() else { return eprintln!("SKIPPED oversized_frames_are_declined (small device)") };
    let Some(mut small_enc) = DnxGpuEncoder::with_device(device, queue) else { panic!("an encoder on the small device") };
    for (profile, w, h, alpha) in [(Profile::Hq, 4096, 16, None), (Profile::R444, 2048, 2048, Some(AlphaCoding::Dct))] {
        let cfg = EncoderConfig { alpha, ..EncoderConfig::new(profile, w, h) };
        let frame = test_frame(profile, w, h, cfg.bit_depth, false, alpha.is_some(), 4);
        let (cpu, gpu, used) = both(&mut small_enc, &cfg, &frame).unwrap();
        assert!(!used, "{profile:?} {w}x{h}: the GPU took the frame");
        assert_eq!(gpu, cpu);
    }
    // and the encoder still takes the next frame that fits
    let cfg = EncoderConfig::new(Profile::Hq, 64, 48);
    let (_, _, used) = both(&mut small_enc, &cfg, &test_frame(Profile::Hq, 64, 48, 8, false, false, 5)).unwrap();
    assert!(used, "a declined frame must not disable the backend");
}

#[test]
fn a_second_device_error_retires_the_backend() {
    let Some(mut enc) = encoder("a_second_device_error_retires_the_backend") else { return };
    let cfg = EncoderConfig::new(Profile::Hq, 64, 48);
    let frame = test_frame(Profile::Hq, 64, 48, 8, false, false, 5);
    // one error: that frame goes to the CPU, the next one is tried again
    assert!(enc.load(&cfg, &frame));
    enc.device_error();
    assert!(enc.bits(40).is_none(), "the failed frame is gone");
    let (_, _, used) = both(&mut enc, &cfg, &frame).unwrap();
    assert!(used, "after one device error");
    // a second: off for good
    enc.device_error();
    assert!(!enc.load(&cfg, &frame));
    let (cpu, gpu, used) = both(&mut enc, &cfg, &frame).unwrap();
    assert!(!used, "after two device errors");
    assert_eq!(gpu, cpu);
}

#[test]
fn interlaced_frames_are_declined() {
    let Some(mut enc) = encoder("interlaced_frames_are_declined") else { return };
    let cfg = EncoderConfig::new(Profile::Hq, 64, 48);
    let mut frame = test_frame(Profile::Hq, 64, 48, 8, false, false, 6);
    frame.interlace = Interlace::TopFieldFirst;
    assert!(!enc.load(&cfg, &frame));
}

/// Flat alpha with full-scale noise in macroblocks 1 and 5 (the rest fits the frame easily).
fn alpha_noise_in_two_macroblocks(profile: Profile, w: u32, h: u32, depth: u8) -> Frame {
    let mut f = test_frame(profile, w, h, depth, false, true, 21);
    let mut s = 777u32;
    let nw = w.div_ceil(16) as usize;
    if let Some(a) = f.alpha.as_mut() {
        for (i, v) in a.iter_mut().enumerate() {
            let (x, y) = (i % w as usize, i / w as usize);
            let m = (y / 16) * nw + x / 16;
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *v = if m == 1 || m == 5 { (s % (1 << depth)) as u16 } else { 200 };
        }
    }
    f
}

#[test]
fn lossless_alpha_codes_noisy_macroblocks_as_raw_samples() {
    let Some(mut enc) = encoder("lossless_alpha_codes_noisy_macroblocks_as_raw_samples") else { return };
    for (profile, depth) in [(Profile::Hq, 8u8), (Profile::R444, 12)] {
        let (w, h) = (128, 64);
        let cfg = EncoderConfig { bit_depth: depth, alpha: Some(AlphaCoding::Lossless), ..EncoderConfig::new(profile, w, h) };
        let frame = alpha_noise_in_two_macroblocks(profile, w, h, depth);
        let name = format!("{profile:?} {depth}-bit");
        // the backend's own answer: the noisy macroblocks' alpha parts are the raw escape (a zero
        // 4-bit header, then 256 samples), the flat ones run-length coded (header 1)
        assert!(enc.load(&cfg, &frame), "{name}");
        let nw = (w / 16) as usize;
        let parts = enc.write(&vec![30; nw * (h / 16) as usize]).unwrap();
        let raw_bits = 4 + 256 * u32::from(depth);
        let mut raw = Vec::new();
        for (s, line) in parts.chunks(2 * nw).enumerate() {
            for (mb, (bytes, bits)) in line.iter().skip(nw).enumerate() {
                let header = bytes.first().map(|b| b >> 4);
                if header == Some(0) {
                    assert_eq!(*bits, raw_bits, "{name}: raw macroblock {mb} of line {s}");
                    raw.push(s * nw + mb);
                } else {
                    assert_eq!(header, Some(1), "{name}: macroblock {mb} of line {s}");
                }
            }
        }
        assert_eq!(raw, vec![1, 5], "{name}: the macroblocks coded raw");
        // and through the encoder: the CPU's frame, alpha decoded exactly
        let (cpu, gpu, used) = both(&mut enc, &cfg, &frame).unwrap();
        assert!(used, "{name}: the GPU did not encode the frame");
        assert!(gpu == cpu, "{name}: the GPU frame is not the CPU's");
        let dec = decode_frame(&gpu).unwrap();
        assert!(dec.alpha.as_ref() == frame.alpha.as_ref(), "{name}: alpha not lossless");
    }
}

#[test]
fn questions_out_of_order_get_no_answer() {
    let Some(mut enc) = encoder("questions_out_of_order_get_no_answer") else { return };
    // nothing loaded
    assert!(enc.bits(10).is_none() && enc.write(&[10]).is_none());
    let cfg = EncoderConfig::new(Profile::Sq, 64, 48);
    assert!(enc.load(&cfg, &test_frame(Profile::Sq, 64, 48, 8, false, false, 7)));
    // quantisers outside 1..=2047, and a write for the wrong number of macroblocks
    assert!(enc.bits(0).is_none() && enc.bits(2048).is_none());
    assert!(enc.write(&[10; 11]).is_none());
    assert!(enc.bits(10).is_some_and(|v| v.len() == 12));
}

#[test]
fn gpu_dnx_frames_decode_in_ffmpeg() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("gpu dnx encode") else { return };
    let Some(mut enc) = encoder("gpu_dnx_frames_decode_in_ffmpeg") else { return };
    let dir = filmcraft_testkit::fixtures_dir("gpu-dnx-enc");
    let mut failures = Vec::new();
    for (profile, depth, rgb, w, h) in [
        (Profile::Hq, 8, false, 1920, 1080),
        (Profile::Lb, 8, false, 1918, 1079),
        (Profile::Sq, 8, false, 18, 13),
        (Profile::Hqx, 10, false, 640, 360),
        (Profile::Hqx, 12, false, 334, 201),
        (Profile::R444, 10, false, 333, 201),
        (Profile::R444, 12, true, 640, 360),
        (Profile::R444, 10, true, 640, 360),
        (Profile::R444, 12, false, 334, 201),
    ] {
        let name = format!("{profile:?}_{depth}bit{}_{w}x{h}", if rgb { "_rgb" } else { "" });
        let cfg = EncoderConfig { bit_depth: depth, ..EncoderConfig::new(profile, w, h) };
        let frames: Vec<Frame> = (0..2).map(|i| test_frame(profile, w, h, depth, rgb, false, 9 + i)).collect();
        let (mut e, mut cpu) = (Encoder::with_config(cfg.clone()).unwrap(), Encoder::with_config(cfg).unwrap());
        let coded: Vec<Vec<u8>> = frames
            .iter()
            .map(|f| {
                let (c, used) = e.encode_with(f, Some(&mut enc)).unwrap();
                assert!(used, "{name}");
                // (so a difference to our decoder below is the CPU encoder's frame's too)
                assert!(c == cpu.encode(f).unwrap(), "{name}: the GPU frame is not the CPU's");
                c
            })
            .collect();
        let path = dir.join(format!("{name}.dnxhd"));
        std::fs::write(&path, coded.concat()).unwrap();
        let fmt = match (chroma(profile), depth, rgb) {
            (ChromaFormat::Yuv444, 10, true) => "gbrp10le",
            (ChromaFormat::Yuv444, _, true) => "gbrp12le",
            (ChromaFormat::Yuv444, 10, false) => "yuv444p10le",
            (ChromaFormat::Yuv444, _, false) => "yuv444p12le",
            (_, 8, _) => "yuv422p",
            (_, 10, _) => "yuv422p10le",
            _ => "yuv422p12le",
        };
        let out = Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-f", "dnxhd", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", fmt, "-"])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || !err.trim().is_empty() {
            failures.push(format!("{name}: ffmpeg: {err}"));
            continue;
        }
        // ffmpeg's decode agrees with ours to within two codes (its IDCT is not ours); at 12-bit
        // 4:4:4 it differs by up to 9 of 4095, the CPU encoder's frames alike (measured: they are
        // the same bytes)
        let tol = if profile == Profile::R444 && depth == 12 { 12 } else { 2 };
        let raw: Vec<u16> = if depth == 8 {
            out.stdout.iter().map(|&b| u16::from(b)).collect()
        } else {
            out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
        };
        let mut at = 0usize;
        for (i, c) in coded.iter().enumerate() {
            let ours = decode_frame(c).unwrap();
            for p in [&ours.y, &ours.cb, &ours.cr] {
                let Some(theirs) = raw.get(at..at + p.len()) else {
                    failures.push(format!("{name} #{i}: ffmpeg returned {} samples", raw.len()));
                    break;
                };
                let max = p.iter().zip(theirs).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                if max > tol {
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

#[test]
#[ignore = "benchmark"]
fn dnx_gpu_encode_bench() {
    let Some(mut enc) = encoder("dnx_gpu_encode_bench") else { return };
    let (w, h, profile) = (3840, 2160, Profile::Hq);
    let frames: Vec<Frame> = (0..30).map(|i| test_frame(profile, w, h, 8, false, false, 100 + i)).collect();
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let mut cpu_enc = Encoder::new(profile, w, h).unwrap();
    let cpu: Vec<f64> = frames
        .iter()
        .map(|f| {
            let t = std::time::Instant::now();
            cpu_enc.encode(f).unwrap();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    let mut gpu_enc = Encoder::new(profile, w, h).unwrap();
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
    eprintln!("DNxHR HQ {w}x{h}, {} frames: CPU median {:.1} ms, GPU median {:.1} ms", frames.len(), median(cpu), median(gpu));
}

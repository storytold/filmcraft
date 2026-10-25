//! Encoder conformance and quality against ffmpeg's ProRes decoder (oracle only).

use crate::common;
use common::*;
use filmcraft_prores::{ChromaFormat, DecodeOptions, Encoder, Frame, Profile, decode_frame, decode_frame_with};

struct Content {
    name: &'static str,
    lavfi: &'static str,
    /// Detailed enough that every profile's rate control is budget-bound.
    busy: bool,
}

const W: u32 = 1920;
const H: u32 = 1080;

fn contents(chroma: ChromaFormat) -> Vec<Content> {
    match chroma {
        ChromaFormat::Yuv422 => vec![
            Content { name: "src422_testsrc2", lavfi: "testsrc2=s=1920x1080:r=25", busy: false },
            Content { name: "src422_mandel", lavfi: "mandelbrot=s=1920x1080:r=25", busy: false },
            Content { name: "src422_noise", lavfi: "testsrc2=s=1920x1080:r=25,noise=alls=40:allf=t", busy: true },
        ],
        ChromaFormat::Yuv444 => vec![
            Content {
                name: "src444a_testsrc2_disc",
                lavfi: "testsrc2=s=1920x1080:r=25,format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='1023*lt(hypot(X-W/2,Y-H/2),400)'",
                busy: false,
            },
            Content { name: "src444a_noise", lavfi: "testsrc2=s=1920x1080:r=25,noise=alls=40:allf=t,format=yuva444p10le", busy: true },
        ],
    }
}

fn native_fmt(chroma: ChromaFormat, alpha: bool) -> &'static str {
    match (chroma, alpha) {
        (ChromaFormat::Yuv422, _) => "yuv422p10le",
        (ChromaFormat::Yuv444, false) => "yuv444p12le",
        (ChromaFormat::Yuv444, true) => "yuva444p12le",
    }
}

#[test]
fn encoder_output_decodes_in_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let mut report = vec![format!(
        "{:<9} {:<18} {:>9} {:>9} {:>6} {:>8} {:>8} {:>7} {:>7}",
        "profile", "content", "target", "avg", "ratio", "psnrY", "psnrC", "ffΔmax", "ffΔ=0%"
    )];
    let mut failures = Vec::new();
    for profile in Profile::ALL {
        let chroma = profile.chroma();
        for c in contents(chroma) {
            let alpha = chroma == ChromaFormat::Yuv444;
            let src = source_frames(&ff, c.name, c.lavfi, 2, W, H, chroma, alpha);
            let mut enc = Encoder::new(profile, W, H);
            let coded: Vec<Vec<u8>> = src.iter().map(|f| enc.encode(f).unwrap()).collect();
            let target = enc.target_frame_bytes() as f64;
            let avg = coded.iter().map(|c| c.len()).sum::<usize>() as f64 / coded.len() as f64;
            let mov = fixture_dir().join(format!("enc_{}_{}.mov", String::from_utf8_lossy(&profile.fourcc()), c.name));
            write_mov(&mov, &profile.fourcc(), W as u16, H as u16, if alpha { 32 } else { 24 }, &coded);
            let fmt = native_fmt(chroma, alpha);
            let (ffraw, err) = ffmpeg_decode(&ff, &mov, fmt);
            if !err.trim().is_empty() {
                failures.push(format!("{profile:?}/{}: ffmpeg reported: {err}", c.name));
            }
            let per = frame_samples(W as usize, H as usize, chroma, alpha);
            assert_eq!(ffraw.len(), per * coded.len(), "{profile:?}/{}", c.name);
            let (mut ffd, mut py, mut pc) = (Stats::default(), 0.0, 0.0);
            for (i, (cf, s)) in coded.iter().zip(&src).enumerate() {
                // Our decode of our stream agrees with ffmpeg's decode of it.
                let ours = decode_frame(cf).unwrap();
                let (l, cc) = compare(&ours, &ffraw[i * per..(i + 1) * per]);
                ffd.merge(&l);
                ffd.merge(&cc);
                // Quality against the 10-bit source.
                let d10 = decode_frame_with(cf, &DecodeOptions { bit_depth: Some(10), threads: true }).unwrap();
                py += psnr(&d10.y, &s.y, 1023.0) / coded.len() as f64;
                pc += psnr(&[d10.cb.clone(), d10.cr.clone()].concat(), &[s.cb.clone(), s.cr.clone()].concat(), 1023.0) / coded.len() as f64;
                if let (Some(a), Some(b)) = (&d10.alpha, &s.alpha) {
                    assert_eq!(a, b, "alpha is lossless");
                }
            }
            let ratio = avg / target;
            report.push(format!(
                "{:<9} {:<18} {:>9.0} {:>9.0} {:>6.3} {:>8.2} {:>8.2} {:>7} {:>7.2}",
                format!("{profile:?}"),
                c.name,
                target,
                avg,
                ratio,
                py,
                pc,
                ffd.max,
                ffd.exact_pct()
            ));
            if ffd.max > 1 {
                failures.push(format!("{profile:?}/{}: ffmpeg decode differs from ours by {}", c.name, ffd.max));
            }
            if ratio > 1.15 || (c.busy && ratio < 0.85) {
                failures.push(format!("{profile:?}/{}: frame size {avg:.0} vs target {target:.0}", c.name));
            }
            if profile == Profile::Hq && !c.busy && py.min(pc) < 45.0 {
                failures.push(format!("HQ/{}: PSNR {py:.2}/{pc:.2} dB below 45", c.name));
            }
        }
    }
    let text = report.join("\n");
    println!("{text}");
    std::fs::write(fixture_dir().join("encode_report.txt"), &text).unwrap();
    assert!(failures.is_empty(), "{}\n{text}", failures.join("\n"));
}

/// Odd sizes and small slices round-trip through ffmpeg too.
#[test]
fn odd_geometry_encodes() {
    let Some(ff) = ffmpeg() else { return };
    for (profile, w, h) in [(Profile::Standard, 1918u32, 1080u32), (Profile::Proxy, 70, 38), (Profile::P4444, 333, 201)] {
        let chroma = profile.chroma();
        let alpha = chroma == ChromaFormat::Yuv444;
        let lav = if alpha {
            format!("mandelbrot=s={w}x{h}:r=25,format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='mod(3*X+Y,1024)'")
        } else {
            format!("mandelbrot=s={w}x{h}:r=25")
        };
        let name = format!("src_odd_{w}x{h}");
        let src = source_frames(&ff, Box::leak(name.into_boxed_str()), &lav, 1, w, h, chroma, alpha);
        let mut enc = Encoder::new(profile, w, h);
        let coded = vec![enc.encode(&src[0]).unwrap()];
        let mov = fixture_dir().join(format!("enc_odd_{w}x{h}.mov"));
        write_mov(&mov, &profile.fourcc(), w as u16, h as u16, 24, &coded);
        let (ffraw, err) = ffmpeg_decode(&ff, &mov, native_fmt(chroma, alpha));
        assert!(err.trim().is_empty(), "{err}");
        let ours: Frame = decode_frame(&coded[0]).unwrap();
        let (l, c) = compare(&ours, &ffraw);
        assert!(l.max <= 1 && c.max <= 1, "{profile:?} {w}x{h}: {l:?} {c:?}");
    }
}

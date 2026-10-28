//! Decoder accuracy against ffmpeg's ProRes decoder (oracle only; skipped without ffmpeg).

use crate::common;
use common::*;
use filmcraft_prores::{AlphaType, ChromaFormat, DecodeOptions, Interlace, decode_frame, decode_frame_with, probe};

fn specs() -> Vec<Spec> {
    let p422 = "format=yuv422p10le";
    let p444 = "format=yuv444p10le";
    let alpha = "format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='mod(X*7+Y*3,1024)'";
    let ks = |p: &'static str| -> Vec<&'static str> { vec!["-c:v", "prores_ks", "-profile:v", p] };
    let aw = |p: &'static str| -> Vec<&'static str> { vec!["-c:v", "prores_aw", "-profile:v", p] };
    let mut v = Vec::new();
    for (i, p) in ["0", "1", "2", "3"].into_iter().enumerate() {
        let names = [
            ["ks_proxy_testsrc2_1080", "ks_lt_testsrc2_1080", "ks_std_testsrc2_1080", "ks_hq_testsrc2_1080"],
            ["ks_proxy_mandel_1918", "ks_lt_mandel_1918", "ks_std_mandel_1918", "ks_hq_mandel_1918"],
            ["ks_proxy_bars_720", "ks_lt_bars_720", "ks_std_bars_720", "ks_hq_bars_720"],
            ["aw_proxy_testsrc2_640", "aw_lt_testsrc2_640", "aw_std_testsrc2_640", "aw_hq_testsrc2_640"],
        ];
        v.push(Spec::new(names[0][i], format!("testsrc2=s=1920x1080:r=25,{p422}"), 2, &ks(p)));
        v.push(Spec::new(names[1][i], format!("mandelbrot=s=1918x1080:r=25,{p422}"), 2, &ks(p)));
        v.push(Spec::new(names[2][i], format!("smptehdbars=s=1280x720:r=25,{p422}"), 1, &ks(p)));
        v.push(Spec::new(names[3][i], format!("testsrc2=s=640x360:r=25,{p422}"), 2, &aw(p)));
    }
    v.push(Spec::new("ks_hq_odd_70x38", format!("testsrc2=s=70x38:r=25,{p422}"), 2, &ks("3")));
    v.push(Spec::new("ks_hq_small_slices", format!("mandelbrot=s=352x288:r=25,{p422}"), 1, &[&ks("3")[..], &["-mbs_per_slice", "2"]].concat()));
    let il = ["-flags", "+ildct+ilme"];
    v.push(Spec::new("ks_hq_ntsc_tff", format!("testsrc2=s=720x486:r=30000/1001,setfield=tff,{p422}"), 2, &[&ks("3")[..], &il].concat()));
    v.push(Spec::new("ks_std_ntsc_bff", format!("mandelbrot=s=720x486:r=30000/1001,setfield=bff,{p422}"), 2, &[&ks("2")[..], &il].concat()));
    v.push(Spec::new("aw_hq_1080i", format!("testsrc2=s=1920x1080:r=30000/1001,setfield=tff,{p422}"), 1, &[&aw("3")[..], &il].concat()));
    v.push(Spec::new("ks_4444_testsrc2_1080", format!("testsrc2=s=1920x1080:r=25,{p444}"), 2, &ks("4")));
    v.push(Spec::new("ks_4444xq_mandel_1918", format!("mandelbrot=s=1918x1080:r=25,{p444}"), 1, &ks("5")));
    v.push(Spec::new("ks_4444_alpha16_640", format!("testsrc2=s=640x360:r=25,{alpha}"), 2, &[&ks("4")[..], &["-alpha_bits", "16"]].concat()));
    v.push(Spec::new("ks_4444_alpha8_640", format!("testsrc2=s=640x360:r=25,{alpha}"), 1, &[&ks("4")[..], &["-alpha_bits", "8"]].concat()));
    v.push(Spec::new("ks_4444xq_alpha_odd", format!("mandelbrot=s=333x201:r=25,{alpha}"), 1, &ks("5")));
    v.push(Spec::new("aw_4444_640", format!("testsrc2=s=640x360:r=25,{p444}"), 1, &aw("4")));
    v.push(Spec::new("aw_4444_alpha_640", format!("testsrc2=s=640x360:r=25,{alpha}"), 1, &aw("4")));
    v.push(Spec::new("ks_4444_ntsc_tff", format!("testsrc2=s=720x486:r=30000/1001,setfield=tff,{p444}"), 1, &[&ks("4")[..], &il].concat()));
    v
}

fn native_pix_fmt(chroma: ChromaFormat, alpha: bool) -> &'static str {
    match (chroma, alpha) {
        (ChromaFormat::Yuv422, _) => "yuv422p10le",
        (ChromaFormat::Yuv444, false) => "yuv444p12le",
        (ChromaFormat::Yuv444, true) => "yuva444p12le",
    }
}

#[test]
fn decode_matches_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let filter = std::env::var("PRORES_FIXTURE").ok();
    let mut report = vec![format!("{:<26} {:>9} {:>5} {:>8} {:>8} {:>8} {:>8}", "fixture", "format", "maxY", "meanY", "maxC/A", "meanC/A", "exact%")];
    let mut worst = 0;
    for spec in specs() {
        if let Some(f) = &filter
            && !spec.name.contains(f.as_str())
        {
            continue;
        }
        let mov = make(&ff, &spec);
        let pkts = packets(&ff, &mov);
        assert_eq!(pkts.len(), spec.frames as usize, "{}", spec.name);
        let hdr = probe(&pkts[0]).unwrap();
        let has_alpha = hdr.alpha != AlphaType::None;
        let fmt = native_pix_fmt(hdr.chroma, has_alpha);
        let raw = reference(&ff, &mov, fmt);
        let per = frame_samples(hdr.width as usize, hdr.height as usize, hdr.chroma, has_alpha);
        assert_eq!(raw.len(), per * pkts.len());
        let (mut sl, mut sc) = (Stats::default(), Stats::default());
        for (i, p) in pkts.iter().enumerate() {
            let f = decode_frame(p).unwrap_or_else(|e| panic!("{}: frame {i}: {e}", spec.name));
            if spec.enc.contains(&"+ildct+ilme") {
                assert_ne!(f.interlace, Interlace::Progressive);
            }
            let (l, c) = compare(&f, &raw[i * per..(i + 1) * per]);
            sl.merge(&l);
            sc.merge(&c);
        }
        let mut all = sl;
        all.merge(&sc);
        worst = worst.max(all.max);
        report.push(format!(
            "{:<26} {:>9} {:>5} {:>8.5} {:>8} {:>8.5} {:>8.3}",
            spec.name,
            &fmt[..fmt.len() - 2],
            sl.max,
            sl.mean(),
            sc.max,
            sc.mean(),
            all.exact_pct()
        ));
    }
    let text = report.join("\n");
    println!("{text}");
    std::fs::write(fixture_dir().join("decode_report.txt"), &text).unwrap();
    assert!(worst <= 1, "decoder deviates from reference by up to {worst} LSB\n{text}");
}

/// 10-bit output of 4444 streams stays within ±1 of ffmpeg's 12-bit output rounded to 10 bits.
#[test]
fn decode_4444_at_10_bits() {
    let Some(ff) = ffmpeg() else { return };
    let spec = Spec::new("ks_4444_alpha16_640", "", 2, &[]);
    let spec = specs().into_iter().find(|s| s.name == spec.name).unwrap();
    let mov = make(&ff, &spec);
    let pkts = packets(&ff, &mov);
    let raw12 = reference(&ff, &mov, "yuva444p12le");
    let per = frame_samples(640, 360, ChromaFormat::Yuv444, true);
    let f = decode_frame_with(&pkts[0], &DecodeOptions { bit_depth: Some(10), threads: true }).unwrap();
    assert_eq!(f.bit_depth, 10);
    let r10: Vec<u16> = raw12[..per].iter().map(|&v| ((v as u32 + 2) >> 2).min(1023) as u16).collect();
    let (l, c) = compare(&f, &r10);
    assert!(l.max <= 1 && c.max <= 1, "{l:?} {c:?}");
}

#[test]
fn single_threaded_matches_threaded() {
    let Some(ff) = ffmpeg() else { return };
    let spec = specs().into_iter().find(|s| s.name == "ks_hq_ntsc_tff").unwrap();
    let mov = make(&ff, &spec);
    let pkts = packets(&ff, &mov);
    let a = decode_frame_with(&pkts[0], &DecodeOptions { bit_depth: None, threads: false }).unwrap();
    let b = decode_frame(&pkts[0]).unwrap();
    assert_eq!(a, b);
}

#[test]
fn color_metadata_is_reported() {
    let Some(ff) = ffmpeg() else { return };
    let spec = Spec::new(
        "ks_hq_bt2020_pq",
        "testsrc2=s=128x64:r=25,format=yuv422p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
        1,
        &["-c:v", "prores_ks", "-profile:v", "3"],
    );
    let mov = make(&ff, &spec);
    let f = decode_frame(&packets(&ff, &mov)[0]).unwrap();
    assert_eq!((f.color.primaries, f.color.transfer, f.color.matrix), (9, 16, 9));
}

/// Pre-generate every decode fixture (`cargo xtask fixtures`).
#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = ffmpeg() else { return };
    for spec in specs() {
        let out = [fixture_dir().join(format!("{}.mov", spec.name))];
        filmcraft_testkit::fixtures::generate_and_report(&format!("prores/{}", spec.name), &out, || Some(make(&ff, &spec)));
    }
}

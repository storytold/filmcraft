//! Decode ffmpeg-made DNxHD / DNxHR fixtures and compare with ffmpeg's own decode.

use crate::common;
use common::*;
use filmcraft_dnx::{DecodeOptions, decode_frame_with};

fn specs() -> Vec<Spec> {
    let t = |s: &str, r: u32| format!("testsrc2=s={s}:r={r}");
    let m = |s: &str, r: u32| format!("mandelbrot=s={s}:r={r}");
    let n = |s: &str, r: u32| format!("testsrc2=s={s}:r={r},noise=alls=60:allf=t,lutyuv=y=clip(val\\,24\\,231):u=clip(val\\,24\\,231):v=clip(val\\,24\\,231)");
    vec![
        // DNxHD (HD profile)
        Spec::new("hd1237_testsrc", t("1920x1080", 25), 2, &["-b:v", "120M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1235_mandel", m("1920x1080", 25), 2, &["-b:v", "185M", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        Spec::new("hd1238_noise", n("1920x1080", 25), 2, &["-b:v", "185M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1253_testsrc", t("1920x1080", 25), 2, &["-b:v", "36M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1250_noise", n("1280x720", 50), 2, &["-b:v", "90M", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        Spec::new("hd1251_testsrc", t("1280x720", 50), 2, &["-b:v", "90M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1252_mandel", m("1280x720", 50), 2, &["-b:v", "60M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1258_testsrc", t("960x720", 50), 2, &["-b:v", "60M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1259_testsrc", t("1440x1080", 25), 2, &["-b:v", "84M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1242_interlaced", t("1920x1080", 25), 2, &["-flags", "+ildct", "-b:v", "120M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1241_interlaced", n("1920x1080", 25), 2, &["-flags", "+ildct", "-b:v", "185M", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        Spec::new("hd1243_interlaced", m("1920x1080", 25), 2, &["-flags", "+ildct", "-b:v", "220M", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hd1244_interlaced", t("1440x1080", 25), 2, &["-flags", "+ildct", "-b:v", "120M", "-pix_fmt", "yuv422p"], "yuv422p"),
        // DNxHR (RI profile)
        Spec::new("hr_lb_testsrc", t("1920x1080", 25), 2, &["-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hr_sq_mandel", m("1920x1080", 25), 2, &["-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hr_hq_noise", n("1920x1080", 25), 2, &["-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hr_hqx_mandel", m("1920x1080", 25), 2, &["-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        Spec::new("hr_hqx_noise_odd", n("1000x562", 25), 2, &["-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        Spec::new("hr_444_testsrc", t("1280x720", 25), 2, &["-profile:v", "dnxhr_444", "-pix_fmt", "yuv444p10le"], "yuv444p10le"),
        Spec::new("hr_444_rgb_mandel", m("640x360", 25), 2, &["-profile:v", "dnxhr_444", "-pix_fmt", "gbrp10le"], "gbrp10le"),
        Spec::new("hr_hq_small", t("256x120", 25), 2, &["-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"], "yuv422p"),
        Spec::new("hr_sq_uhd", t("3840x2160", 25), 1, &["-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"], "yuv422p"),
    ]
}

/// Generate every fixture (used by `cargo xtask fixtures dnx`).
#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = ffmpeg() else { return };
    for s in specs() {
        make(&ff, &s);
    }
}

#[test]
fn decode_matches_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let mut worst = 0;
    for spec in specs() {
        let mov = make(&ff, &spec);
        let pk = packets(&ff, &mov);
        assert_eq!(pk.len(), spec.frames as usize, "{}", spec.name);
        let raw = reference(&ff, &mov, spec.pix_fmt);
        let mut st = Stats::default();
        let mut off = 0;
        for (i, p) in pk.iter().enumerate() {
            let f =
                decode_frame_with(p, &DecodeOptions { threads: i % 2 == 0, ..Default::default() }).unwrap_or_else(|e| panic!("{} frame {i}: {e}", spec.name));
            let ours = planes(&f, spec.pix_fmt);
            let n = frame_samples(&f);
            st.add(&ours, &raw[off..off + n]);
            off += n;
        }
        assert_eq!(off, raw.len(), "{}: frame size mismatch", spec.name);
        println!(
            "{:<22} cid {} max {} mean {:.5} exact {:.3}% beyond ±1: {}",
            spec.name,
            filmcraft_dnx::probe(&pk[0]).unwrap().cid,
            st.max,
            st.mean(),
            st.exact_pct(),
            st.over1
        );
        worst = worst.max(st.max);
        // the remaining differences are ffmpeg's integer IDCT: ±1, at most ~50 samples per million at ±2
        assert!(st.over1 * 10_000 <= st.n, "{}: {} samples beyond ±1", spec.name, st.over1);
    }
    assert!(worst <= 2, "max error {worst}");
}

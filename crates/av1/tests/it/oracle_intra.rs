//! Intra-frame bit-exactness against libdav1d (run as an external ffmpeg process).

use crate::common;
use common::*;

const NO_FILTERS: &str = "enable-cdef=0:enable-restoration=0:enable-dlf=0:keyint=1";

fn specs_no_filters() -> Vec<Spec> {
    let p = |preset: &'static str, crf: &'static str| -> Vec<&'static str> { vec!["-preset", preset, "-crf", crf, "-svtav1-params", NO_FILTERS] };
    vec![
        Spec::new("intra_nofilt_testsrc_128", "testsrc2=s=128x128:r=25", 1, "yuv420p", &p("8", "30")),
        Spec::new("intra_nofilt_mandel_352x288_p4", "mandelbrot=s=352x288:r=25", 2, "yuv420p", &p("4", "20")),
        Spec::new("intra_nofilt_testsrc_odd_p2", "testsrc2=s=203x117:r=25", 2, "yuv420p", &p("2", "35")),
        Spec::new("intra_nofilt_noise_p6", "testsrc2=s=320x240:r=25,noise=alls=30:allf=t", 2, "yuv420p", &p("6", "10")),
        Spec::new("intra_nofilt_10bit_p4", "mandelbrot=s=256x192:r=25", 2, "yuv420p10le", &p("4", "25")),
        Spec::new("intra_nofilt_720p_p8", "testsrc2=s=1280x720:r=25", 1, "yuv420p", &p("8", "40")),
    ]
}

#[test]
fn intra_no_filters() {
    let Some(ff) = ffmpeg() else { return };
    for s in specs_no_filters() {
        check_bit_exact(&ff, &s);
    }
}

fn specs_filters() -> Vec<Spec> {
    let p =
        |preset: &'static str, crf: &'static str, params: &'static str| -> Vec<&'static str> { vec!["-preset", preset, "-crf", crf, "-svtav1-params", params] };
    vec![
        Spec::new("intra_dlf_mandel", "mandelbrot=s=352x288:r=25", 2, "yuv420p", &p("6", "40", "enable-cdef=0:enable-restoration=0:keyint=1")),
        Spec::new("intra_cdef_testsrc", "testsrc2=s=352x288:r=25", 2, "yuv420p", &p("6", "40", "enable-restoration=0:keyint=1")),
        Spec::new("intra_lr_mandel", "mandelbrot=s=352x288:r=25", 2, "yuv420p", &p("4", "40", "enable-cdef=0:keyint=1")),
        Spec::new("intra_all_testsrc", "testsrc2=s=640x360:r=25", 2, "yuv420p", &p("4", "45", "keyint=1")),
        Spec::new("intra_all_10bit", "mandelbrot=s=320x240:r=25", 2, "yuv420p10le", &p("4", "40", "keyint=1")),
        Spec::new("intra_qmatrix_noise", "testsrc2=s=352x288:r=25,noise=alls=10:allf=t", 2, "yuv420p", &p("6", "30", "enable-qm=1:qm-min=0:qm-max=8:keyint=1")),
        Spec::new("intra_all_noise_odd", "testsrc2=s=250x150:r=25,noise=alls=20:allf=t", 2, "yuv420p", &p("3", "35", "keyint=1")),
        // 128x128 superblocks, intra edge filter off, Wiener LR on all planes
        Spec::new("intra_all_grad_noise_10bit", "gradients=s=640x360:r=25,noise=alls=25:allf=t", 1, "yuv420p10le", &p("1", "50", "keyint=1")),
    ]
}

#[test]
fn intra_with_filters() {
    let Some(ff) = ffmpeg() else { return };
    for s in specs_filters() {
        check_bit_exact(&ff, &s);
    }
}

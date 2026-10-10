//! libaom conformance test vectors (downloaded on first use), compared with libdav1d.

mod common;
use common::*;

/// Vectors the decoder handles today (more are added as stages land).
const VECTORS: &[&str] =
    &["av1-1-b8-02-allintra.ivf", "av1-1-b8-05-mv.ivf", "av1-1-b8-06-mfmv.ivf", "av1-1-b8-24-monochrome.ivf", "av1-1-b10-23-film_grain-50.ivf"];

#[test]
fn conformance_vectors() {
    let Some(ff) = ffmpeg_dav1d() else { return };
    for name in VECTORS {
        let Some(path) = test_vector(name) else { return };
        check_file(&ff, name, &path);
    }
}

/// The wider libaom set (slow: many downloads and decodes):
/// `cargo test -p filmcraft-av1 --test conformance -- --ignored`; `AV1_VECTORS=a,b` overrides it.
#[test]
#[ignore]
fn conformance_vectors_extended() {
    let Some(ff) = ffmpeg_dav1d() else { return };
    let list: Vec<String> = match std::env::var("AV1_VECTORS") {
        Ok(v) => v.split(',').map(str::to_string).collect(),
        Err(_) => EXTENDED.iter().map(|s| s.to_string()).collect(),
    };
    let mut failed = Vec::new();
    for name in &list {
        let Some(path) = test_vector(name) else { continue };
        let r = std::panic::catch_unwind(|| check_file(&ff, name, &path));
        if r.is_err() {
            failed.push(name.clone());
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}

const EXTENDED: &[&str] = &[
    "av1-1-b8-01-size-16x16.ivf",
    "av1-1-b8-01-size-66x66.ivf",
    "av1-1-b8-01-size-196x196.ivf",
    "av1-1-b8-01-size-226x226.ivf",
    "av1-1-b8-00-quantizer-00.ivf",
    "av1-1-b8-00-quantizer-31.ivf",
    "av1-1-b8-00-quantizer-63.ivf",
    "av1-1-b10-00-quantizer-00.ivf",
    "av1-1-b10-00-quantizer-40.ivf",
    "av1-1-b8-04-cdfupdate.ivf",
    "av1-1-b8-05-mv.ivf",
    "av1-1-b8-06-mfmv.ivf",
    "av1-1-b8-22-svc-L1T2.ivf",
    "av1-1-b8-22-svc-L2T1.ivf",
    "av1-1-b8-22-svc-L2T2.ivf",
    "av1-1-b8-24-monochrome.ivf",
    "av1-1-b10-24-monochrome.ivf",
    "av1-1-b8-16-intra_only-intrabc-extreme-dv.ivf",
    "av1-1-b8-16-intra-only.ivf",
    "av1-1-b8-23-film_grain-50.ivf",
    "av1-1-b10-23-film_grain-50.ivf",
];

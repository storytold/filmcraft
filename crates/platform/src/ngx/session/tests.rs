//! Hardware-free checks for the NGX session helpers: readback unpacking, driver DLL choice and
//! runtime initialisation retries.

use std::fs::File;

use filmcraft_render::Image;

use super::{dlss_search_dir, get_or_try_init, readback_len, unpack_rgba16f};

/// One RGBA16F pixel: `r`, `g`, `b`, `a` as little-endian half floats.
fn px16(r: u16, g: u16, b: u16, a: u16) -> [u8; 8] {
    let [r0, r1] = r.to_le_bytes();
    let [g0, g1] = g.to_le_bytes();
    let [b0, b1] = b.to_le_bytes();
    let [a0, a1] = a.to_le_bytes();
    [r0, r1, g0, g1, b0, b1, a0, a1]
}

#[test]
fn readback_without_data_is_an_error_not_a_black_frame() {
    let curr = Image::new(2, 2);
    let err = unpack_rgba16f(None, 16, 2, 2, &curr).unwrap_err();
    assert!(err.contains("no data"), "{err}");
}

#[test]
fn readback_with_a_short_row_pitch_is_an_error() {
    let curr = Image::new(4, 2);
    let raw = vec![0u8; 64];
    // a 4-pixel RGBA16F row is 32 bytes; a 24-byte pitch cannot hold it
    let err = unpack_rgba16f(Some(&raw), 24, 4, 2, &curr).unwrap_err();
    assert!(err.contains("row pitch"), "{err}");
    assert!(readback_len(24, 4, 2).is_err());
}

#[test]
fn readback_shorter_than_the_frame_is_an_error() {
    let curr = Image::new(2, 2);
    let raw = vec![0u8; 30];
    let err = unpack_rgba16f(Some(&raw), 16, 2, 2, &curr).unwrap_err();
    assert!(err.contains("needs 32"), "{err}");
}

#[test]
fn readback_with_padded_rows_unpacks_every_pixel_and_keeps_source_alpha() {
    let (w, h, pitch) = (2usize, 2usize, 24usize);
    let mut curr = Image::new(w, h);
    for (i, a) in [0.1f32, 0.2, 0.3, 0.4].iter().enumerate() {
        curr.px[i * 4 + 3] = *a;
    }
    // 0x3800 = 0.5, 0x3C00 = 1.0, 0x3400 = 0.25; the last 8 bytes of each row are padding
    let mut raw = vec![0xEEu8; pitch * (h - 1) + w * 8];
    raw[0..8].copy_from_slice(&px16(0x3800, 0x3C00, 0x3400, 0));
    raw[8..16].copy_from_slice(&px16(0x3C00, 0x3800, 0x3400, 0));
    raw[24..32].copy_from_slice(&px16(0x3400, 0x3400, 0x3400, 0));
    raw[32..40].copy_from_slice(&px16(0x3C00, 0x3C00, 0x3C00, 0));
    assert_eq!(readback_len(pitch, w, h).unwrap(), raw.len());

    let out = unpack_rgba16f(Some(&raw), pitch, w, h, &curr).unwrap();
    assert_eq!(out.px[0..4], [0.5, 1.0, 0.25, 0.1]);
    assert_eq!(out.px[4..8], [1.0, 0.5, 0.25, 0.2]);
    assert_eq!(out.px[8..12], [0.25, 0.25, 0.25, 0.3]);
    assert_eq!(out.px[12..16], [1.0, 1.0, 1.0, 0.4]);
}

#[test]
fn only_an_installed_feature_is_accepted_as_an_explicit_search_directory() {
    let sdk = tempfile::tempdir().unwrap();
    File::create(sdk.path().join("ota.bin")).unwrap();
    assert!(dlss_search_dir(sdk.path().into()).is_err());
    File::create(sdk.path().join("nvngx_dlss.dll")).unwrap();
    assert_eq!(dlss_search_dir(sdk.path().into()).unwrap(), sdk.path());
}

#[test]
fn a_failed_runtime_initialisation_is_retried_and_a_success_is_kept() {
    let mut slot: Option<u32> = None;
    assert_eq!(get_or_try_init(&mut slot, || Err("driver updating".into())), Err("driver updating".to_string()));
    assert!(slot.is_none(), "the failure is not cached");

    assert_eq!(get_or_try_init(&mut slot, || Ok(7)), Ok(&mut 7));
    let mut called = false;
    assert_eq!(
        get_or_try_init(&mut slot, || {
            called = true;
            Err("must not run".into())
        }),
        Ok(&mut 7)
    );
    assert!(!called, "an initialised runtime is reused");
}

#[test]
fn half_subnormal_midpoints_round_to_even() {
    let unit = 2.0f32.powi(-24);
    for (x, expected) in [(0.5, 0), (1.5, 2), (2.5, 2), (2.5001, 3), (1023.5, 1024)] {
        assert_eq!(super::f32_to_f16(unit * x), expected);
        assert_eq!(super::f32_to_f16(-unit * x), 0x8000 | expected);
    }
}

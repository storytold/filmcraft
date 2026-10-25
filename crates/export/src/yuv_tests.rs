//! The 8-bit 4:2:0 converters that feed the H.264 encoders: BT.709 limited-range codes, chroma as
//! the mean of each 2x2 block, odd edges, and no panic on a short picture.
use super::*;

/// The conversion written out per chroma sample over whole rows: the definition the codes follow.
fn reference(px: &dyn Fn(usize) -> (f32, f32, f32), w: usize, h: usize, kr: f32, kb: f32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (kg, sb, sr) = (1.0 - kr - kb, 2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (mut y, mut u, mut v) = (vec![0u8; w * h], vec![0u8; cw * ch], vec![0u8; cw * ch]);
    for cy in 0..ch {
        let (mut us, mut vs, mut cnt) = (vec![0f32; cw], vec![0f32; cw], vec![0f32; cw]);
        for row in cy * 2..(cy * 2 + 2).min(h) {
            for x in 0..w {
                let (r, g, b) = px(row * w + x);
                let yy = kr * r + kg * g + kb * b;
                y[row * w + x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                us[x / 2] += (b - yy) / sb;
                vs[x / 2] += (r - yy) / sr;
                cnt[x / 2] += 1.0;
            }
        }
        for cx in 0..cw {
            u[cy * cw + cx] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            v[cy * cw + cx] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
        }
    }
    (y, u, v)
}

/// [`rgba_to_yuv420_8`]'s three planes for an RGBA8 picture.
fn rgba(px: &[u8], w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    rgba_to_yuv420_8(px, w, h, &mut y, &mut u, &mut v);
    (y, u, v)
}

/// `n` pseudo-random numbers from `seed` (xorshift), the same on every run.
fn noise(n: usize, seed: u64) -> impl Iterator<Item = u64> {
    let mut s = seed | 1;
    std::iter::repeat_with(move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    })
    .take(n)
}

/// Black, white and the three primaries give the BT.709 limited-range codes.
#[test]
fn primaries_have_the_bt709_limited_range_codes() {
    for (px, want) in [
        ([0u8, 0, 0], (16u8, 128u8, 128u8)),
        ([255, 255, 255], (235, 128, 128)),
        ([255, 0, 0], (63, 102, 240)),
        ([0, 255, 0], (173, 42, 26)),
        ([0, 0, 255], (32, 240, 118)),
    ] {
        let pic: Vec<u8> = [px[0], px[1], px[2], 255].repeat(4);
        let (y, u, v) = rgba(&pic, 2, 2);
        assert_eq!((y, u, v), (vec![want.0; 4], vec![want.1], vec![want.2]), "{px:?}");
    }
}

/// RGBA8 pictures, noise and pure black/white, give the reference codes on odd sizes, around the
/// 64-pixel blocks and in full HD.
#[test]
fn rgba_codes_follow_the_formula_on_every_size() {
    // odd sizes, sizes around the converter's 64-pixel blocks, and a full HD picture
    for (w, h) in [(1, 1), (1, 2), (2, 1), (3, 3), (63, 5), (64, 4), (65, 7), (127, 3), (129, 9), (641, 361), (1920, 1080)] {
        for seed in [1, 2] {
            let pic: Vec<u8> = noise(w * h * 4, seed + w as u64).map(|r| if seed == 2 { [0, 255][(r & 1) as usize] } else { r as u8 }).collect();
            let px = |i: usize| (f32::from(pic[i * 4]) / 255.0, f32::from(pic[i * 4 + 1]) / 255.0, f32::from(pic[i * 4 + 2]) / 255.0);
            assert!(rgba(&pic, w, h) == reference(&px, w, h, 0.2126, 0.0722), "{w}x{h} seed {seed}");
        }
    }
}

/// Float pictures with NaN, infinities, negative, out-of-range and subnormal values give the
/// reference codes (BT.2020 matrix).
#[test]
fn float_codes_follow_the_formula_with_hostile_values() {
    let specials = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0, -1.0, 2.0, 0.5, 1e-40, 0.499_999_97];
    let (kr, kb) = (0.2627, 0.0593);
    for (w, h) in [(1, 1), (3, 3), (65, 7), (130, 2)] {
        let pic: Vec<f32> =
            noise(w * h * 3, w as u64).map(|r| if r % 5 == 0 { specials[(r >> 8) as usize % specials.len()] } else { (r % 1000) as f32 / 999.0 }).collect();
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        rgbf_to_yuv420_8(&pic, w, h, kr, kb, &mut y, &mut u, &mut v);
        let px = |i: usize| (pic[i * 3], pic[i * 3 + 1], pic[i * 3 + 2]);
        assert!((y, u, v) == reference(&px, w, h, kr, kb), "{w}x{h}");
    }
}

/// A picture shorter than its size converts its rows and leaves the rest black, and an empty one
/// gives empty planes.
#[test]
fn a_short_picture_converts_without_panicking() {
    // two of four rows: the rows it has keep their colour, the missing ones come out black
    let pic: Vec<u8> = [255u8, 255, 255, 255].repeat(4 * 2);
    let (y, u, v) = rgba(&pic, 4, 4);
    assert_eq!(y, [vec![235; 8], vec![16; 8]].concat());
    assert_eq!((u, v), (vec![128; 4], vec![128; 4]));
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    rgbf_to_yuv420_8(&[], 3, 3, 0.2126, 0.0722, &mut y, &mut u, &mut v);
    assert_eq!((y.len(), u.len(), v.len()), (9, 4, 4));
    // nothing to convert
    assert_eq!(rgba(&[], 0, 0), (vec![], vec![], vec![]));
}

/// The H.264 encoder returns an error naming the picture it got for a short RGBA or HDR picture, and
/// for a whole picture of another size; one of its own size encodes.
#[test]
fn the_h264_encoder_refuses_a_picture_of_the_wrong_size() {
    let settings = ExportSettings::default();
    let Some(Ok(mut enc)) = h264_factory(Format::H264, 64, 32, FrameRate::FPS_24, &settings) else { panic!("built-in H.264 encoder") };
    let short = vec![0u8; 64 * 31 * 4];
    let Err(e) = enc.encode(&EncoderFrame { width: 64, height: 32, rgba: &short, hdr: None, index: 0 }) else { panic!("a short picture was encoded") };
    assert!(e.to_string().contains("7936"), "the error names what it got: {e}");
    let hdr = vec![0f32; 7];
    let Err(e) = enc.encode(&EncoderFrame { width: 64, height: 32, rgba: &[], hdr: Some(&hdr), index: 0 }) else { panic!("a short HDR picture was encoded") };
    assert!(e.to_string().contains("7 values"), "the error names what it got: {e}");
    // a whole picture of another size
    let other = vec![0u8; 32 * 32 * 4];
    let Err(e) = enc.encode(&EncoderFrame { width: 32, height: 32, rgba: &other, hdr: None, index: 0 }) else { panic!("a 32x32 picture was encoded") };
    assert!(e.to_string().contains("32x32"), "the error names what it got: {e}");
    let full = vec![128u8; 64 * 32 * 4];
    assert!(enc.encode(&EncoderFrame { width: 64, height: 32, rgba: &full, hdr: None, index: 0 }).is_ok());
}

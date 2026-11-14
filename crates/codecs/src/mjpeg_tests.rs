use crate::video::{MjpegDecoder, VideoDecoder};
use filmcraft_frame::PixelData;

fn shared_rgba(bytes: &[u8]) -> Vec<u8> {
    let mut dec = MjpegDecoder;
    let frames = dec.decode(bytes, 0).unwrap();
    let [frame] = frames.as_slice() else { panic!("expected one MJPEG frame") };
    match &frame.frame.data {
        PixelData::Rgba8(data) => data.as_ref().clone(),
        other => panic!("unexpected FilmCraft pixel data: {other:?}"),
    }
}

fn without_dht(jpeg: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    let mut at = 2;
    while at + 1 < jpeg.len() {
        assert_eq!(jpeg[at], 0xFF, "expected marker at {at}");
        let marker = jpeg[at + 1];
        if marker == 0xDA {
            out.extend_from_slice(&jpeg[at..]);
            break;
        }
        let len = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        if marker != 0xC4 {
            out.extend_from_slice(&jpeg[at..at + 2 + len]);
        }
        at += 2 + len;
    }
    out
}

#[test]
fn shared_mjpeg_matches_image_oracle_for_progressive_and_avi1_missing_dht() {
    let progressive = include_bytes!("../tests/fixtures/mjpeg-progressive-huffman.jpg");
    let oracle = image::load_from_memory_with_format(progressive, image::ImageFormat::Jpeg).unwrap().to_rgba8().into_raw();
    let decoded = shared_rgba(progressive);
    assert_eq!(decoded.len(), oracle.len());
    let mad = decoded.iter().zip(&oracle).map(|(&a, &b)| f32::from(a.abs_diff(b))).sum::<f32>() / decoded.len() as f32;
    assert!(mad < 2.0, "progressive JPEG mean absolute difference {mad}");

    let baseline = include_bytes!("../tests/fixtures/mjpeg-avi1-baseline.jpg");
    let baseline_expected = shared_rgba(baseline);
    let baseline_oracle = image::load_from_memory_with_format(baseline, image::ImageFormat::Jpeg).unwrap().to_rgba8().into_raw();
    let baseline_mad = baseline_expected.iter().zip(&baseline_oracle).map(|(&a, &b)| f32::from(a.abs_diff(b))).sum::<f32>() / baseline_expected.len() as f32;
    eprintln!("baseline JPEG image-decoder mean absolute difference {baseline_mad:.4} levels/channel");
    assert!(baseline_mad < 2.0, "baseline JPEG mean absolute difference {baseline_mad}");
    let avi1 = without_dht(baseline);
    assert_eq!(shared_rgba(&avi1), baseline_expected, "AVI1 Annex K fallback should restore baseline pixels");

    let cmyk = include_bytes!("../tests/fixtures/mjpeg-cmyk.jpg");
    let oracle = image::load_from_memory_with_format(cmyk, image::ImageFormat::Jpeg).unwrap().to_rgba8().into_raw();
    let decoded = shared_rgba(cmyk);
    assert_eq!(decoded.len(), oracle.len());
    let mad = decoded.iter().zip(&oracle).map(|(&a, &b)| f32::from(a.abs_diff(b))).sum::<f32>() / decoded.len() as f32;
    assert!(mad < 2.0, "CMYK JPEG mean absolute difference {mad}");
}

use super::*;
use crate::{FrameRequest, MediaError, MediaKind, open_bytes};

const W: u32 = 4;
const H: u32 = 2;

/// A 4×2 RGBA picture: the left half `rgb` with alpha `left_alpha`, the right half `rgb` opaque.
fn picture(rgb: [u8; 3], left_alpha: u8) -> Vec<u8> {
    let mut px = Vec::new();
    for _y in 0..H {
        for x in 0..W {
            let a = if x < W / 2 { left_alpha } else { 255 };
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
    }
    px
}

/// An APNG whose frames are `frames` (RGBA, W×H) with delays `(numerator, denominator)` seconds.
fn apng(frames: &[(Vec<u8>, (u16, u16))]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, W, H);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.set_animated(frames.len() as u32, 0).unwrap();
        let mut w = e.write_header().unwrap();
        for (px, (num, den)) in frames {
            w.set_frame_delay(*num, *den).unwrap();
            w.set_dispose_op(png::DisposeOp::None).unwrap();
            w.set_blend_op(png::BlendOp::Source).unwrap();
            w.write_image_data(px).unwrap();
        }
        w.finish().unwrap();
    }
    out
}

/// A GIF whose frames are `frames` (RGBA, W×H) with delays in milliseconds.
fn gif(frames: &[(Vec<u8>, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = image::codecs::gif::GifEncoder::new(&mut out);
        e.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
        let frames = frames.iter().map(|(px, ms)| {
            let img = image::RgbaImage::from_raw(W, H, px.clone()).unwrap();
            image::Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(*ms, 1))
        });
        e.encode_frames(frames).unwrap();
    }
    out
}

/// A small animated WebP built from lossless WebP frame bitstreams.
fn webp(frames: &[(Vec<u8>, u32)]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(kind);
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        out.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            out.push(0);
        }
    }

    fn u24(out: &mut [u8], value: u32) {
        out[0] = value as u8;
        out[1] = (value >> 8) as u8;
        out[2] = (value >> 16) as u8;
    }

    fn frame_chunk(pixels: &[u8], duration_ms: u32) -> Vec<u8> {
        let mut encoded = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded).encode(pixels, W, H, image::ExtendedColorType::Rgba8).unwrap();

        let mut pos = 12;
        let bitstream = loop {
            let header = encoded.get(pos..pos + 8).unwrap();
            let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            let end = pos + 8 + size + size % 2;
            if &header[..4] == b"VP8L" {
                break encoded.get(pos..end).unwrap().to_vec();
            }
            pos = end;
        };

        let mut data = vec![0; 16];
        u24(&mut data[6..9], W - 1);
        u24(&mut data[9..12], H - 1);
        u24(&mut data[12..15], duration_ms);
        data[15] = 0x02; // replace the previous canvas instead of blending
        data.extend_from_slice(&bitstream);
        data
    }

    let mut chunks = Vec::new();
    let mut extended = [0; 10];
    extended[0] = 0x12; // alpha and animation flags
    u24(&mut extended[4..7], W - 1);
    u24(&mut extended[7..10], H - 1);
    chunk(&mut chunks, b"VP8X", &extended);
    chunk(&mut chunks, b"ANIM", &[0, 0, 0, 0, 0, 0]);
    for (pixels, duration_ms) in frames {
        chunk(&mut chunks, b"ANMF", &frame_chunk(pixels, *duration_ms));
    }

    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(chunks.len() + 4).unwrap().to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunks);
    out
}

/// Convert a whole number of milliseconds to media ticks.
fn ms(n: i64) -> Tick {
    Tick(n * TICKS_PER_SECOND / 1000)
}

/// Read the decoded RGBA pixels shown at a media time.
fn pixels(s: &dyn MediaSource, t: Tick) -> Vec<u8> {
    match &s.video_frame(FrameRequest::full(t)).unwrap().data {
        filmcraft_frame::PixelData::Rgba8(p) => p.to_vec(),
        _ => panic!("not RGBA"),
    }
}

#[test]
fn apng_plays_every_frame_with_its_timing_and_keeps_transparency() {
    let (red, green, blue) = (picture([255, 0, 0], 0), picture([0, 255, 0], 128), picture([0, 0, 255], 255));
    let bytes = apng(&[(red.clone(), (1, 10)), (green.clone(), (1, 5)), (blue.clone(), (1, 10))]);
    let s = open_bytes("spin.png", bytes.into(), &[]).unwrap();
    let info = s.info();
    assert_eq!(info.kind, MediaKind::Movie, "an APNG is a clip, not a still (#402)");
    assert_eq!(info.duration, ms(400));
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.codec.as_str()), (W, H, "APNG"));
    assert!(v.has_alpha, "transparent pixels are reported");
    assert_eq!(pixels(&*s, Tick::ZERO), red, "fully transparent pixels keep alpha 0, not black");
    assert_eq!(pixels(&*s, ms(99)), red);
    assert_eq!(pixels(&*s, ms(100)), green);
    assert_eq!(pixels(&*s, ms(299)), green, "the second frame lasts its 200 ms");
    assert_eq!(pixels(&*s, ms(300)), blue);
    assert_eq!(pixels(&*s, ms(10_000)), blue, "past the end: the last frame");
    assert_eq!(pixels(&*s, Tick(-5)), red, "before the start: the first frame");
}

#[test]
fn identical_frames_merge_into_one_longer_frame() {
    let (a, b) = (picture([10, 20, 30], 255), picture([40, 50, 60], 255));
    let bytes = apng(&[(a.clone(), (1, 10)), (a.clone(), (1, 10)), (a, (1, 10)), (b.clone(), (1, 10))]);
    let s = open("hold.png", &bytes).unwrap().unwrap();
    assert_eq!(s.frame_count(), 2);
    assert_eq!(s.frame_start(1), Some(ms(300)));
    assert_eq!(s.info().duration, ms(400));
    assert!(!s.info().video.as_ref().unwrap().has_alpha);
    assert_eq!(s.info().frame_rate(), FrameRate::new(10, 1), "the most common frame length is 100 ms");
    assert_eq!(pixels(&s, ms(350)), b);
}

#[test]
fn animated_gif_plays_every_frame_with_transparency() {
    let (red, green) = (picture([255, 0, 0], 0), picture([0, 255, 0], 255));
    let bytes = gif(&[(red, 40), (green, 0)]);
    let s = open_bytes("wave.gif", bytes.into(), &[]).unwrap();
    let info = s.info();
    assert_eq!(info.kind, MediaKind::Movie);
    assert_eq!(info.video.as_ref().unwrap().codec, "GIF");
    assert_eq!(info.duration, ms(140), "40 ms, then a zero delay played as 100 ms as browsers do");
    assert!(info.video.as_ref().unwrap().has_alpha);
    let first = pixels(&*s, Tick::ZERO);
    assert_eq!(first[3], 0, "the GIF's transparent pixels stay transparent");
    assert_eq!(&first[12..16], &[255, 0, 0, 255]);
    assert_eq!(&pixels(&*s, ms(40))[0..4], &[0, 255, 0, 255]);
}

#[test]
fn animated_webp_plays_every_frame_with_timing_and_transparency() {
    let (red, green) = (picture([255, 0, 0], 128), picture([0, 255, 0], 0));
    let bytes = webp(&[(red.clone(), 40), (green.clone(), 60)]);
    let s = open_bytes("wave.webp", bytes.into(), &[]).unwrap();
    let info = s.info();
    assert_eq!(info.kind, MediaKind::Movie);
    assert_eq!(info.video.as_ref().unwrap().codec, "WebP");
    assert_eq!(info.duration, ms(100));
    assert!(info.video.as_ref().unwrap().has_alpha);
    assert_eq!(pixels(&*s, Tick::ZERO), red);
    assert_eq!(pixels(&*s, ms(39)), red);
    assert_eq!(pixels(&*s, ms(40)), green);
}

#[test]
fn identical_webp_frames_remain_a_clip_when_merged() {
    let frame = picture([20, 40, 60], 255);
    let bytes = webp(&[(frame.clone(), 40), (frame, 60)]);
    let decoded = open("hold.webp", &bytes).expect("animated WebP").expect("decodes");
    assert_eq!(decoded.frame_count(), 1, "equal pictures still merge");
    assert_eq!(decoded.info().kind, MediaKind::Movie);
    assert_eq!(decoded.info().duration, ms(100), "merged frames keep their encoded duration");

    let imported = open_bytes("hold.webp", bytes.into(), &[]).unwrap();
    assert_eq!(imported.info().kind, MediaKind::Movie, "the still-image fallback must not discard the duration");
    assert_eq!(imported.info().duration, ms(100));
}

#[test]
fn animated_webp_rejects_an_oversized_canvas_before_decoding_frames() {
    let frame = picture([1, 2, 3], 255);
    let mut bytes = webp(&[(frame.clone(), 40), (frame, 60)]);
    // VP8X canvas width and height are stored as 24-bit values minus one.
    for (offset, dimension) in [(24, 25_000u32), (27, 25_000u32)] {
        let value = (dimension - 1).to_le_bytes();
        bytes[offset..offset + 3].copy_from_slice(&value[..3]);
    }
    assert!(matches!(
        open("large.webp", &bytes),
        Some(Err(MediaError::Unsupported(message))) if message.contains("frame limit")
    ));
}

#[test]
fn single_frame_files_stay_stills() {
    let one_frame_apng = apng(&[(picture([1, 2, 3], 255), (1, 10))]);
    let one_frame_gif = gif(&[(picture([1, 2, 3], 255), 100)]);
    let one_frame_webp = webp(&[(picture([1, 2, 3], 255), 100)]);
    let mut plain = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_raw(W, H, picture([1, 2, 3], 0)).unwrap().write_to(&mut plain, image::ImageFormat::Png).unwrap();
    let mut plain_webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut plain_webp).encode(&picture([1, 2, 3], 0), W, H, image::ExtendedColorType::Rgba8).unwrap();
    for (name, bytes) in
        [("one.png", one_frame_apng), ("one.gif", one_frame_gif), ("one.webp", one_frame_webp), ("plain.png", plain.into_inner()), ("plain.webp", plain_webp)]
    {
        assert!(open(name, &bytes).is_none(), "{name}");
        let s = open_bytes(name, bytes.into(), &[]).unwrap();
        assert_eq!(s.info().kind, MediaKind::Still, "{name}");
    }
}

#[test]
fn damaged_files_never_panic() {
    let frames: Vec<_> = (0..6u8).map(|i| (picture([i * 40, 0, 255 - i * 40], i * 40), (1, 10))).collect();
    let png_bytes = apng(&frames);
    let gif_bytes = gif(&frames.iter().map(|(p, _)| (p.clone(), 100)).collect::<Vec<_>>());
    let webp_bytes = webp(&frames.iter().map(|(p, _)| (p.clone(), 100)).collect::<Vec<_>>());
    for whole in [png_bytes, gif_bytes, webp_bytes] {
        for cut in (0..whole.len()).step_by(7) {
            // truncated: opens as fewer frames, a still or an error, never a panic
            let _ = open_bytes("cut", whole[..cut].to_vec().into(), &[]);
        }
        let mut flipped = whole.clone();
        for i in (40..flipped.len()).step_by(11) {
            if let Some(b) = flipped.get_mut(i) {
                *b ^= 0x5A;
            }
        }
        let _ = open_bytes("flipped", flipped.into(), &[]);
    }
}

#[test]
fn a_damaged_later_frame_keeps_the_frames_before_it() {
    let frames: Vec<_> = (0..4u8).map(|i| (picture([i * 60, 1, 2], 255), (1, 10))).collect();
    let whole = apng(&frames);
    // cut inside the last frame's data: the first three frames still play
    let cut = whole.len() - 20;
    let s = open("cut.png", &whole[..cut]).expect("still animated").expect("opens");
    assert_eq!(s.frame_count(), 3);
    assert_eq!(s.info().duration, ms(300));
    assert_eq!(pixels(&s, Tick::ZERO), frames[0].0);
}

#[test]
fn frame_delays_have_browser_minimums_and_never_overflow() {
    let d = |n: u32, den: u32| Delay::from_numer_denom_ms(n, den);
    assert_eq!(Format::Gif.frame_ticks(d(0, 1)), ms(100));
    assert_eq!(Format::Gif.frame_ticks(d(10, 1)), ms(100));
    assert_eq!(Format::Gif.frame_ticks(d(20, 1)), ms(20));
    assert_eq!(Format::Apng.frame_ticks(d(0, 1)), ms(10));
    assert_eq!(Format::Apng.frame_ticks(d(40, 1)), ms(40));
    assert_eq!(Format::Apng.frame_ticks(d(1000, 3)), Tick(TICKS_PER_SECOND / 3));
    assert!(Format::Apng.frame_ticks(d(u32::MAX, 1)).0 > 0);
    assert_eq!(Format::WebP.frame_ticks(d(0, 1)), ms(10));
    assert_eq!(Format::WebP.frame_ticks(d(40, 1)), ms(40));
}

#[test]
fn clip_rate_follows_the_common_frame_length() {
    assert_eq!(rate_of(&[ms(40), ms(40), ms(100)]), FrameRate::new(25, 1));
    assert_eq!(rate_of(&[ms(5_000), ms(5_000)]), FrameRate::FPS_30, "0.2 fps is not a video rate");
    assert_eq!(rate_of(&[]), FrameRate::FPS_30);
}

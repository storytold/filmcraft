//! ProRes buffer identity and pixels, in their own process because the frame pool is global.

use std::sync::Arc;

use filmcraft_codecs::{VideoDecoder, video::ProResDecoder};
use filmcraft_frame::PixelData;
use filmcraft_prores::{Encoder, Frame, Profile};

#[test]
fn prores_recycled_planes_preserve_pixels_alpha_and_shared_frames() {
    let mut decoder = ProResDecoder::default();
    let mut held = None;
    let mut held_pixels = Vec::new();
    let mut recycled_plane_ptrs = [std::ptr::null(); 3];
    let mut invalidated_plane_ptrs = Vec::new();
    for (i, (profile, width, height)) in [
        (Profile::Hq, 256, 256),
        (Profile::Hq, 256, 256),
        (Profile::Hq, 256, 256),
        (Profile::Hq, 256, 256),
        (Profile::Hq, 256, 256),
        (Profile::P4444, 512, 128),
        (Profile::P4444Xq, 512, 128),
        (Profile::Lt, 32, 16),
    ]
    .into_iter()
    .enumerate()
    {
        let with_alpha = matches!(profile, Profile::P4444 | Profile::P4444Xq);
        let mut input = Frame::new(width, height, profile.chroma(), if with_alpha { 12 } else { 10 }, with_alpha);
        input.y.fill(100 + i as u16 * 20);
        if let Some(a) = &mut input.alpha {
            a.fill(2048);
        }
        let sample = Encoder::new(profile, width, height).encode(&input).unwrap();
        let expected = filmcraft_prores::decode_frame(&sample).unwrap();
        let mut frames = decoder.decode(&sample, i as i64).unwrap();
        assert_eq!(frames.len(), 1);
        let decoded = frames.pop().unwrap();
        assert_eq!(decoded.pts, i as i64);
        assert_eq!((decoded.frame.width, decoded.frame.height), (expected.width, expected.height));
        let PixelData::Yuv16 { planes, bits, alpha, .. } = &decoded.frame.data else { panic!("expected planar ProRes output") };
        assert_eq!(*bits, expected.bit_depth as u32);
        assert_eq!(*planes[0], expected.y);
        assert_eq!(*planes[1], expected.cb);
        assert_eq!(*planes[2], expected.cr);
        assert_eq!(alpha.as_ref().map(|a| a.as_slice()), expected.alpha.as_deref());
        let mut plane_ptrs = planes.each_ref().map(|p| p.as_ptr());
        // Equal-sized chroma buffers can exchange places in the pool.
        plane_ptrs.sort_unstable();
        if i == 1 {
            recycled_plane_ptrs = plane_ptrs;
        } else if i == 2 {
            assert_eq!(plane_ptrs, recycled_plane_ptrs, "the third HQ decode must reuse all three planes recycled from the second");
        } else if i == 3 || i == 4 {
            assert!(
                plane_ptrs.iter().all(|p| !invalidated_plane_ptrs.contains(p)),
                "decode {i} must not take pooled planes using hints cleared by an error or reset"
            );
        }
        if i == 0 {
            held_pixels = decoded.frame.to_rgba8();
            held = Some(decoded.frame.clone());
        }
        if i == 2 {
            assert!(decoder.decode(&[0; 4], 99).is_err());
        } else if i == 3 {
            decoder.reset();
        }
        if i == 2 || i == 3 {
            // Recycle only after invalidation: the pool keeps these allocations alive for the next decode.
            invalidated_plane_ptrs.extend(plane_ptrs);
        }
        filmcraft_frame::pool::recycle(Arc::new(decoded.frame));
    }
    assert_eq!(held.unwrap().to_rgba8(), held_pixels, "a retained frame must never be overwritten");
    decoder.reset();
    assert!(decoder.flush().is_empty());
}

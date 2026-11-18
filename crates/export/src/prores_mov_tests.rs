//! [`crate::encode_prores_mov`]: streamed ProRes `.mov` from float frames, with hostile sizes and rates.

use std::io::Cursor;

use craft_video::prores::Profile;

use crate::{ExportError, FrameRate, encode_prores_mov};

fn grey(w: usize, h: usize, v: f32) -> Vec<f32> {
    vec![v; w * h * 3]
}

fn encode(profile: Profile, frames: Vec<crate::Result<Vec<f32>>>) -> crate::Result<(Vec<u8>, u64)> {
    encode_prores_mov(Cursor::new(Vec::new()), 48, 32, FrameRate::new(25, 1), profile, frames).map(|(c, n)| (c.into_inner(), n))
}

#[test]
fn frames_stream_into_one_sample_each_and_keep_the_neutral_code_in_4444() {
    for profile in [Profile::P4444, Profile::Standard] {
        let (bytes, n) = encode(profile, vec![Ok(grey(48, 32, 0.5)), Ok(grey(48, 32, 0.5)), Ok(grey(48, 32, 0.5))]).unwrap();
        assert_eq!(n, 3);
        let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
        let vt = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        assert_eq!(file.tracks[vt].samples.len(), 3, "{profile:?}");
        let sample = file.read_sample(bytes.as_slice(), vt, 0).unwrap();
        let fr = craft_video::prores::decode_frame(&sample).unwrap();
        // 10-bit limited range: 0.5 grey is Y 502, Cb = Cr = 512 (no motion in the UV pass); the
        // decoder may hand 4444 back at 12 bits
        let shift = i32::from(fr.bit_depth.saturating_sub(10));
        for &y in &fr.y {
            assert!(((i32::from(y) >> shift) - 502).abs() <= 1, "{profile:?}: Y {y} at {} bits", fr.bit_depth);
        }
        for &c in fr.cb.iter().chain(fr.cr.iter()) {
            assert!(((i32::from(c) >> shift) - 512).abs() <= 1, "{profile:?}: chroma {c} at {} bits", fr.bit_depth);
        }
    }
}

#[test]
fn a_frame_of_the_wrong_size_is_an_error_not_a_panic() {
    for bad in [Vec::new(), grey(47, 32, 0.5), grey(48, 33, 0.5)] {
        let r = encode(Profile::Standard, vec![Ok(grey(48, 32, 0.5)), Ok(bad)]);
        assert!(matches!(r, Err(ExportError::Encode(_))), "{r:?}");
    }
}

#[test]
fn an_error_from_the_frame_source_stops_the_encode() {
    let r = encode(Profile::Standard, vec![Ok(grey(48, 32, 0.5)), Err(ExportError::Cancelled), Ok(grey(48, 32, 0.5))]);
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
}

#[test]
fn hostile_sizes_and_rates_are_rejected() {
    let rate = FrameRate::new(25, 1);
    for (w, h) in [(0, 32), (48, 0), (crate::PRORES_MOV_MAX_SIDE + 1, 32)] {
        let r = encode_prores_mov(Cursor::new(Vec::new()), w, h, rate, Profile::Standard, Vec::<crate::Result<Vec<f32>>>::new());
        assert!(matches!(r, Err(ExportError::Unsupported(_))), "{w}x{h}");
    }
    for (num, den) in [(0, 1), (25, 0), (-25, 1), (i64::MAX, 1), (25, i64::MIN)] {
        let r = encode_prores_mov(Cursor::new(Vec::new()), 48, 32, FrameRate { num, den }, Profile::Standard, Vec::<crate::Result<Vec<f32>>>::new());
        assert!(matches!(r, Err(ExportError::Unsupported(_))), "{num}/{den}");
    }
}

#[test]
fn hdr_4444_preserves_highlights_signal_and_partial_alpha_in_the_file() {
    use crate::{ColorSignal, ProResMovFormat, ProResMovFrame, encode_prores_mov_frames};
    use filmcraft_color::{ColorPipeline, OutputTransform, WorkingSpace};
    for (working, signal) in [(WorkingSpace::Rec2100Pq, ColorSignal::PQ), (WorkingSpace::Rec2100Hlg, ColorSignal::HLG)] {
        let pipe = ColorPipeline { working, ..ColorPipeline::REC709 };
        let transform = OutputTransform::new(&pipe, working.output_space());
        let peak = transform.encode([3.0; 3])[0];
        let white = transform.encode([1.0; 3])[0];
        let format = ProResMovFormat { profile: Profile::P4444, signal, alpha: true };
        let frame = ProResMovFrame { rgb: grey(48, 32, peak), alpha: Some(vec![512; 48 * 32]) };
        let (writer, _) = encode_prores_mov_frames(Cursor::new(Vec::new()), 48, 32, FrameRate::new(25, 1), format, [Ok(frame)]).unwrap();
        let bytes = writer.into_inner();
        let mov = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
        let vt = mov.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        let sample = mov.read_sample(bytes.as_slice(), vt, 0).unwrap();
        let frame = craft_video::prores::decode_frame(&sample).unwrap();
        let shift = frame.bit_depth.saturating_sub(10);
        let y = f32::from(frame.y[0] >> shift);
        assert!((y - (64.0 + 876.0 * peak)).abs() < 2.0 && y > 64.0 + 876.0 * white);
        let a = frame.alpha.unwrap()[0] >> shift;
        assert!(a.abs_diff(512) <= 1, "decoded alpha at 10 bits: {a}");
        assert_eq!(frame.color.transfer, signal.transfer);
    }
}

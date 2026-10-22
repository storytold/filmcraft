#[path = "common/apple_log.rs"]
mod fixture;
use filmcraft_color::{ColorSpace, Matrix, Range};
use filmcraft_media::FrameRequest;
use filmcraft_time::Tick;

#[test]
fn explicit_quicktime_apple_log_is_detected_in_metadata_and_frames() {
    let source = filmcraft_codecs::open_bytes("original.mov", fixture::movie(Some(fixture::APPLE_LOG), true, 9).into()).unwrap();
    assert_eq!(ColorSpace::from_info(&source.info().video.as_ref().unwrap().color), ColorSpace::AppleLog);
    let frame = source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
    assert_eq!(ColorSpace::from_info(&frame.color), ColorSpace::AppleLog);
    assert_eq!((frame.color.matrix, frame.color.range), (Matrix::Bt2020Ncl, Range::Limited));
    assert_eq!((frame.width, frame.height), (64, 64));
}

#[test]
fn apple_log_without_colr_preserves_the_decoders_matrix_and_range() {
    for (code, matrix) in [(1, Matrix::Bt709), (9, Matrix::Bt2020Ncl)] {
        let source = filmcraft_codecs::open_bytes("original.mov", fixture::movie(Some(fixture::APPLE_LOG), false, code).into()).unwrap();
        assert_eq!(ColorSpace::from_info(&source.info().video.as_ref().unwrap().color), ColorSpace::AppleLog);
        let frame = source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
        assert_eq!(ColorSpace::from_info(&frame.color), ColorSpace::AppleLog);
        assert_eq!((frame.color.matrix, frame.color.range), (matrix, Range::Limited));
    }
}

#[test]
fn rec2020_and_unknown_or_malformed_log_identifiers_do_not_infer_apple_log() {
    let too_long = vec![b'a'; 1024];
    for identifier in [
        None,
        Some(b"com.apple.rec2020.apple-log2".as_slice()),
        Some(b"apple-log".as_slice()),
        Some(b"\xff\xfe".as_slice()),
        Some(b"".as_slice()),
        Some(too_long.as_slice()),
    ] {
        let source = filmcraft_codecs::open_bytes("iPhone-AppleLog.mov", fixture::movie(identifier, true, 9).into()).unwrap();
        assert_eq!(ColorSpace::from_info(&source.info().video.as_ref().unwrap().color), ColorSpace::Rec2020);
        let frame = source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
        assert_eq!(ColorSpace::from_info(&frame.color), ColorSpace::Rec2020);
    }
}

#[test]
fn duplicate_or_truncated_log_boxes_do_not_override_valid_colr() {
    let duplicate = fixture::movie_with_logs(&[fixture::APPLE_LOG, fixture::APPLE_LOG], true, 9);
    let mut truncated = fixture::movie(Some(fixture::APPLE_LOG), true, 9);
    let offset = truncated.windows(4).position(|b| b == b"logs").unwrap();
    truncated[offset - 4..offset].copy_from_slice(&u32::MAX.to_be_bytes());
    for bytes in [duplicate, truncated] {
        let source = filmcraft_codecs::open_bytes("original.mov", bytes.into()).unwrap();
        let frame = source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
        assert_eq!(ColorSpace::from_info(&frame.color), ColorSpace::Rec2020);
    }
}

use super::*;

#[test]
fn slate_rendering_releases_cache_lock_before_parallel_or_nested_work() {
    let info = GeneratorSource::demo(filmcraft_media::generators::DemoScene::Aurora).info().clone();
    let source = SlateSource::new(info, "synthetic", OfflineReason::Missing);
    let frame = source.cached_frame(2, 1, || {
        // Fails immediately on the original locking policy, before attempting reentry.
        // Real slate rendering reaches Rayon through Image::to_rgba8 and can reenter here
        // when a worker steals another frame request for this source.
        assert!(source.last.try_lock().is_ok(), "slate cache must not be held while rendering");
        let nested = source.cached_frame(1, 1, || vec![10, 20, 30, 255]);
        assert_eq!(nested.to_rgba8(), vec![10, 20, 30, 255]);
        vec![1, 2, 3, 255, 4, 5, 6, 255]
    });
    assert_eq!((frame.width, frame.height), (2, 1));
    assert_eq!(frame.to_rgba8(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
    let cached = source.cached_frame(2, 1, || panic!("a matching frame must be cached"));
    assert!(Arc::ptr_eq(&frame, &cached));
}

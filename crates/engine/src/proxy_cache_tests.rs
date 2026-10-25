use super::*;
use std::sync::atomic::AtomicUsize;

/// Count actual file reads separately from source resolution requests.
struct Files {
    original: Vec<u8>,
    proxy: Vec<u8>,
    missing: bool,
    loading: bool,
    original_reads: AtomicUsize,
    proxy_reads: AtomicUsize,
}
impl Services for Files {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        if path.ends_with("proxy.wav") {
            self.proxy_reads.fetch_add(1, Ordering::Relaxed);
            return Ok(self.proxy.clone());
        }
        self.original_reads.fetch_add(1, Ordering::Relaxed);
        if self.loading {
            return Err(filmcraft_media::pending::would_block());
        }
        if self.missing {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "generated original unavailable"));
        }
        Ok(self.original.clone())
    }
    fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
}
fn fixture() -> (MediaPool, MediaClip, Files) {
    let files = Files {
        original: filmcraft_media::wav::write_wav16(&(0..128).flat_map(|_| (1..=8).map(|ch| ch as f32 * 0.05)).collect::<Vec<_>>(), 8, 48_000),
        proxy: filmcraft_media::wav::write_wav16(&vec![0.01; 256], 2, 48_000),
        missing: false,
        loading: false,
        original_reads: AtomicUsize::new(0),
        proxy_reads: AtomicUsize::new(0),
    };
    let pool = MediaPool::default();
    let info = pool.open_file("original.wav", &files).unwrap().info().clone();
    files.original_reads.store(0, Ordering::Relaxed);
    let media = MediaClip {
        media: MediaRef::File { path: "original.wav".into() },
        info,
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: Some(MediaRef::File { path: "proxy.wav".into() }),
        identity: None,
    };
    (pool, media, files)
}

#[test]
fn missing_original_is_not_reopened_for_every_preview_request() {
    let (pool, media, mut files) = fixture();
    files.missing = true;
    let mut project = Project::default();
    let item = project.add_item("generated", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Media(media), None);
    pool.set_use_proxies(true);
    let first = pool.source_for(&project, item, &files).unwrap();
    let counts = (files.proxy_reads.load(Ordering::Relaxed), files.original_reads.load(Ordering::Relaxed));
    assert_eq!(counts, (1, 2), "one proxy attempt plus one cached full-resolution offline decision");
    for _ in 0..12 {
        let next = pool.source_for(&project, item, &files).unwrap();
        assert!(Arc::ptr_eq(&first, &next));
    }
    assert_eq!(files.proxy_reads.load(Ordering::Relaxed), counts.0, "do not reopen the proxy per frame");
    assert_eq!(files.original_reads.load(Ordering::Relaxed), counts.1, "do not retry unavailable original per frame");
    assert_eq!(pool.open_sources(), 1, "negative decisions are not opened sources");
    assert_eq!(pool.offline_status(item).unwrap().reason, OfflineReason::Missing);
    files.missing = false;
    pool.remove(item);
    let recovered = pool.source_for(&project, item, &files).unwrap();
    assert_eq!(recovered.audio(0, 16, 48_000).unwrap().channels.len(), 8);
    assert!(pool.offline_status(item).is_none(), "recovery clears the old missing status");
}

#[test]
fn inserting_a_refreshed_original_rebuilds_proxy_audio() {
    for insert in 0..3 {
        let (pool, media, mut files) = fixture();
        let first = pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).unwrap();
        let old = first.audio(0, 16, 48_000).unwrap();
        files.original = filmcraft_media::wav::write_wav16(&vec![-0.25; 128 * 8], 8, 48_000);
        let fresh = pool.open_file("original.wav", &files).unwrap();
        match insert {
            0 => pool.insert_file(ItemId(1), "original.wav", fresh.clone()),
            1 => pool.insert_keyed(ItemId(1), media_key(&media), fresh.clone()),
            _ => pool.insert(ItemId(1), fresh.clone()),
        }
        let rebuilt = pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).unwrap();
        assert!(!Arc::ptr_eq(&first, &rebuilt), "insertion {insert} invalidates the proxy decision");
        let actual = rebuilt.audio(0, 16, 48_000).unwrap();
        assert_ne!(actual.channels, old.channels);
        assert_eq!(actual.channels, fresh.audio(0, 16, 48_000).unwrap().channels);
    }
}

#[test]
fn proxy_decisions_follow_original_metadata_and_reference() {
    let (pool, mut media, mut files) = fixture();
    let original_info = media.info.clone();
    media.info = pool.open_file("proxy.wav", &files).unwrap().info().clone();
    let stereo = pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).unwrap();
    assert_eq!(stereo.audio(0, 16, 48_000).unwrap().channels.len(), 2);
    media.info = original_info;
    let multichannel = pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).unwrap();
    assert_eq!(multichannel.audio(0, 16, 48_000).unwrap().channels.len(), 8, "metadata changed without a proxy path change");
    pool.remove(ItemId(1));
    files.missing = true;
    assert!(pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).is_none());
    files.missing = false;
    media.media = MediaRef::File { path: "replacement.wav".into() };
    assert!(pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).is_some(), "source reference changes retry the failed decision");
}

#[test]
fn negative_decisions_retry_after_clear_insert_or_proxy_change() {
    for reset in 0..5 {
        let (pool, media, mut files) = fixture();
        files.missing = true;
        assert!(pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).is_none());
        files.missing = false;
        let path = match reset {
            0 => {
                pool.clear();
                "proxy.wav"
            }
            1 => {
                pool.remove(ItemId(1));
                "proxy.wav"
            }
            2 => {
                let src = pool.open_file("original.wav", &files).unwrap();
                pool.insert_file(ItemId(1), "original.wav", src);
                "proxy.wav"
            }
            3 => {
                let src = pool.open_file("original.wav", &files).unwrap();
                pool.insert_keyed(ItemId(1), media_key(&media), src);
                "proxy.wav"
            }
            _ => "replacement-proxy.wav",
        };
        let src = pool.proxy_source(ItemId(1), path, &media, &files).unwrap();
        assert_eq!(src.audio(0, 16, 48_000).unwrap().channels.len(), 8);
    }
}

#[test]
fn pending_original_reads_are_not_cached_as_permanent_failures() {
    let (pool, media, mut files) = fixture();
    filmcraft_media::pending::take();
    files.loading = true;
    assert!(pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).is_none());
    assert!(filmcraft_media::pending::take());
    files.loading = false;
    assert!(pool.proxy_source(ItemId(1), "proxy.wav", &media, &files).is_some());
}

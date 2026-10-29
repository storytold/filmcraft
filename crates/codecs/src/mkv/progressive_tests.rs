//! A WebM that arrives in pieces: the first picture needs only the front of the file, requests past
//! the indexed part wait, and the index grows with the bytes (see [`super::index`]).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use filmcraft_frame::PixelData;
use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_media::{ByteReader, FrameRequest, MediaSource, pending};
use filmcraft_time::Tick;

use super::MkvSource;

const FRAMES: i64 = 40;

/// A flat grey Motion-JPEG picture: its level says which frame it is.
fn picture(k: i64) -> Vec<u8> {
    let img = image::GrayImage::from_pixel(16, 16, image::Luma([10 + 5 * k as u8]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95).encode_image(&img).expect("jpeg");
    out
}

/// Ten pictures and 100 ms of PCM per second-long Cluster, four Clusters.
fn webm() -> Vec<u8> {
    let mut v = TrackSpec::new(TrackKind::Video, "V_MJPEG");
    v.video_size = Some((16, 16));
    v.default_duration_ns = Some(100_000_000);
    let mut a = TrackSpec::new(TrackKind::Audio, "A_PCM/INT/LIT");
    a.audio = Some((48_000.0, 1, Some(16)));
    let mut w = MkvWriter::new(io::Cursor::new(Vec::new()), vec![v, a], MuxOptions::default()).expect("writer");
    for k in 0..FRAMES {
        let pts = k * 100_000_000;
        w.write_frame(0, pts, true, &picture(k), Some(100_000_000)).expect("picture");
        // a level that says which packet it is
        let pcm: Vec<u8> = (0..4_800).flat_map(|_| (700 * (k as i16 + 1)).to_le_bytes()).collect();
        w.write_frame(1, pts, true, &pcm, Some(100_000_000)).expect("sound");
    }
    w.finish().expect("finish").into_inner()
}

/// A reader that has loaded a prefix of the file, like the web build's chunk cache: reads past it
/// are deferred (`WouldBlock`, [`pending`] set).
struct Growing {
    data: Arc<[u8]>,
    loaded: AtomicU64,
}

impl ByteReader for Growing {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end =
            offset.checked_add(buf.len() as u64).filter(|e| *e <= self.len()).ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "past the end"))?;
        if end > self.loaded.load(Ordering::SeqCst) {
            return Err(pending::would_block());
        }
        buf.copy_from_slice(&self.data[offset as usize..end as usize]);
        Ok(())
    }
    fn ready_prefix(&self) -> u64 {
        self.loaded.load(Ordering::SeqCst)
    }
}

struct Fixture {
    bytes: Arc<[u8]>,
    /// End of each Cluster.
    cluster_ends: Vec<u64>,
}

fn fixture() -> Fixture {
    let bytes: Arc<[u8]> = webm().into();
    let file = filmcraft_matroska::open(&bytes[..]).expect("open");
    assert_eq!(file.clusters.len(), 4, "one Cluster per second");
    Fixture { cluster_ends: file.clusters.iter().map(|c| c.end).collect(), bytes }
}

fn growing(f: &Fixture, loaded: u64) -> Arc<Growing> {
    Arc::new(Growing { data: f.bytes.clone(), loaded: AtomicU64::new(loaded) })
}

fn level(src: &MkvSource, k: i64) -> Result<u8, filmcraft_media::MediaError> {
    let frame = src.video_frame(FrameRequest::full(Tick::from_rational(k, 1, 10)))?;
    let PixelData::Rgba8(px) = &frame.data else { panic!("MJPEG decodes to RGBA8") };
    Ok(px[0])
}

#[test]
fn the_first_picture_needs_only_the_front_of_the_file() {
    let f = fixture();
    let reader = growing(&f, f.cluster_ends[0]);
    let src = MkvSource::open_reader("a.webm", reader.clone()).expect("opens from the first Cluster");
    // Info/Duration, not the samples, gives the length of a file that is not all indexed
    assert_eq!(src.info().duration, Tick::from_rational(FRAMES, 1, 10));
    assert!(src.info().video.as_ref().expect("video").bitrate.is_none(), "unknown until the whole file is indexed");
    assert_eq!(src.info().audio_streams.len(), 1);
    // the first second is served, the second is still on its way
    let first = level(&src, 0).expect("first picture");
    assert!(level(&src, 9).is_ok());
    assert!(level(&src, 10).is_err());
    assert!(pending::take(), "a request past the index is retried later, not offline or damaged");
    // pictures stay what they were
    assert_eq!(level(&src, 0).expect("again"), first);
}

#[test]
fn requests_past_the_index_wait_until_their_bytes_have_arrived() {
    let f = fixture();
    let reader = growing(&f, f.cluster_ends[0]);
    let src = MkvSource::open_reader("a.webm", reader.clone()).expect("open");
    // bytes arrive, but not the whole second Cluster: it waits
    reader.loaded.store(f.cluster_ends[1] - 1, Ordering::SeqCst);
    assert!(level(&src, 10).is_err() && pending::take());
    reader.loaded.store(f.cluster_ends[1], Ordering::SeqCst);
    assert!(level(&src, 10).is_ok());
    assert!(level(&src, 19).is_ok());
    assert!(level(&src, 20).is_err() && pending::take());
    // the rest arrives at once
    reader.loaded.store(f.bytes.len() as u64, Ordering::SeqCst);
    for k in 0..FRAMES {
        assert!(level(&src, k).is_ok(), "picture {k}");
    }
    assert!(src.info().video.as_ref().is_some(), "info is not rewritten");
    assert!(src.tables().complete());
}

#[test]
fn a_grown_index_gives_what_a_complete_one_gives() {
    let f = fixture();
    let whole = MkvSource::open("whole.webm", f.bytes.clone()).expect("open whole");
    let reader = growing(&f, f.cluster_ends[0]);
    let src = MkvSource::open_reader("a.webm", reader.clone()).expect("open");
    for (n, end) in f.cluster_ends.iter().enumerate() {
        reader.loaded.store(*end, Ordering::SeqCst);
        for k in (n as i64 * 10)..(n as i64 * 10 + 10) {
            assert_eq!(level(&src, k).expect("grown"), level(&whole, k).expect("whole"), "picture {k}");
        }
    }
    // the same pictures in reverse (nothing is re-indexed or re-read wrongly)
    for k in (0..FRAMES).rev() {
        assert_eq!(level(&src, k).expect("grown"), level(&whole, k).expect("whole"), "picture {k}");
    }
}

#[test]
fn audio_follows_the_index() {
    let f = fixture();
    let whole = MkvSource::open("whole.webm", f.bytes.clone()).expect("open whole");
    let reader = growing(&f, f.cluster_ends[0]);
    let src = MkvSource::open_reader("a.webm", reader.clone()).expect("open");
    // 100 ms from 0.2 s: in the first Cluster
    let early = src.audio(9_600, 4_800, 48_000).expect("early audio");
    assert_eq!(early.channels[0], whole.audio(9_600, 4_800, 48_000).expect("whole").channels[0]);
    assert!((early.channels[0][100] - 2_100.0 / 32_768.0).abs() < 1e-3, "the third packet");
    // 100 ms from 2.2 s: not indexed yet
    assert!(src.audio(105_600, 4_800, 48_000).is_err() && pending::take());
    reader.loaded.store(f.cluster_ends[3], Ordering::SeqCst);
    let late = src.audio(105_600, 4_800, 48_000).expect("late audio");
    assert_eq!(late.channels[0], whole.audio(105_600, 4_800, 48_000).expect("whole").channels[0]);
}

#[test]
fn nothing_loaded_is_retried_not_an_error() {
    let f = fixture();
    let reader = growing(&f, 0);
    assert!(MkvSource::open_reader("a.webm", reader.clone()).is_err());
    assert!(pending::take());
    reader.loaded.store(f.cluster_ends[0], Ordering::SeqCst);
    assert!(MkvSource::open_reader("a.webm", reader).is_ok());
}

#[test]
fn a_reader_with_everything_is_indexed_as_a_whole() {
    let f = fixture();
    let src = MkvSource::open_reader("a.webm", growing(&f, f.bytes.len() as u64)).expect("open");
    assert!(src.tables().complete());
    assert!(src.info().video.as_ref().expect("video").bitrate.is_some());
}

#[test]
fn truncated_files_never_panic() {
    let f = fixture();
    for len in (0..f.bytes.len()).step_by(997) {
        let reader = Arc::new(Growing { data: f.bytes[..len].into(), loaded: AtomicU64::new(len as u64 / 2) });
        let Ok(src) = MkvSource::open_reader("cut.webm", reader.clone()) else { continue };
        reader.loaded.store(len as u64, Ordering::SeqCst);
        for k in [0, 7, 15, 39] {
            let _ = level(&src, k);
        }
        let _ = src.audio(0, 4_800, 48_000);
        let _ = src.audio(150_000, 4_800, 48_000);
        pending::take();
    }
}

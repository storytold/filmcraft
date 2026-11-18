//! A failed pre-roll read on an audio seek is not cached (#5014).
//!
//! A non-sequential audio request resets the decoder and primes it with the packets before the
//! target. When one of those reads failed (an I/O error, or a web Blob read deferred with
//! [`filmcraft_media::pending`]), the target was decoded without its pre-roll and cached, so every
//! retry returned the same wrong samples. The read must be retried on the next request instead.

use std::io::{self, Cursor};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_media::reader::ByteReader;
use filmcraft_media::{MediaSource, pending};

use crate::{MkvSource, Mp4Source};

const RATE: u32 = 48_000;
/// The packet a request at [`START`] decodes first; the packet before it is its pre-roll.
const TARGET: usize = 20;
const START: i64 = TARGET as i64 * 1024 + 100;
const FRAMES: usize = 4096;

/// A second of a stereo tone encoded with our AAC encoder: the AudioSpecificConfig and the units.
fn aac() -> (Vec<u8>, Vec<Vec<u8>>) {
    let tone = |hz: f32| (0..RATE as usize).map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin()).collect::<Vec<f32>>();
    let (l, r) = (tone(440.0), tone(660.0));
    let mut enc = craft_audio::aac::Encoder::new(craft_audio::aac::EncoderConfig::cbr(RATE, 2, 160_000)).unwrap();
    let mut units = enc.encode(&[&l, &r]);
    units.extend(enc.flush());
    (enc.audio_specific_config(), units)
}

fn mp4(asc: &[u8], units: &[Vec<u8>]) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::aac(asc.to_vec(), 2, RATE), RATE)).unwrap();
    for u in units {
        w.write_sample(t, WriteSample { data: u, duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

fn mkv(asc: &[u8], units: &[Vec<u8>]) -> Arc<[u8]> {
    let mut spec = TrackSpec::new(TrackKind::Audio, "A_AAC");
    spec.codec_private = asc.to_vec();
    spec.audio = Some((RATE as f64, 2, None));
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], MuxOptions::default()).unwrap();
    for (k, u) in units.iter().enumerate() {
        let ns = (k as i64 * 1024 * 1_000_000_000) / RATE as i64;
        w.write_frame(0, ns, true, u, None).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// Serves `data`, but once armed fails the first read that touches byte `at`, once.
struct FailOnce {
    data: Arc<[u8]>,
    at: u64,
    defer: bool,
    armed: AtomicBool,
}

impl ByteReader for FailOnce {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if (offset..offset + buf.len() as u64).contains(&self.at) && self.armed.swap(false, Ordering::SeqCst) {
            return Err(if self.defer { pending::would_block() } else { io::Error::other("read failed once") });
        }
        let s = offset as usize;
        buf.copy_from_slice(self.data.get(s..s + buf.len()).ok_or(io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
}

/// The request, retried while it was deferred (as export does).
fn request(src: &dyn MediaSource) -> Vec<Vec<f32>> {
    for _ in 0..8 {
        let r = src.audio(START, FRAMES, RATE);
        if !pending::take() {
            return r.unwrap().channels;
        }
    }
    panic!("the request stayed deferred");
}

fn differing(a: &[Vec<f32>], b: &[Vec<f32>]) -> usize {
    a.iter().zip(b).map(|(x, y)| x.iter().zip(y).filter(|(p, q)| p.to_bits() != q.to_bits()).count()).sum()
}

fn check(container: &str, bytes: Arc<[u8]>, units: &[Vec<u8>], open: impl Fn(Arc<dyn ByteReader>) -> Box<dyn MediaSource>) {
    let pre = &units[TARGET - 1];
    let at = bytes.windows(pre.len()).position(|w| w == &pre[..]).expect("the pre-roll packet is stored whole") as u64;
    let clean: Arc<dyn ByteReader> = Arc::new(FailOnce { data: bytes.clone(), at, defer: false, armed: AtomicBool::new(false) });
    let want = request(open(clean).as_ref());
    for defer in [false, true] {
        let reader = Arc::new(FailOnce { data: bytes.clone(), at, defer, armed: AtomicBool::new(false) });
        let src = open(reader.clone());
        reader.armed.store(true, Ordering::SeqCst);
        let first = request(src.as_ref());
        assert!(!reader.armed.load(Ordering::SeqCst), "{container}: the pre-roll read was made");
        // a deferred read is retried within `request`; an I/O error is concealed for this request
        if defer {
            let n = differing(&first, &want);
            assert_eq!(n, 0, "{container}, deferred: {n} samples differ from a clean source after the retry");
        }
        let n = differing(&request(src.as_ref()), &want);
        assert_eq!(n, 0, "{container}, deferred {defer}: {n} samples differ when asked again");
    }
}

#[test]
fn failed_preroll_read_is_retried_not_cached() {
    let (asc, units) = aac();
    check("mp4", mp4(&asc, &units), &units, |r| Box::new(Mp4Source::open_reader("tone.mp4", r).unwrap()));
    check("mkv", mkv(&asc, &units), &units, |r| Box::new(MkvSource::open_reader("tone.mkv", r).unwrap()));
}

//! #5013: opening a Matroska/WebM Opus source must report a read error instead of building
//! a timeline from the failed packet.
//!
//! `opus_starts` used to turn a failed packet read into "duration unknown", so
//! `contiguous_starts` fell back to the packet's millisecond block timestamp and every later
//! packet chained from it: the audio after the failed read shifted by up to 1 ms for the life
//! of the source, with the open still succeeding. A reader that fails one chosen read once
//! (an I/O error, or a deferred `pending::would_block()`) must therefore fail the open, or
//! the decoded audio must match a clean open exactly.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

use filmcraft_media::reader::{ByteReader, MemReader, SharedReader};

use crate::common::*;

/// A reader over `data` that fails read number `fail` once, then serves every read.
struct OneShot {
    data: Arc<[u8]>,
    reads: AtomicU64,
    fail: u64,
    done: AtomicBool,
    defer: bool,
}

impl ByteReader for OneShot {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if self.reads.fetch_add(1, SeqCst) == self.fail && !self.done.swap(true, SeqCst) {
            return Err(if self.defer { filmcraft_media::pending::would_block() } else { std::io::Error::other("one-shot read failure") });
        }
        let s = off as usize;
        buf.copy_from_slice(self.data.get(s..s + buf.len()).ok_or(std::io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
}

/// A reader that counts reads, for finding how many an open performs.
struct Counting {
    inner: MemReader,
    reads: AtomicU64,
}

impl ByteReader for Counting {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads.fetch_add(1, SeqCst);
        self.inner.read_at(off, buf)
    }
}

fn open(name: &str, reader: SharedReader) -> Result<filmcraft_media::SharedSource, filmcraft_media::MediaError> {
    filmcraft_media::reader::open_reader_within(name, reader, &filmcraft_codecs::reader_openers(), &filmcraft_codecs::openers(), u64::MAX)
}

/// The whole audio track as one flat Vec per channel.
fn decode_all(src: &filmcraft_media::SharedSource, frames: usize) -> Vec<Vec<f32>> {
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut done = 0;
    while done < frames {
        let n = 48_000.min(frames - done);
        let buf = src.audio(done as i64, n, 48_000).unwrap();
        if out.is_empty() {
            out = vec![Vec::with_capacity(frames); buf.channels.len()];
        }
        for (ch, samples) in buf.channels.iter().enumerate() {
            out[ch].extend_from_slice(samples);
        }
        done += n;
    }
    out
}

#[test]
fn opus_open_reports_a_failed_read() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = fixture(
        &ff,
        "h264_opus_5013.mkv",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=6",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=6,aformat=channel_layouts=stereo",
            "-c:v",
            "libx264",
            "-c:a",
            "libopus",
            "-shortest",
        ],
    ) else {
        eprintln!("SKIPPED: this ffmpeg can't encode h264_opus_5013.mkv");
        return;
    };
    let data = bytes(&path);
    let name = "h264_opus_5013.mkv";

    // Reference: a clean open, decoded in full.
    let clean = open(name, Arc::new(MemReader(data.clone()))).unwrap();
    let frames = (clean.info().duration.seconds() * 48_000.0).round() as usize;
    assert!(frames > 48_000, "fixture too short: {frames} frames");
    let reference = decode_all(&clean, frames);

    // How many reads does a clean open perform? Every read of the open is a candidate for
    // the one-shot failure below.
    let counting = Arc::new(Counting { inner: MemReader(data.clone()), reads: AtomicU64::new(0) });
    let _ = open(name, counting.clone()).unwrap();
    let open_reads = counting.reads.load(SeqCst);
    assert!(open_reads > 0, "open performed no reads");

    // An I/O error on any single read of the open must either fail the open (or the decode
    // that follows it) or leave the decoded audio bit-identical to the clean open: never a
    // silently shifted timeline.
    for k in 0..open_reads {
        let failing = Arc::new(OneShot { data: data.clone(), reads: AtomicU64::new(0), fail: k, done: AtomicBool::new(false), defer: false });
        match open(name, failing) {
            Err(_) => continue,
            Ok(src) => match decode_all_checked(&src, frames) {
                Err(_) => continue,
                Ok(got) => {
                    assert_eq!(got.len(), reference.len(), "failing read #{k}: channel count changed");
                    for (ch, (g, r)) in got.iter().zip(reference.iter()).enumerate() {
                        assert_eq!(g.len(), r.len(), "failing read #{k}: channel {ch} length changed");
                        let diffs = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                        assert_eq!(diffs, 0, "failing read #{k} shifted the audio: channel {ch} has {diffs} differing samples");
                    }
                }
            },
        }
    }

    // The same, with a deferred read: the open must fail, and the failure must be reported
    // through `pending` so the caller retries instead of caching a bad source.
    for k in 0..open_reads {
        let failing = Arc::new(OneShot { data: data.clone(), reads: AtomicU64::new(0), fail: k, done: AtomicBool::new(false), defer: true });
        match open(name, failing) {
            Err(_) => assert!(filmcraft_media::pending::take(), "failing read #{k} did not set pending"),
            Ok(_) => panic!("deferred read #{k} opened the source anyway"),
        }
    }
}

/// Like [`decode_all`], but a decode error is returned instead of panicking.
fn decode_all_checked(src: &filmcraft_media::SharedSource, frames: usize) -> Result<Vec<Vec<f32>>, filmcraft_media::MediaError> {
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut done = 0;
    while done < frames {
        let n = 48_000.min(frames - done);
        let buf = src.audio(done as i64, n, 48_000)?;
        if out.is_empty() {
            out = vec![Vec::with_capacity(frames); buf.channels.len()];
        }
        for (ch, samples) in buf.channels.iter().enumerate() {
            out[ch].extend_from_slice(samples);
        }
        done += n;
    }
    Ok(out)
}

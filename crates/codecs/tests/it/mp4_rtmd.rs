//! Sony XAVC MP4 with no `tmcd` track: the start timecode lives in an `rtmd` metadata track
//! (issue #460). A hand-built file exercises the whole probe path; the byte layout of the `rtmd`
//! sample is the one real XAVC footage uses (checked against ffprobe's `timecode` tag).

use std::sync::Arc;

use filmcraft_time::FrameRate;

/// Minimal ISO-BMFF box writer (the demuxer only needs headers and sample tables here).
#[derive(Default)]
struct B(Vec<u8>);

impl B {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0);
    }
    /// A container box (`size` + `kind` + children).
    fn container(&mut self, kind: &[u8; 4], f: impl FnOnce(&mut B)) {
        let at = self.0.len();
        self.u32(0);
        self.bytes(kind);
        f(self);
        let len = (self.0.len() - at) as u32;
        self.0[at..at + 4].copy_from_slice(&len.to_be_bytes());
    }
    /// A full box (`size` + `kind` + version/flags + body).
    fn full(&mut self, kind: &[u8; 4], f: impl FnOnce(&mut B)) {
        self.container(kind, |b| {
            b.zeros(4);
            f(b);
        });
    }
    fn leaf(&mut self, kind: &[u8; 4], payload: &[u8]) {
        self.container(kind, |b| b.bytes(payload));
    }
}

/// A `stsd` visual sample entry (JPEG keeps the video track decodable enough to open).
fn video_entry(b: &mut B) {
    b.bytes(&[0; 6]);
    b.u16(1);
    b.u16(0);
    b.u16(0);
    b.bytes(&[0; 4]);
    b.u32(0);
    b.u32(0);
    b.u16(1920);
    b.u16(1080);
    b.u32(0x0048_0000);
    b.u32(0x0048_0000);
    b.u32(0);
    b.u16(1);
    b.bytes(&[0; 32]);
    b.u16(24);
    b.u16(0xFFFF);
}

/// A video track at 59.94 fps, plus a Sony `rtmd` track whose first sample is `rtmd_sample`.
fn file_with_rtmd(rtmd_sample: &[u8]) -> Vec<u8> {
    file_with_rtmd_claimed(rtmd_sample, rtmd_sample.len() as u32)
}

/// As [`file_with_rtmd`] but the `rtmd` sample's `stsz` size is `claimed` (which may exceed the
/// bytes actually present, as a corrupt file can).
fn file_with_rtmd_claimed(rtmd_sample: &[u8], claimed: u32) -> Vec<u8> {
    let mut b = B::default();
    b.leaf(b"ftyp", b"isom\x00\x00\x00\x00isom");
    let mdat_start = b.0.len();
    let mdat: Vec<u8> = [&[1, 2, 3, 4][..], rtmd_sample].concat();
    b.leaf(b"mdat", &mdat);
    let video_off = (mdat_start + 8) as u32;
    let rtmd_off = video_off + 4;
    b.container(b"moov", |b| {
        b.full(b"mvhd", |b| {
            b.zeros(8);
            b.u32(1000);
            b.u32(1001);
            b.zeros(80);
        });
        // Video track (id 1): one sample of 1001/60000 s → 59.94 fps.
        b.container(b"trak", |b| {
            b.full(b"tkhd", |b| {
                b.zeros(8);
                b.u32(1);
                b.zeros(4);
                b.u32(1001);
                b.zeros(52);
                b.u32(1920 << 16);
                b.u32(1080 << 16);
            });
            b.container(b"mdia", |b| {
                b.full(b"mdhd", |b| {
                    b.zeros(8);
                    b.u32(60000);
                    b.u32(1001);
                    b.u16(0x55C4);
                    b.u16(0);
                });
                b.full(b"hdlr", |b| {
                    b.zeros(4);
                    b.bytes(b"vide");
                    b.zeros(12);
                    b.bytes(b"Video\0");
                });
                b.container(b"minf", |b| {
                    b.container(b"stbl", |b| {
                        b.full(b"stsd", |b| {
                            b.u32(1);
                            b.container(b"jpeg", video_entry);
                        });
                        b.full(b"stts", |b| {
                            b.u32(1);
                            b.u32(1);
                            b.u32(1001);
                        });
                        b.full(b"stsc", |b| {
                            b.u32(1);
                            b.u32(1);
                            b.u32(1);
                            b.u32(1);
                        });
                        b.full(b"stsz", |b| {
                            b.u32(0);
                            b.u32(1);
                            b.u32(4);
                        });
                        b.full(b"stco", |b| {
                            b.u32(1);
                            b.u32(video_off);
                        });
                    });
                });
            });
        });
        // Sony rtmd metadata track (id 2).
        b.container(b"trak", |b| {
            b.full(b"tkhd", |b| {
                b.zeros(8);
                b.u32(2);
                b.zeros(4);
                b.u32(1);
                b.zeros(52);
                b.zeros(8);
            });
            b.container(b"mdia", |b| {
                b.full(b"mdhd", |b| {
                    b.zeros(8);
                    b.u32(1);
                    b.u32(1);
                    b.u16(0x55C4);
                    b.u16(0);
                });
                b.full(b"hdlr", |b| {
                    b.zeros(4);
                    b.bytes(b"meta");
                    b.zeros(12);
                    b.bytes(b"Timed Metadata\0");
                });
                b.container(b"minf", |b| {
                    b.container(b"stbl", |b| {
                        b.full(b"stsd", |b| {
                            b.u32(1);
                            b.leaf(b"rtmd", &[0, 0, 0, 0, 0, 0, 0, 1]);
                        });
                        b.full(b"stts", |b| {
                            b.u32(1);
                            b.u32(1);
                            b.u32(1);
                        });
                        b.full(b"stsc", |b| {
                            b.u32(1);
                            b.u32(1);
                            b.u32(1);
                            b.u32(1);
                        });
                        b.full(b"stsz", |b| {
                            b.u32(0);
                            b.u32(1);
                            b.u32(claimed);
                        });
                        b.full(b"stco", |b| {
                            b.u32(1);
                            b.u32(rtmd_off);
                        });
                    });
                });
            });
        });
    });
    b.0
}

/// A Sony `rtmd` sample: hours, minutes, seconds at 0x0d–0x0f, a drop-frame flag at 0x10 and the
/// frame number at 0x11, then the start of the SMPTE KLV key.
fn rtmd_sample(h: u8, m: u8, s: u8, f: u8, drop: u8) -> Vec<u8> {
    let mut v = vec![0u8; 24];
    v[0x0d] = h;
    v[0x0e] = m;
    v[0x0f] = s;
    v[0x10] = drop;
    v[0x11] = f;
    v[0x12..0x16].copy_from_slice(&[0x06, 0x0e, 0x2b, 0x34]);
    v
}

#[test]
fn rtmd_start_timecode_is_read() {
    let file = file_with_rtmd(&rtmd_sample(3, 37, 12, 34, 0));
    let src = filmcraft_codecs::open_bytes("fx3.mp4", Arc::from(file.into_boxed_slice())).unwrap();
    let rate = FrameRate::new(60000, 1001);
    let tc = src.info().start_timecode.expect("rtmd start timecode");
    assert_eq!(tc, 781_954);
    assert_eq!(filmcraft_time::format_timecode_frames(tc, rate, false), "03:37:12:34");
}

#[test]
fn rtmd_without_usable_sample_is_ignored() {
    // Too short to carry the timecode block: no start timecode, and no panic.
    let file = file_with_rtmd(&[0u8; 6]);
    let src = filmcraft_codecs::open_bytes("short.mp4", Arc::from(file.into_boxed_slice())).unwrap();
    assert_eq!(src.info().start_timecode, None);
}

#[test]
fn huge_claimed_rtmd_sample_does_not_exhaust_memory() {
    // The stsz size claims 16 MiB but only the header bytes exist: only the fixed-size header is
    // read, so a corrupt size cannot drive a huge allocation during open (issue #460 review).
    let file = file_with_rtmd_claimed(&rtmd_sample(1, 2, 3, 4, 0), 16 * 1024 * 1024);
    let src = filmcraft_codecs::open_bytes("big.mp4", Arc::from(file.into_boxed_slice())).unwrap();
    assert_eq!(src.info().start_timecode, Some(223_384)); // 01:02:03:04 at 59.94
}

#[test]
fn damaged_timecode_fields_are_not_published() {
    // Frame 60 at 59.94 fps (valid range is 0–59): unusable metadata, not a fabricated time.
    let file = file_with_rtmd(&rtmd_sample(3, 37, 12, 60, 0));
    let src = filmcraft_codecs::open_bytes("bad.mp4", Arc::from(file.into_boxed_slice())).unwrap();
    assert_eq!(src.info().start_timecode, None);
}

#[test]
fn drop_frame_skipped_labels_are_not_published() {
    // 00:01:00;00 at 59.94 fps: frames 00-03 of a non-tenth minute don't exist in drop-frame
    // counting, so this metadata is unusable rather than a start time.
    let file = file_with_rtmd(&rtmd_sample(0, 1, 0, 0, 1));
    let src = filmcraft_codecs::open_bytes("df_bad.mp4", Arc::from(file.into_boxed_slice())).unwrap();
    assert_eq!(src.info().start_timecode, None);
    // The first real label of that minute (frame 04) is accepted.
    let file = file_with_rtmd(&rtmd_sample(0, 1, 0, 4, 1));
    let src = filmcraft_codecs::open_bytes("df_ok.mp4", Arc::from(file.into_boxed_slice())).unwrap();
    assert_eq!(src.info().start_timecode, Some(3600));
}

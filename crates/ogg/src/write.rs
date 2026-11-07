//! Ogg page writer (RFC 3533) and the comment-header packets of Ogg Opus (RFC 7845 §5.2) and Ogg
//! Vorbis (Vorbis I §5).
//!
//! [`PageWriter`] laces packets into pages of one logical bitstream: lacing values of 255 with a
//! shorter last value per packet, at most 255 per page, continued packets flagged on the next
//! page, the granule position of the last packet completed on each page (-1 on a page where no
//! packet completes), page sequence numbers, the CRC-32, and the beginning- and end-of-stream
//! flags. Header packets are flushed to pages of their own, as both mappings require.

use crate::crc32;

/// Default size at which an audio page is closed before the next packet is added.
pub const PAGE_TARGET_BYTES: usize = 4096;
/// Default number of packets after which an audio page is closed (one second of 20 ms Opus).
pub const PAGE_TARGET_PACKETS: usize = 50;

/// Writes the pages of one logical bitstream.
#[derive(Clone, Debug)]
pub struct PageWriter {
    serial: u32,
    seq: u32,
    lacing: Vec<u8>,
    body: Vec<u8>,
    /// Granule position of the last packet completed on the pending page.
    granule: Option<i64>,
    /// The pending page starts with the continuation of a packet.
    continued: bool,
    packets: usize,
    bos_written: bool,
    finished: bool,
    target_bytes: usize,
    target_packets: usize,
}

impl PageWriter {
    pub fn new(serial: u32) -> PageWriter {
        PageWriter {
            serial,
            seq: 0,
            lacing: Vec::with_capacity(255),
            body: Vec::new(),
            granule: None,
            continued: false,
            packets: 0,
            bos_written: false,
            finished: false,
            target_bytes: PAGE_TARGET_BYTES,
            target_packets: PAGE_TARGET_PACKETS,
        }
    }

    /// Closes audio pages at `bytes` of payload or `packets` packets, whichever comes first.
    pub fn with_page_target(mut self, bytes: usize, packets: usize) -> PageWriter {
        self.target_bytes = bytes.max(1);
        self.target_packets = packets.max(1);
        self
    }

    /// Pages written so far.
    pub fn pages(&self) -> u32 {
        self.seq
    }

    /// A header packet on pages of its own (granule position 0); the first one opens the stream.
    pub fn header(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = self.flush();
        out.extend(self.add(data, 0));
        out.extend(self.flush());
        out
    }

    /// Adds an audio packet that ends at granule position `granule`; returns the pages it
    /// completed. The pending page is closed first once it is full enough, so the last packet
    /// always stays pending for [`Self::finish`] to mark the end of the stream.
    pub fn packet(&mut self, data: &[u8], granule: i64) -> Vec<u8> {
        let mut out = Vec::new();
        if self.body.len() >= self.target_bytes || self.packets >= self.target_packets {
            out = self.flush();
        }
        out.extend(self.add(data, granule));
        out
    }

    /// Closes the pending page now, if there is one.
    pub fn flush(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.lacing.is_empty() {
            self.emit(&mut out, false);
        }
        out
    }

    /// Ends the stream: the pending page (or an empty page when nothing is pending) with the
    /// end-of-stream flag. `last_granule` replaces the granule position of the final page (Ogg
    /// Opus end trimming); `None` keeps the last packet's.
    pub fn finish(&mut self, last_granule: Option<i64>) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if let Some(g) = last_granule {
            self.granule = Some(g);
        }
        self.emit(&mut out, true);
        self.finished = true;
        out
    }

    fn add(&mut self, data: &[u8], granule: i64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut rest = data;
        loop {
            if self.lacing.len() == 255 {
                // The packet continues on the next page.
                self.emit(&mut out, false);
                self.continued = true;
            }
            let n = rest.len().min(255);
            self.lacing.push(n as u8);
            self.body.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if n < 255 {
                break;
            }
        }
        self.granule = Some(granule);
        self.packets += 1;
        out
    }

    fn emit(&mut self, out: &mut Vec<u8>, eos: bool) {
        let mut flags = 0u8;
        if self.continued {
            flags |= 0x01;
        }
        if !self.bos_written {
            flags |= 0x02;
            self.bos_written = true;
        }
        if eos {
            flags |= 0x04;
        }
        let start = out.len();
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(flags);
        out.extend_from_slice(&self.granule.unwrap_or(-1).to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.push(self.lacing.len() as u8);
        out.extend_from_slice(&self.lacing);
        out.extend_from_slice(&self.body);
        let crc = crc32(&out[start..]);
        out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.seq = self.seq.wrapping_add(1);
        self.lacing.clear();
        self.body.clear();
        self.granule = None;
        self.continued = false;
        self.packets = 0;
    }
}

/// A comment header body: vendor string and `KEY=value` comments, little-endian lengths.
fn comments(magic: &[u8], vendor: &str, tags: &[(&str, &str)]) -> Vec<u8> {
    let mut v = magic.to_vec();
    let len = |n: usize| u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes();
    v.extend(len(vendor.len()));
    v.extend_from_slice(vendor.as_bytes());
    let tags: Vec<String> = tags.iter().filter(|(k, _)| valid_key(k)).map(|(k, val)| format!("{k}={val}")).collect();
    v.extend(len(tags.len()));
    for t in &tags {
        v.extend(len(t.len()));
        v.extend_from_slice(t.as_bytes());
    }
    v
}

/// A comment field name: printable ASCII 0x20–0x7D other than `=` (Vorbis I §5.2.1), not empty.
fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|b| (0x20..=0x7D).contains(&b) && b != b'=')
}

/// The `OpusTags` packet (RFC 7845 §5.2). Comments whose name is not a valid field name are
/// left out.
pub fn opus_tags(vendor: &str, tags: &[(&str, &str)]) -> Vec<u8> {
    comments(b"OpusTags", vendor, tags)
}

/// The Vorbis comment header packet (Vorbis I §5: type 3, `vorbis`, comments, framing bit).
pub fn vorbis_comment(vendor: &str, tags: &[(&str, &str)]) -> Vec<u8> {
    let mut v = comments(b"\x03vorbis", vendor, tags);
    v.push(1);
    v
}

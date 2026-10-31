//! PES packet headers (H.222.0 §2.4.3.6; ISO/IEC 11172-1 §2.4.3.3 for MPEG-1 packets).

/// A parsed PES header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PesHeader {
    /// Bytes from the packet start code to the first payload byte.
    pub header_len: usize,
    /// Raw 33-bit timestamps.
    pub pts: Option<i64>,
    pub dts: Option<i64>,
}

/// Stream ids whose packets have no optional header (§2.4.3.7).
pub(crate) fn plain_stream(id: u8) -> bool {
    matches!(id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF)
}

pub(crate) fn timestamp(b: &[u8]) -> i64 {
    (((b[0] >> 1) & 7) as i64) << 30 | (b[1] as i64) << 22 | ((b[2] >> 1) as i64) << 15 | (b[3] as i64) << 7 | (b[4] >> 1) as i64
}

/// Parse a PES header at the start of `b` (which begins with `00 00 01 stream_id`). `None` when
/// `b` is too short to hold the whole header or it is malformed.
pub(crate) fn parse(b: &[u8]) -> Option<PesHeader> {
    if b.len() < 6 || b[0] != 0 || b[1] != 0 || b[2] != 1 {
        return None;
    }
    if plain_stream(b[3]) {
        return Some(PesHeader { header_len: 6, pts: None, dts: None });
    }
    if b.len() >= 9 && b[6] & 0xC0 == 0x80 {
        // ISO/IEC 13818-1 syntax
        let flags = b[7];
        let header_len = 9 + b[8] as usize;
        if b.len() < header_len {
            return None;
        }
        let (mut pts, mut dts) = (None, None);
        if flags & 0x80 != 0 && header_len >= 14 {
            pts = Some(timestamp(&b[9..14]));
            if flags & 0x40 != 0 && header_len >= 19 {
                dts = Some(timestamp(&b[14..19]));
            }
        }
        return Some(PesHeader { header_len, pts, dts });
    }
    // ISO/IEC 11172-1 syntax: stuffing, STD buffer, timestamps
    let mut p = 6;
    let mut stuffing = 0;
    while b.get(p) == Some(&0xFF) {
        p += 1;
        stuffing += 1;
        if stuffing > 16 {
            return None;
        }
    }
    let c = *b.get(p)?;
    if c >> 6 == 1 {
        p += 2;
    }
    let c = *b.get(p)?;
    let (mut pts, mut dts) = (None, None);
    match c >> 4 {
        2 => {
            pts = Some(timestamp(b.get(p..p + 5)?));
            p += 5;
        }
        3 => {
            pts = Some(timestamp(b.get(p..p + 5)?));
            dts = Some(timestamp(b.get(p + 5..p + 10)?));
            p += 10;
        }
        0 if c == 0x0F => p += 1,
        _ => return None,
    }
    Some(PesHeader { header_len: p, pts, dts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_bytes(prefix: u8, v: i64) -> [u8; 5] {
        [(prefix << 4) | (((v >> 30) & 7) as u8) << 1 | 1, (v >> 22) as u8, (((v >> 15) & 0x7F) as u8) << 1 | 1, (v >> 7) as u8, ((v & 0x7F) as u8) << 1 | 1]
    }

    #[test]
    fn mpeg2_and_mpeg1_headers() {
        let pts = 0x1_2345_6789i64;
        let dts = 0x0_8765_4321i64;
        let mut b = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0xC0, 10];
        b.extend(ts_bytes(3, pts));
        b.extend(ts_bytes(1, dts));
        b.push(0xAA);
        let h = parse(&b).unwrap();
        assert_eq!((h.header_len, h.pts, h.dts), (19, Some(pts), Some(dts)));
        // MPEG-1: stuffing, STD buffer, PTS only
        let mut b = vec![0, 0, 1, 0xC0, 0, 20, 0xFF, 0xFF, 0x40, 0x20];
        b.extend(ts_bytes(2, 90_000));
        let h = parse(&b).unwrap();
        assert_eq!((h.header_len, h.pts, h.dts), (15, Some(90_000), None));
        assert_eq!(parse(&[0, 0, 1, 0xC0, 0, 1, 0x0F]).unwrap().header_len, 7);
        assert_eq!(parse(&[0, 0, 1, 0xBE, 0, 4]).unwrap().header_len, 6);
        assert!(parse(&[0, 0, 1, 0xE0, 0, 0, 0x80, 0x80]).is_none());
    }
}

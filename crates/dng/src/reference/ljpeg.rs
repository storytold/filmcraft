//! The lossless JPEG decoder of PR #1068, kept unchanged as the reference (tests and benchmarks
//! only).

use crate::{Error, Result};

/// A decoded lossless JPEG image.
#[derive(Debug)]
pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub precision: u8,
    pub predictor: u8,
    /// Samples, interleaved by component, line after line, shifted back up by the point transform.
    pub samples: Vec<u16>,
}

const LOOKUP_BITS: u32 = 9;

/// A Huffman table for decoding (T.81 Annex C and F.2.2.3).
#[derive(Clone)]
struct Huffman {
    /// For codes of up to LOOKUP_BITS bits: (length, value) indexed by the next LOOKUP_BITS bits;
    /// length 0 means "longer code".
    fast: Vec<(u8, u8)>,
    maxcode: [i32; 17],
    valptr: [i32; 17],
    mincode: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(bits: &[u8; 16], values: Vec<u8>) -> Result<Huffman> {
        // C.2: code sizes and codes
        let mut sizes = Vec::new();
        for (l, &n) in bits.iter().enumerate() {
            for _ in 0..n {
                sizes.push(l as u8 + 1);
            }
        }
        if sizes.len() != values.len() || sizes.is_empty() {
            return Err(Error::Malformed("lossless JPEG: Huffman table counts"));
        }
        let mut codes = Vec::with_capacity(sizes.len());
        let mut code: u32 = 0;
        let mut si = sizes[0];
        let mut k = 0;
        while k < sizes.len() {
            while k < sizes.len() && sizes[k] == si {
                codes.push(code);
                code += 1;
                k += 1;
            }
            // the codes of one length must fit in that length
            if code > (1u32 << si) {
                return Err(Error::Malformed("lossless JPEG: Huffman table over-subscribed"));
            }
            code <<= 1;
            si += 1;
        }
        // F.2.2.3 decoder tables
        let mut maxcode = [-1i32; 17];
        let mut valptr = [0i32; 17];
        let mut mincode = [0i32; 17];
        let mut j = 0usize;
        for l in 1..=16usize {
            let n = usize::from(bits[l - 1]);
            if n == 0 {
                maxcode[l] = -1;
            } else {
                valptr[l] = j as i32;
                mincode[l] = codes[j] as i32;
                j += n;
                maxcode[l] = codes[j - 1] as i32;
            }
        }
        let mut fast = vec![(0u8, 0u8); 1 << LOOKUP_BITS];
        for (i, (&c, &s)) in codes.iter().zip(&sizes).enumerate() {
            let s = u32::from(s);
            if s <= LOOKUP_BITS {
                let shift = LOOKUP_BITS - s;
                let base = (c << shift) as usize;
                for f in fast.iter_mut().skip(base).take(1 << shift) {
                    *f = (s as u8, values[i]);
                }
            }
        }
        Ok(Huffman { fast, maxcode, valptr, mincode, values })
    }
}

/// Entropy-coded data reader: removes stuffed zero bytes and stops at markers (feeding zeros).
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    /// The marker that ended the data (after 0xFF), if one was met.
    marker: Option<u8>,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Bits { data, pos, acc: 0, n: 0, marker: None }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let mut byte = 0u8;
            if self.marker.is_none() {
                match self.data.get(self.pos) {
                    Some(&0xFF) => match self.data.get(self.pos + 1) {
                        Some(&0x00) => {
                            byte = 0xFF;
                            self.pos += 2;
                        }
                        Some(&m) => self.marker = Some(m),
                        None => self.marker = Some(0xD9),
                    },
                    Some(&b) => {
                        byte = b;
                        self.pos += 1;
                    }
                    None => self.marker = Some(0xD9),
                }
            }
            self.acc |= u64::from(byte) << (56 - self.n);
            self.n += 8;
        }
    }

    fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        (self.acc >> (64 - k)) as u32
    }

    fn skip(&mut self, k: u32) {
        self.acc <<= k;
        self.n -= k;
    }

    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        let v = self.peek(k);
        self.skip(k);
        v
    }

    /// Decode one Huffman symbol (F.2.2.3).
    fn decode(&mut self, h: &Huffman) -> Result<u8> {
        let p = self.peek(LOOKUP_BITS) as usize;
        if let Some(&(len, v)) = h.fast.get(p)
            && len > 0
        {
            self.skip(u32::from(len));
            return Ok(v);
        }
        let mut code = self.bits(1) as i32;
        for l in 1..=16usize {
            if code <= h.maxcode[l] {
                let i = h.valptr[l] + code - h.mincode[l];
                return h.values.get(i as usize).copied().ok_or(Error::Malformed("lossless JPEG: bad Huffman code"));
            }
            code = (code << 1) | self.bits(1) as i32;
        }
        Err(Error::Malformed("lossless JPEG: bad Huffman code"))
    }

    /// Skip to the byte after the next RSTn marker, starting a new bit stream there.
    fn restart(&mut self) -> Result<()> {
        // the decoder may have looked ahead into fill bits; find the marker in the bytes
        let mut p = self.pos;
        loop {
            match (self.data.get(p), self.data.get(p + 1)) {
                (Some(0xFF), Some(&m)) if (0xD0..=0xD7).contains(&m) => {
                    *self = Bits::new(self.data, p + 2);
                    return Ok(());
                }
                (Some(_), Some(_)) => p += 1,
                _ => return Err(Error::Malformed("lossless JPEG: missing restart marker")),
            }
        }
    }
}

struct Component {
    id: u8,
    table: usize,
}

/// Decode a lossless JPEG stream. `max_samples` caps the picture (width × height × components).
pub fn decode(data: &[u8], max_samples: usize) -> Result<Decoded> {
    if data.get(0..2) != Some(&[0xFF, 0xD8]) {
        return Err(Error::Malformed("lossless JPEG: no SOI marker"));
    }
    let mut pos = 2usize;
    let mut tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut frame: Option<(u8, usize, usize, Vec<Component>)> = None;
    let mut restart_interval = 0usize;
    loop {
        // markers may be preceded by fill bytes 0xFF
        while data.get(pos) == Some(&0xFF) && data.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        let (Some(&0xFF), Some(&m)) = (data.get(pos), data.get(pos + 1)) else {
            return Err(Error::Malformed("lossless JPEG: expected a marker"));
        };
        pos += 2;
        if m == 0xD9 {
            return Err(Error::Malformed("lossless JPEG: no scan"));
        }
        if (0xD0..=0xD8).contains(&m) || m == 0x01 {
            continue; // markers without a segment
        }
        let len_b = data.get(pos..pos + 2).ok_or(Error::Truncated)?;
        let len = usize::from(u16::from_be_bytes([len_b[0], len_b[1]]));
        if len < 2 {
            return Err(Error::Malformed("lossless JPEG: segment length"));
        }
        let seg = data.get(pos + 2..pos + len).ok_or(Error::Truncated)?;
        match m {
            0xC3 => {
                let [p, y0, y1, x0, x1, nf, rest @ ..] = seg else {
                    return Err(Error::Malformed("lossless JPEG: short frame header"));
                };
                let (precision, nf) = (*p, usize::from(*nf));
                let height = usize::from(u16::from_be_bytes([*y0, *y1]));
                let width = usize::from(u16::from_be_bytes([*x0, *x1]));
                if !(2..=16).contains(&precision) {
                    return Err(Error::Unsupported(format!("lossless JPEG precision {precision}")));
                }
                if height == 0 {
                    return Err(Error::Unsupported("lossless JPEG with the height in a DNL marker".into()));
                }
                if width == 0 || !(1..=4).contains(&nf) || rest.len() < nf * 3 {
                    return Err(Error::Malformed("lossless JPEG: frame header"));
                }
                let mut comps = Vec::with_capacity(nf);
                for c in rest.as_chunks::<3>().0.iter().take(nf) {
                    if c[1] != 0x11 {
                        return Err(Error::Unsupported("subsampled lossless JPEG components".into()));
                    }
                    comps.push(Component { id: c[0], table: 0 });
                }
                frame = Some((precision, width, height, comps));
            }
            0xC4 => {
                let mut s = seg;
                while !s.is_empty() {
                    let [tcth, counts @ ..] = s else { break };
                    let bits: [u8; 16] = counts.get(..16).and_then(|b| b.try_into().ok()).ok_or(Error::Truncated)?;
                    let n: usize = bits.iter().map(|&b| usize::from(b)).sum();
                    let values = counts.get(16..16 + n).ok_or(Error::Truncated)?.to_vec();
                    let th = usize::from(tcth & 0x0F);
                    if tcth >> 4 != 0 || th > 3 {
                        return Err(Error::Malformed("lossless JPEG: Huffman table class or id"));
                    }
                    if values.iter().any(|&v| v > 16) {
                        return Err(Error::Malformed("lossless JPEG: difference category above 16"));
                    }
                    tables[th] = Some(Huffman::new(&bits, values)?);
                    s = counts.get(16 + n..).unwrap_or(&[]);
                }
            }
            0xDD => {
                let r = seg.get(0..2).ok_or(Error::Truncated)?;
                restart_interval = usize::from(u16::from_be_bytes([r[0], r[1]]));
            }
            0xDA => {
                let Some((precision, width, height, mut comps)) = frame.take() else {
                    return Err(Error::Malformed("lossless JPEG: scan before the frame header"));
                };
                let ns = usize::from(*seg.first().ok_or(Error::Truncated)?);
                if ns != comps.len() {
                    return Err(Error::Unsupported("lossless JPEG with more than one scan".into()));
                }
                for k in 0..ns {
                    let c = seg.get(1 + 2 * k..3 + 2 * k).ok_or(Error::Truncated)?;
                    let comp = comps.iter_mut().find(|x| x.id == c[0]).ok_or(Error::Malformed("lossless JPEG: unknown scan component"))?;
                    comp.table = usize::from(c[1] >> 4);
                    if comp.table > 3 {
                        return Err(Error::Malformed("lossless JPEG: Huffman table id"));
                    }
                }
                let tail = seg.get(1 + 2 * ns..4 + 2 * ns).ok_or(Error::Truncated)?;
                let (predictor, pt) = (tail[0], tail[2] & 0x0F);
                if !(1..=7).contains(&predictor) {
                    return Err(Error::Unsupported(format!("lossless JPEG predictor {predictor}")));
                }
                if pt >= precision {
                    return Err(Error::Malformed("lossless JPEG: point transform"));
                }
                let n = width.checked_mul(height).and_then(|v| v.checked_mul(comps.len())).ok_or(Error::TooLarge)?;
                if n > max_samples {
                    return Err(Error::TooLarge);
                }
                let mut hufs = Vec::with_capacity(comps.len());
                for c in &comps {
                    hufs.push(tables.get(c.table).and_then(|t| t.as_ref()).ok_or(Error::Malformed("lossless JPEG: missing Huffman table"))?);
                }
                let samples = scan(data, pos + len, &hufs, width, height, precision, predictor, pt, restart_interval)?;
                return Ok(Decoded { width, height, components: comps.len(), precision, predictor, samples });
            }
            0xC0..=0xC2 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(Error::Unsupported("lossy (DCT) JPEG in a DNG".into()));
            }
            _ => {} // APPn, COM, DQT … are skipped
        }
        pos += len;
    }
}

/// Decode the entropy-coded data of one scan (H.1.2).
fn scan(data: &[u8], start: usize, hufs: &[&Huffman], width: usize, height: usize, precision: u8, predictor: u8, pt: u8, restart: usize) -> Result<Vec<u16>> {
    let nc = hufs.len();
    let row_len = width * nc;
    if restart != 0 && !restart.is_multiple_of(width) {
        return Err(Error::Unsupported("lossless JPEG restart interval that splits a line".into()));
    }
    let lines_per_interval = if restart == 0 { usize::MAX } else { restart / width };
    let mut out = vec![0u16; row_len * height];
    let mut bits = Bits::new(data, start);
    let initial = 1i32 << (precision - pt - 1);
    let mut line_in_interval = 0usize;
    for y in 0..height {
        if line_in_interval == lines_per_interval {
            bits.restart()?;
            line_in_interval = 0;
        }
        let first_line = line_in_interval == 0;
        let (above, cur) = if y == 0 {
            (None, &mut out[..row_len])
        } else {
            let (a, c) = out.split_at_mut(y * row_len);
            (Some(&a[(y - 1) * row_len..]), &mut c[..row_len])
        };
        for x in 0..width {
            for (c, h) in hufs.iter().enumerate() {
                let i = x * nc + c;
                let ssss = u32::from(bits.decode(h)?);
                let diff = match ssss {
                    0 => 0,
                    16 => 32768,
                    s => {
                        let v = bits.bits(s) as i32;
                        if v < (1 << (s - 1)) { v - (1 << s) + 1 } else { v }
                    }
                };
                let px = match above {
                    _ if first_line && x == 0 => initial,
                    _ if first_line => i32::from(cur[i - nc]),
                    Some(a) if x == 0 => i32::from(a[i]),
                    Some(a) => predict(predictor, i32::from(cur[i - nc]), i32::from(a[i]), i32::from(a[i - nc])),
                    None => initial,
                };
                cur[i] = ((px + diff) & 0xFFFF) as u16;
            }
        }
        line_in_interval += 1;
    }
    if pt > 0 {
        for s in &mut out {
            *s <<= pt;
        }
    }
    Ok(out)
}

/// Table H.1 predictors (`ra` left, `rb` above, `rc` above left).
pub fn predict(p: u8, ra: i32, rb: i32, rc: i32) -> i32 {
    match p {
        1 => ra,
        2 => rb,
        3 => rc,
        4 => ra + rb - rc,
        5 => ra + ((rb - rc) >> 1),
        6 => rb + ((ra - rc) >> 1),
        _ => (ra + rb) >> 1,
    }
}

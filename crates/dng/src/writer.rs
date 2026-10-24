//! A small DNG writer for synthesising test files (feature `writer`; not used by the app): a
//! TIFF container with the raw image in IFD 0 or in a SubIFD behind an 8-bit preview, strips or
//! tiles, uncompressed samples of any depth or lossless JPEG (T.81 process 14) with any predictor,
//! and whatever extra tags the caller adds.

use crate::ljpeg::predict;
use crate::tiff::tags;

/// A tag value.
#[derive(Clone, Debug)]
pub enum Value {
    Byte(Vec<u8>),
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<(u32, u32)>),
    SRational(Vec<(i32, i32)>),
    Undefined(Vec<u8>),
}

impl Value {
    fn code(&self) -> u16 {
        match self {
            Value::Byte(_) => 1,
            Value::Ascii(_) => 2,
            Value::Short(_) => 3,
            Value::Long(_) => 4,
            Value::Rational(_) => 5,
            Value::Undefined(_) => 7,
            Value::SRational(_) => 10,
        }
    }

    fn count(&self) -> usize {
        match self {
            Value::Byte(v) | Value::Undefined(v) => v.len(),
            Value::Ascii(s) => s.len() + 1,
            Value::Short(v) => v.len(),
            Value::Long(v) => v.len(),
            Value::Rational(v) => v.len(),
            Value::SRational(v) => v.len(),
        }
    }

    fn bytes(&self, be: bool) -> Vec<u8> {
        let w16 = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        let w32 = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        match self {
            Value::Byte(v) | Value::Undefined(v) => v.clone(),
            Value::Ascii(s) => s.bytes().chain([0]).collect(),
            Value::Short(v) => v.iter().flat_map(|&x| w16(x)).collect(),
            Value::Long(v) => v.iter().flat_map(|&x| w32(x)).collect(),
            Value::Rational(v) => v.iter().flat_map(|&(n, d)| w32(n).into_iter().chain(w32(d))).collect(),
            Value::SRational(v) => v.iter().flat_map(|&(n, d)| w32(n as u32).into_iter().chain(w32(d as u32))).collect(),
        }
    }

    /// Signed rationals with denominator 10000 from floats (colour matrices and the like).
    pub fn srationals(v: &[f64]) -> Value {
        Value::SRational(v.iter().map(|&x| ((x * 10_000.0).round() as i32, 10_000)).collect())
    }

    /// Unsigned rationals with denominator 10000.
    pub fn rationals(v: &[f64]) -> Value {
        Value::Rational(v.iter().map(|&x| ((x * 10_000.0).round().max(0.0) as u32, 10_000)).collect())
    }
}

/// How the raw data is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    /// Lossless JPEG: `predictor` 1–7; each tile / strip is coded as a JPEG of `components`
    /// interleaved components (its width × samples per pixel divided by `components`), with a
    /// restart marker every `restart_lines` lines (0: none) and point transform `pt`.
    LosslessJpeg {
        predictor: u8,
        components: usize,
        restart_lines: usize,
        pt: u8,
    },
    /// Baseline (lossy) JPEG at `quality` 1–100 (8-bit samples; one sample per pixel as a
    /// greyscale JPEG, three as a colour one, which the encoder stores as YCbCr).
    LossyJpeg {
        quality: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Strips { rows: usize },
    Tiles { width: usize, height: usize },
}

/// What to write.
#[derive(Clone, Debug)]
pub struct Options {
    pub width: usize,
    pub height: usize,
    pub samples_per_pixel: usize,
    pub bits: u32,
    pub big_endian: bool,
    pub compression: Compression,
    pub layout: Layout,
    /// CFA repeat (rows, columns) and pattern colour codes; `None` writes LinearRaw.
    pub cfa: Option<(usize, usize, Vec<u8>)>,
    /// Put an 8-bit preview in IFD 0 and the raw image in a SubIFD (as cameras do).
    pub preview: bool,
    /// Extra tags of IFD 0 (colour, CinemaDNG…).
    pub main_tags: Vec<(u16, Value)>,
    /// Extra tags of the raw IFD (black and white levels, crop…).
    pub raw_tags: Vec<(u16, Value)>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            width: 8,
            height: 8,
            samples_per_pixel: 1,
            bits: 16,
            big_endian: false,
            compression: Compression::None,
            layout: Layout::Strips { rows: 8 },
            cfa: Some((2, 2, vec![0, 1, 1, 2])),
            preview: false,
            main_tags: Vec::new(),
            raw_tags: Vec::new(),
        }
    }
}

/// Pack `rows` rows of `per_row` samples of `bits` bits (8/16-bit in byte order `be`, other
/// depths most significant bit first, each row starting on a byte boundary).
pub fn pack(samples: &[u16], per_row: usize, bits: u32, be: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for row in samples.chunks(per_row.max(1)) {
        match bits {
            8 => out.extend(row.iter().map(|&v| v as u8)),
            16 => out.extend(row.iter().flat_map(|&v| if be { v.to_be_bytes() } else { v.to_le_bytes() })),
            _ => {
                let (mut acc, mut n) = (0u32, 0u32);
                for &v in row {
                    acc = (acc << bits) | (u32::from(v) & ((1 << bits) - 1));
                    n += bits;
                    while n >= 8 {
                        n -= 8;
                        out.push((acc >> n) as u8);
                    }
                    acc &= (1 << n) - 1;
                }
                if n > 0 {
                    out.push((acc << (8 - n)) as u8);
                }
            }
        }
    }
    out
}

/// Entropy-coded bit output with 0xFF stuffing.
struct BitOut {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitOut {
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    /// Pad the last byte with 1 bits.
    fn flush(&mut self) {
        while self.n != 0 {
            self.put(1, 1);
        }
    }
}

/// Encode `samples` (`width` × `height` × `comps`, interleaved) as lossless JPEG.
pub fn lj92(samples: &[u16], width: usize, height: usize, comps: usize, precision: u8, predictor: u8, restart_lines: usize, pt: u8) -> Vec<u8> {
    let comps = comps.clamp(1, 4);
    let row = width * comps;
    let initial = 1i32 << (precision - pt - 1);
    // differences, line by line
    let mut cats: Vec<Vec<u32>> = vec![vec![0; 17]; comps];
    let mut coded: Vec<(usize, u32, u32, i32)> = Vec::with_capacity(samples.len()); // (comp, ssss, extra, line)
    let s = |i: usize| i32::from(samples.get(i).copied().unwrap_or(0) >> pt);
    for y in 0..height {
        let first_line = restart_lines == 0 && y == 0 || restart_lines != 0 && y % restart_lines == 0;
        for x in 0..width {
            for c in 0..comps {
                let i = y * row + x * comps + c;
                let px = if first_line && x == 0 {
                    initial
                } else if first_line {
                    s(i - comps)
                } else if x == 0 {
                    s(i - row)
                } else {
                    predict(predictor, s(i - comps), s(i - row), s(i - row - comps))
                };
                let d16 = (s(i) - px).rem_euclid(65_536);
                let (ssss, extra) = if d16 == 32_768 {
                    (16, 0)
                } else {
                    let d = if d16 >= 32_768 { d16 - 65_536 } else { d16 };
                    let ssss = 32 - d.unsigned_abs().leading_zeros();
                    let extra = if d >= 0 { d as u32 } else { (d + (1 << ssss) - 1) as u32 };
                    (ssss, extra)
                };
                cats[c][ssss as usize] += 1;
                coded.push((c, ssss, extra, y as i32));
            }
        }
    }
    // per component: the used categories by frequency get 2-bit then 6-bit codes
    let mut tables = Vec::new();
    for counts in &cats {
        let mut syms: Vec<u8> = (0..17u8).filter(|&k| counts[usize::from(k)] > 0).collect();
        syms.sort_by_key(|&k| std::cmp::Reverse(counts[usize::from(k)]));
        if syms.is_empty() {
            syms.push(0);
        }
        let mut code_of = [(0u32, 0u32); 17];
        for (i, &k) in syms.iter().enumerate() {
            code_of[usize::from(k)] = if i < 3 { (i as u32, 2) } else { (0b110000 + (i as u32 - 3), 6) };
        }
        let mut bits = [0u8; 16];
        bits[1] = syms.len().min(3) as u8;
        bits[5] = syms.len().saturating_sub(3) as u8;
        tables.push((bits, syms, code_of));
    }
    let mut out = vec![0xFF, 0xD8];
    // SOF3
    let mut sof = vec![precision, (height >> 8) as u8, height as u8, (width >> 8) as u8, width as u8, comps as u8];
    for c in 0..comps {
        sof.extend([c as u8, 0x11, 0]);
    }
    segment(&mut out, 0xC3, &sof);
    let mut dht = Vec::new();
    for (c, (bits, syms, _)) in tables.iter().enumerate() {
        dht.push(c as u8);
        dht.extend(bits);
        dht.extend(syms);
    }
    segment(&mut out, 0xC4, &dht);
    if restart_lines != 0 {
        let ri = (restart_lines * width) as u16;
        segment(&mut out, 0xDD, &ri.to_be_bytes());
    }
    let mut sos = vec![comps as u8];
    for c in 0..comps {
        sos.extend([c as u8, (c as u8) << 4]);
    }
    sos.extend([predictor, 0, pt]);
    segment(&mut out, 0xDA, &sos);
    let mut bo = BitOut { out, acc: 0, n: 0 };
    let mut line = 0;
    let mut rst = 0u8;
    for (c, ssss, extra, y) in coded {
        if restart_lines != 0 && y != line && (y as usize).is_multiple_of(restart_lines) {
            bo.flush();
            bo.out.extend([0xFF, 0xD0 + rst]);
            rst = (rst + 1) % 8;
        }
        line = y;
        let (code, len) = tables[c].2[ssss as usize];
        bo.put(code, len);
        if ssss != 16 {
            bo.put(extra, ssss);
        }
    }
    bo.flush();
    let mut out = bo.out;
    out.extend([0xFF, 0xD9]);
    out
}

/// Encode `w` × `h` × `spp` 8-bit samples (1 or 3 per pixel) as a baseline JPEG (empty if the
/// encoder refuses them).
pub fn lossy_jpeg(samples: &[u16], w: usize, h: usize, spp: usize, quality: u8) -> Vec<u8> {
    use image::ImageEncoder;
    let bytes: Vec<u8> = samples.iter().map(|&v| v.min(255) as u8).collect();
    let color = if spp == 3 { image::ExtendedColorType::Rgb8 } else { image::ExtendedColorType::L8 };
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    if enc.write_image(&bytes, w as u32, h as u32, color).is_err() {
        return Vec::new();
    }
    out
}

fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend([0xFF, marker]);
    out.extend(((body.len() + 2) as u16).to_be_bytes());
    out.extend(body);
}

/// Append an IFD at the end of `buf` (word aligned) and return its offset.
fn put_ifd(buf: &mut Vec<u8>, mut entries: Vec<(u16, Value)>, be: bool) -> u32 {
    if buf.len() % 2 == 1 {
        buf.push(0);
    }
    entries.sort_by_key(|e| e.0);
    let at = buf.len();
    let n = entries.len();
    let w16 = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let w32 = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let mut ext_at = at + 2 + 12 * n + 4;
    let mut head = Vec::new();
    let mut ext = Vec::new();
    head.extend(w16(n as u16));
    for (tag, v) in &entries {
        let b = v.bytes(be);
        head.extend(w16(*tag));
        head.extend(w16(v.code()));
        head.extend(w32(v.count() as u32));
        if b.len() <= 4 {
            let mut inline = b.clone();
            inline.resize(4, 0);
            head.extend(inline);
        } else {
            head.extend(w32(ext_at as u32));
            ext.extend(&b);
            if b.len() % 2 == 1 {
                ext.push(0);
            }
            ext_at += b.len().div_ceil(2) * 2;
        }
    }
    head.extend(w32(0));
    buf.extend(head);
    buf.extend(ext);
    at as u32
}

/// Encode the strips or tiles of `samples`, appending them to `buf`; returns (offsets, counts).
fn put_chunks(buf: &mut Vec<u8>, samples: &[u16], o: &Options) -> (Vec<u32>, Vec<u32>) {
    let spp = o.samples_per_pixel;
    let (cw, ch) = match o.layout {
        Layout::Strips { rows } => (o.width, rows.max(1)),
        Layout::Tiles { width, height } => (width.max(1), height.max(1)),
    };
    let tiles = matches!(o.layout, Layout::Tiles { .. });
    let (mut offs, mut counts) = (Vec::new(), Vec::new());
    for ty in (0..o.height).step_by(ch) {
        for tx in (0..o.width).step_by(cw) {
            let rows = if tiles { ch } else { ch.min(o.height - ty) };
            // the chunk's samples; tiles past the image edge repeat the edge pixel
            let mut chunk = Vec::with_capacity(cw * rows * spp);
            for r in 0..rows {
                let y = (ty + r).min(o.height - 1);
                for c in 0..cw {
                    let x = (tx + c).min(o.width - 1);
                    for s in 0..spp {
                        chunk.push(samples.get((y * o.width + x) * spp + s).copied().unwrap_or(0));
                    }
                }
            }
            let data = match o.compression {
                Compression::None => pack(&chunk, cw * spp, o.bits, o.big_endian),
                Compression::LosslessJpeg { predictor, components, restart_lines, pt } => {
                    let comps = components.max(1);
                    lj92(&chunk, cw * spp / comps, rows, comps, o.bits as u8, predictor, restart_lines, pt)
                }
                Compression::LossyJpeg { quality } => lossy_jpeg(&chunk, cw, rows, spp, quality),
            };
            if buf.len() % 2 == 1 {
                buf.push(0);
            }
            offs.push(buf.len() as u32);
            counts.push(data.len() as u32);
            buf.extend(data);
        }
    }
    (offs, counts)
}

/// Write a DNG holding `samples` (`width` × `height` × `samples_per_pixel`, interleaved).
pub fn write(samples: &[u16], o: &Options) -> Vec<u8> {
    let be = o.big_endian;
    let mut buf = if be { b"MM\0*\0\0\0\0".to_vec() } else { b"II*\0\0\0\0\0".to_vec() };
    let (offs, counts) = put_chunks(&mut buf, samples, o);
    let mut raw = vec![
        (tags::NEW_SUBFILE_TYPE, Value::Long(vec![0])),
        (tags::IMAGE_WIDTH, Value::Long(vec![o.width as u32])),
        (tags::IMAGE_LENGTH, Value::Long(vec![o.height as u32])),
        (tags::BITS_PER_SAMPLE, Value::Short(vec![o.bits as u16; o.samples_per_pixel])),
        (
            tags::COMPRESSION,
            Value::Short(vec![match o.compression {
                Compression::None => 1,
                Compression::LosslessJpeg { .. } => 7,
                Compression::LossyJpeg { .. } => 34892,
            }]),
        ),
        (tags::SAMPLES_PER_PIXEL, Value::Short(vec![o.samples_per_pixel as u16])),
        (tags::PLANAR_CONFIGURATION, Value::Short(vec![1])),
    ];
    match o.layout {
        Layout::Strips { rows } => {
            raw.push((tags::ROWS_PER_STRIP, Value::Long(vec![rows as u32])));
            raw.push((tags::STRIP_OFFSETS, Value::Long(offs)));
            raw.push((tags::STRIP_BYTE_COUNTS, Value::Long(counts)));
        }
        Layout::Tiles { width, height } => {
            raw.push((tags::TILE_WIDTH, Value::Long(vec![width as u32])));
            raw.push((tags::TILE_LENGTH, Value::Long(vec![height as u32])));
            raw.push((tags::TILE_OFFSETS, Value::Long(offs)));
            raw.push((tags::TILE_BYTE_COUNTS, Value::Long(counts)));
        }
    }
    match &o.cfa {
        Some((r, c, p)) => {
            raw.push((tags::PHOTOMETRIC, Value::Short(vec![32803])));
            raw.push((tags::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![*r as u16, *c as u16])));
            raw.push((tags::CFA_PATTERN, Value::Byte(p.clone())));
        }
        None => raw.push((tags::PHOTOMETRIC, Value::Short(vec![34892]))),
    }
    // later entries replace earlier ones with the same tag
    let merge = |base: &mut Vec<(u16, Value)>, extra: &[(u16, Value)]| {
        for (t, v) in extra {
            base.retain(|e| e.0 != *t);
            base.push((*t, v.clone()));
        }
    };
    merge(&mut raw, &o.raw_tags);
    let mut main = vec![(tags::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0])), (tags::UNIQUE_CAMERA_MODEL, Value::Ascii("FilmCraft Synthetic".into()))];
    let ifd0 = if o.preview {
        let raw_at = put_ifd(&mut buf, raw, be);
        // a 2 × 2 RGB preview
        if buf.len() % 2 == 1 {
            buf.push(0);
        }
        let px_at = buf.len() as u32;
        buf.extend([128u8; 12]);
        let mut p = vec![
            (tags::NEW_SUBFILE_TYPE, Value::Long(vec![1])),
            (tags::IMAGE_WIDTH, Value::Long(vec![2])),
            (tags::IMAGE_LENGTH, Value::Long(vec![2])),
            (tags::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8])),
            (tags::COMPRESSION, Value::Short(vec![1])),
            (tags::PHOTOMETRIC, Value::Short(vec![2])),
            (tags::STRIP_OFFSETS, Value::Long(vec![px_at])),
            (tags::SAMPLES_PER_PIXEL, Value::Short(vec![3])),
            (tags::ROWS_PER_STRIP, Value::Long(vec![2])),
            (tags::STRIP_BYTE_COUNTS, Value::Long(vec![12])),
            (tags::SUB_IFDS, Value::Long(vec![raw_at])),
        ];
        p.append(&mut main);
        merge(&mut p, &o.main_tags);
        put_ifd(&mut buf, p, be)
    } else {
        raw.append(&mut main);
        merge(&mut raw, &o.main_tags);
        put_ifd(&mut buf, raw, be)
    };
    let off = if be { ifd0.to_be_bytes() } else { ifd0.to_le_bytes() };
    buf[4..8].copy_from_slice(&off);
    buf
}

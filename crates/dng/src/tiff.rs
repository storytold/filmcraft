//! The TIFF container (TIFF 6.0 §2, as DNG uses it): byte order, image file directories (IFDs),
//! their entries and values, and the SubIFD tree.
//!
//! Every offset and count comes from the file and is checked before use: an entry whose value lies
//! outside the file is an error when it is read, the walk visits at most [`MAX_IFDS`] directories,
//! never the same one twice (cyclic chains end), and goes at most [`MAX_DEPTH`] SubIFD levels deep.

use std::collections::HashSet;

use crate::{Error, Result};

/// Most IFDs one file may have (main chain and SubIFDs together).
pub const MAX_IFDS: usize = 64;
/// Deepest SubIFD nesting followed.
pub const MAX_DEPTH: usize = 4;
/// Invalid SubIFD edges count too: otherwise a file can make failed offset probes quadratic.
const MAX_IFD_ATTEMPTS: usize = MAX_IFDS * 16;

/// TIFF field types (TIFF 6.0 §2, plus type 13 IFD from TIFF Technical Note 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Byte,
    Ascii,
    Short,
    Long,
    Rational,
    SByte,
    Undefined,
    SShort,
    SLong,
    SRational,
    Float,
    Double,
    Ifd,
}

impl Type {
    fn from_code(c: u16) -> Option<Type> {
        Some(match c {
            1 => Type::Byte,
            2 => Type::Ascii,
            3 => Type::Short,
            4 => Type::Long,
            5 => Type::Rational,
            6 => Type::SByte,
            7 => Type::Undefined,
            8 => Type::SShort,
            9 => Type::SLong,
            10 => Type::SRational,
            11 => Type::Float,
            12 => Type::Double,
            13 => Type::Ifd,
            _ => return None,
        })
    }

    /// Bytes per value.
    pub fn size(self) -> usize {
        match self {
            Type::Byte | Type::Ascii | Type::SByte | Type::Undefined => 1,
            Type::Short | Type::SShort => 2,
            Type::Long | Type::SLong | Type::Float | Type::Ifd => 4,
            Type::Rational | Type::SRational | Type::Double => 8,
        }
    }
}

/// One directory entry.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub tag: u16,
    pub ty: Type,
    pub count: u32,
    /// Where the value bytes start: inside the entry when they fit in four bytes, else the offset
    /// the entry holds (not yet checked against the file length).
    value_at: u64,
}

/// One image file directory.
#[derive(Clone, Debug, Default)]
pub struct Ifd {
    /// File offset of the directory.
    pub offset: u64,
    pub entries: Vec<Entry>,
    /// Nesting: 0 for the main chain, 1 for its SubIFDs, …
    pub depth: usize,
}

impl Ifd {
    pub fn get(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }
}

/// A TIFF file in memory.
#[derive(Clone, Copy)]
pub struct Tiff<'a> {
    pub data: &'a [u8],
    pub big_endian: bool,
}

impl<'a> Tiff<'a> {
    /// Read the header. Classic TIFF only (BigTIFF, version 43, is not used by DNG).
    pub fn new(data: &'a [u8]) -> Result<(Tiff<'a>, u64)> {
        let big_endian = match data.get(0..4) {
            Some(b"II*\0") => false,
            Some(b"MM\0*") => true,
            _ => return Err(Error::NotDng),
        };
        let t = Tiff { data, big_endian };
        let first = u64::from(t.u32_at(4)?);
        Ok((t, first))
    }

    fn bytes_at(&self, off: u64, len: usize) -> Result<&'a [u8]> {
        let start = usize::try_from(off).map_err(|_| Error::Malformed("offset outside the file"))?;
        let end = start.checked_add(len).ok_or(Error::Malformed("offset outside the file"))?;
        self.data.get(start..end).ok_or(Error::Malformed("offset outside the file"))
    }

    pub fn u16_of(&self, b: [u8; 2]) -> u16 {
        if self.big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
    }

    pub fn u32_of(&self, b: [u8; 4]) -> u32 {
        if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    }

    fn u16_at(&self, off: u64) -> Result<u16> {
        let b = self.bytes_at(off, 2)?;
        Ok(self.u16_of([b[0], b[1]]))
    }

    fn u32_at(&self, off: u64) -> Result<u32> {
        let b = self.bytes_at(off, 4)?;
        Ok(self.u32_of([b[0], b[1], b[2], b[3]]))
    }

    /// Read the IFD at `off`: its entries and the offset of the next IFD (0 at the end of a chain).
    pub fn read_ifd(&self, off: u64, depth: usize) -> Result<(Ifd, u64)> {
        let n = self.u16_at(off)?;
        let mut entries = Vec::with_capacity(usize::from(n));
        for i in 0..u64::from(n) {
            let at = off + 2 + i * 12;
            let e = self.bytes_at(at, 12)?;
            let tag = self.u16_of([e[0], e[1]]);
            let code = self.u16_of([e[2], e[3]]);
            let count = self.u32_of([e[4], e[5], e[6], e[7]]);
            // unknown field types are skipped (TIFF 6.0: readers ignore them)
            let Some(ty) = Type::from_code(code) else { continue };
            let len = u64::from(count).saturating_mul(ty.size() as u64);
            let value_at = if len <= 4 { at + 8 } else { u64::from(self.u32_of([e[8], e[9], e[10], e[11]])) };
            entries.push(Entry { tag, ty, count, value_at });
        }
        let next = u64::from(self.u32_at(off + 2 + u64::from(n) * 12)?);
        Ok((Ifd { offset: off, entries, depth }, next))
    }

    /// Every IFD of the file: the main chain from `first` and, depth first, the SubIFDs (tag 330) of
    /// each. Bounded by [`MAX_IFDS`] and [`MAX_DEPTH`]; a directory reached twice ends that branch.
    pub fn all_ifds(&self, first: u64) -> Result<Vec<Ifd>> {
        let mut out: Vec<Ifd> = Vec::new();
        let mut seen = HashSet::new();
        let mut attempts = 0usize;
        let mut stack: Vec<(u64, usize)> = vec![(first, 0)];
        while let Some((off, depth)) = stack.pop() {
            let mut next = off;
            while next != 0 {
                if !seen.insert(next) {
                    break;
                }
                attempts += 1;
                if attempts > MAX_IFD_ATTEMPTS {
                    return Err(Error::Malformed("too many IFD offsets"));
                }
                if out.len() >= MAX_IFDS {
                    return Err(Error::Malformed("too many IFDs"));
                }
                let (ifd, after) = match self.read_ifd(next, depth) {
                    Ok(x) => x,
                    // a broken directory ends the chain; the first one must be readable
                    Err(e) if out.is_empty() => return Err(e),
                    Err(_) => break,
                };
                if depth < MAX_DEPTH
                    && let Some(sub) = ifd.get(tags::SUB_IFDS)
                    && let Ok(offs) = self.uints(sub)
                {
                    for o in offs.into_iter().rev() {
                        stack.push((o, depth + 1));
                    }
                }
                out.push(ifd);
                next = after;
            }
        }
        Ok(out)
    }

    /// The value bytes of an entry.
    pub fn value_bytes(&self, e: &Entry) -> Result<&'a [u8]> {
        let len = (e.count as usize).checked_mul(e.ty.size()).ok_or(Error::Malformed("entry too large"))?;
        self.bytes_at(e.value_at, len)
    }

    /// The `i`-th value of a numeric entry as f64 (rationals divided; a zero denominator is an
    /// error).
    fn number(&self, e: &Entry, b: &[u8], i: usize) -> Result<f64> {
        let s = e.ty.size();
        let v = b.get(i * s..i * s + s).ok_or(Error::Malformed("entry too short"))?;
        let u16v = |o: usize| self.u16_of([v[o], v[o + 1]]);
        let u32v = |o: usize| self.u32_of([v[o], v[o + 1], v[o + 2], v[o + 3]]);
        Ok(match e.ty {
            Type::Byte | Type::Undefined => f64::from(v[0]),
            Type::SByte => f64::from(v[0] as i8),
            Type::Short => f64::from(u16v(0)),
            Type::SShort => f64::from(u16v(0) as i16),
            Type::Long | Type::Ifd => f64::from(u32v(0)),
            Type::SLong => f64::from(u32v(0) as i32),
            Type::Rational => {
                let d = u32v(4);
                if d == 0 {
                    return Err(Error::Malformed("rational with a zero denominator"));
                }
                f64::from(u32v(0)) / f64::from(d)
            }
            Type::SRational => {
                let d = u32v(4) as i32;
                if d == 0 {
                    return Err(Error::Malformed("rational with a zero denominator"));
                }
                f64::from(u32v(0) as i32) / f64::from(d)
            }
            Type::Float => f64::from(f32::from_bits(u32v(0))),
            Type::Double => {
                let mut a = [0u8; 8];
                a.copy_from_slice(&v[..8]);
                if self.big_endian { f64::from_be_bytes(a) } else { f64::from_le_bytes(a) }
            }
            Type::Ascii => return Err(Error::Malformed("text where a number belongs")),
        })
    }

    /// All values of a numeric entry as f64. Non-finite values are an error.
    pub fn floats(&self, e: &Entry) -> Result<Vec<f64>> {
        let b = self.value_bytes(e)?;
        let mut out = Vec::with_capacity(e.count as usize);
        for i in 0..e.count as usize {
            let v = self.number(e, b, i)?;
            if !v.is_finite() {
                return Err(Error::Malformed("non-finite number"));
            }
            out.push(v);
        }
        Ok(out)
    }

    /// All values of an unsigned integer entry (BYTE, SHORT, LONG, IFD).
    pub fn uints(&self, e: &Entry) -> Result<Vec<u64>> {
        if !matches!(e.ty, Type::Byte | Type::Undefined | Type::Short | Type::Long | Type::Ifd) {
            return Err(Error::Malformed("expected an unsigned integer field"));
        }
        let b = self.value_bytes(e)?;
        let s = e.ty.size();
        Ok(b.chunks_exact(s)
            .map(|v| match s {
                1 => u64::from(v[0]),
                2 => u64::from(self.u16_of([v[0], v[1]])),
                _ => u64::from(self.u32_of([v[0], v[1], v[2], v[3]])),
            })
            .collect())
    }

    /// The first value of an unsigned integer entry.
    pub fn uint(&self, e: &Entry) -> Result<u64> {
        self.uints(e)?.first().copied().ok_or(Error::Malformed("empty field"))
    }

    /// An ASCII entry up to its first NUL, lossily decoded.
    pub fn ascii(&self, e: &Entry) -> Result<String> {
        let b = self.value_bytes(e)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).trim().to_string())
    }
}

/// Tag numbers (TIFF 6.0, TIFF/EP, DNG 1.7, CinemaDNG).
pub mod tags {
    pub const NEW_SUBFILE_TYPE: u16 = 254;
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const MAKE: u16 = 271;
    pub const MODEL: u16 = 272;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    pub const PLANAR_CONFIGURATION: u16 = 284;
    pub const PREDICTOR: u16 = 317;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const SUB_IFDS: u16 = 330;
    pub const SAMPLE_FORMAT: u16 = 339;
    pub const CFA_REPEAT_PATTERN_DIM: u16 = 33421;
    pub const CFA_PATTERN: u16 = 33422;
    pub const DNG_VERSION: u16 = 50706;
    pub const UNIQUE_CAMERA_MODEL: u16 = 50708;
    pub const CFA_PLANE_COLOR: u16 = 50710;
    pub const CFA_LAYOUT: u16 = 50711;
    pub const LINEARIZATION_TABLE: u16 = 50712;
    pub const BLACK_LEVEL_REPEAT_DIM: u16 = 50713;
    pub const BLACK_LEVEL: u16 = 50714;
    pub const BLACK_LEVEL_DELTA_H: u16 = 50715;
    pub const BLACK_LEVEL_DELTA_V: u16 = 50716;
    pub const WHITE_LEVEL: u16 = 50717;
    pub const DEFAULT_CROP_ORIGIN: u16 = 50719;
    pub const DEFAULT_CROP_SIZE: u16 = 50720;
    pub const COLOR_MATRIX_1: u16 = 50721;
    pub const COLOR_MATRIX_2: u16 = 50722;
    pub const CAMERA_CALIBRATION_1: u16 = 50723;
    pub const CAMERA_CALIBRATION_2: u16 = 50724;
    pub const ANALOG_BALANCE: u16 = 50727;
    pub const AS_SHOT_NEUTRAL: u16 = 50728;
    pub const AS_SHOT_WHITE_XY: u16 = 50729;
    pub const BASELINE_EXPOSURE: u16 = 50730;
    pub const CALIBRATION_ILLUMINANT_1: u16 = 50778;
    pub const CALIBRATION_ILLUMINANT_2: u16 = 50779;
    pub const ACTIVE_AREA: u16 = 50829;
    pub const OPCODE_LIST_1: u16 = 51008;
    pub const OPCODE_LIST_2: u16 = 51009;
    pub const OPCODE_LIST_3: u16 = 51022;
    pub const FORWARD_MATRIX_1: u16 = 50964;
    pub const FORWARD_MATRIX_2: u16 = 50965;
    /// CinemaDNG: SMPTE ST 12-1 time codes, 8 bytes each.
    pub const TIME_CODES: u16 = 51043;
    /// CinemaDNG: frames per second (SRATIONAL).
    pub const FRAME_RATE: u16 = 51044;
    /// CinemaDNG: the camera's reel name.
    pub const REEL_NAME: u16 = 51081;
}

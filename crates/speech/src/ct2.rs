//! A reader for CTranslate2 model files (`model.bin`), the format of the "faster-whisper"
//! conversions, so a Whisper model the user already has in that form can be used as is.
//!
//! Layout of format version 6, as found in the files: `u32` version, the model spec name and
//! `u32` spec revision, `u32` variable count, then per variable its name, `u8` rank, `u32`
//! dimensions, `u8` data type, `u32` byte count and the raw little-endian data; finally `u32`
//! alias count and (alias, target) name pairs. Names are a `u16` byte length followed by the
//! bytes (NUL-terminated). Data types: 0 f32, 1 i8, 2 i16, 3 i32, 4 f16, 5 bf16.
//!
//! The file is untrusted: counts, ranks, name lengths and sizes are capped and checked against
//! the file length before anything is allocated; tensors are read one at a time.

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::SpeechError;
use crate::safetensors::{Dtype, convert};

const MAX_VARS: u32 = 100_000;
const MAX_RANK: u8 = 8;

/// Element type of a stored variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarType {
    Float(Dtype),
    I8,
    I16,
    I32,
}

impl VarType {
    fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => VarType::Float(Dtype::F32),
            1 => VarType::I8,
            2 => VarType::I16,
            3 => VarType::I32,
            4 => VarType::Float(Dtype::F16),
            5 => VarType::Float(Dtype::Bf16),
            _ => return None,
        })
    }

    fn size(self) -> u64 {
        match self {
            VarType::Float(d) => d.size(),
            VarType::I8 => 1,
            VarType::I16 => 2,
            VarType::I32 => 4,
        }
    }
}

/// One variable of the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub ty: VarType,
    pub shape: Vec<usize>,
    /// Absolute byte offset of the data.
    pub offset: u64,
    pub bytes: u64,
}

fn bad(msg: impl std::fmt::Display) -> SpeechError {
    SpeechError::Model(format!("CTranslate2 model: {msg}"))
}

/// An open CTranslate2 model file.
pub struct Ct2 {
    file: std::fs::File,
    pub spec: String,
    pub vars: BTreeMap<String, Var>,
}

struct Cursor<R> {
    r: R,
    pos: u64,
    len: u64,
}

impl<R: Read + Seek> Cursor<R> {
    fn bytes(&mut self, n: u64) -> Result<Vec<u8>, SpeechError> {
        if n > self.len.saturating_sub(self.pos) {
            return Err(bad("truncated file"));
        }
        let mut b = vec![0u8; n as usize];
        self.r.read_exact(&mut b).map_err(bad)?;
        self.pos += n;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8, SpeechError> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, SpeechError> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, SpeechError> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self) -> Result<String, SpeechError> {
        let n = self.u16()?;
        let b = self.bytes(n as u64)?;
        let b = b.strip_suffix(&[0]).unwrap_or(&b);
        String::from_utf8(b.to_vec()).map_err(|_| bad("name is not UTF-8"))
    }
    fn skip(&mut self, n: u64) -> Result<(), SpeechError> {
        if n > self.len.saturating_sub(self.pos) {
            return Err(bad("truncated file"));
        }
        self.r.seek(SeekFrom::Current(n as i64)).map_err(bad)?;
        self.pos += n;
        Ok(())
    }
}

/// Parse the variable index of a CTranslate2 model held by `r` (`len` bytes).
pub fn parse<R: Read + Seek>(r: R, len: u64) -> Result<(String, BTreeMap<String, Var>), SpeechError> {
    let mut c = Cursor { r, pos: 0, len };
    let version = c.u32()?;
    if version != 6 {
        return Err(bad(format!("unsupported format version {version}")));
    }
    let spec = c.string()?;
    let _revision = c.u32()?;
    let n = c.u32()?;
    if n > MAX_VARS {
        return Err(bad("too many variables"));
    }
    let mut vars = BTreeMap::new();
    for _ in 0..n {
        let name = c.string()?;
        let rank = c.u8()?;
        if rank > MAX_RANK {
            return Err(bad(format!("{name}: rank {rank}")));
        }
        let mut shape = Vec::with_capacity(rank as usize);
        let mut numel: u64 = 1;
        for _ in 0..rank {
            let d = c.u32()?;
            numel = numel.checked_mul(d as u64).ok_or_else(|| bad(format!("{name}: shape overflows")))?;
            shape.push(d as usize);
        }
        let ty = VarType::from_code(c.u8()?).ok_or_else(|| bad(format!("{name}: unknown data type")))?;
        let bytes = c.u32()? as u64;
        if numel.checked_mul(ty.size()) != Some(bytes) {
            return Err(bad(format!("{name}: {bytes} bytes for shape {shape:?}")));
        }
        let offset = c.pos;
        c.skip(bytes)?;
        vars.insert(name, Var { ty, shape, offset, bytes });
    }
    // aliases (e.g. the output projection tied to the embedding)
    if c.pos < c.len {
        let n = c.u32()?;
        if n > MAX_VARS {
            return Err(bad("too many aliases"));
        }
        for _ in 0..n {
            let alias = c.string()?;
            let target = c.string()?;
            if let Some(v) = vars.get(&target).cloned() {
                vars.entry(alias).or_insert(v);
            }
        }
    }
    Ok((spec, vars))
}

impl Ct2 {
    pub fn open(path: &Path) -> Result<Self, SpeechError> {
        let ctx = |e: std::io::Error| SpeechError::Model(format!("{}: {e}", path.display()));
        let file = std::fs::File::open(path).map_err(ctx)?;
        let len = file.metadata().map_err(ctx)?.len();
        let (spec, vars) = parse(BufReader::new(file.try_clone().map_err(ctx)?), len)?;
        Ok(Self { file, spec, vars })
    }

    /// Read rows `rows` (along the first dimension) of the float variable `name` as `f32`; the
    /// variable's other dimensions must be `rest`.
    pub fn read_rows(&mut self, name: &str, rows: std::ops::Range<usize>, rest: &[usize]) -> Result<Vec<f32>, SpeechError> {
        let (raw, dt) = self.rows_bytes(name, rows, rest)?;
        Ok(convert(&raw, dt))
    }

    /// Rows `rows` of the float variable `name` as stored: half-precision bit patterns, or `f32`.
    pub fn read_rows_raw(&mut self, name: &str, rows: std::ops::Range<usize>, rest: &[usize]) -> Result<crate::safetensors::Raw, SpeechError> {
        let (raw, dt) = self.rows_bytes(name, rows, rest)?;
        Ok(match dt {
            Dtype::F16 => crate::safetensors::Raw::F16(raw.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()),
            _ => crate::safetensors::Raw::F32(convert(&raw, dt)),
        })
    }

    fn rows_bytes(&mut self, name: &str, rows: std::ops::Range<usize>, rest: &[usize]) -> Result<(Vec<u8>, Dtype), SpeechError> {
        let v = self.vars.get(name).ok_or_else(|| bad(format!("missing variable {name}")))?.clone();
        if v.shape.get(1..) != Some(rest) {
            return Err(bad(format!("{name}: shape {:?}, expected [_, {rest:?}]", v.shape)));
        }
        let VarType::Float(dt) = v.ty else {
            return Err(bad(format!("{name} is quantized; use a float16 or float32 conversion")));
        };
        let row: u64 = rest.iter().product::<usize>() as u64 * dt.size();
        let first = v.shape.first().copied().unwrap_or(1);
        if rows.start > rows.end || rows.end > first {
            return Err(bad(format!("{name}: rows {rows:?} of {first}")));
        }
        let (start, len) = (v.offset + rows.start as u64 * row, (rows.end - rows.start) as u64 * row);
        if start + len > v.offset + v.bytes {
            return Err(bad(format!("{name}: out of range")));
        }
        let mut raw = vec![0u8; len as usize];
        self.file.seek(SeekFrom::Start(start)).map_err(bad)?;
        self.file.read_exact(&mut raw).map_err(bad)?;
        Ok((raw, dt))
    }

    /// Read the float variable `name` (of shape `shape`) as `f32`.
    pub fn read(&mut self, name: &str, shape: &[usize]) -> Result<Vec<f32>, SpeechError> {
        let found = self.vars.get(name).map(|v| v.shape.clone()).unwrap_or_default();
        if found != shape {
            return Err(bad(format!("{name}: shape {found:?}, expected {shape:?}")));
        }
        self.read_rows(name, 0..shape.first().copied().unwrap_or(1), shape.get(1..).unwrap_or_default())
    }

    /// An integer scalar (model options such as `encoder/num_heads`).
    pub fn scalar(&mut self, name: &str) -> Option<i64> {
        let v = self.vars.get(name)?.clone();
        if !v.shape.is_empty() || v.bytes > 4 {
            return None;
        }
        let mut b = vec![0u8; v.bytes as usize];
        self.file.seek(SeekFrom::Start(v.offset)).ok()?;
        self.file.read_exact(&mut b).ok()?;
        Some(match v.ty {
            VarType::I8 => b.first().map(|&x| x as i8 as i64)?,
            VarType::I16 => i16::from_le_bytes([*b.first()?, *b.get(1)?]) as i64,
            VarType::I32 => i32::from_le_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]) as i64,
            VarType::Float(_) => return None,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Serialise variables `(name, shape, type code, data)` and aliases in the version-6 layout.
    pub fn write(vars: &[(&str, &[u32], u8, Vec<u8>)], aliases: &[(&str, &str)]) -> Vec<u8> {
        let s = |out: &mut Vec<u8>, t: &str| {
            out.extend(((t.len() + 1) as u16).to_le_bytes());
            out.extend(t.as_bytes());
            out.push(0);
        };
        let mut out = 6u32.to_le_bytes().to_vec();
        s(&mut out, "WhisperSpec");
        out.extend(3u32.to_le_bytes());
        out.extend((vars.len() as u32).to_le_bytes());
        for (name, shape, ty, data) in vars {
            s(&mut out, name);
            out.push(shape.len() as u8);
            for d in *shape {
                out.extend(d.to_le_bytes());
            }
            out.push(*ty);
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(data);
        }
        out.extend((aliases.len() as u32).to_le_bytes());
        for (a, t) in aliases {
            s(&mut out, a);
            s(&mut out, t);
        }
        out
    }

    fn sample() -> Vec<u8> {
        let w: Vec<u8> = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let h: Vec<u8> = [0x3c00u16, 0xc000].iter().flat_map(|v| v.to_le_bytes()).collect();
        write(&[("enc/w", &[3, 2], 0, w), ("enc/h", &[2], 4, h), ("enc/num_heads", &[], 2, 20i16.to_le_bytes().to_vec())], &[("dec/proj", "enc/w")])
    }

    #[test]
    fn reads_variables_rows_scalars_and_aliases() {
        let dir = std::env::temp_dir().join(format!("filmcraft-ct2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("model.bin");
        std::fs::write(&p, sample()).unwrap();
        let mut m = Ct2::open(&p).unwrap();
        assert_eq!(m.spec, "WhisperSpec");
        assert_eq!(m.read("enc/w", &[3, 2]).unwrap(), vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(m.read_rows("enc/w", 1..3, &[2]).unwrap(), vec![3.0, 4.0, 5.0, 6.0]);
        assert_eq!(m.read("dec/proj", &[3, 2]).unwrap().len(), 6);
        assert_eq!(m.read("enc/h", &[2]).unwrap(), vec![1.0, -2.0]);
        assert_eq!(m.scalar("enc/num_heads"), Some(20));
        assert!(m.read("enc/w", &[2, 3]).is_err());
        assert!(m.read_rows("enc/w", 2..4, &[2]).is_err());
        assert!(m.read_rows("enc/w", 0..1, &[3]).is_err());
        assert!(m.read("enc/num_heads", &[]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_files_are_errors_never_panics() {
        let base = sample();
        let parse_bytes = |b: &[u8]| parse(std::io::Cursor::new(b.to_vec()), b.len() as u64);
        assert!(parse_bytes(&base).is_ok());
        let mut v5 = base.clone();
        v5[0] = 5;
        assert!(parse_bytes(&v5).is_err());
        // a variable claiming more bytes than its shape, an absurd count
        let w = write(&[("x", &[2], 0, vec![0; 12])], &[]);
        assert!(parse_bytes(&w).is_err());
        let mut many = base.clone();
        let at = 4 + 2 + "WhisperSpec".len() + 1 + 4;
        many[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_bytes(&many).is_err());
        let mut seed = 0x9e37_79b9u32;
        for i in 0..600 {
            let mut b = base.clone();
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            match i % 3 {
                0 => b.truncate(seed as usize % b.len()),
                1 => {
                    let at = seed as usize % b.len();
                    b[at] ^= 1 << (seed >> 29);
                }
                _ => {
                    let at = seed as usize % b.len();
                    b[at] = (seed >> 24) as u8;
                }
            }
            let r = std::panic::catch_unwind(|| {
                let _ = parse_bytes(&b);
            });
            assert!(r.is_ok(), "panicked on mutation {i}");
        }
    }
}

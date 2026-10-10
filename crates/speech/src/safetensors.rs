//! A reader for the `safetensors` weight format, written from its published description
//! (<https://github.com/huggingface/safetensors>, "Format"): an 8-byte little-endian header
//! length `N`, `N` bytes of JSON mapping tensor names to `{"dtype", "shape", "data_offsets"}`
//! (byte offsets relative to the end of the header, `[begin, end)`), then the tensor data.
//!
//! Model files are untrusted input: the header is size-capped, every shape, offset and size is
//! checked (overflow included) against the file before anything is allocated, and tensors are
//! read one at a time straight from the file, so loading never holds the whole file in memory.
//! F32, F16 and BF16 tensors are converted to `f32`.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::SpeechError;

/// Largest JSON header accepted (real headers are well under 1 MiB).
const MAX_HEADER: u64 = 64 << 20;
/// Largest number of dimensions accepted for a tensor.
const MAX_RANK: usize = 8;

/// Element type of a stored tensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dtype {
    F32,
    F16,
    Bf16,
}

impl Dtype {
    pub fn size(self) -> u64 {
        match self {
            Dtype::F32 => 4,
            Dtype::F16 | Dtype::Bf16 => 2,
        }
    }
}

/// Where one tensor lives in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct TensorInfo {
    pub dtype: Dtype,
    pub shape: Vec<usize>,
    /// Absolute byte range in the file.
    pub begin: u64,
    pub end: u64,
}

impl TensorInfo {
    pub fn numel(&self) -> usize {
        self.shape.iter().product()
    }
}

fn bad(msg: impl std::fmt::Display) -> SpeechError {
    SpeechError::Model(format!("safetensors: {msg}"))
}

/// Parse the header that starts a safetensors file of `file_len` bytes. `head` must hold at least
/// the first `8 + N` bytes. Returns the tensors by name.
pub fn parse_header(head: &[u8], file_len: u64) -> Result<BTreeMap<String, TensorInfo>, SpeechError> {
    let n = header_len(head)?;
    let data_start = 8u64.checked_add(n).ok_or_else(|| bad("header length overflows"))?;
    if data_start > file_len {
        return Err(bad("header is longer than the file"));
    }
    let json = head.get(8..usize::try_from(data_start).map_err(|_| bad("header too large"))?).ok_or_else(|| bad("truncated header"))?;
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| bad(format!("header: {e}")))?;
    let obj = v.as_object().ok_or_else(|| bad("header is not a JSON object"))?;
    let data_len = file_len - data_start;
    let mut out = BTreeMap::new();
    for (name, t) in obj {
        if name == "__metadata__" {
            continue;
        }
        let dtype = match t.get("dtype").and_then(|d| d.as_str()) {
            Some("F32") => Dtype::F32,
            Some("F16") => Dtype::F16,
            Some("BF16") => Dtype::Bf16,
            Some(d) => return Err(bad(format!("{name}: unsupported dtype {d}"))),
            None => return Err(bad(format!("{name}: no dtype"))),
        };
        let shape_v = t.get("shape").and_then(|s| s.as_array()).ok_or_else(|| bad(format!("{name}: no shape")))?;
        if shape_v.len() > MAX_RANK {
            return Err(bad(format!("{name}: rank {} is too large", shape_v.len())));
        }
        let mut shape = Vec::with_capacity(shape_v.len());
        let mut numel: u64 = 1;
        for d in shape_v {
            let d = d.as_u64().ok_or_else(|| bad(format!("{name}: bad dimension")))?;
            numel = numel.checked_mul(d).ok_or_else(|| bad(format!("{name}: shape overflows")))?;
            shape.push(usize::try_from(d).map_err(|_| bad(format!("{name}: dimension too large")))?);
        }
        let offs = t.get("data_offsets").and_then(|o| o.as_array()).ok_or_else(|| bad(format!("{name}: no data_offsets")))?;
        let (Some(b), Some(e)) = (offs.first().and_then(|x| x.as_u64()), offs.get(1).and_then(|x| x.as_u64())) else {
            return Err(bad(format!("{name}: bad data_offsets")));
        };
        if offs.len() != 2 || e < b || e > data_len {
            return Err(bad(format!("{name}: data_offsets [{b}, {e}] outside the {data_len}-byte data section")));
        }
        let bytes = numel.checked_mul(dtype.size()).ok_or_else(|| bad(format!("{name}: size overflows")))?;
        if e - b != bytes {
            return Err(bad(format!("{name}: {} bytes stored for {bytes} bytes of data", e - b)));
        }
        out.insert(name.clone(), TensorInfo { dtype, shape, begin: data_start + b, end: data_start + e });
    }
    Ok(out)
}

fn header_len(head: &[u8]) -> Result<u64, SpeechError> {
    let b: [u8; 8] = head.get(..8).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("file is shorter than 8 bytes"))?;
    let n = u64::from_le_bytes(b);
    if n > MAX_HEADER {
        return Err(bad(format!("header of {n} bytes is too large")));
    }
    Ok(n)
}

/// An open safetensors file.
pub struct SafeTensors {
    file: std::fs::File,
    pub tensors: BTreeMap<String, TensorInfo>,
}

impl SafeTensors {
    pub fn open(path: &Path) -> Result<Self, SpeechError> {
        let ctx = |e: std::io::Error| SpeechError::Model(format!("{}: {e}", path.display()));
        let mut file = std::fs::File::open(path).map_err(ctx)?;
        let file_len = file.metadata().map_err(ctx)?.len();
        let mut len = [0u8; 8];
        file.read_exact(&mut len).map_err(|_| bad("file is shorter than 8 bytes"))?;
        let n = header_len(&len)?;
        if n.saturating_add(8) > file_len {
            return Err(bad("header is longer than the file"));
        }
        let mut head = len.to_vec();
        head.resize(8 + n as usize, 0);
        file.read_exact(&mut head[8..]).map_err(ctx)?;
        let tensors = parse_header(&head, file_len)?;
        Ok(Self { file, tensors })
    }

    pub fn get(&self, name: &str) -> Option<&TensorInfo> {
        self.tensors.get(name)
    }

    /// Read tensor `name` (of shape `shape`) as stored: the raw bit patterns of a half-precision
    /// tensor, anything else as `f32`.
    pub fn read_raw(&mut self, name: &str, shape: &[usize]) -> Result<Raw, SpeechError> {
        let dtype = self.tensors.get(name).map(|t| t.dtype);
        if dtype == Some(Dtype::F16) {
            let raw = self.read_bytes(name, shape)?;
            return Ok(Raw::F16(raw.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()));
        }
        self.read_f32(name, shape).map(Raw::F32)
    }

    fn read_bytes(&mut self, name: &str, shape: &[usize]) -> Result<Vec<u8>, SpeechError> {
        let info = self.tensors.get(name).ok_or_else(|| bad(format!("missing tensor {name}")))?.clone();
        if info.shape != shape {
            return Err(bad(format!("{name}: shape {:?}, expected {shape:?}", info.shape)));
        }
        let len = usize::try_from(info.end - info.begin).map_err(|_| bad(format!("{name}: too large")))?;
        let mut raw = vec![0u8; len];
        self.file.seek(SeekFrom::Start(info.begin)).map_err(|e| bad(format!("{name}: {e}")))?;
        self.file.read_exact(&mut raw).map_err(|e| bad(format!("{name}: {e}")))?;
        Ok(raw)
    }

    /// Read tensor `name` as `f32`, checking that its shape is `shape`.
    pub fn read_f32(&mut self, name: &str, shape: &[usize]) -> Result<Vec<f32>, SpeechError> {
        let info = self.tensors.get(name).ok_or_else(|| bad(format!("missing tensor {name}")))?.clone();
        if info.shape != shape {
            return Err(bad(format!("{name}: shape {:?}, expected {shape:?}", info.shape)));
        }
        let len = usize::try_from(info.end - info.begin).map_err(|_| bad(format!("{name}: too large")))?;
        let mut raw = vec![0u8; len];
        self.file.seek(SeekFrom::Start(info.begin)).map_err(|e| bad(format!("{name}: {e}")))?;
        self.file.read_exact(&mut raw).map_err(|e| bad(format!("{name}: {e}")))?;
        Ok(convert(&raw, info.dtype))
    }
}

/// Tensor data as read: half-precision bit patterns, or `f32`.
pub enum Raw {
    F16(Vec<u16>),
    F32(Vec<f32>),
}

/// Convert little-endian raw tensor bytes to `f32` (in parallel for large tensors).
pub fn convert(raw: &[u8], dtype: Dtype) -> Vec<f32> {
    use rayon::prelude::*;
    const CHUNK: usize = 1 << 16;
    let es = dtype.size() as usize;
    let mut out = vec![0f32; raw.len() / es];
    let work = |(o, r): (&mut [f32], &[u8])| match dtype {
        Dtype::F32 => o.iter_mut().zip(r.as_chunks::<4>().0).for_each(|(o, b)| *o = f32::from_le_bytes(*b)),
        Dtype::F16 => o.iter_mut().zip(r.as_chunks::<2>().0).for_each(|(o, b)| *o = f16_to_f32(u16::from_le_bytes(*b))),
        Dtype::Bf16 => o.iter_mut().zip(r.as_chunks::<2>().0).for_each(|(o, b)| *o = bf16_to_f32(u16::from_le_bytes(*b))),
    };
    if out.len() > CHUNK {
        out.par_chunks_mut(CHUNK).zip(raw.par_chunks(CHUNK * es)).for_each(work);
    } else {
        work((&mut out, raw));
    }
    out
}

/// IEEE 754 binary16 → binary32, exact for every input (subnormals, infinities and NaNs included).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = (h >> 10) & 0x1f;
    let mant = (h & 0x3ff) as u32;
    let bits = match exp {
        // zero and subnormals: mant · 2⁻²⁴, exact in f32
        0 => {
            let v = mant as f32 * (1.0 / 16_777_216.0);
            return if sign != 0 { -v } else { v };
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        e => sign | ((e as u32 + 112) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

/// bfloat16 → binary32 (bfloat16 is the upper half of a binary32).
pub fn bf16_to_f32(h: u16) -> f32 {
    f32::from_bits((h as u32) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(header: &str, data: &[u8]) -> Vec<u8> {
        let mut v = (header.len() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(header.as_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn f16_conversion_is_exact() {
        // reference: the value's definition
        for h in 0..=u16::MAX {
            let s = if h & 0x8000 != 0 { -1.0f64 } else { 1.0 };
            let e = ((h >> 10) & 0x1f) as i32;
            let m = (h & 0x3ff) as f64;
            let v = f16_to_f32(h);
            match e {
                0 => assert_eq!(v as f64, s * m * 2f64.powi(-24), "{h:#x}"),
                31 if m == 0.0 => assert_eq!(v as f64, s * f64::INFINITY),
                31 => assert!(v.is_nan()),
                _ => assert_eq!(v as f64, s * (1.0 + m / 1024.0) * 2f64.powi(e - 15), "{h:#x}"),
            }
            if e == 0 && m == 0.0 {
                assert_eq!(v.is_sign_negative(), s < 0.0);
            }
        }
        assert_eq!(bf16_to_f32(0x3f80), 1.0);
        assert_eq!(bf16_to_f32(0xc000), -2.0);
    }

    #[test]
    fn parses_and_reads() {
        let data: Vec<u8> = [1.0f32, -2.5].iter().flat_map(|v| v.to_le_bytes()).chain([0x00, 0x3c, 0x00, 0xc0]).collect();
        let bytes = file(
            r#"{"__metadata__":{"format":"pt"},"a":{"dtype":"F32","shape":[2],"data_offsets":[0,8]},"b":{"dtype":"F16","shape":[1,2],"data_offsets":[8,12]}}"#,
            &data,
        );
        let h = parse_header(&bytes, bytes.len() as u64).unwrap();
        assert_eq!(h["a"].shape, vec![2]);
        assert_eq!(h["b"].dtype, Dtype::F16);
        let dir = std::env::temp_dir().join(format!("filmcraft-st-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.safetensors");
        std::fs::write(&p, &bytes).unwrap();
        let mut st = SafeTensors::open(&p).unwrap();
        assert_eq!(st.read_f32("a", &[2]).unwrap(), vec![1.0, -2.5]);
        assert_eq!(st.read_f32("b", &[1, 2]).unwrap(), vec![1.0, -2.0]);
        assert!(st.read_f32("b", &[2]).is_err());
        assert!(st.read_f32("missing", &[1]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_headers_are_errors() {
        let ok = r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}"#;
        let cases: &[(&str, usize)] = &[
            (ok, 4),                                                                                 // data truncated
            (r#"{"a":{"dtype":"F32","shape":[3],"data_offsets":[0,8]}}"#, 8),                        // size mismatch
            (r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[8,0]}}"#, 8),                        // end < begin
            (r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[0]}}"#, 8),                          // one offset
            (r#"{"a":{"dtype":"I64","shape":[1],"data_offsets":[0,8]}}"#, 8),                        // dtype
            (r#"{"a":{"dtype":"F32","shape":[-1],"data_offsets":[0,8]}}"#, 8),                       // negative dim
            (r#"{"a":{"dtype":"F32","shape":[4294967296,4294967296,16],"data_offsets":[0,8]}}"#, 8), // overflow
            (r#"{"a":{"dtype":"F32","shape":[1,1,1,1,1,1,1,1,1],"data_offsets":[0,4]}}"#, 8),        // rank
            (r#"["not an object"]"#, 0),
            (r#"{"a":"#, 0),
            (r#"{"a":{"shape":[2],"data_offsets":[0,8]}}"#, 8),
        ];
        for (h, n) in cases {
            let bytes = file(h, &vec![0u8; *n]);
            assert!(parse_header(&bytes, bytes.len() as u64).is_err(), "{h}");
        }
        assert!(parse_header(&[1, 2, 3], 3).is_err());
        // header length larger than the file / than the cap
        let mut huge = u64::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(parse_header(&huge, huge.len() as u64).is_err());
        let mut long = 100u64.to_le_bytes().to_vec();
        long.extend_from_slice(b"{}");
        assert!(parse_header(&long, long.len() as u64).is_err());
    }

    #[test]
    fn mutated_files_never_panic() {
        let data: Vec<u8> = (0..32u8).collect();
        let base = file(r#"{"w":{"dtype":"F16","shape":[4,4],"data_offsets":[0,32]},"__metadata__":{}}"#, &data);
        let dir = std::env::temp_dir().join(format!("filmcraft-st-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.safetensors");
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for i in 0..400 {
            let mut b = base.clone();
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            match i % 3 {
                0 => b.truncate((seed as usize) % b.len()),
                1 => {
                    let at = (seed as usize) % b.len();
                    b[at] ^= 1 << ((seed >> 32) % 8);
                }
                _ => {
                    let at = (seed as usize) % 8;
                    b[at] = (seed >> 40) as u8;
                }
            }
            std::fs::write(&p, &b).unwrap();
            let r = std::panic::catch_unwind(|| {
                if let Ok(mut st) = SafeTensors::open(&p) {
                    let _ = st.read_f32("w", &[4, 4]);
                }
            });
            assert!(r.is_ok(), "panicked on mutation {i}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

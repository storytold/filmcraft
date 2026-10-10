//! Reading NVIDIA NeMo model archives (`.nemo`) in pure Rust, without Python.
//!
//! A `.nemo` file is an uncompressed tar archive holding `model_config.yaml`, the weights
//! `model_weights.ckpt` (a PyTorch `torch.save` zip archive: a pickled state dict plus one stored
//! file per tensor storage) and the tokenizer files. Every layer here treats the file as hostile:
//! sizes and counts are capped, offsets are checked against the file length, nothing panics, and
//! tensor bytes are read straight from the archive with no temporary copy of the checkpoint.
//!
//! - [`tar`]: ustar/PAX/GNU member index.
//! - [`zip`]: stored-only zip central directory (zip64 aware) inside a byte range of the file.
//! - [`pickle`]: the subset of the pickle VM that `torch.save` emits for a state dict.
//! - [`yaml`]: the indentation subset of YAML that `model_config.yaml` uses.
//! - [`spm`]: SentencePiece `tokenizer.model` (protobuf) pieces, for decoding token ids.
//!
//! These parsers are compiled in every build (no model runtime needed), so their hostile-input
//! tests run in the default test suite.

pub mod pickle;
pub mod spm;
pub mod tar;
pub mod yaml;
pub mod zip;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::SpeechError;
pub use pickle::{DType, TensorInfo};

/// Upper bound for one tensor (4 GiB): larger is a corrupt file for the models we load.
pub const MAX_TENSOR_BYTES: u64 = 1 << 32;
/// Upper bound for small text members (config, tokenizer).
pub const MAX_SMALL_MEMBER: u64 = 64 << 20;

fn bad(msg: impl Into<String>) -> SpeechError {
    SpeechError::Model(msg.into())
}

/// Read exactly `len` bytes at `offset` (bounded by `MAX_TENSOR_BYTES`).
pub(crate) fn read_at(f: &mut File, offset: u64, len: u64) -> Result<Vec<u8>, SpeechError> {
    if len > MAX_TENSOR_BYTES {
        return Err(bad(format!("member of {len} bytes is too large")));
    }
    let len = usize::try_from(len).map_err(|_| bad("member too large for this platform"))?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| bad("out of memory"))?;
    f.by_ref().take(len as u64).read_to_end(&mut buf)?;
    if buf.len() != len {
        return Err(bad("file ends inside a member"));
    }
    Ok(buf)
}

/// One tensor of the checkpoint, located in the `.nemo` file.
#[derive(Clone, Debug)]
pub struct TensorEntry {
    pub info: TensorInfo,
    /// Absolute file offset of the tensor's first element.
    pub offset: u64,
}

/// An opened `.nemo` archive: member index, configuration text and tensor index.
pub struct Nemo {
    path: PathBuf,
    members: Vec<tar::Member>,
    /// Tensors by state-dict name, in pickle order.
    pub tensors: Vec<(String, TensorEntry)>,
    /// `model_config.yaml`.
    pub config: yaml::Yaml,
}

impl Nemo {
    /// Index `path`: the tar members, the config and the checkpoint's tensors (no tensor data is read).
    pub fn open(path: &Path) -> Result<Self, SpeechError> {
        let mut f = File::open(path).map_err(|e| bad(format!("{}: {e}", path.display())))?;
        let file_len = f.metadata()?.len();
        let members = tar::index(&mut f, file_len)?;
        let find = |suffix: &str| members.iter().find(|m| m.name == suffix || m.name.ends_with(&format!("/{suffix}")));
        let cfg_m = find("model_config.yaml").ok_or_else(|| bad("the archive has no model_config.yaml"))?;
        if cfg_m.size > MAX_SMALL_MEMBER {
            return Err(bad("model_config.yaml is too large"));
        }
        let cfg_text = read_at(&mut f, cfg_m.offset, cfg_m.size)?;
        let config = yaml::Yaml::parse(&String::from_utf8_lossy(&cfg_text))?;
        let ck = find("model_weights.ckpt").ok_or_else(|| bad("the archive has no model_weights.ckpt"))?.clone();
        let entries = zip::index(&mut f, ck.offset, ck.size)?;
        let pkl = entries.iter().find(|e| e.name.ends_with("data.pkl")).ok_or_else(|| bad("the checkpoint has no data.pkl"))?;
        if pkl.size > pickle::MAX_PICKLE {
            return Err(bad("data.pkl is too large"));
        }
        let prefix = pkl.name.strip_suffix("data.pkl").unwrap_or("").to_string();
        if let Some(bo) = entries.iter().find(|e| e.name == format!("{prefix}byteorder")) {
            let b = read_at(&mut f, bo.offset, bo.size.min(16))?;
            if b != b"little" {
                return Err(bad("only little-endian checkpoints are supported"));
            }
        }
        let pickled = read_at(&mut f, pkl.offset, pkl.size)?;
        let infos = pickle::state_dict(&pickled)?;
        let mut tensors = Vec::with_capacity(infos.len());
        for (name, info) in infos {
            let data_name = format!("{prefix}data/{}", info.storage);
            let Some(e) = entries.iter().find(|e| e.name == data_name) else {
                return Err(bad(format!("tensor {name}: storage {} is missing", info.storage)));
            };
            let elem = info.dtype.size() as u64;
            // the bytes the tensor spans must lie inside its storage
            let span = info.span_elements().ok_or_else(|| bad(format!("tensor {name}: bad shape")))?;
            let end = info.offset.checked_add(span).and_then(|v| v.checked_mul(elem)).ok_or_else(|| bad(format!("tensor {name}: bad size")))?;
            if end > e.size || span.checked_mul(elem).is_none_or(|b| b > MAX_TENSOR_BYTES) {
                return Err(bad(format!("tensor {name} lies outside its storage")));
            }
            let offset = e.offset.checked_add(info.offset * elem).ok_or_else(|| bad("bad offset"))?;
            tensors.push((name, TensorEntry { info, offset }));
        }
        Ok(Self { path: path.to_path_buf(), members, tensors, config })
    }

    /// A member's bytes (small members only: tokenizer, vocabulary).
    pub fn read_member(&self, name: &str) -> Result<Vec<u8>, SpeechError> {
        let m = self
            .members
            .iter()
            .find(|m| m.name == name || m.name.ends_with(&format!("/{name}")) || m.name.ends_with(&format!("_{name}")))
            .ok_or_else(|| bad(format!("the archive has no {name}")))?;
        if m.size > MAX_SMALL_MEMBER {
            return Err(bad(format!("{name} is too large")));
        }
        let mut f = File::open(&self.path)?;
        read_at(&mut f, m.offset, m.size)
    }

    /// The tensor index entry for `name`.
    pub fn tensor(&self, name: &str) -> Option<&TensorEntry> {
        self.tensors.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// Read tensor `name` as `f32` values (row-major, contiguous) with its shape.
    pub fn read_f32(&self, f: &mut File, name: &str) -> Result<(Vec<usize>, Vec<f32>), SpeechError> {
        let t = self.tensor(name).ok_or_else(|| bad(format!("the checkpoint has no tensor {name}")))?;
        let info = &t.info;
        let n = info.numel().ok_or_else(|| bad(format!("tensor {name}: bad shape")))?;
        if !info.is_contiguous() {
            return Err(bad(format!("tensor {name} is not contiguous")));
        }
        let elem = info.dtype.size() as u64;
        let bytes = read_at(f, t.offset, n.checked_mul(elem).ok_or_else(|| bad("bad size"))?)?;
        let v: Vec<f32> = match info.dtype {
            DType::F32 => bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
            DType::F16 => bytes.as_chunks::<2>().0.iter().map(|b| f16_to_f32(u16::from_le_bytes(*b))).collect(),
            DType::BF16 => bytes.as_chunks::<2>().0.iter().map(|b| f32::from_bits(u32::from(u16::from_le_bytes(*b)) << 16)).collect(),
            other => return Err(bad(format!("tensor {name} has type {other:?}, expected floating point"))),
        };
        Ok((info.shape.clone(), v))
    }

    /// Open the archive file for [`Nemo::read_f32`].
    pub fn file(&self) -> Result<File, SpeechError> {
        File::open(&self.path).map_err(|e| bad(format!("{}: {e}", self.path.display())))
    }
}

/// IEEE half to single precision.
fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    let bits = match (exp, man) {
        (0, 0) => sign,
        (0, m) => {
            // subnormal: normalise
            let mut e = 127 - 15 + 1;
            let mut m = m;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
        (0x1f, m) => sign | 0x7f80_0000 | (m << 13),
        (e, m) => sign | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
pub(crate) mod testutil {
    //! Builders for synthetic archives (tar, stored zip, pickled state dict).

    /// A ustar header block for a regular file (`typeflag` '0') or other type.
    pub fn tar_header(name: &str, size: u64, typeflag: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        let s = format!("{size:011o}");
        h[124..135].copy_from_slice(s.as_bytes());
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        let c = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(c.as_bytes());
        h
    }

    pub fn tar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in members {
            out.extend_from_slice(&tar_header(name, data.len() as u64, b'0'));
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.resize(out.len() + 1024, 0);
        out
    }

    /// A stored (uncompressed) zip archive.
    pub fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data) in members {
            let off = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&0u32.to_le_bytes()); // crc (unchecked)
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// A protocol-2 pickle of `OrderedDict(name -> FloatStorage tensor)`, the way `torch.save`
    /// writes it. Each tensor: (name, storage key, storage offset, shape).
    pub fn state_dict_pickle(tensors: &[(&str, &str, u64, &[u64])]) -> Vec<u8> {
        let mut p = vec![0x80, 2];
        p.extend_from_slice(b"ccollections\nOrderedDict\nq\x00)Rq\x01(");
        let s = |p: &mut Vec<u8>, s: &str| {
            p.push(b'X');
            p.extend_from_slice(&(s.len() as u32).to_le_bytes());
            p.extend_from_slice(s.as_bytes());
        };
        let int = |p: &mut Vec<u8>, v: u64| {
            p.push(b'J');
            p.extend_from_slice(&(v as i32).to_le_bytes());
        };
        for (name, key, off, shape) in tensors {
            s(&mut p, name);
            p.extend_from_slice(b"ctorch._utils\n_rebuild_tensor_v2\n(");
            p.push(b'(');
            s(&mut p, "storage");
            p.extend_from_slice(b"ctorch\nFloatStorage\n");
            s(&mut p, key);
            s(&mut p, "cpu");
            int(&mut p, shape.iter().product::<u64>() + off);
            p.extend_from_slice(b"tQ");
            int(&mut p, *off);
            p.push(b'(');
            for d in shape.iter() {
                int(&mut p, *d);
            }
            p.push(b't');
            p.push(b'(');
            let mut stride = vec![1u64; shape.len()];
            for i in (0..shape.len().saturating_sub(1)).rev() {
                stride[i] = stride[i + 1] * shape[i + 1];
            }
            for d in &stride {
                int(&mut p, *d);
            }
            p.push(b't');
            p.push(0x89);
            p.extend_from_slice(b"h\x00)R");
            p.extend_from_slice(b"tR");
        }
        p.extend_from_slice(b"u}b.");
        p
    }

    /// A `.nemo`-like tar: config, a checkpoint of the given f32 tensors, and extra members.
    pub fn nemo(config: &str, tensors: &[(&str, &[u64], Vec<f32>)], extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut spec = Vec::new();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, (name, shape, data)) in tensors.iter().enumerate() {
            spec.push((*name, i.to_string(), *shape));
            files.push((format!("model_weights/data/{i}"), data.iter().flat_map(|v| v.to_le_bytes()).collect()));
        }
        let spec2: Vec<(&str, &str, u64, &[u64])> = spec.iter().map(|(n, k, s)| (*n, k.as_str(), 0, *s)).collect();
        let pkl = state_dict_pickle(&spec2);
        let mut zm: Vec<(&str, &[u8])> = vec![("model_weights/data.pkl", &pkl), ("model_weights/byteorder", b"little")];
        for (n, d) in &files {
            zm.push((n.as_str(), d.as_slice()));
        }
        let z = zip(&zm);
        let mut tm: Vec<(&str, &[u8])> = vec![("./model_config.yaml", config.as_bytes()), ("./model_weights.ckpt", &z)];
        tm.extend_from_slice(extra);
        tar(&tm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("filmcraft-nemo-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn opens_a_synthetic_archive_and_reads_tensors() {
        let bytes = testutil::nemo(
            "encoder:\n  d_model: 4\n",
            &[("a.weight", &[2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), ("b", &[2], vec![-1.0, 0.5])],
            &[("./abc_tokenizer.model", b"xyz")],
        );
        let p = tmp("ok.nemo", &bytes);
        let n = Nemo::open(&p).unwrap();
        assert_eq!(n.config.int("encoder.d_model"), Some(4));
        assert_eq!(n.tensors.len(), 2);
        let mut f = n.file().unwrap();
        assert_eq!(n.read_f32(&mut f, "a.weight").unwrap(), (vec![2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]));
        assert_eq!(n.read_f32(&mut f, "b").unwrap().1, vec![-1.0, 0.5]);
        assert!(n.read_f32(&mut f, "missing").is_err());
        assert_eq!(n.read_member("tokenizer.model").unwrap(), b"xyz");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn hostile_archives_fail_cleanly() {
        let good = testutil::nemo("encoder:\n  d_model: 4\n", &[("w", &[3], vec![1.0, 2.0, 3.0])], &[]);
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let p = std::env::temp_dir().join(format!("filmcraft-nemo-{}-fuzz.nemo", std::process::id()));
        for i in 0..600 {
            let mut b = good.clone();
            match i % 4 {
                // truncation
                0 => b.truncate((next() as usize) % b.len()),
                // bit flips
                1 => {
                    for _ in 0..1 + next() % 8 {
                        let at = (next() as usize) % b.len();
                        b[at] ^= 1 << (next() % 8);
                    }
                }
                // corrupt sizes and offsets: overwrite a random 4-byte run with an extreme value
                2 => {
                    let at = (next() as usize) % (b.len() - 4);
                    let v = [u32::MAX, 0x7fff_ffff, 0, 0x8000_0000][(next() % 4) as usize];
                    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
                // octal size fields full of 7s
                _ => {
                    let at = (next() as usize) % (b.len() - 12);
                    b[at..at + 11].copy_from_slice(b"77777777777");
                }
            }
            std::fs::write(&p, &b).unwrap();
            let r = std::panic::catch_unwind(|| {
                if let Ok(n) = Nemo::open(&p) {
                    let mut f = n.file().unwrap();
                    let _ = n.read_f32(&mut f, "w");
                    let _ = n.read_member("model_config.yaml");
                }
            });
            assert!(r.is_ok(), "case {i} panicked");
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn half_precision_converts() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!((f16_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-12);
        assert!(f16_to_f32(0x7c00).is_infinite());
    }
}

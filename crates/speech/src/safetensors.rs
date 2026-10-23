//! A streaming safetensors reader for large models.
//!
//! `VarBuilder::from_buffered_safetensors` keeps the whole file in memory while it copies each
//! tensor out, so a 2.5 GB model peaks at 5 GB. This reads the header, then each tensor's bytes
//! straight from the file, so the peak is the model plus one tensor.
//!
//! Format (Hugging Face safetensors): an 8-byte little-endian header length, a JSON object mapping
//! tensor names to `{dtype, shape, data_offsets: [begin, end]}` (offsets into the data that follows
//! the header), an optional `__metadata__` entry, then the tensor data. Every number in the header
//! is checked against the file before it is used.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use candle_core::{DType, Device, Tensor};

use crate::SpeechError;

/// Largest header accepted (the real ones are a few hundred kB).
const MAX_HEADER: u64 = 64 << 20;

fn err(path: &Path, what: impl std::fmt::Display) -> SpeechError {
    SpeechError::Model(format!("{}: {what}", path.display()))
}

/// Load every `F32` tensor of `path` onto `device` (other dtypes, such as batch-norm step
/// counters, are skipped).
pub fn load_f32(path: &Path, device: &Device) -> Result<HashMap<String, Tensor>, SpeechError> {
    let mut f = std::fs::File::open(path).map_err(|e| err(path, e))?;
    let file_len = f.metadata().map_err(|e| err(path, e))?.len();
    let mut len8 = [0u8; 8];
    f.read_exact(&mut len8).map_err(|e| err(path, e))?;
    let header_len = u64::from_le_bytes(len8);
    if header_len > MAX_HEADER || header_len.saturating_add(8) > file_len {
        return Err(err(path, format!("bad header length {header_len}")));
    }
    let mut header = vec![0u8; header_len as usize];
    f.read_exact(&mut header).map_err(|e| err(path, e))?;
    let header: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&header).map_err(|e| err(path, e))?;
    let data_start = 8 + header_len;
    let data_len = file_len - data_start;
    let mut out = HashMap::with_capacity(header.len());
    for (name, v) in &header {
        if name == "__metadata__" || v["dtype"].as_str() != Some("F32") {
            continue;
        }
        let shape: Vec<usize> = v["shape"]
            .as_array()
            .ok_or_else(|| err(path, format!("{name}: no shape")))?
            .iter()
            .map(|d| d.as_u64().and_then(|d| usize::try_from(d).ok()).ok_or_else(|| err(path, format!("{name}: bad shape"))))
            .collect::<Result<_, _>>()?;
        let offs = v["data_offsets"].as_array().ok_or_else(|| err(path, format!("{name}: no data_offsets")))?;
        let (Some(begin), Some(end)) = (offs.first().and_then(|x| x.as_u64()), offs.get(1).and_then(|x| x.as_u64())) else {
            return Err(err(path, format!("{name}: bad data_offsets")));
        };
        let elems = shape.iter().try_fold(1usize, |a, &d| a.checked_mul(d)).ok_or_else(|| err(path, format!("{name}: shape overflows")))?;
        let bytes = elems.checked_mul(4).ok_or_else(|| err(path, format!("{name}: shape overflows")))?;
        if end < begin || end > data_len || (end - begin) != bytes as u64 {
            return Err(err(path, format!("{name}: data_offsets {begin}..{end} do not fit shape {shape:?}")));
        }
        f.seek(SeekFrom::Start(data_start + begin)).map_err(|e| err(path, e))?;
        let mut buf = vec![0u8; bytes];
        f.read_exact(&mut buf).map_err(|e| err(path, e))?;
        let t = Tensor::from_raw_buffer(&buf, DType::F32, &shape, device).map_err(|e| err(path, format!("{name}: {e}")))?;
        out.insert(name.clone(), t);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, header: &str, data: &[u8]) {
        let mut b = (header.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(header.as_bytes());
        b.extend_from_slice(data);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn reads_f32_and_rejects_hostile_headers() {
        let dir = std::env::temp_dir().join(format!("filmcraft-safetensors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ok.safetensors");
        let data: Vec<u8> = [1.0f32, 2.0, 3.0, 4.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        write(
            &p,
            r#"{"__metadata__":{},"w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]},"n":{"dtype":"I64","shape":[],"data_offsets":[16,16]}}"#,
            &data,
        );
        let t = load_f32(&p, &Device::Cpu).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t["w"].to_vec2::<f32>().unwrap(), vec![vec![1.0, 2.0], vec![3.0, 4.0]]);

        // offsets past the end, a shape that does not match the bytes, a huge shape, a huge header
        for h in [
            r#"{"w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,999]}}"#,
            r#"{"w":{"dtype":"F32","shape":[3,2],"data_offsets":[0,16]}}"#,
            r#"{"w":{"dtype":"F32","shape":[18446744073709551615,4],"data_offsets":[0,16]}}"#,
        ] {
            write(&p, h, &data);
            assert!(load_f32(&p, &Device::Cpu).is_err(), "{h}");
        }
        let mut b = u64::MAX.to_le_bytes().to_vec();
        b.extend_from_slice(b"{}");
        std::fs::write(&p, b).unwrap();
        assert!(load_f32(&p, &Device::Cpu).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

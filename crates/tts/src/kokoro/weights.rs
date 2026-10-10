//! Loading the Kokoro-82M checkpoint, its config and voice packs.
//!
//! The official release is a PyTorch zip checkpoint (`kokoro-v1_0.pth`) holding five state
//! dictionaries (`bert`, `bert_encoder`, `predictor`, `text_encoder`, `decoder`), each with a
//! `module.` prefix. They are read with candle's restricted pickle reader, which only rebuilds
//! tensors and never runs code from the file. Weight-normalised layers (`weight_g`, `weight_v`) are
//! fused into a plain weight on load. Voice packs (`voices/<id>.pt`) hold one bare float32 tensor
//! `[510, 1, 256]`, read straight from the zip archive.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use candle_core::{DType, Device, Tensor};

use crate::TtsError;

pub(crate) fn err(e: impl std::fmt::Display) -> TtsError {
    TtsError::Model(e.to_string())
}

/// The architecture this code implements; a checkpoint whose config differs is refused.
pub(crate) struct Config {
    pub vocab: HashMap<char, u32>,
}

/// Values of `config.json` this implementation is written for.
const EXPECT: &[(&str, &str)] = &[
    ("hidden_dim", "512"),
    ("style_dim", "128"),
    ("n_token", "178"),
    ("max_dur", "50"),
    ("n_layer", "3"),
    ("text_encoder_kernel_size", "5"),
    ("istftnet.gen_istft_n_fft", "20"),
    ("istftnet.gen_istft_hop_size", "5"),
    ("istftnet.upsample_rates", "[10,6]"),
    ("istftnet.upsample_kernel_sizes", "[20,12]"),
    ("istftnet.resblock_kernel_sizes", "[3,7,11]"),
    ("istftnet.upsample_initial_channel", "512"),
    ("plbert.hidden_size", "768"),
    ("plbert.num_attention_heads", "12"),
    ("plbert.num_hidden_layers", "12"),
    ("plbert.intermediate_size", "2048"),
];

pub(crate) fn read_config(path: &Path) -> Result<Config, TtsError> {
    let bytes = std::fs::read(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    if bytes.len() > 1 << 20 {
        return Err(err("config.json is too large"));
    }
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(err)?;
    for (key, want) in EXPECT {
        let mut cur = &v;
        for part in key.split('.') {
            cur = cur.get(part).ok_or_else(|| err(format!("config.json: missing `{key}`")))?;
        }
        let got = serde_json::to_string(cur).map_err(err)?;
        if got != *want {
            return Err(err(format!("config.json: `{key}` is {got}, this build supports {want}")));
        }
    }
    let table = v.get("vocab").and_then(|t| t.as_object()).ok_or_else(|| err("config.json: missing `vocab`"))?;
    let mut vocab = HashMap::new();
    for (sym, id) in table {
        let mut chars = sym.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { continue };
        let Some(id) = id.as_u64().filter(|i| *i > 0 && *i < 178) else { continue };
        vocab.insert(c, id as u32);
    }
    if vocab.len() < 100 {
        return Err(err("config.json: the phoneme vocabulary is incomplete"));
    }
    Ok(Config { vocab })
}

/// All model tensors by full name (`decoder.generator.conv_post.weight`…), weight norm fused.
pub(crate) struct Weights {
    map: HashMap<String, Tensor>,
}

const PARTS: [&str; 5] = ["bert", "bert_encoder", "predictor", "text_encoder", "decoder"];

impl Weights {
    pub(crate) fn load(path: &Path) -> Result<Weights, TtsError> {
        let mut raw: HashMap<String, Tensor> = HashMap::new();
        for part in PARTS {
            let tensors = candle_core::pickle::read_all_with_key(path, Some(part)).map_err(|e| err(format!("{}: {e}", path.display())))?;
            if tensors.is_empty() {
                return Err(err(format!("{}: no `{part}` weights", path.display())));
            }
            for (name, t) in tensors {
                let name = name.strip_prefix("module.").unwrap_or(&name);
                let t = t.to_dtype(DType::F32).map_err(err)?;
                raw.insert(format!("{part}.{name}"), t);
            }
        }
        // fuse weight norm: w = g · v / ‖v‖ (norm over every dim but the first)
        let mut map = HashMap::with_capacity(raw.len());
        let gs: Vec<String> = raw.keys().filter(|k| k.ends_with(".weight_g")).cloned().collect();
        for g_name in gs {
            let base = g_name.trim_end_matches("_g").to_string();
            let v_name = format!("{base}_v");
            let (Some(g), Some(v)) = (raw.remove(&g_name), raw.remove(&v_name)) else {
                return Err(err(format!("{g_name} without {v_name}")));
            };
            let norm = v.sqr().map_err(err)?.sum_keepdim(1).map_err(err)?.sum_keepdim(2).map_err(err)?.sqrt().map_err(err)?;
            let w = v.broadcast_div(&norm).map_err(err)?.broadcast_mul(&g).map_err(err)?;
            map.insert(base, w);
        }
        map.extend(raw);
        Ok(Weights { map })
    }

    pub(crate) fn get(&self, name: &str) -> Result<Tensor, TtsError> {
        self.map.get(name).cloned().ok_or_else(|| err(format!("missing weight `{name}`")))
    }

    /// `get` with an expected shape.
    pub(crate) fn shaped(&self, name: &str, dims: &[usize]) -> Result<Tensor, TtsError> {
        let t = self.get(name)?;
        if t.dims() != dims {
            return Err(err(format!("weight `{name}` is {:?}, expected {dims:?}", t.dims())));
        }
        Ok(t)
    }
}

/// Number of style vectors in a voice pack (one per input length 1..=510).
pub(crate) const PACK_LEN: usize = 510;
pub(crate) const STYLE: usize = 256;

/// Read a voice pack: a zip whose `<name>/data/0` holds `[510, 1, 256]` little-endian float32.
pub(crate) fn read_voice(path: &Path) -> Result<Vec<f32>, TtsError> {
    let f = std::fs::File::open(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let name = (0..z.len())
        .filter_map(|i| z.by_index(i).ok().map(|e| e.name().to_string()))
        .find(|n| n.ends_with("/data/0"))
        .ok_or_else(|| err(format!("{}: not a voice pack", path.display())))?;
    let order = (0..z.len()).filter_map(|i| z.by_index(i).ok().map(|e| e.name().to_string())).find(|n| n.ends_with("/byteorder"));
    if let Some(o) = order {
        let mut s = String::new();
        z.by_name(&o).map_err(err)?.take(16).read_to_string(&mut s).map_err(err)?;
        if s.trim() != "little" {
            return Err(err(format!("{}: unsupported byte order `{s}`", path.display())));
        }
    }
    let want = PACK_LEN * STYLE * 4;
    let mut e = z.by_name(&name).map_err(err)?;
    if e.size() != want as u64 {
        return Err(err(format!("{}: voice data is {} bytes, expected {want}", path.display(), e.size())));
    }
    let mut buf = Vec::with_capacity(want);
    e.by_ref().take(want as u64).read_to_end(&mut buf).map_err(err)?;
    if buf.len() != want {
        return Err(err(format!("{}: truncated voice data", path.display())));
    }
    let v: Vec<f32> = buf.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    if v.iter().any(|x| !x.is_finite()) {
        return Err(err(format!("{}: voice data is not finite", path.display())));
    }
    Ok(v)
}

/// The style vector for an input of `n` phonemes: `[1, 256]`.
pub(crate) fn style_for(pack: &[f32], n: usize, dev: &Device) -> Result<Tensor, TtsError> {
    let row = n.clamp(1, PACK_LEN) - 1;
    let s = pack.get(row * STYLE..(row + 1) * STYLE).ok_or_else(|| err("voice pack too short"))?;
    Tensor::from_slice(s, (1, STYLE), dev).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("filmcraft-kokoro-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A voice-pack-shaped zip with `data` as `x/data/0`.
    fn pack(dir: &Path, data: &[u8], order: &str) -> std::path::PathBuf {
        let p = dir.join("v.pt");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        z.start_file("x/data.pkl", o).unwrap();
        z.write_all(b"not used").unwrap();
        z.start_file("x/byteorder", o).unwrap();
        z.write_all(order.as_bytes()).unwrap();
        z.start_file("x/data/0", o).unwrap();
        z.write_all(data).unwrap();
        z.finish().unwrap();
        p
    }

    fn floats(n: usize, v: f32) -> Vec<u8> {
        std::iter::repeat_n(v, n).flat_map(f32::to_le_bytes).collect()
    }

    #[test]
    fn reads_a_well_formed_voice_pack_and_picks_the_row_by_length() {
        let d = tmp("ok");
        let mut data = floats(PACK_LEN * STYLE, 0.0);
        // row 4 (inputs of 5 phonemes) starts with 7.0
        data[4 * STYLE * 4..4 * STYLE * 4 + 4].copy_from_slice(&7.0f32.to_le_bytes());
        let v = read_voice(&pack(&d, &data, "little")).unwrap();
        assert_eq!(v.len(), PACK_LEN * STYLE);
        let s = style_for(&v, 5, &Device::Cpu).unwrap();
        assert_eq!(s.dims(), &[1, STYLE]);
        assert_eq!(s.to_vec2::<f32>().unwrap()[0][0], 7.0);
        // lengths outside 1..=510 clamp instead of failing
        assert!(style_for(&v, 0, &Device::Cpu).is_ok());
        assert!(style_for(&v, 100_000, &Device::Cpu).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hostile_voice_packs_are_errors_not_panics() {
        let d = tmp("bad");
        let cases: Vec<(&str, std::path::PathBuf)> =
            vec![("short", pack(&d, &floats(10, 0.0), "little")), ("long", pack(&d.join(".."), &floats(PACK_LEN * STYLE + 1, 0.0), "little"))];
        for (what, p) in cases {
            assert!(read_voice(&p).is_err(), "{what}");
        }
        let p = pack(&d, &floats(PACK_LEN * STYLE, 0.0), "big");
        assert!(read_voice(&p).is_err(), "big endian");
        let p = pack(&d, &floats(PACK_LEN * STYLE, f32::NAN), "little");
        assert!(read_voice(&p).is_err(), "NaN");
        let junk = d.join("junk.pt");
        std::fs::write(&junk, b"PK\x03\x04 definitely not a zip").unwrap();
        assert!(read_voice(&junk).is_err());
        std::fs::write(&junk, b"").unwrap();
        assert!(read_voice(&junk).is_err());
        assert!(read_voice(&d.join("missing.pt")).is_err());
        // a checkpoint that isn't one
        assert!(Weights::load(&junk).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn config_must_match_the_implemented_architecture() {
        let d = tmp("cfg");
        let p = d.join("config.json");
        let good = r#"{"hidden_dim":512,"style_dim":128,"n_token":178,"max_dur":50,"n_layer":3,"text_encoder_kernel_size":5,
            "istftnet":{"gen_istft_n_fft":20,"gen_istft_hop_size":5,"upsample_rates":[10,6],"upsample_kernel_sizes":[20,12],"resblock_kernel_sizes":[3,7,11],"upsample_initial_channel":512},
            "plbert":{"hidden_size":768,"num_attention_heads":12,"num_hidden_layers":12,"intermediate_size":2048},"vocab":VOCAB}"#;
        let vocab: String = format!("{{{}}}", (0..110).map(|i| format!("\"{}\":{}", char::from_u32(0x100 + i).unwrap(), i + 1)).collect::<Vec<_>>().join(","));
        std::fs::write(&p, good.replace("VOCAB", &vocab)).unwrap();
        assert_eq!(read_config(&p).unwrap().vocab.len(), 110);
        std::fs::write(&p, good.replace("VOCAB", &vocab).replace("\"hidden_dim\":512", "\"hidden_dim\":256")).unwrap();
        assert!(read_config(&p).err().unwrap().to_string().contains("hidden_dim"));
        std::fs::write(&p, good.replace("VOCAB", "{\"a\":1}")).unwrap();
        assert!(read_config(&p).is_err(), "tiny vocabulary");
        for junk in ["", "{", "[]", "null", "{\"vocab\":5}"] {
            std::fs::write(&p, junk).unwrap();
            assert!(read_config(&p).is_err(), "{junk}");
        }
        std::fs::write(&p, vec![b' '; (1 << 20) + 1]).unwrap();
        assert!(read_config(&p).is_err(), "oversized");
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! CPU-only CLIP ViT-B/32 embeddings. No network, file discovery or automatic downloads.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub const DIMENSIONS: usize = 512;
pub const MODEL_ID: &str = "clip-vit-base-patch32-laion2b";

/// Model implementations can be substituted in engine tests without downloading weights.
pub trait Embedder: Send + Sync {
    fn text(&self, text: &str) -> Result<Vec<f32>, String>;
    fn image(&self, width: u32, height: u32, rgba: &[u8]) -> Result<Vec<f32>, String>;
}

pub fn available() -> bool {
    cfg!(feature = "clip")
}

/// Reject corrupt cached vectors rather than propagating NaNs or a wrong model's dimensions.
pub fn normalize(mut v: Vec<f32>) -> Result<Vec<f32>, String> {
    if v.len() != DIMENSIONS || v.iter().any(|n| !n.is_finite()) {
        return Err("invalid visual embedding".into());
    }
    let norm = v.iter().map(|n| f64::from(*n).powi(2)).sum::<f64>().sqrt();
    if !norm.is_finite() || norm < 1e-12 {
        return Err("empty visual embedding".into());
    }
    for n in &mut v {
        *n = (f64::from(*n) / norm) as f32;
    }
    Ok(v)
}

pub fn similarity(a: &[f32], b: &[f32]) -> Result<f32, String> {
    if a.len() != DIMENSIONS || b.len() != DIMENSIONS {
        return Err("wrong embedding dimensions".into());
    }
    let dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    if !dot.is_finite() {
        return Err("non-finite embedding score".into());
    }
    Ok(dot.clamp(-1.0, 1.0))
}

pub fn load(dir: &std::path::Path) -> Result<std::sync::Arc<dyn Embedder>, String> {
    #[cfg(feature = "clip")]
    {
        Ok(std::sync::Arc::new(clip::Clip::load(dir)?))
    }
    #[cfg(not(feature = "clip"))]
    {
        let _ = dir;
        Err("local visual search is unavailable in this build (enable visual-search)".into())
    }
}

#[cfg(feature = "clip")]
mod encoder;

#[cfg(feature = "clip")]
mod clip {
    use super::*;
    use crate::encoder::{ClipConfig, ClipModel};
    use candle_core::{DType, Device, Tensor};

    pub struct Clip {
        model: ClipModel,
        tokenizer: tokenizers::Tokenizer,
    }
    fn error(e: impl std::fmt::Display) -> String {
        e.to_string()
    }
    impl Clip {
        pub fn load(dir: &std::path::Path) -> Result<Self, String> {
            let weights = dir.join("pytorch_model.bin");
            if !std::fs::metadata(&weights).is_ok_and(|m| m.len() == 605239073) {
                return Err("CLIP model is not installed; download it from Text → Search → Visuals".into());
            }
            let vb = candle_nn::VarBuilder::from_pth(weights, DType::F32, &Device::Cpu).map_err(error)?;
            let config = ClipConfig::vit_base_patch32();
            let model = ClipModel::new(vb, &config).map_err(error)?;
            let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).map_err(error)?;
            Ok(Self { model, tokenizer })
        }
        fn vector(t: Tensor) -> Result<Vec<f32>, String> {
            normalize(t.flatten_all().and_then(|t| t.to_vec1::<f32>()).map_err(error)?)
        }
    }
    impl Embedder for Clip {
        fn text(&self, text: &str) -> Result<Vec<f32>, String> {
            if text.trim().is_empty() || text.len() > 1024 {
                return Err("visual query must contain 1–1024 bytes".into());
            }
            let encoded = self.tokenizer.encode(text, true).map_err(error)?;
            let ids = encoded.get_ids();
            if ids.is_empty() || ids.len() > 77 {
                return Err("visual query is too long for CLIP (77 tokens including delimiters)".into());
            }
            let input = Tensor::from_slice(ids, (1, ids.len()), &Device::Cpu).map_err(error)?;
            Self::vector(self.model.get_text_features(&input).map_err(error)?)
        }
        fn image(&self, width: u32, height: u32, rgba: &[u8]) -> Result<Vec<f32>, String> {
            let pixels = u64::from(width) * u64::from(height);
            if width == 0 || height == 0 || pixels > 16_777_216 || rgba.len() as u64 != pixels * 4 {
                return Err("invalid or oversized visual-search frame".into());
            }
            let image = image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or("invalid RGBA frame")?;
            // Match CLIP's shortest-side resize, centre crop and RGB mean/std preprocessing.
            let short = width.min(height);
            let w = (u64::from(width) * 224 / u64::from(short)) as u32;
            let h = (u64::from(height) * 224 / u64::from(short)) as u32;
            if w > 4096 || h > 4096 {
                return Err("visual-search frame aspect ratio is too extreme".into());
            }
            let image = image::imageops::resize(&image, w, h, image::imageops::FilterType::CatmullRom);
            let crop = image::imageops::crop_imm(&image, (w - 224) / 2, (h - 224) / 2, 224, 224).to_image();
            let mean = [0.48145466, 0.4578275, 0.40821073];
            let std = [0.26862954, 0.2613026, 0.2757771];
            let mut data = vec![0.0_f32; 3 * 224 * 224];
            for (i, p) in crop.pixels().enumerate() {
                for c in 0..3 {
                    data[c * 224 * 224 + i] = (f32::from(p[c]) / 255.0 - mean[c]) / std[c];
                }
            }
            let input = Tensor::from_vec(data, (1, 3, 224, 224), &Device::Cpu).map_err(error)?;
            Self::vector(self.model.get_image_features(&input).map_err(error)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_vectors_and_dimension_mismatches_fail() {
        assert!(normalize(vec![0.0; DIMENSIONS]).is_err());
        assert!(normalize(vec![f32::NAN; DIMENSIONS]).is_err());
        assert!(normalize(vec![1.0; DIMENSIONS - 1]).is_err());
        let a = normalize(vec![1.0; DIMENSIONS]).unwrap();
        assert!((similarity(&a, &a).unwrap() - 1.0).abs() < 1e-5);
        assert!(similarity(&a, &[]).is_err());
    }
}

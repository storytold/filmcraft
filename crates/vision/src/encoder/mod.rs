// Adapted from Hugging Face Candle 0.9.2 (Hugging Face contributors).
// Source: https://github.com/huggingface/candle/tree/3b39794c14b1de8ba4c2d10d49354d557a415547/candle-transformers/src/models/clip
// Licensed under Apache-2.0; see LICENSE-APACHE in this directory.
// Changes: standard GELU for the pinned LAION model; unused inference APIs removed.

//! Contrastive Language-Image Pre-Training
//!
//! Contrastive Language-Image Pre-Training (CLIP) is an architecture trained on
//! pairs of images with related texts.
//!
//! - 💻 [GH Link](https://github.com/openai/CLIP)
//! - 💻 Transformers Python [reference implementation](https://github.com/huggingface/transformers/tree/f6fa0f0bf0796ac66f201f23bdb8585de1609add/src/transformers/models/clip)
//! - 🤗 [HF Model](https://huggingface.co/openai/clip-vit-large-patch14-336)
//!

use self::{
    text_model::{Activation, ClipTextTransformer},
    vision_model::ClipVisionTransformer,
};
use candle_core::{Result, Tensor};

pub mod text_model;
pub mod vision_model;

#[derive(Clone, Debug)]
pub struct ClipModel {
    text_model: ClipTextTransformer,
    vision_model: ClipVisionTransformer,
    visual_projection: candle_nn::Linear,
    text_projection: candle_nn::Linear,
}

#[derive(Clone, Debug)]
pub enum EncoderConfig {
    Text(text_model::ClipTextConfig),
    Vision(vision_model::ClipVisionConfig),
}

impl EncoderConfig {
    pub fn embed_dim(&self) -> usize {
        match self {
            Self::Text(c) => c.embed_dim,
            Self::Vision(c) => c.embed_dim,
        }
    }

    pub fn num_attention_heads(&self) -> usize {
        match self {
            Self::Text(c) => c.num_attention_heads,
            Self::Vision(c) => c.num_attention_heads,
        }
    }

    pub fn intermediate_size(&self) -> usize {
        match self {
            Self::Text(c) => c.intermediate_size,
            Self::Vision(c) => c.intermediate_size,
        }
    }

    pub fn num_hidden_layers(&self) -> usize {
        match self {
            Self::Text(c) => c.num_hidden_layers,
            Self::Vision(c) => c.num_hidden_layers,
        }
    }

    pub fn activation(&self) -> Activation {
        match self {
            Self::Text(c) => c.activation,
            Self::Vision(c) => c.activation,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClipConfig {
    pub text_config: text_model::ClipTextConfig,
    pub vision_config: vision_model::ClipVisionConfig,
}

impl ClipConfig {
    // base image size is 224, model size is 600Mb
    pub fn vit_base_patch32() -> Self {
        let text_config = text_model::ClipTextConfig::vit_base_patch32();
        let vision_config = vision_model::ClipVisionConfig::vit_base_patch32();

        Self { text_config, vision_config }
    }
}

impl ClipModel {
    pub fn new(vs: candle_nn::VarBuilder, c: &ClipConfig) -> Result<Self> {
        let text_model = ClipTextTransformer::new(vs.pp("text_model"), &c.text_config)?;
        let vision_model = ClipVisionTransformer::new(vs.pp("vision_model"), &c.vision_config)?;
        let visual_projection = candle_nn::linear_no_bias(c.vision_config.embed_dim, c.vision_config.projection_dim, vs.pp("visual_projection"))?;
        let text_projection = candle_nn::linear_no_bias(c.text_config.embed_dim, c.text_config.projection_dim, vs.pp("text_projection"))?;
        Ok(Self { text_model, vision_model, visual_projection, text_projection })
    }

    pub fn get_text_features(&self, input_ids: &Tensor) -> Result<Tensor> {
        input_ids.apply(&self.text_model)?.apply(&self.text_projection)
    }

    pub fn get_image_features(&self, pixel_values: &Tensor) -> Result<Tensor> {
        pixel_values.apply(&self.vision_model)?.apply(&self.visual_projection)
    }
}

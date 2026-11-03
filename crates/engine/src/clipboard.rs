//! The application clipboard: the one place a Copy writes and a Paste reads.
//!
//! Whatever was copied last is what [`Session`](crate::Session)'s `edit.paste` applies, wherever it
//! was copied from: timeline clips with their sequence markers (`EditorState::clipboard`), captions,
//! the Effect Controls effects in [`EffectClipboard`], or the keyframes copied from its lane in
//! [`EffectKeyframeClipboard`]. Every Copy replaces the clipboard, so only one of them holds
//! anything at a time — copy keyframes and Paste pastes keyframes, copy clips and Paste pastes
//! clips.

use filmcraft_project::{EffectInstance, Keyframe};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

/// Effects copied in Effect Controls, with the clip and source time they came from, so a paste
/// retimes their keyframes to each target's source time ([`Session`](crate::Session)'s
/// `effects.paste`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct EffectClipboard {
    pub source_clip: u64,
    pub source_in: Tick,
    pub effects: Vec<EffectInstance>,
}

/// One copied keyframe of one animated parameter, timed from the playhead it was copied at.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectKeyframeClipboardEntry {
    pub effect: String,
    #[serde(default)]
    pub instance: usize,
    pub param: String,
    #[serde(default)]
    pub mask: Option<usize>,
    pub offset: Tick,
    pub keyframe: Keyframe,
}

/// The effect an instance of a copied keyframe came from, so pasting recreates a missing one.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectKeyframeTemplate {
    pub effect: String,
    #[serde(default)]
    pub instance: usize,
    pub template: EffectInstance,
}

/// Selected animated values and timing copied from the Effect Controls lane.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct EffectKeyframeClipboard {
    pub entries: Vec<EffectKeyframeClipboardEntry>,
    pub effects: Vec<EffectKeyframeTemplate>,
}

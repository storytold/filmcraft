//! Narrations: audio generated from text (the Text to Speech panel).
//!
//! A [`Narration`] belongs to the project item of the WAV file that was generated for it
//! (`Project::narrations`, keyed by the item id), so every clip of that item can be clicked to
//! reopen the script and voice settings. Editing a narration writes a new WAV (a new item) and
//! swaps the clip over to it; the old item and its narration stay, so undo restores them exactly.

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

/// Longest script accepted, in bytes.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Pace limits (speaking rate multiplier).
pub const MIN_PACE: f64 = 0.5;
pub const MAX_PACE: f64 = 2.0;

/// Text to Speech ▸ Advanced ▸ Vocal pitch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VocalPitch {
    ExtraLow,
    Low,
    #[default]
    Default,
    High,
    ExtraHigh,
}

impl VocalPitch {
    pub const ALL: [VocalPitch; 5] = [VocalPitch::ExtraLow, VocalPitch::Low, VocalPitch::Default, VocalPitch::High, VocalPitch::ExtraHigh];

    pub fn label(self) -> &'static str {
        match self {
            VocalPitch::ExtraLow => "Extra low",
            VocalPitch::Low => "Low",
            VocalPitch::Default => "Default",
            VocalPitch::High => "High",
            VocalPitch::ExtraHigh => "Extra high",
        }
    }

    /// The command / serde id (`extraLow`, `low`, `default`, `high`, `extraHigh`).
    pub fn id(self) -> &'static str {
        match self {
            VocalPitch::ExtraLow => "extraLow",
            VocalPitch::Low => "low",
            VocalPitch::Default => "default",
            VocalPitch::High => "high",
            VocalPitch::ExtraHigh => "extraHigh",
        }
    }

    pub fn from_id(s: &str) -> Option<VocalPitch> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        Self::ALL.into_iter().find(|p| p.id().to_ascii_lowercase() == n)
    }

    /// Pitch shift in semitones.
    pub fn semitones(self) -> f64 {
        match self {
            VocalPitch::ExtraLow => -6.0,
            VocalPitch::Low => -3.0,
            VocalPitch::Default => 0.0,
            VocalPitch::High => 3.0,
            VocalPitch::ExtraHigh => 6.0,
        }
    }
}

/// The script and voice settings a narration was generated from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Narration {
    /// The script, with pause markers (`[pause 1s]`) as typed.
    pub text: String,
    /// BCP 47 language tag (`en-US`).
    pub language: String,
    /// Voice id (`tts.voices`).
    pub voice: String,
    #[serde(default)]
    pub pitch: VocalPitch,
    /// Speaking rate, [`MIN_PACE`]–[`MAX_PACE`].
    pub pace: f64,
    /// Length of the speech in the generated file. The file can be longer: when an edit made the
    /// speech shorter than its clip, the file is padded with silence to the clip's length.
    pub speech_duration: Tick,
}

impl Narration {
    /// Check values read from a project file or a command.
    pub fn check(&self) -> Result<(), String> {
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(format!("the script is longer than {} KB", MAX_TEXT_BYTES / 1024));
        }
        if !(MIN_PACE..=MAX_PACE).contains(&self.pace) {
            return Err(format!("pace must be between {MIN_PACE} and {MAX_PACE}"));
        }
        if self.speech_duration < Tick::ZERO {
            return Err("negative speech duration".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Narration {
        Narration {
            text: "Hello there. [pause 1s] Welcome.".into(),
            language: "en-US".into(),
            voice: "basic-female".into(),
            pitch: VocalPitch::High,
            pace: 1.25,
            speech_duration: Tick::from_seconds_f64(2.5),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let n = sample();
        let s = serde_json::to_string(&n).unwrap();
        assert!(s.contains("\"pitch\":\"high\""), "{s}");
        assert_eq!(serde_json::from_str::<Narration>(&s).unwrap(), n);
    }

    #[test]
    fn pitch_ids_round_trip() {
        for p in VocalPitch::ALL {
            assert_eq!(VocalPitch::from_id(p.id()), Some(p));
        }
        assert_eq!(VocalPitch::from_id("Extra High"), Some(VocalPitch::ExtraHigh));
        assert_eq!(VocalPitch::from_id("loud"), None);
    }

    #[test]
    fn check_rejects_hostile_values() {
        let mut n = sample();
        n.pace = f64::NAN;
        assert!(n.check().is_err());
        n.pace = 3.0;
        assert!(n.check().is_err());
        let mut n = sample();
        n.text = "a".repeat(MAX_TEXT_BYTES + 1);
        assert!(n.check().is_err());
        let mut n = sample();
        n.speech_duration = Tick(-1);
        assert!(n.check().is_err());
        assert!(sample().check().is_ok());
    }
}

//! Multi-camera source sequences and merged clips.
//!
//! A **multi-camera source sequence** is an ordinary [`Sequence`](crate::Sequence) whose
//! [`Sequence::multicam`](crate::Sequence::multicam) describes its cameras: each camera (angle) is
//! one video track plus its audio tracks, with the clips already synchronised. Edited into another
//! sequence it is a nested clip; with [`TrackItem::multicam`](crate::TrackItem::multicam) enabled
//! the clip shows only the selected angle's video track (video clips) or plays the selected
//! angle's audio (audio clips, when the source switches audio), instead of the composite.
//!
//! A **merged clip** is a sequence too ([`Sequence::merged`](crate::Sequence::merged)): one video
//! clip and up to 16 separately recorded audio clips, synchronised, used like a single clip.

use serde::{Deserialize, Serialize};

use crate::{ItemId, Sequence, TrackId};

/// How a multi-camera source sequence plays audio (Create Multi-Camera Source Sequence ▸ Audio).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MulticamAudio {
    /// The audio of camera 1 (plus audio-only clips); the other cameras' audio tracks are muted.
    #[default]
    Camera1,
    /// Every camera's audio, mixed.
    AllCameras,
    /// The audio of the selected angle (plus audio-only clips): multi-camera audio clips switch
    /// with their angle.
    SwitchAudio,
}

impl MulticamAudio {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
            "camera1" | "cam1" => Some(Self::Camera1),
            "all" | "allcameras" => Some(Self::AllCameras),
            "switch" | "switchaudio" => Some(Self::SwitchAudio),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Camera1 => "camera1",
            Self::AllCameras => "all",
            Self::SwitchAudio => "switch",
        }
    }
}

/// One camera angle of a multi-camera source sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub name: String,
    /// The angle's video track (None for an audio-only source, which is never an angle of the
    /// grid but is mixed in).
    pub video_track: Option<TrackId>,
    /// The angle's audio tracks.
    #[serde(default)]
    pub audio_tracks: Vec<TrackId>,
    /// Shown in the Multi-Camera view (Edit Cameras).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The project item the angle was made from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ItemId>,
}

fn yes() -> bool {
    true
}

/// [`Sequence::multicam`](crate::Sequence::multicam): what makes a sequence a multi-camera source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MulticamSource {
    /// Angles in camera order (angle `n` = `cameras[n]`). Audio-only sources come last and have no
    /// video track.
    pub cameras: Vec<Camera>,
    pub audio: MulticamAudio,
    /// How the clips were synchronised (`in`, `out`, `timecode`, `marker`, `audio`), informational.
    #[serde(default)]
    pub sync: String,
}

impl MulticamSource {
    /// Angles that have a video track (the grid), in camera order: `(angle, camera)`.
    pub fn video_angles(&self) -> impl Iterator<Item = (usize, &Camera)> {
        self.cameras.iter().enumerate().filter(|(_, c)| c.video_track.is_some())
    }
    /// Enabled video angles: the cameras shown in the Multi-Camera view (keys 1–9 pick from these).
    pub fn shown_angles(&self) -> Vec<usize> {
        self.video_angles().filter(|(_, c)| c.enabled).map(|(i, _)| i).collect()
    }
    /// The audio tracks that play for `angle` (None = all of them, as in a plain nest).
    /// Audio-only sources always play; camera audio follows [`MulticamAudio`].
    pub fn audible_tracks(&self, angle: Option<usize>) -> Vec<TrackId> {
        let mut v = Vec::new();
        for (i, c) in self.cameras.iter().enumerate() {
            let audio_only = c.video_track.is_none();
            let on = audio_only
                || match self.audio {
                    MulticamAudio::Camera1 => i == self.first_video_angle().unwrap_or(0),
                    MulticamAudio::AllCameras => true,
                    MulticamAudio::SwitchAudio => Some(i) == angle.or(self.first_video_angle()),
                };
            if on {
                v.extend(c.audio_tracks.iter().copied());
            }
        }
        v
    }
    pub fn first_video_angle(&self) -> Option<usize> {
        self.video_angles().map(|(i, _)| i).next()
    }
}

/// [`TrackItem::multicam`](crate::TrackItem::multicam): the angle a multi-camera clip shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MulticamSel {
    /// Multi-Camera ▸ Enable. Off: the clip plays like any nested sequence.
    pub enabled: bool,
    /// Camera index into [`MulticamSource::cameras`] (video clips: the picture; audio clips: the
    /// sound when the source switches audio).
    pub angle: u32,
}

/// [`Sequence::merged`](crate::Sequence::merged): what makes a sequence a merged clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MergedClip {
    /// The video clip (None: audio-only merge).
    pub video: Option<ItemId>,
    pub audio: Vec<ItemId>,
    /// How the clips were synchronised.
    #[serde(default)]
    pub sync: String,
}

impl crate::TrackItem {
    /// The angle this clip shows when it is an enabled multi-camera clip, else None (it plays as an
    /// ordinary nest). Any nested sequence can be a multi-camera clip: without camera data its
    /// video tracks are the angles.
    pub fn multicam_angle(&self, _nested: &Sequence) -> Option<usize> {
        self.multicam.filter(|m| m.enabled).map(|m| m.angle as usize)
    }
}

impl Sequence {
    /// The cameras of this sequence used as a multi-camera source: its camera data, or for a plain
    /// sequence one camera per video track (named after the track) with all audio mixed.
    pub fn cameras(&self) -> MulticamSource {
        if let Some(m) = &self.multicam {
            return m.clone();
        }
        MulticamSource {
            cameras: self
                .video_tracks
                .iter()
                .enumerate()
                .map(|(i, t)| Camera {
                    name: t.name.clone(),
                    video_track: Some(t.id),
                    audio_tracks: self.audio_tracks.get(i).map(|a| vec![a.id]).unwrap_or_default(),
                    enabled: true,
                    source: None,
                })
                .collect(),
            audio: MulticamAudio::AllCameras,
            sync: String::new(),
        }
    }
    /// Video track index of `angle` when this sequence is used as a multi-camera source.
    pub fn angle_video_track_index(&self, angle: usize) -> Option<usize> {
        match &self.multicam {
            Some(mc) => {
                let id = mc.cameras.get(angle)?.video_track?;
                self.video_tracks.iter().position(|t| t.id == id)
            }
            None => (angle < self.video_tracks.len()).then_some(angle),
        }
    }
    /// A copy whose audio tracks are muted except those audible for `angle` (None = the source's
    /// default: camera 1, all, or the first angle when switching). Plain sequences are unchanged.
    pub fn with_angle_audio(&self, angle: Option<usize>) -> Sequence {
        let mut q = self.clone();
        if let Some(mc) = &self.multicam {
            let on = mc.audible_tracks(angle);
            for t in &mut q.audio_tracks {
                if !on.contains(&t.id) {
                    t.muted = true;
                }
            }
        }
        q
    }
}

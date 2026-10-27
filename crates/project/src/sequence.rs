use super::track::{AudioChannels, Marker, Track, TrackItem, TrackKind};
use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub par: (u32, u32),
    pub sample_rate: u32,
    pub drop_frame: bool,
    /// Editing mode / preset name shown in the settings dialog.
    pub preset: String,
    pub audio_master: AudioChannels,
    /// Preview file codec name.
    pub preview_codec: String,
    pub max_bit_depth: bool,
    pub max_render_quality: bool,
    /// Working colour space (display label; [`SequenceSettings::color`] is authoritative).
    pub working_space: String,
    /// Colour pipeline: working space (Rec. 709 / Rec. 2100 PQ / HLG), wide-gamut compositing,
    /// Auto Tone Map Media.
    #[serde(default = "default_color_pipeline")]
    pub color: filmcraft_color::ColorPipeline,
}

fn default_color_pipeline() -> filmcraft_color::ColorPipeline {
    filmcraft_color::ColorPipeline::REC709
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::FPS_23_976,
            par: (1, 1),
            sample_rate: 48_000,
            drop_frame: false,
            preset: "HD 1080p 23.976".into(),
            audio_master: AudioChannels::Stereo,
            preview_codec: "ProRes 422".into(),
            max_bit_depth: false,
            max_render_quality: false,
            working_space: "Rec. 709".into(),
            color: filmcraft_color::ColorPipeline::REC709,
        }
    }
}

/// Largest frame side, in pixels.
pub const MAX_FRAME_SIDE: u32 = 32_768;

/// Largest frame area, in pixels (16384 x 16384: 16K and 16384 x 8192 panoramas fit).
pub const MAX_FRAME_PIXELS: u64 = 268_435_456;

/// Bound working-frame allocations while retaining 16K and wide panoramic frames.
pub fn validate_frame_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_FRAME_SIDE || height > MAX_FRAME_SIDE || u64::from(width) * u64::from(height) > MAX_FRAME_PIXELS {
        return Err(format!("frame size must be positive, at most {MAX_FRAME_SIDE} pixels per side and {MAX_FRAME_PIXELS} pixels total"));
    }
    Ok(())
}

/// The most a pixel aspect ratio may stretch a picture either way (8:1 or 1:8). Real footage stays
/// well inside (2:1 for 2x anamorphic lenses, 4:3 for HDV, 0.9 for NTSC DV); a ratio beyond it is
/// garbage from a damaged file and is treated as square.
pub const MAX_PIXEL_ASPECT: f64 = 8.0;

/// A pixel aspect ratio fit to compute with, in lowest terms, or `None` when it is unset (a zero
/// term, as decoders report "not signalled") or stretches more than [`MAX_PIXEL_ASPECT`] either
/// way. Ratios come from media, project and interchange files, so they are untrusted.
pub fn checked_par(par: (u32, u32)) -> Option<(u32, u32)> {
    let (n, d) = par;
    if n == 0 || d == 0 {
        return None;
    }
    let r = f64::from(n) / f64::from(d);
    if !(1.0 / MAX_PIXEL_ASPECT..=MAX_PIXEL_ASPECT).contains(&r) {
        return None;
    }
    let (mut a, mut b) = (n, d);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let g = a.max(1);
    Some((n / g, d / g))
}

/// [`checked_par`], with square pixels for a ratio that is unset or makes no sense.
pub fn sane_par(par: (u32, u32)) -> (u32, u32) {
    checked_par(par).unwrap_or((1, 1))
}

/// The width of one pixel over its height ([`sane_par`] first): 1.333… for 4:3 pixels.
pub fn par_ratio(par: (u32, u32)) -> f64 {
    let (n, d) = sane_par(par);
    f64::from(n) / f64::from(d)
}

/// The size of a `size` picture with `par` pixels in the pixels of a frame whose pixel aspect is
/// `frame_par`: the width scales by par / frame_par, the height stays. A 1440 x 1080 picture with
/// 4:3 pixels covers 1920 x 1080 square pixels.
pub fn conformed_size(size: (u32, u32), par: (u32, u32), frame_par: (u32, u32)) -> (f64, f64) {
    let (par, frame_par) = (sane_par(par), sane_par(frame_par));
    let w = if par == frame_par { f64::from(size.0) } else { f64::from(size.0) * par_ratio(par) / par_ratio(frame_par) };
    (w, f64::from(size.1))
}

impl SequenceSettings {
    pub fn validate(&self) -> Result<(), String> {
        validate_frame_size(self.width, self.height)?;
        if self.frame_rate.num <= 0
            || self.frame_rate.den <= 0
            || self.frame_rate.as_f64() > 1000.0
            || self.frame_rate.num > i64::from(u32::MAX)
            || self.frame_rate.den > i64::from(u32::MAX)
            || (i128::from(TICKS_PER_SECOND) * i128::from(self.frame_rate.den) / i128::from(self.frame_rate.num.max(1))) > i128::from(Tick::MAX.0)
        {
            return Err("frame rate must be positive, at most 1000 fps, with unsigned 32-bit rational components".into());
        }
        if self.sample_rate == 0 || self.sample_rate > 384_000 {
            return Err("sample rate must be between 1 and 384000 Hz".into());
        }
        if self.par.0 == 0 || self.par.1 == 0 {
            return Err("pixel aspect ratio must be positive".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub mark_in: Option<Tick>,
    pub mark_out: Option<Tick>,
    /// Work area bar (optional; Premiere hides it by default now).
    pub work_area: Option<TimeRange>,
    /// Timecode of the first frame (frames), "Start Time" in Sequence settings.
    pub start_timecode: i64,
    /// Master audio volume (dB).
    #[serde(default)]
    pub master_volume_db: f64,
    /// Mix track inserts (`EffectInstance::post_fader` picks the side).
    #[serde(default)]
    pub master_effects: Vec<EffectInstance>,
    /// Mix track automation (`volume` lane) and mode.
    #[serde(default)]
    pub master_mixer: MixerStrip,
    /// Audio submix tracks (no clips); tracks and sends route into them, they route to the Mix
    /// or to a submix after them.
    #[serde(default)]
    pub submix_tracks: Vec<Track>,
    /// Caption tracks (drawn above the video tracks; first = top). Older files have none (serde default).
    #[serde(default)]
    pub caption_tracks: Vec<CaptionTrack>,
    /// Set on multi-camera source sequences (cameras, audio mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam: Option<MulticamSource>,
    /// Set on merged clips (the merged video and audio items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<MergedClip>,
    /// Split In/Out points (Markers ▸ Mark Split): per-channel overrides of `mark_in`/`mark_out`.
    #[serde(default, skip_serializing_if = "SplitMarks::is_empty")]
    pub split: SplitMarks,
}

/// Split edit points: separate video and audio In/Out points (Markers ▸ Mark Split). `None` means
/// the channel uses the ordinary In/Out point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SplitMarks {
    pub video_in: Option<Tick>,
    pub video_out: Option<Tick>,
    pub audio_in: Option<Tick>,
    pub audio_out: Option<Tick>,
}

impl SplitMarks {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// The video In point given the ordinary In point.
    pub fn video_in_or(&self, mark_in: Option<Tick>) -> Option<Tick> {
        self.video_in.or(mark_in)
    }
    pub fn video_out_or(&self, mark_out: Option<Tick>) -> Option<Tick> {
        self.video_out.or(mark_out)
    }
    pub fn audio_in_or(&self, mark_in: Option<Tick>) -> Option<Tick> {
        self.audio_in.or(mark_in)
    }
    pub fn audio_out_or(&self, mark_out: Option<Tick>) -> Option<Tick> {
        self.audio_out.or(mark_out)
    }
}

impl Sequence {
    pub fn duration(&self) -> Tick {
        let media = self.video_tracks.iter().chain(&self.audio_tracks).map(Track::end).max().unwrap_or(Tick::ZERO);
        media.max(self.caption_tracks.iter().map(CaptionTrack::end).max().unwrap_or(Tick::ZERO))
    }
    pub fn caption_track(&self, id: TrackId) -> Option<&CaptionTrack> {
        self.caption_tracks.iter().find(|t| t.id == id)
    }
    pub fn caption_track_mut(&mut self, id: TrackId) -> Option<&mut CaptionTrack> {
        self.caption_tracks.iter_mut().find(|t| t.id == id)
    }
    /// Find a caption anywhere: (caption track id, caption).
    pub fn find_caption(&self, id: ClipId) -> Option<(TrackId, &Caption)> {
        self.caption_tracks.iter().find_map(|t| t.caption(id).map(|c| (t.id, c)))
    }
    pub fn tracks(&self, kind: TrackKind) -> &Vec<Track> {
        match kind {
            TrackKind::Video => &self.video_tracks,
            TrackKind::Audio => &self.audio_tracks,
        }
    }
    pub fn tracks_mut(&mut self, kind: TrackKind) -> &mut Vec<Track> {
        match kind {
            TrackKind::Video => &mut self.video_tracks,
            TrackKind::Audio => &mut self.audio_tracks,
        }
    }
    pub fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.video_tracks.iter().chain(self.audio_tracks.iter())
    }
    pub fn all_tracks_mut(&mut self) -> impl Iterator<Item = &mut Track> {
        self.video_tracks.iter_mut().chain(self.audio_tracks.iter_mut())
    }
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.all_tracks().find(|t| t.id == id)
    }
    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.all_tracks_mut().find(|t| t.id == id)
    }
    /// Find a track item anywhere: (track id, item).
    pub fn find_item(&self, id: ClipId) -> Option<(TrackId, &TrackItem)> {
        self.all_tracks().find_map(|t| t.item(id).map(|i| (t.id, i)))
    }
    pub fn find_item_mut(&mut self, id: ClipId) -> Option<(TrackId, &mut TrackItem)> {
        self.all_tracks_mut().find_map(|t| {
            let tid = t.id;
            t.item_mut(id).map(|i| (tid, i))
        })
    }
    pub fn frame_rate(&self) -> FrameRate {
        self.settings.frame_rate
    }
    /// Sorted, de-duplicated edit points (clip boundaries) on all tracks.
    pub fn edit_points(&self) -> Vec<Tick> {
        let mut v: Vec<Tick> = self.all_tracks().flat_map(|t| t.items.iter().flat_map(|i| [i.start, i.end()])).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
    pub fn check(&self) -> Result<(), String> {
        self.check_bounds()?;
        for t in self.all_tracks() {
            t.check()?;
        }
        for t in &self.caption_tracks {
            t.check()?;
        }
        Ok(())
    }
    /// The hostile-value bounds alone (settings, marks and every time range), without the
    /// structural invariants of [`Sequence::check`]: a project that breaks only those (e.g. an
    /// overlap written by an older build) still opens, but nothing in it can overflow.
    pub fn check_bounds(&self) -> Result<(), String> {
        self.settings.validate()?;
        if self.mark_in.into_iter().chain(self.mark_out).any(|t| t < Tick::MIN || t > Tick::MAX)
            || self.work_area.is_some_and(|r| !bounded_time_range(r.start, r.duration))
        {
            return Err("sequence marks or work area are outside supported time bounds".into());
        }
        for t in self.all_tracks() {
            t.check_bounds()?;
        }
        for t in &self.caption_tracks {
            t.check_bounds()?;
        }
        Ok(())
    }
}

/// Bound persisted timeline arithmetic before any start + duration operation: start, duration and
/// end all stay within `Tick::MIN..=Tick::MAX`, leaving headroom for later edit math. Negative
/// positions remain supported; the sign of the duration is a structural rule (track items and
/// captions require a positive one in their own invariant checks), not a bound.
pub(crate) fn bounded_time_range(start: Tick, duration: Tick) -> bool {
    let within = |t: i64| (Tick::MIN.0..=Tick::MAX.0).contains(&t);
    within(start.0) && within(duration.0) && start.0.checked_add(duration.0).is_some_and(within)
}

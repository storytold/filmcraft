//! The corpus: media we generate ourselves and the test projects authored on it.
//!
//! Every media file is rendered by FilmCraft from its own procedural generators (demo footage,
//! Bars and Tone, a tone) into ProRes 422 HQ QuickTime and 24-bit WAV, so the corpus is original
//! work, reproducible from nothing, and readable by Premiere Pro. Each [`CaseDef`] builds one
//! small sequence that isolates one thing (a cut, a Motion parameter, a keyframe ramp, a
//! transition, one effect, a speed change, an audio level): what Premiere renders of it is then
//! comparable with what we render, number by number.
//!
//! Cases whose whole content survives FCP7 XML (`xml_complete`) need nothing but an import on
//! the Premiere side; the others carry the manual steps (`premiere_steps`) a person applies after
//! the import, because FCP7 XML has no vocabulary for Premiere's standard effects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_export::{AudioSettings, ExportSettings, Format, Progress};
use filmcraft_geom::Vec2;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource, SharedSource};
use filmcraft_project::{
    ClipId, EffectInstance, ItemId, ItemKind, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, TrackItem, TrackKind, Transition,
    TransitionId, find_effect, resolve_auto_points,
};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde::{Deserialize, Serialize};

use crate::Error;

/// Size, rate and length of every corpus sequence (and of the media, which is longer).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub sample_rate: u32,
    /// Length of every case's sequence in seconds.
    pub seconds: u32,
}

impl Default for CorpusSettings {
    fn default() -> Self {
        Self { width: 640, height: 360, frame_rate: FrameRate::FPS_24, sample_rate: 48_000, seconds: 3 }
    }
}

impl CorpusSettings {
    /// Frames in one case's sequence.
    pub fn frames(&self) -> i64 {
        i64::from(self.seconds) * self.frame_rate.num / self.frame_rate.den.max(1)
    }

    pub fn duration(&self) -> Tick {
        self.frame_rate.tick_of(self.frames())
    }

    /// Media is long enough for a 200 % speed clip over the whole sequence, plus a handle.
    pub fn media_duration(&self) -> Tick {
        Tick(TICKS_PER_SECOND.saturating_mul(i64::from(self.seconds).saturating_mul(2).saturating_add(1)))
    }

    /// Audio samples in one case's sequence.
    pub fn samples(&self) -> i64 {
        i64::from(self.seconds) * i64::from(self.sample_rate)
    }
}

/// The generated media files of a corpus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub footage_a: PathBuf,
    pub footage_b: PathBuf,
    pub bars: PathBuf,
    pub tone: PathBuf,
}

/// Which media file a case places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaFile {
    /// Demo footage with its own (noise and beat) sound track.
    FootageA,
    /// A second, visually different demo footage with sound.
    FootageB,
    /// Bars and Tone.
    Bars,
    /// A −12 dBFS 440 Hz tone, audio only.
    Tone,
}

impl Media {
    pub fn path(&self, f: MediaFile) -> &Path {
        match f {
            MediaFile::FootageA => &self.footage_a,
            MediaFile::FootageB => &self.footage_b,
            MediaFile::Bars => &self.bars,
            MediaFile::Tone => &self.tone,
        }
    }

    pub fn files(&self) -> [&Path; 4] {
        [&self.footage_a, &self.footage_b, &self.bars, &self.tone]
    }
}

/// The sidecar that records which settings the media in a directory was generated with.
pub const MEDIA_SETTINGS_FILE: &str = "settings.json";

/// Generate the media files into `dir`. A present file is kept while `settings.json` beside it
/// records the same settings (the generators are deterministic, so it is the right file); media
/// made with other settings (`--size`, `--seconds`) is removed and generated again.
pub fn ensure_media(dir: &Path, s: &CorpusSettings) -> Result<Media, Error> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    let media = Media { footage_a: dir.join("footage_a.mov"), footage_b: dir.join("footage_b.mov"), bars: dir.join("bars.mov"), tone: dir.join("tone.wav") };
    let stamp = dir.join(MEDIA_SETTINGS_FILE);
    let recorded = std::fs::read(&stamp).ok().and_then(|b| serde_json::from_slice::<CorpusSettings>(&b).ok());
    if recorded != Some(*s) {
        for path in media.files().into_iter().chain(std::iter::once(stamp.as_path())) {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::Io(path.to_path_buf(), e)),
            }
        }
    }
    generate(&media.footage_a, Generator::Demo(DemoScene::OceanSunset), Format::ProRes, s)?;
    generate(&media.footage_b, Generator::Demo(DemoScene::CityNight), Format::ProRes, s)?;
    generate(&media.bars, Generator::BarsAndTone, Format::ProRes, s)?;
    generate(&media.tone, Generator::Tone { hz: 440.0, db: -12.0 }, Format::Wav, s)?;
    // written last: a run that stops half-way leaves no stamp, and the next one starts over
    let json = serde_json::to_vec_pretty(s).map_err(|e| Error::Other(format!("media settings: {e}")))?;
    std::fs::write(&stamp, json).map_err(|e| Error::Io(stamp.clone(), e))?;
    Ok(media)
}

fn generate(path: &Path, generator: Generator, format: Format, s: &CorpusSettings) -> Result<(), Error> {
    if std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0) {
        return Ok(());
    }
    let dur = s.media_duration();
    let mut p = Project::new("fidelity-media");
    let settings = SequenceSettings { width: s.width, height: s.height, frame_rate: s.frame_rate, sample_rate: s.sample_rate, ..Default::default() };
    let seq = p.new_sequence("media", settings, 1, 1, None);
    let src = GeneratorSource::new(generator.clone(), s.width, s.height, s.frame_rate, dur);
    let info = src.info().clone();
    let clip = MediaClip {
        media: MediaRef::Generator(generator),
        info: info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: Vec::new(),
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item(&info.name, Label::Iris, ItemKind::Media(clip), None);
    let range = TimeRange::new(Tick::ZERO, dur);
    let bad = |what: &str| Error::Media(path.to_path_buf(), what.to_string());
    if info.has_video() {
        let ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, range, s.frame_rate).ok_or_else(|| bad("cannot place the generator on V1"))?;
        p.sequence_mut(seq).and_then(|q| q.video_tracks.get_mut(0)).ok_or_else(|| bad("no V1"))?.items.push(ti);
    }
    if info.has_audio() {
        let ti = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, s.frame_rate).ok_or_else(|| bad("cannot place the generator on A1"))?;
        p.sequence_mut(seq).and_then(|q| q.audio_tracks.get_mut(0)).ok_or_else(|| bad("no A1"))?.items.push(ti);
    }
    let mut map = SourceMap::default();
    map.0.insert(item, Arc::new(src) as SharedSource);
    // write beside the final name, then rename: a half-written file is never taken for media
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let partial = path.with_extension(format!("partial.{ext}"));
    let export = ExportSettings {
        format,
        path: partial.to_string_lossy().to_string(),
        range: Some(range),
        include_audio: info.has_audio(),
        prores_profile: "hq".into(),
        audio: AudioSettings { bits: 24, ..Default::default() },
        ..Default::default()
    };
    filmcraft_export::export(&Arc::new(p), seq, &export, &map, &Progress::default()).map_err(|e| Error::Export(path.to_path_buf(), e.to_string()))?;
    std::fs::rename(&partial, path).map_err(|e| Error::Io(path.to_path_buf(), e))
}

/// What a case's scores are judged against. Fields that don't apply to the case's kind are
/// `None`. The defaults are provisional until calibrated on real Premiere renders: the baseline
/// `cut` case shows the floor set by codec and colour-pipeline differences alone, and every other
/// video case should be read against it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tolerance {
    pub min_psnr_db: Option<f64>,
    pub max_de2000_mean: Option<f64>,
    pub max_de2000_p95: Option<f64>,
    pub min_snr_db: Option<f64>,
    pub max_lag_samples: Option<i64>,
}

impl Tolerance {
    /// A video case: PSNR ≥ 35 dB, mean ΔE₀₀ ≤ 2, 95th percentile ≤ 6.
    pub const VIDEO: Tolerance =
        Tolerance { min_psnr_db: Some(35.0), max_de2000_mean: Some(2.0), max_de2000_p95: Some(6.0), min_snr_db: None, max_lag_samples: None };
    /// The baseline: a plain cut should decode alike within codec rounding (PSNR ≥ 40 dB, mean
    /// ΔE₀₀ ≤ 1).
    pub const BASELINE: Tolerance =
        Tolerance { min_psnr_db: Some(40.0), max_de2000_mean: Some(1.0), max_de2000_p95: Some(3.0), min_snr_db: None, max_lag_samples: None };
    /// An audio case: SNR ≥ 40 dB at an alignment within 2 samples.
    pub const AUDIO: Tolerance = Tolerance { min_psnr_db: None, max_de2000_mean: None, max_de2000_p95: None, min_snr_db: Some(40.0), max_lag_samples: Some(2) };
}

/// Which side of a case is scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compare {
    Video,
    Audio,
}

/// One authored test project.
/// One effect a script applies to the clip on V1 in Premiere after the import: its Effects panel
/// name on Premiere Pro 2026 and the Effect Controls values that differ from the defaults. The
/// same thing as the case's `premiere_steps`, machine-readable (`premiere/render.py` runs them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PremiereEffect {
    pub name: &'static str,
    pub params: &'static [(&'static str, PremiereValue)],
}

/// A value of an Effect Controls property.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PremiereValue {
    Number(f64),
    Bool(bool),
}

impl From<PremiereValue> for serde_json::Value {
    fn from(v: PremiereValue) -> Self {
        match v {
            PremiereValue::Number(n) => serde_json::Value::from(n),
            PremiereValue::Bool(b) => serde_json::Value::from(b),
        }
    }
}

pub struct CaseDef {
    pub id: &'static str,
    pub title: &'static str,
    /// Report group: Baseline, Motion, Opacity, Transitions, Effects, Speed, Audio.
    pub group: &'static str,
    /// The Premiere feature under test, as its Effect Controls / Effects panel name.
    pub effect: &'static str,
    pub compare: Compare,
    /// What difference this case isolates.
    pub about: &'static str,
    /// Whether FCP7 XML carries everything (nothing to do in Premiere but import and export).
    pub xml_complete: bool,
    /// What to do in Premiere after the import, when the XML cannot carry it.
    pub premiere_steps: &'static [&'static str],
    /// The same steps for a script (empty iff `xml_complete`).
    pub premiere_effects: &'static [PremiereEffect],
    /// Set while the case is known to fail against Premiere for a filed reason: the failure is
    /// reported as *known* and left out of the pass rate, and the mark comes off with the fix.
    pub known_failure: Option<&'static str>,
    pub tolerance: Tolerance,
    build: fn(&mut Builder) -> Result<(), Error>,
}

/// A case built on the generated media, ready to render and export.
pub struct BuiltCase {
    pub def: &'static CaseDef,
    pub project: Arc<Project>,
    pub seq: ItemId,
    pub sources: SourceMap,
    pub range: TimeRange,
}

/// Build `def` on `media`.
pub fn build(def: &'static CaseDef, media: &Media, s: &CorpusSettings) -> Result<BuiltCase, Error> {
    let mut b = Builder::new(def, media, s)?;
    (def.build)(&mut b)?;
    let range = TimeRange::new(Tick::ZERO, s.duration());
    Ok(BuiltCase { def, project: Arc::new(b.project), seq: b.seq, sources: b.sources, range })
}

pub fn case(id: &str) -> Option<&'static CaseDef> {
    CASES.iter().find(|c| c.id == id)
}

/// Builds one case's project: a sequence of the corpus size with two video and two audio tracks.
pub struct Builder {
    settings: CorpusSettings,
    media: Media,
    project: Project,
    seq: ItemId,
    sources: SourceMap,
    imported: HashMap<MediaFile, ItemId>,
    next_transition: u64,
    case: &'static str,
}

impl Builder {
    fn new(def: &CaseDef, media: &Media, s: &CorpusSettings) -> Result<Self, Error> {
        let mut project = Project::new(def.id);
        let settings = SequenceSettings { width: s.width, height: s.height, frame_rate: s.frame_rate, sample_rate: s.sample_rate, ..Default::default() };
        let seq = project.new_sequence(def.id, settings, 2, 2, None);
        Ok(Self { settings: *s, media: media.clone(), project, seq, sources: SourceMap::default(), imported: HashMap::new(), next_transition: 1, case: def.id })
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error::Case(self.case.to_string(), msg.into())
    }

    /// Frames in the sequence.
    pub fn frames(&self) -> i64 {
        self.settings.frames()
    }

    fn tick(&self, frame: i64) -> Tick {
        self.settings.frame_rate.tick_of(frame)
    }

    /// Import a corpus media file (once) as a project item backed by the file on disk, read
    /// through the same decoders the app uses.
    pub fn import(&mut self, f: MediaFile) -> Result<ItemId, Error> {
        if let Some(id) = self.imported.get(&f) {
            return Ok(*id);
        }
        let path = self.media.path(f).to_path_buf();
        let src = crate::reference::open_media(&path).map_err(|e| Error::Media(path.clone(), e))?;
        let info = src.info().clone();
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| info.name.clone());
        let clip = MediaClip {
            media: MediaRef::File { path: path.to_string_lossy().to_string() },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            offline: false,
            proxy: None,
            identity: None,
        };
        let id = self.project.add_item(&name, Label::Iris, ItemKind::Media(clip), None);
        self.sources.0.insert(id, src);
        self.imported.insert(f, id);
        Ok(id)
    }

    fn place(&mut self, kind: TrackKind, track: usize, item: ItemId, start: i64, dur: i64, source_in: i64) -> Result<ClipId, Error> {
        if dur <= 0 || start < 0 || source_in < 0 {
            return Err(self.err(format!("bad placement: start {start}, duration {dur}, source in {source_in}")));
        }
        let rate = self.settings.frame_rate;
        let source = TimeRange::new(self.tick(source_in), self.tick(dur));
        let mut ti = self.project.make_track_item(item, kind, self.tick(start), source, rate).ok_or_else(|| self.err("item cannot be placed"))?;
        if kind == TrackKind::Video {
            let frame = (self.settings.width, self.settings.height);
            let src = filmcraft_render::source_size(&self.project, item).unwrap_or(frame);
            for e in &mut ti.effects {
                resolve_auto_points(e, frame, src);
            }
        }
        let id = ti.id;
        let seq = self.project.sequence_mut(self.seq).ok_or_else(|| Error::Case(self.case.to_string(), "no sequence".into()))?;
        let tracks = match kind {
            TrackKind::Video => &mut seq.video_tracks,
            TrackKind::Audio => &mut seq.audio_tracks,
        };
        let t = tracks.get_mut(track).ok_or_else(|| Error::Case(self.case.to_string(), format!("no track {track}")))?;
        if t.items.iter().any(|x| x.start < ti.start + ti.duration && ti.start < x.start + x.duration) {
            return Err(Error::Case(self.case.to_string(), format!("overlapping clips on track {track}")));
        }
        t.items.push(ti);
        t.sort();
        Ok(id)
    }

    /// Place `item` on video track `track` (0 = V1): `start` and `dur` in sequence frames,
    /// `source_in` in media frames.
    pub fn video(&mut self, track: usize, item: ItemId, start: i64, dur: i64, source_in: i64) -> Result<ClipId, Error> {
        self.place(TrackKind::Video, track, item, start, dur, source_in)
    }

    /// Place `item`'s sound on audio track `track` (0 = A1).
    pub fn audio(&mut self, track: usize, item: ItemId, start: i64, dur: i64, source_in: i64) -> Result<ClipId, Error> {
        self.place(TrackKind::Audio, track, item, start, dur, source_in)
    }

    fn clip_mut(&mut self, clip: ClipId) -> Result<&mut TrackItem, Error> {
        let case = self.case;
        let seq = self.project.sequence_mut(self.seq).ok_or_else(|| Error::Case(case.to_string(), "no sequence".into()))?;
        seq.find_item_mut(clip).map(|(_, ti)| ti).ok_or_else(|| Error::Case(case.to_string(), format!("no clip {clip:?}")))
    }

    fn effect_mut(&mut self, clip: ClipId, effect: &str) -> Result<&mut EffectInstance, Error> {
        let case = self.case;
        self.clip_mut(clip)?.effect_mut(effect).ok_or_else(|| Error::Case(case.to_string(), format!("clip has no effect {effect}")))
    }

    /// Set a parameter of an effect the clip already has (intrinsic `motion`, `opacity`,
    /// `volume`, `panner`, or one added with [`Builder::add_effect`]).
    pub fn set(&mut self, clip: ClipId, effect: &str, param: &str, value: ParamValue) -> Result<(), Error> {
        let case = self.case;
        let p = self.effect_mut(clip, effect)?.param_mut(param).ok_or_else(|| Error::Case(case.to_string(), format!("{effect} has no parameter {param}")))?;
        p.value = value;
        Ok(())
    }

    /// Add a keyframe at clip frame `frame` (linear interpolation, Premiere's default for these
    /// parameters).
    pub fn keyframe(&mut self, clip: ClipId, effect: &str, param: &str, frame: i64, value: ParamValue) -> Result<(), Error> {
        let case = self.case;
        let t = self.tick(frame);
        let p = self.effect_mut(clip, effect)?.param_mut(param).ok_or_else(|| Error::Case(case.to_string(), format!("{effect} has no parameter {param}")))?;
        p.put_keyframe(t, value);
        Ok(())
    }

    /// Append a standard effect with parameter values.
    pub fn add_effect(&mut self, clip: ClipId, effect: &str, params: &[(&str, ParamValue)]) -> Result<(), Error> {
        let mut e = find_effect(effect).ok_or_else(|| self.err(format!("no effect {effect}")))?.instance();
        for (k, v) in params {
            e.param_mut(k).ok_or_else(|| self.err(format!("{effect} has no parameter {k}")))?.value = v.clone();
        }
        self.clip_mut(clip)?.effects.push(e);
        Ok(())
    }

    /// A transition of `dur` frames centred on the cut at sequence frame `cut`, on video or
    /// audio track `track`.
    pub fn transition(
        &mut self,
        kind: TrackKind,
        track: usize,
        effect: &str,
        from: Option<ClipId>,
        to: Option<ClipId>,
        cut: i64,
        dur: i64,
    ) -> Result<(), Error> {
        if dur <= 0 || dur % 2 != 0 {
            return Err(self.err(format!("transition duration {dur} must be even and positive")));
        }
        let e = find_effect(effect).ok_or_else(|| self.err(format!("no transition {effect}")))?.instance();
        let tr = Transition {
            id: TransitionId(self.next_transition),
            effect: e,
            start: self.tick(cut - dur / 2),
            duration: self.tick(dur),
            from,
            to,
            align: Default::default(),
            reverse: false,
        };
        self.next_transition += 1;
        let case = self.case;
        let seq = self.project.sequence_mut(self.seq).ok_or_else(|| Error::Case(case.to_string(), "no sequence".into()))?;
        let tracks = match kind {
            TrackKind::Video => &mut seq.video_tracks,
            TrackKind::Audio => &mut seq.audio_tracks,
        };
        tracks.get_mut(track).ok_or_else(|| Error::Case(case.to_string(), format!("no track {track}")))?.transitions.push(tr);
        Ok(())
    }

    /// Play the clip at `speed` (1.0 = normal) over its unchanged timeline duration.
    pub fn speed(&mut self, clip: ClipId, speed: f64) -> Result<(), Error> {
        if !(speed.is_finite() && speed > 0.0) {
            return Err(self.err(format!("bad speed {speed}")));
        }
        self.clip_mut(clip)?.speed = speed;
        Ok(())
    }
}

fn fl(v: f64) -> ParamValue {
    ParamValue::Float(v)
}

// ---- the cases ----

/// Clip A on V1 for the first half, clip B for the second.
fn cut(b: &mut Builder) -> Result<(), Error> {
    let (a, c) = (b.import(MediaFile::FootageA)?, b.import(MediaFile::FootageB)?);
    let (n, h) = (b.frames(), b.frames() / 2);
    b.video(0, a, 0, h, 0)?;
    b.video(0, c, h, n - h, 0)?;
    Ok(())
}

fn motion_scale_position(b: &mut Builder) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.video(0, a, 0, n, 0)?;
    b.set(c, "motion", "scale", fl(50.0))?;
    let (w, h) = (f64::from(b.settings.width), f64::from(b.settings.height));
    b.set(c, "motion", "position", ParamValue::Vec2(Vec2::new(w * 0.35, h * 0.4)))
}

fn motion_rotation(b: &mut Builder) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.video(0, a, 0, n, 0)?;
    b.set(c, "motion", "scale", fl(60.0))?;
    b.set(c, "motion", "rotation", fl(15.0))
}

/// A at 50 % opacity on V2 over B on V1.
fn opacity_50(b: &mut Builder) -> Result<(), Error> {
    let (a, c) = (b.import(MediaFile::FootageA)?, b.import(MediaFile::FootageB)?);
    let n = b.frames();
    b.video(0, c, 0, n, 0)?;
    let top = b.video(1, a, 0, n, 0)?;
    b.set(top, "opacity", "opacity", fl(50.0))
}

/// A on V2 over B, opacity keyframed 0 → 100 % across the sequence.
fn opacity_ramp(b: &mut Builder) -> Result<(), Error> {
    let (a, c) = (b.import(MediaFile::FootageA)?, b.import(MediaFile::FootageB)?);
    let n = b.frames();
    b.video(0, c, 0, n, 0)?;
    let top = b.video(1, a, 0, n, 0)?;
    b.keyframe(top, "opacity", "opacity", 0, fl(0.0))?;
    b.keyframe(top, "opacity", "opacity", n - 1, fl(100.0))
}

/// Motion scale keyframed 100 → 40 % across the sequence.
fn scale_ramp(b: &mut Builder) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.video(0, a, 0, n, 0)?;
    b.keyframe(c, "motion", "scale", 0, fl(100.0))?;
    b.keyframe(c, "motion", "scale", n - 1, fl(40.0))
}

/// A then B with a video transition of a third of the sequence centred on the cut.
fn with_video_transition(b: &mut Builder, effect: &str) -> Result<(), Error> {
    let (a, c) = (b.import(MediaFile::FootageA)?, b.import(MediaFile::FootageB)?);
    let (n, h) = (b.frames(), b.frames() / 2);
    let d = (n / 3) & !1;
    let left = b.video(0, a, 0, h, 0)?;
    // B starts `d` frames into its media so the transition has a head handle
    let right = b.video(0, c, h, n - h, d)?;
    b.transition(TrackKind::Video, 0, effect, Some(left), Some(right), h, d)
}

fn cross_dissolve(b: &mut Builder) -> Result<(), Error> {
    with_video_transition(b, "cross_dissolve")
}

fn dip_to_black(b: &mut Builder) -> Result<(), Error> {
    with_video_transition(b, "dip_to_black")
}

fn with_effect(b: &mut Builder, effect: &str, params: &[(&str, ParamValue)]) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.video(0, a, 0, n, 0)?;
    b.add_effect(c, effect, params)
}

fn gaussian_blur(b: &mut Builder) -> Result<(), Error> {
    with_effect(b, "gaussian_blur", &[("blurriness", fl(20.0))])
}

fn brightness_contrast(b: &mut Builder) -> Result<(), Error> {
    with_effect(b, "brightness_contrast", &[("brightness", fl(20.0)), ("contrast", fl(30.0))])
}

fn crop(b: &mut Builder) -> Result<(), Error> {
    with_effect(b, "crop", &[("left", fl(10.0)), ("top", fl(15.0)), ("right", fl(20.0)), ("bottom", fl(5.0))])
}

fn black_white(b: &mut Builder) -> Result<(), Error> {
    with_effect(b, "black_white", &[])
}

fn invert(b: &mut Builder) -> Result<(), Error> {
    with_effect(b, "invert", &[])
}

fn with_speed(b: &mut Builder, speed: f64) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.video(0, a, 0, n, 0)?;
    b.speed(c, speed)
}

fn speed_50(b: &mut Builder) -> Result<(), Error> {
    with_speed(b, 0.5)
}

fn speed_200(b: &mut Builder) -> Result<(), Error> {
    with_speed(b, 2.0)
}

/// A's sound then B's sound on A1.
fn audio_cut(b: &mut Builder) -> Result<(), Error> {
    let (a, c) = (b.import(MediaFile::FootageA)?, b.import(MediaFile::FootageB)?);
    let (n, h) = (b.frames(), b.frames() / 2);
    b.audio(0, a, 0, h, 0)?;
    b.audio(0, c, h, n - h, 0)?;
    Ok(())
}

fn audio_gain_minus6(b: &mut Builder) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.audio(0, a, 0, n, 0)?;
    b.set(c, "volume", "level", fl(-6.0))
}

fn audio_pan_left(b: &mut Builder) -> Result<(), Error> {
    let t = b.import(MediaFile::Tone)?;
    let n = b.frames();
    let c = b.audio(0, t, 0, n, 0)?;
    b.set(c, "panner", "balance", fl(-50.0))
}

/// A's sound then the tone with a Constant Power crossfade centred on the cut.
fn audio_constant_power(b: &mut Builder) -> Result<(), Error> {
    let (a, t) = (b.import(MediaFile::FootageA)?, b.import(MediaFile::Tone)?);
    let (n, h) = (b.frames(), b.frames() / 2);
    let d = (n / 3) & !1;
    let left = b.audio(0, a, 0, h, 0)?;
    let right = b.audio(0, t, h, n - h, d)?;
    b.transition(TrackKind::Audio, 0, "constant_power", Some(left), Some(right), h, d)
}

/// Clip volume keyframed 0 → −24 dB across the sequence.
fn audio_level_ramp(b: &mut Builder) -> Result<(), Error> {
    let a = b.import(MediaFile::FootageA)?;
    let n = b.frames();
    let c = b.audio(0, a, 0, n, 0)?;
    b.keyframe(c, "volume", "level", 0, fl(0.0))?;
    b.keyframe(c, "volume", "level", n - 1, fl(-24.0))
}

const NONE: &[&str] = &[];
const NO_EFFECTS: &[PremiereEffect] = &[];

const fn num(v: f64) -> PremiereValue {
    PremiereValue::Number(v)
}

/// Every case of the corpus, in report order.
pub const CASES: &[CaseDef] = &[
    CaseDef {
        id: "cut",
        title: "Hard cut",
        group: "Baseline",
        effect: "",
        compare: Compare::Video,
        about: "Two clips cut together, no effects: the floor set by ProRes decoding and the colour pipeline alone. Read every other video case against this one.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::BASELINE,
        build: cut,
    },
    CaseDef {
        id: "motion_scale_position",
        title: "Motion: Scale 50 %, Position off centre",
        group: "Motion",
        effect: "Motion",
        compare: Compare::Video,
        about: "Scale and Position of the intrinsic Motion effect (FCP7 Basic Motion): placement, scaling filter and edge handling.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: motion_scale_position,
    },
    CaseDef {
        id: "motion_rotation",
        title: "Motion: Rotation 15°, Scale 60 %",
        group: "Motion",
        effect: "Motion",
        compare: Compare::Video,
        about: "Rotation about the anchor point: resampling quality and edge antialiasing.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: motion_rotation,
    },
    CaseDef {
        id: "opacity_50",
        title: "Opacity 50 % over another clip",
        group: "Opacity",
        effect: "Opacity",
        compare: Compare::Video,
        about: "Normal blend at half opacity over a clip on V1: compositing maths (linear vs display-referred mixing).",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: opacity_50,
    },
    CaseDef {
        id: "opacity_ramp",
        title: "Opacity keyframes 0 → 100 %",
        group: "Opacity",
        effect: "Opacity (keyframes)",
        compare: Compare::Video,
        about: "Two linear keyframes across the clip: keyframe timing and interpolation of an opacity fade.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: opacity_ramp,
    },
    CaseDef {
        id: "scale_ramp",
        title: "Scale keyframes 100 → 40 %",
        group: "Motion",
        effect: "Motion (keyframes)",
        compare: Compare::Video,
        about: "A linear scale ramp: keyframe interpolation of a spatial parameter and the scaling filter at every size in between.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: scale_ramp,
    },
    CaseDef {
        id: "cross_dissolve",
        title: "Cross Dissolve",
        group: "Transitions",
        effect: "Cross Dissolve",
        compare: Compare::Video,
        about: "A dissolve of a third of the sequence centred on the cut: transition alignment, handles and the dissolve curve.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some("28 dB: Premiere blends the dissolve in gamma-encoded space, our mid-dissolve is brighter (render)"),
        tolerance: Tolerance::VIDEO,
        build: cross_dissolve,
    },
    CaseDef {
        id: "dip_to_black",
        title: "Dip to Black",
        group: "Transitions",
        effect: "Dip to Black",
        compare: Compare::Video,
        about: "Fade out through black and in again: the two-sided fade curve.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some(
            "Premiere imports our Dip to Black as a Cross Dissolve; its own XML calls it `Dip to Color Dissolve` with a `dipcolor` (interchange)",
        ),
        tolerance: Tolerance::VIDEO,
        build: dip_to_black,
    },
    CaseDef {
        id: "gaussian_blur",
        title: "Gaussian Blur (Legacy) 20",
        group: "Effects",
        effect: "Gaussian Blur (Legacy)",
        compare: Compare::Video,
        about: "Blurriness 20, both dimensions, no edge repeat: kernel size for a given Blurriness and edge handling. \
                This is the classic Gaussian Blur; Premiere Pro 2026 gave that plain name to its new Impact-based blur and renamed the classic one.",
        xml_complete: false,
        premiere_steps: &[
            "Apply Video Effects ▸ Blur & Sharpen ▸ Gaussian Blur (Legacy) to the clip on V1 (not the new Gaussian Blur).",
            "Blurriness 20 · Blur Dimensions: Horizontal and Vertical · Repeat Edge Pixels: off (Premiere's default is on).",
        ],
        premiere_effects: &[PremiereEffect {
            name: "Gaussian Blur (Legacy)",
            params: &[("Blurriness", num(20.0)), ("Repeat Edge Pixels", PremiereValue::Bool(false))],
        }],
        known_failure: Some("33 dB against Gaussian Blur (Legacy): the kernel for a given Blurriness differs (render)"),
        tolerance: Tolerance::VIDEO,
        build: gaussian_blur,
    },
    CaseDef {
        id: "brightness_contrast",
        title: "Brightness & Contrast +20 / +30",
        group: "Effects",
        effect: "Brightness & Contrast",
        compare: Compare::Video,
        about: "Brightness 20, Contrast 30: the transfer curve of the two controls.",
        xml_complete: false,
        premiere_steps: &["Apply Video Effects ▸ Color Correction ▸ Brightness & Contrast to the clip on V1.", "Brightness 20 · Contrast 30."],
        premiere_effects: &[PremiereEffect { name: "Brightness & Contrast", params: &[("Brightness", num(20.0)), ("Contrast", num(30.0))] }],
        known_failure: Some("34.4 dB: the Brightness and Contrast curves differ (render)"),
        tolerance: Tolerance::VIDEO,
        build: brightness_contrast,
    },
    CaseDef {
        id: "crop",
        title: "Crop 10 / 15 / 20 / 5 %",
        group: "Effects",
        effect: "Crop",
        compare: Compare::Video,
        about: "Left 10, Top 15, Right 20, Bottom 5 %, no zoom, no feather: edge positions and what fills the cropped area.",
        xml_complete: false,
        premiere_steps: &[
            "Premiere imports the XML's Crop filter as a Crop effect on the clip on V1 with every edge at 0: set its values (do not add a second Crop).",
            "Left 10 % · Top 15 % · Right 20 % · Bottom 5 % · Zoom: off · Edge Feather 0.",
        ],
        premiere_effects: &[PremiereEffect { name: "Crop", params: &[("Left", num(10.0)), ("Top", num(15.0)), ("Right", num(20.0)), ("Bottom", num(5.0))] }],
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: crop,
    },
    CaseDef {
        id: "black_white",
        title: "Black & White",
        group: "Effects",
        effect: "Black & White",
        compare: Compare::Video,
        about: "The luma weights of the desaturation.",
        xml_complete: false,
        premiere_steps: &["Apply Video Effects ▸ Image Control ▸ Black & White to the clip on V1."],
        premiere_effects: &[PremiereEffect { name: "Black & White", params: &[] }],
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: black_white,
    },
    CaseDef {
        id: "invert",
        title: "Invert (RGB)",
        group: "Effects",
        effect: "Invert",
        compare: Compare::Video,
        about: "Channel inversion at defaults: which values are inverted (code values vs linear light).",
        xml_complete: false,
        premiere_steps: &["Apply Video Effects ▸ Channel ▸ Invert to the clip on V1.", "Channel: RGB · Blend With Original 0 % (the defaults)."],
        premiere_effects: &[PremiereEffect { name: "Invert", params: &[] }],
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: invert,
    },
    CaseDef {
        id: "speed_50",
        title: "Speed 50 %",
        group: "Speed",
        effect: "Speed/Duration",
        compare: Compare::Video,
        about: "Half speed over the whole sequence with frame sampling (no blending): which source frame each output frame shows.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: speed_50,
    },
    CaseDef {
        id: "speed_200",
        title: "Speed 200 %",
        group: "Speed",
        effect: "Speed/Duration",
        compare: Compare::Video,
        about: "Double speed: frame selection when skipping source frames.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: None,
        tolerance: Tolerance::VIDEO,
        build: speed_200,
    },
    CaseDef {
        id: "audio_cut",
        title: "Audio cut",
        group: "Audio",
        effect: "",
        compare: Compare::Audio,
        about: "Two sound clips cut together at unity: the audio baseline (decoding, mixing at 0 dB, cut placement to the sample).",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some("our FCP7 XML writes a stereo clip as one clip item, which Premiere plays as mono channel 1 at −3 dB (interchange)"),
        tolerance: Tolerance::AUDIO,
        build: audio_cut,
    },
    CaseDef {
        id: "audio_gain_minus6",
        title: "Clip volume −6 dB",
        group: "Audio",
        effect: "Volume",
        compare: Compare::Audio,
        about: "Clip Volume level −6 dB (FCP7 Audio Levels): the dB-to-gain law.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some("our FCP7 XML writes a stereo clip as one clip item, which Premiere plays as mono channel 1 at −3 dB (interchange)"),
        tolerance: Tolerance::AUDIO,
        build: audio_gain_minus6,
    },
    CaseDef {
        id: "audio_pan_left",
        title: "Pan / Balance −50",
        group: "Audio",
        effect: "Panner",
        compare: Compare::Audio,
        about: "A stereo tone balanced half way left: the balance law.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some("our FCP7 XML writes a stereo clip as one clip item, which Premiere plays as mono channel 1 at −3 dB (interchange)"),
        tolerance: Tolerance::AUDIO,
        build: audio_pan_left,
    },
    CaseDef {
        id: "audio_constant_power",
        title: "Constant Power crossfade",
        group: "Audio",
        effect: "Constant Power",
        compare: Compare::Audio,
        about: "A constant-power crossfade of a third of the sequence between footage sound and a tone: the crossfade curve.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some("our FCP7 XML writes a stereo clip as one clip item, which Premiere plays as mono channel 1 at −3 dB (interchange)"),
        tolerance: Tolerance::AUDIO,
        build: audio_constant_power,
    },
    CaseDef {
        id: "audio_level_ramp",
        title: "Volume keyframes 0 → −24 dB",
        group: "Audio",
        effect: "Volume (keyframes)",
        compare: Compare::Audio,
        about: "Two linear volume keyframes: whether the ramp is linear in dB or in gain.",
        xml_complete: true,
        premiere_steps: NONE,
        premiere_effects: NO_EFFECTS,
        known_failure: Some("our FCP7 XML writes a stereo clip as one clip item, which Premiere plays as mono channel 1 at −3 dB (interchange)"),
        tolerance: Tolerance::AUDIO,
        build: audio_level_ramp,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_ids_are_unique_and_each_group_is_known() {
        let mut ids: Vec<&str> = CASES.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate case id");
        for c in CASES {
            assert!(["Baseline", "Motion", "Opacity", "Transitions", "Effects", "Speed", "Audio"].contains(&c.group), "{}: group {}", c.id, c.group);
            assert!(c.xml_complete == c.premiere_steps.is_empty(), "{}: manual steps iff the XML is incomplete", c.id);
            assert!(c.xml_complete == c.premiere_effects.is_empty(), "{}: scripted effects iff the XML is incomplete", c.id);
            assert!(c.known_failure.is_none_or(|why| why.len() > 10), "{}: a known failure names its reason", c.id);
            for e in c.premiere_effects {
                assert!(!e.name.is_empty() && e.params.iter().all(|(k, _)| !k.is_empty()), "{}: effect names", c.id);
            }
            assert!(c.id.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'), "{}: id is a file name", c.id);
            match c.compare {
                Compare::Video => assert!(c.tolerance.min_psnr_db.is_some() && c.tolerance.min_snr_db.is_none(), "{}", c.id),
                Compare::Audio => assert!(c.tolerance.min_snr_db.is_some() && c.tolerance.min_psnr_db.is_none(), "{}", c.id),
            }
        }
        assert!(case("cut").is_some() && case("nope").is_none());
    }

    #[test]
    fn settings_derive_frame_and_sample_counts() {
        let s = CorpusSettings::default();
        assert_eq!((s.frames(), s.samples()), (72, 144_000));
        assert_eq!(s.duration(), Tick(3 * TICKS_PER_SECOND));
        assert_eq!(s.media_duration(), Tick(7 * TICKS_PER_SECOND));
        let tiny = CorpusSettings { seconds: 1, frame_rate: FrameRate::FPS_24, ..s };
        assert_eq!(tiny.frames(), 24);
    }

    /// Media is kept across runs with the same settings and made again when they change.
    #[test]
    fn media_follows_the_settings() {
        let dir = std::env::temp_dir().join(format!("filmcraft-fidelity-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let one = CorpusSettings { width: 160, height: 90, seconds: 1, ..Default::default() };
        let media = ensure_media(&dir, &one).unwrap();
        let stamp = dir.join(MEDIA_SETTINGS_FILE);
        let recorded = || serde_json::from_slice::<CorpusSettings>(&std::fs::read(&stamp).unwrap()).unwrap();
        assert_eq!(recorded(), one);
        let tone_len = std::fs::metadata(&media.tone).unwrap().len();
        let footage_len = std::fs::metadata(&media.footage_a).unwrap().len();

        // the same settings: a present file is kept as it is
        std::fs::write(&media.tone, b"kept").unwrap();
        let again = ensure_media(&dir, &one).unwrap();
        assert_eq!(again, media);
        assert_eq!(std::fs::read(&media.tone).unwrap(), b"kept");

        // longer sequences: everything is made again, and longer (3 s -> 5 s of tone)
        let two = CorpusSettings { seconds: 2, ..one };
        let media = ensure_media(&dir, &two).unwrap();
        let longer = std::fs::metadata(&media.tone).unwrap().len();
        assert!(longer > tone_len * 3 / 2, "{longer} vs {tone_len}");
        assert_eq!(recorded(), two);

        // another size: the picture media changes
        let wide = CorpusSettings { width: 320, height: 180, ..two };
        let media = ensure_media(&dir, &wide).unwrap();
        assert_ne!(std::fs::metadata(&media.footage_a).unwrap().len(), footage_len);

        // an unreadable stamp means: start over
        std::fs::write(&stamp, b"not json").unwrap();
        std::fs::write(&media.tone, b"stale").unwrap();
        let media = ensure_media(&dir, &wide).unwrap();
        assert_eq!(std::fs::metadata(&media.tone).unwrap().len(), longer);
        assert_eq!(recorded(), wide);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

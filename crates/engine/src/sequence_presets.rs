//! Custom sequence presets: the user's own entries next to the built-in ones
//! ([`filmcraft_project::sequence_presets`]), saved from New Sequence or Sequence Settings and
//! offered in New Sequence's Custom group and when making a new project.
//!
//! A preset keeps the name, frame size, pixel aspect ratio, timebase (with its timecode format),
//! fields, the audio settings (sample rate, Mix channel format) and the track layout (video and
//! audio tracks, the audio tracks' type). Presets belong to the user, not to a project: they
//! persist in `<data dir>/sequence-presets.json`
//! (`{"format": "filmcraft.sequence-presets", "version": 1, "presets": […]}`), are not undoable,
//! and a damaged file loads as no presets (the reason goes to the log). Names are unique among
//! the custom and the built-in presets, compared case-insensitively, so `file.newSequence
//! {preset}` and `file.newProject {sequencePreset}` find either kind by name ([`pick`]).
//!
//! Commands: `sequence.presets` (both kinds; custom ones have `custom: true` and the audio and
//! track settings), `sequence.preset.save {name, replace?, sequence?, …settings}`,
//! `sequence.preset.rename {name, to}`, `sequence.preset.delete {name}`.

use std::path::{Path, PathBuf};

use filmcraft_project::{AudioChannels, SequenceSettings};
use filmcraft_time::FrameRate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, checked_u32_p, str_p};
use crate::{Result, Session};

pub const FILE_FORMAT: &str = "filmcraft.sequence-presets";
const LIBRARY_FILE: &str = "sequence-presets.json";
/// The group custom presets are listed under.
pub const CUSTOM_GROUP: &str = "Custom";
/// Most custom presets kept (the file is the user's, but read as untrusted).
pub const MAX_PRESETS: usize = 1000;
/// Longest preset name, in characters.
pub const MAX_NAME: usize = 128;
/// Most tracks of each kind a preset (and New Sequence) makes.
const MAX_TRACKS: u32 = 256;

/// How the frames are scanned. FilmCraft sequences are progressive; the field is kept so a
/// preset file can say so explicitly (and later grow interlaced orders).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Fields {
    #[default]
    Progressive,
}

/// One custom sequence preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomSequencePreset {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub pixel_aspect: (u32, u32),
    pub frame_rate: FrameRate,
    #[serde(default)]
    pub drop_frame: bool,
    #[serde(default)]
    pub fields: Fields,
    pub sample_rate: u32,
    /// The Mix track's channel format.
    pub audio_master: AudioChannels,
    pub video_tracks: u32,
    pub audio_tracks: u32,
    /// The type of every audio track; `None`: standard (stereo) tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_track_type: Option<AudioChannels>,
}

impl CustomSequencePreset {
    /// The sequence settings this preset makes (named after it; colour and preview settings at
    /// their defaults).
    pub fn settings(&self) -> SequenceSettings {
        SequenceSettings {
            width: self.width,
            height: self.height,
            frame_rate: self.frame_rate,
            par: filmcraft_project::sane_par(self.pixel_aspect),
            sample_rate: self.sample_rate,
            drop_frame: self.drop_frame && self.frame_rate.supports_drop_frame(),
            preset: self.name.clone(),
            audio_master: self.audio_master,
            ..SequenceSettings::default()
        }
    }

    /// Whether `st` still has this preset's picture (size, timebase and pixel aspect ratio).
    pub fn matches(&self, st: &SequenceSettings) -> bool {
        (st.width, st.height, st.frame_rate, filmcraft_project::sane_par(st.par))
            == (self.width, self.height, self.frame_rate, filmcraft_project::sane_par(self.pixel_aspect))
    }

    /// Why this preset can't be used, if it can't: a bad name, settings a sequence can't have,
    /// too many tracks.
    pub fn validate(&self) -> std::result::Result<(), String> {
        check_name(&self.name)?;
        if filmcraft_project::checked_par(self.pixel_aspect) != Some(filmcraft_project::sane_par(self.pixel_aspect)) {
            return Err(format!("pixel aspect ratio must be between 1:{0} and {0}:1", filmcraft_project::MAX_PIXEL_ASPECT));
        }
        self.settings().validate()?;
        if self.video_tracks > MAX_TRACKS || self.audio_tracks > MAX_TRACKS {
            return Err(format!("at most {MAX_TRACKS} video and {MAX_TRACKS} audio tracks"));
        }
        Ok(())
    }

    /// The JSON `sequence.presets` lists it as.
    pub fn to_json(&self) -> Value {
        json!({
            "group": CUSTOM_GROUP, "custom": true, "name": self.name, "width": self.width, "height": self.height,
            "fps": self.frame_rate.as_f64(), "fpsNum": self.frame_rate.num, "fpsDen": self.frame_rate.den,
            "pixelAspect": self.pixel_aspect, "dropFrame": self.drop_frame && self.frame_rate.supports_drop_frame(),
            "fields": "progressive", "sampleRate": self.sample_rate, "mix": mix_name(self.audio_master),
            "video": self.video_tracks, "audio": self.audio_tracks,
            "trackType": self.audio_track_type.map_or("Standard", mix_name),
        })
    }
}

/// `Stereo`, `Mono`, `5.1` or `Adaptive`: the names `file.newSequence {mix}` takes.
pub fn mix_name(c: AudioChannels) -> &'static str {
    match c {
        AudioChannels::Mono => "Mono",
        AudioChannels::Stereo => "Stereo",
        AudioChannels::Surround51 => "5.1",
        AudioChannels::Adaptive => "Adaptive",
    }
}

/// A preset name: trimmed, 1 to [`MAX_NAME`] characters, no control characters.
fn check_name(name: &str) -> std::result::Result<(), String> {
    if name.trim().is_empty() {
        return Err("a preset needs a name".into());
    }
    if name.trim() != name {
        return Err("a preset name can't start or end with spaces".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("a preset name is at most {MAX_NAME} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("a preset name can't contain control characters".into());
    }
    Ok(())
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The name of the built-in preset called `name` (case-insensitively), if any.
fn builtin_named(name: &str) -> Option<&'static str> {
    filmcraft_project::sequence_presets_cached().iter().find(|p| same_name(p.name, name)).map(|p| p.name)
}

#[derive(Default, Serialize, Deserialize)]
struct PresetFile {
    format: String,
    version: u32,
    #[serde(default)]
    presets: Vec<CustomSequencePreset>,
}

/// The parsed presets of a preset file: the usable ones, in order, first of each name; or why
/// the file isn't one.
pub fn parse_file(bytes: &[u8]) -> std::result::Result<Vec<CustomSequencePreset>, String> {
    let f: PresetFile = serde_json::from_slice(bytes).map_err(|e| format!("not a sequence preset file: {e}"))?;
    if f.format != FILE_FORMAT {
        return Err(format!("not a sequence preset file (format `{}`)", f.format));
    }
    if f.version > 1 {
        return Err(format!("sequence preset file version {} is newer than this build reads (1)", f.version));
    }
    let mut out: Vec<CustomSequencePreset> = Vec::new();
    for p in f.presets {
        match p.validate() {
            Ok(()) if builtin_named(&p.name).is_none() && !out.iter().any(|o| same_name(&o.name, &p.name)) => out.push(p),
            Ok(()) => log::warn!("sequence preset `{}` skipped: the name is taken", p.name),
            Err(e) => log::warn!("sequence preset `{}` skipped: {e}", p.name),
        }
        if out.len() >= MAX_PRESETS {
            break;
        }
    }
    Ok(out)
}

/// The bytes of a preset file holding `presets`.
pub fn file_bytes(presets: &[CustomSequencePreset]) -> Vec<u8> {
    let f = PresetFile { format: FILE_FORMAT.into(), version: 1, presets: presets.to_vec() };
    serde_json::to_vec_pretty(&f).unwrap_or_default()
}

/// The user's custom sequence presets (+ where they persist).
#[derive(Default)]
pub struct SequencePresetLibrary {
    pub user: Vec<CustomSequencePreset>,
    dir: Option<PathBuf>,
}

impl SequencePresetLibrary {
    /// Persist in `data_dir` (and load what is there).
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.to_path_buf());
        let path = data_dir.join(LIBRARY_FILE);
        if let Ok(bytes) = std::fs::read(&path) {
            match parse_file(&bytes) {
                Ok(p) => self.user = p,
                Err(e) => log::warn!("{}: {e}", path.display()),
            }
        }
    }

    fn persist(&self) -> std::result::Result<(), String> {
        let Some(d) = &self.dir else { return Ok(()) };
        let _ = std::fs::create_dir_all(d);
        filmcraft_format::atomic_write(&d.join(LIBRARY_FILE), &file_bytes(&self.user)).map_err(|e| e.to_string())
    }

    /// The custom preset called `name` (case-insensitively).
    pub fn find(&self, name: &str) -> Option<&CustomSequencePreset> {
        self.user.iter().find(|p| same_name(&p.name, name))
    }

    /// Add `preset`, or replace the settings of the custom one of the same name (which keeps its
    /// name) when `replace`. Refused: a bad
    /// preset, a built-in preset's name, a taken name without `replace`, too many presets.
    pub fn save(&mut self, preset: CustomSequencePreset, replace: bool) -> std::result::Result<(), String> {
        preset.validate()?;
        if let Some(b) = builtin_named(&preset.name) {
            return Err(format!("`{b}` is a built-in preset; choose another name"));
        }
        let before = self.user.clone();
        match self.user.iter().position(|p| same_name(&p.name, &preset.name)) {
            Some(i) if replace => {
                // the settings are replaced; the preset keeps its name (as typed when it was made)
                if let Some(slot) = self.user.get_mut(i) {
                    *slot = CustomSequencePreset { name: std::mem::take(&mut slot.name), ..preset };
                }
            }
            Some(_) => return Err(format!("a preset called `{}` exists; replace it or choose another name", preset.name)),
            None if self.user.len() >= MAX_PRESETS => return Err(format!("at most {MAX_PRESETS} custom presets")),
            None => self.user.push(preset),
        }
        self.persist_or(before)
    }

    /// Rename the custom preset `from` to `to`.
    pub fn rename(&mut self, from: &str, to: &str) -> std::result::Result<(), String> {
        check_name(to)?;
        let i = self.user.iter().position(|p| same_name(&p.name, from)).ok_or_else(|| format!("no custom preset called `{from}`"))?;
        if let Some(b) = builtin_named(to) {
            return Err(format!("`{b}` is a built-in preset; choose another name"));
        }
        if self.user.iter().enumerate().any(|(j, p)| j != i && same_name(&p.name, to)) {
            return Err(format!("a preset called `{to}` exists"));
        }
        let before = self.user.clone();
        if let Some(p) = self.user.get_mut(i) {
            p.name = to.to_string();
        }
        self.persist_or(before)
    }

    /// Delete the custom preset `name`.
    pub fn delete(&mut self, name: &str) -> std::result::Result<(), String> {
        let i = self.user.iter().position(|p| same_name(&p.name, name)).ok_or_else(|| format!("no custom preset called `{name}`"))?;
        let before = self.user.clone();
        self.user.remove(i);
        self.persist_or(before)
    }

    /// Write the presets; on failure put `before` back and say why.
    fn persist_or(&mut self, before: Vec<CustomSequencePreset>) -> std::result::Result<(), String> {
        self.persist().map_err(|e| {
            self.user = before;
            format!("could not save the sequence presets: {e}")
        })
    }
}

/// A preset picked by name for a new sequence: its name, the settings it makes and, for a custom
/// preset, its track layout (video tracks, audio tracks, audio track type).
pub struct PresetPick {
    pub name: String,
    pub settings: SequenceSettings,
    pub tracks: Option<(u32, u32, Option<AudioChannels>)>,
}

impl PresetPick {
    /// Whether `st` still has this preset's picture (size, timebase and pixel aspect ratio).
    pub fn matches(&self, st: &SequenceSettings) -> bool {
        let a = &self.settings;
        (st.width, st.height, st.frame_rate, filmcraft_project::sane_par(st.par)) == (a.width, a.height, a.frame_rate, filmcraft_project::sane_par(a.par))
    }
}

/// The preset called `name`: a built-in one (exact name), else a custom one (case-insensitive).
pub fn pick(s: &Session, name: &str) -> Option<PresetPick> {
    if let Some(b) = filmcraft_project::sequence_preset(name) {
        return Some(PresetPick { name: b.name.to_string(), settings: b.settings(), tracks: None });
    }
    s.sequence_presets.find(name).map(|c| PresetPick {
        name: c.name.clone(),
        settings: c.settings(),
        tracks: Some((c.video_tracks, c.audio_tracks, c.audio_track_type)),
    })
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let mut v: Vec<Value> = filmcraft_project::sequence_presets_cached()
        .iter()
        .map(|p| {
            json!({"group": p.group, "name": p.name, "width": p.width, "height": p.height, "fps": p.frame_rate.as_f64(),
                   "fpsNum": p.frame_rate.num, "fpsDen": p.frame_rate.den, "pixelAspect": p.par})
        })
        .collect();
    v.extend(s.sequence_presets.user.iter().map(CustomSequencePreset::to_json));
    Ok(Value::Array(v))
}

/// `sequence.preset.save`: the settings of `sequence` (an id; else the active sequence, else the
/// defaults), with any of `file.newSequence`'s parameters over them.
fn save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.preset.save";
    let name = str_p(p, "name").map(str::trim).unwrap_or("").to_string();
    check_name(&name).map_err(|e| bad(CMD, e))?;
    let base = match p.get("sequence").filter(|v| !v.is_null()) {
        Some(v) => {
            let id = v.as_u64().map(filmcraft_project::ItemId).ok_or_else(|| bad(CMD, "`sequence` must be a sequence id"))?;
            Some(s.project.sequence(id).ok_or_else(|| bad(CMD, format!("no sequence {}", id.0)))?)
        }
        None => s.active_sequence(),
    };
    let st = base.map(|q| q.settings.clone()).unwrap_or_default();
    let (mut video, mut audio) = base.map_or((3, 3), |q| (q.video_tracks.len() as u32, q.audio_tracks.len() as u32));
    let mut track_type = base.and_then(|q| {
        let first = q.audio_tracks.first()?.channels;
        q.audio_tracks.iter().all(|t| t.channels == first).then_some(first).filter(|c| *c != AudioChannels::Stereo)
    });
    let mut preset = CustomSequencePreset {
        name,
        width: st.width,
        height: st.height,
        pixel_aspect: filmcraft_project::sane_par(st.par),
        frame_rate: st.frame_rate,
        drop_frame: st.drop_frame,
        fields: Fields::Progressive,
        sample_rate: st.sample_rate,
        audio_master: st.audio_master,
        video_tracks: video,
        audio_tracks: audio,
        audio_track_type: track_type,
    };
    if let Some(w) = checked_u32_p(p, "width", CMD)? {
        preset.width = w;
    }
    if let Some(h) = checked_u32_p(p, "height", CMD)? {
        preset.height = h;
    }
    if let Some(r) = crate::commands::fps_p(p, CMD)? {
        preset.frame_rate = r;
        preset.drop_frame = r.supports_drop_frame();
    }
    if let Some(par) = crate::commands::pixel_aspect_param(p, CMD)? {
        preset.pixel_aspect = par;
    }
    if let Some(df) = bool_p(p, "dropFrame") {
        if df && !preset.frame_rate.supports_drop_frame() {
            return Err(bad(CMD, format!("drop-frame timecode needs a 29.97, 59.94 or 119.88 fps timebase, not {}", preset.frame_rate)));
        }
        preset.drop_frame = df;
    }
    if let Some(f) = str_p(p, "fields")
        && !f.eq_ignore_ascii_case("progressive")
    {
        return Err(bad(CMD, format!("unsupported fields `{f}` (FilmCraft sequences are progressive)")));
    }
    if let Some(sr) = checked_u32_p(p, "sampleRate", CMD)? {
        preset.sample_rate = sr;
    }
    if let Some(m) = str_p(p, "mix") {
        preset.audio_master = crate::mixer::channels_from(m).ok_or_else(|| bad(CMD, format!("unknown mix `{m}` (Stereo, Mono, 5.1, Adaptive)")))?;
    }
    if let Some(n) = checked_u32_p(p, "video", CMD)? {
        video = n;
    }
    if let Some(n) = checked_u32_p(p, "audio", CMD)? {
        audio = n;
    }
    if let Some(t) = str_p(p, "trackType") {
        let c = crate::mixer::channels_from(t).ok_or_else(|| bad(CMD, format!("unknown track type `{t}` (Standard, Mono, 5.1, Adaptive)")))?;
        track_type = (c != AudioChannels::Stereo).then_some(c);
    }
    (preset.video_tracks, preset.audio_tracks, preset.audio_track_type) = (video, audio, track_type);
    let replace = bool_p(p, "replace").unwrap_or(false);
    let out = preset.to_json();
    s.sequence_presets.save(preset, replace).map_err(|e| bad(CMD, e))?;
    Ok(out)
}

fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.preset.rename";
    let from = str_p(p, "name").ok_or_else(|| bad(CMD, "need `name`, the preset to rename"))?;
    let to = str_p(p, "to").map(str::trim).ok_or_else(|| bad(CMD, "need `to`, the new name"))?;
    s.sequence_presets.rename(from, to).map_err(|e| bad(CMD, e))?;
    Ok(json!({"name": to}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.preset.delete";
    let name = str_p(p, "name").ok_or_else(|| bad(CMD, "need `name`"))?;
    s.sequence_presets.delete(name).map_err(|e| bad(CMD, e))?;
    Ok(json!({"deleted": name}))
}

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("sequence.presets", "List Sequence Presets", "{}", list, false),
        spec(
            "sequence.preset.save",
            "Save Sequence Preset",
            r#"{"name":str,"replace":bool=false,"sequence":id?,"width":u32?,"height":u32?,"fps":f64?,"pixelAspect":[num,den]?,"dropFrame":bool?,"fields":"progressive"?,"sampleRate":u32?,"mix":"Stereo|Mono|5.1|Adaptive"?,"video":n?,"audio":n?,"trackType":"Standard|Mono|5.1|Adaptive"?}"#,
            save,
            true,
        ),
        spec("sequence.preset.rename", "Rename Sequence Preset", r#"{"name":str,"to":str}"#, rename, true),
        spec("sequence.preset.delete", "Delete Sequence Preset", r#"{"name":str}"#, delete, true),
    ]
}

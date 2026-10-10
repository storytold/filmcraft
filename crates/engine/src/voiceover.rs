//! Voice-over recording (the timeline track header's Voice-over Record button).
//!
//! Recording follows Premiere's model: a *record point* R (the playhead, or the sequence In point
//! when In/Out are set: punch-in / punch-out), playback starts a pre-roll before it at the
//! *capture start* C = max(0, R − pre-roll), the input is captured from C, and when recording stops
//! (or playback reaches the Out point) the audio from R on is written to a WAV file, imported and
//! placed on the record track at R as one undoable step. The pre-roll audio is not kept.
//!
//! | command | does |
//! |---|---|
//! | `audio.voiceover.settings` | get / set the Voice-Over Record Settings (preferences), list input devices |
//! | `audio.voiceover.start` | arm and start capturing; returns R, C, the punch-out and the countdown cue times |
//! | `audio.voiceover.sync` | the host's playback actually started at `time`: restart the capture there |
//! | `audio.voiceover.stop` | stop capturing; write, import and place the recording (one undo step) |
//!
//! Input goes through the [`AudioInput`] trait. The desktop app installs a cpal input; headless
//! sessions (CLI, MCP, tests) use [`SyntheticInput`], a deterministic generated signal that produces
//! exactly as many samples as the timeline asks for, so a recording lands sample-accurately.
//!
//! Files are named `<Name> <n>.wav` (Voice-over Record Settings ▸ Name, default `Voice-over`, `n`
//! the first number not used by a project item or an existing file) and saved in Project Settings ▸
//! Scratch Disks ▸ Captured Audio and Video, else next to the project file, else (unsaved project)
//! in `Voice-over Recordings` in the data directory or the system temporary directory. They are
//! mono 32-bit float WAV at the input's sample rate.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_project::{Sequence, TrackId, TrackKind};
use filmcraft_time::{Tick, TimeRange};

use crate::commands::{CommandSpec, bad, bool_p, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

// ------------------------------------------------------------------------------------- input

/// What an input delivers once started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct InputFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// An audio input device (cpal on desktop, [`SyntheticInput`] headless).
pub trait AudioInput: Send {
    /// Select the host for device discovery and the next take, preserving any active capture.
    fn configure_host(&mut self, _host: &str) {}
    /// Names of the devices that can be chosen as Source.
    fn devices(&self) -> Vec<String>;
    /// Input channels of `device` ("" = the default device).
    fn channels(&self, device: &str) -> u16;
    /// Start capturing from `device` ("" = default), preferably at `sample_rate`.
    fn start(&mut self, device: &str, sample_rate: u32) -> std::result::Result<InputFormat, String>;
    /// Up to `frames` captured frames (planar, every device channel), oldest first. A live device
    /// returns what it has captured; a synthetic one generates exactly `frames`.
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>>;
    /// A device error must not be saved as a successful silent take.
    fn error(&self) -> Option<String> {
        None
    }
    /// Drop everything captured so far: the next sample read is "now".
    fn discard(&mut self);
    fn stop(&mut self);
}

/// The signal a [`SyntheticInput`] produces.
#[derive(Clone, Debug, PartialEq)]
pub enum Synthetic {
    /// Channel 0: a 0.5 impulse every `period` samples starting at sample 0; channel 1: a 0.25
    /// 440 Hz sine.
    Clicks { period: usize },
    /// These samples (planar), then silence.
    Buffer(Vec<Vec<f32>>),
}

/// A deterministic input for headless sessions and tests.
pub struct SyntheticInput {
    pub signal: Synthetic,
    rate: u32,
    pos: usize,
}

impl SyntheticInput {
    pub const DEVICE: &'static str = "Synthetic Input";
    pub fn new(signal: Synthetic) -> Self {
        Self { signal, rate: 48_000, pos: 0 }
    }
    /// Clicks every quarter second at 48 kHz.
    pub fn clicks() -> Self {
        Self::new(Synthetic::Clicks { period: 12_000 })
    }
}

impl AudioInput for SyntheticInput {
    fn devices(&self) -> Vec<String> {
        vec![Self::DEVICE.to_string()]
    }
    fn channels(&self, _device: &str) -> u16 {
        match &self.signal {
            Synthetic::Clicks { .. } => 2,
            Synthetic::Buffer(b) => b.len().max(1) as u16,
        }
    }
    fn start(&mut self, device: &str, sample_rate: u32) -> std::result::Result<InputFormat, String> {
        self.rate = sample_rate.max(1);
        self.pos = 0;
        Ok(InputFormat { sample_rate: self.rate, channels: self.channels(device) })
    }
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>> {
        let p0 = self.pos;
        self.pos += frames;
        match &self.signal {
            Synthetic::Clicks { period } => {
                let period = (*period).max(1);
                let w = 2.0 * std::f64::consts::PI * 440.0 / self.rate as f64;
                let c0 = (p0..p0 + frames).map(|i| if i % period == 0 { 0.5 } else { 0.0 }).collect();
                let c1 = (p0..p0 + frames).map(|i| (0.25 * (w * i as f64).sin()) as f32).collect();
                vec![c0, c1]
            }
            Synthetic::Buffer(b) => b.iter().map(|c| (p0..p0 + frames).map(|i| c.get(i).copied().unwrap_or(0.0)).collect()).collect(),
        }
    }
    fn discard(&mut self) {
        self.pos = 0;
    }
    fn stop(&mut self) {}
}

// ------------------------------------------------------------------------------------- settings

/// Voice-Over Record Settings (persisted in the preferences as `voiceOver`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VoiceOverPrefs {
    /// Source device ("" = Settings ▸ Audio Hardware ▸ Default Input, else the system default).
    pub source: String,
    /// Input channel recorded (0-based; the file is mono).
    pub input_channel: u32,
    /// Base name of the recorded files and clips (`<name> <n>`).
    pub name: String,
    /// Beep once a second during the pre-roll and at the record point.
    pub countdown_sound_cues: bool,
    /// Playback starts this long before the record point (seconds).
    pub preroll_seconds: f64,
    /// Playback continues this long after the punch-out (Out) point (seconds).
    pub postroll_seconds: f64,
}

impl Default for VoiceOverPrefs {
    fn default() -> Self {
        Self { source: String::new(), input_channel: 0, name: "Voice-over".into(), countdown_sound_cues: true, preroll_seconds: 2.0, postroll_seconds: 2.0 }
    }
}

// ------------------------------------------------------------------------------------- state

/// A recording in progress.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub seq: filmcraft_project::ItemId,
    pub track: TrackId,
    /// Record point (sequence time): where the clip starts.
    pub record_start: Tick,
    /// Timeline time of the first captured sample.
    pub capture_start: Tick,
    /// Out point when punching in (recording ends there).
    pub punch_out: Option<Tick>,
    pub format: InputFormat,
    pub channel: u32,
}

/// Voice-over state of a session: the input device and the recording in progress.
#[derive(Default)]
pub struct VoiceOver {
    /// Host input (None = a [`SyntheticInput`] is created on first use).
    pub input: Option<Box<dyn AudioInput>>,
    pub rec: Option<Recording>,
    /// Frames already read from the input in this recording (captured before stop).
    captured: Vec<f32>,
    /// A stopped take awaiting a successful save; retries keep its original end point.
    pending_stop: Option<Tick>,
}

impl VoiceOver {
    pub fn recording(&self) -> bool {
        self.rec.is_some()
    }
    fn input(&mut self) -> &mut Box<dyn AudioInput> {
        self.input.get_or_insert_with(|| Box::new(SyntheticInput::clicks()))
    }
}

/// One countdown beep (1 kHz, 100 ms, −12 dBFS with 5 ms fades) at `sr`.
pub fn cue_tone(sr: u32) -> Vec<f32> {
    let n = (sr as usize) / 10;
    let f = (sr as usize / 200).max(1);
    (0..n)
        .map(|i| {
            let env = (i.min(n - 1 - i) as f32 / f as f32).min(1.0);
            0.25 * env * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin()
        })
        .collect()
}

/// Countdown cue times: one per whole second of pre-roll before the record point, and the record
/// point itself.
pub fn cue_times(capture_start: Tick, record_start: Tick) -> Vec<Tick> {
    let sec = Tick::from_seconds_f64(1.0);
    let mut v = Vec::new();
    let mut t = record_start;
    while t > capture_start {
        t -= sec;
        if t >= capture_start {
            v.push(t);
        }
    }
    v.reverse();
    v.push(record_start);
    v
}

/// Mono 32-bit float WAV.
pub fn write_wav_f32(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 4;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 4).to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

// ------------------------------------------------------------------------------------- commands

fn audio_track_ref(seq: &Sequence, v: &Value) -> Option<TrackId> {
    let id = crate::mixer::strip_ref(seq, v)?;
    seq.audio_tracks.iter().any(|t| t.id == id).then_some(id)
}

/// The track a recording goes to: `track`, else the record-armed audio track, else the first
/// targeted audio track, else A1.
pub fn record_track(s: &Session, p: &Value) -> Result<TrackId> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if let Some(v) = p.get("track") {
        return audio_track_ref(seq, v).ok_or_else(|| bad("audio.voiceover.start", format!("no audio track {v}")));
    }
    if let Some(t) = seq.audio_tracks.iter().find(|t| t.mixer.record_arm) {
        return Ok(t.id);
    }
    let tg = s.targeting().targeted;
    seq.audio_tracks
        .iter()
        .find(|t| tg.contains(&t.id))
        .or(seq.audio_tracks.first())
        .map(|t| t.id)
        .ok_or_else(|| bad("audio.voiceover.start", "the sequence has no audio track"))
}

fn can_start(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.voiceover.recording() {
        return Err("a voice-over is already recording".into());
    }
    if s.active_sequence().is_some_and(|q| q.audio_tracks.is_empty()) {
        return Err("the sequence has no audio track".into());
    }
    Ok(())
}

fn is_recording(s: &Session) -> std::result::Result<(), String> {
    if s.voiceover.recording() { Ok(()) } else { Err("no voice-over is recording".into()) }
}

fn settings_json(s: &mut Session) -> Value {
    let vo = s.prefs.voice_over.clone();
    let source = if vo.source.is_empty() { s.prefs.audio_hardware.default_input.clone() } else { vo.source.clone() };
    let input = s.voiceover.input();
    let devices = input.devices();
    let channels = input.channels(&source);
    json!({
        "settings": vo,
        "devices": devices,
        "channels": channels,
        "recording": s.voiceover.rec,
    })
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    let mut vo = s.prefs.voice_over.clone();
    if let Some(v) = str_p(p, "source") {
        vo.source = v.to_string();
    }
    if let Some(v) = u64_p(p, "inputChannel") {
        vo.input_channel = v.min(63) as u32;
    }
    if let Some(v) = str_p(p, "name") {
        let v = v.trim();
        if v.is_empty() {
            return Err(bad("audio.voiceover.settings", "the name can't be empty"));
        }
        vo.name = v.to_string();
    }
    if let Some(v) = bool_p(p, "countdownSoundCues") {
        vo.countdown_sound_cues = v;
    }
    if let Some(v) = f64_p(p, "prerollSeconds") {
        vo.preroll_seconds = v.clamp(0.0, 60.0);
    }
    if let Some(v) = f64_p(p, "postrollSeconds") {
        vo.postroll_seconds = v.clamp(0.0, 60.0);
    }
    if vo != s.prefs.voice_over {
        let mut prefs = s.prefs.clone();
        prefs.voice_over = vo;
        s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(settings_json(s))
}

fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let track = record_track(s, p)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if seq.audio_tracks.iter().any(|t| t.id == track && t.locked) {
        return Err(bad("audio.voiceover.start", "the record track is locked"));
    }
    let sr = seq.settings.sample_rate.max(1);
    let (mark_in, mark_out) = (seq.mark_in, seq.mark_out);
    let vo = s.prefs.voice_over.clone();
    let punch = mark_in.is_some() && mark_out.is_some_and(|o| Some(o) > mark_in);
    let record_start = if punch { mark_in.unwrap_or_default() } else { time_p(s, p, "").unwrap_or(s.playhead()) };
    if record_start < Tick::ZERO {
        return Err(bad("audio.voiceover.start", "the record point cannot be negative"));
    }
    let preroll = Tick::from_seconds_f64(f64_p(p, "preroll").unwrap_or(vo.preroll_seconds).clamp(0.0, 60.0));
    let capture_start = (record_start - preroll).max(Tick::ZERO);
    let punch_out = if punch { mark_out } else { None };
    let device = if vo.source.is_empty() { s.prefs.audio_hardware.default_input.clone() } else { vo.source.clone() };
    let format = s.voiceover.input().start(&device, sr).map_err(|e| EngineError::Other(format!("voice-over input: {e}")))?;
    let channel = vo.input_channel.min(format.channels.saturating_sub(1) as u32);
    s.voiceover.captured.clear();
    s.voiceover.pending_stop = None;
    s.voiceover.rec = Some(Recording { seq: seq_id, track, record_start, capture_start, punch_out, format, channel });
    let cues: Vec<i64> = if vo.countdown_sound_cues { cue_times(capture_start, record_start).into_iter().map(|t| t.0).collect() } else { Vec::new() };
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    Ok(json!({
        "recording": true,
        "track": crate::mixer::strip_label(seq, track),
        "recordStart": record_start.0,
        "captureStart": capture_start.0,
        "punchOut": punch_out.map(|t| t.0),
        "postroll": Tick::from_seconds_f64(vo.postroll_seconds).0,
        "cues": cues,
        "sampleRate": format.sample_rate,
    }))
}

/// Playback really started at `time`: drop what was captured so far and count from there.
fn sync(s: &mut Session, p: &Value) -> Result<Value> {
    if s.voiceover.pending_stop.is_some() {
        return Err(bad("audio.voiceover.sync", "save or discard the stopped take before restarting capture"));
    }
    if s.voiceover.rec.is_none() {
        return Err(bad("audio.voiceover.sync", "no voice-over is recording"));
    }
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    s.voiceover.input().discard();
    s.voiceover.captured.clear();
    s.voiceover.pending_stop = None;
    let Some(rec) = s.voiceover.rec.as_mut() else { return Err(bad("audio.voiceover.sync", "no voice-over is recording")) };
    rec.capture_start = t.clamp(Tick::ZERO, rec.record_start);
    Ok(json!({"captureStart": rec.capture_start.0}))
}

/// Directory recordings are written to.
fn record_dir(s: &Session, p: &Value) -> String {
    media_dir(s, p, "Voice-over Recordings")
}

/// Directory generated media (recordings, narrations) is written to: `dir`, else Project
/// Settings ▸ Scratch Disks ▸ Captured, else next to the project, else `fallback` in the data or
/// temporary directory.
pub(crate) fn media_dir(s: &Session, p: &Value, fallback: &str) -> String {
    if let Some(d) = str_p(p, "dir") {
        return d.to_string();
    }
    if let Some(d) = s.project.settings.scratch.captured.clone().filter(|d| !d.is_empty()) {
        return d;
    }
    if let Some(d) = s.path.as_deref().and_then(|x| std::path::Path::new(x).parent()).filter(|d| !d.as_os_str().is_empty()) {
        return d.to_string_lossy().into_owned();
    }
    let base = s.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.to_path_buf()).unwrap_or_else(|| crate::temp_dir().join("FilmCraft"));
    base.join(fallback).to_string_lossy().into_owned()
}

/// `<dir>/<base> <n>.wav` with the first `n` used by neither a project item nor an existing file;
/// an error once `n` passes 99 999 (bounded: `file_size` is a filesystem call per try).
pub(crate) fn unique_wav_path(s: &Session, dir: &str, base: &str) -> Result<String> {
    let names: std::collections::HashSet<String> = s.project.items.values().map(|i| i.name.clone()).collect();
    for k in 1..=99_999u32 {
        let file = format!("{base} {k}.wav");
        let path = std::path::Path::new(dir).join(&file).to_string_lossy().into_owned();
        if !names.contains(&file) && s.services.file_size(&path).is_err() {
            return Ok(path);
        }
    }
    Err(EngineError::Other(format!("{dir}: no free file name for \"{base} <n>.wav\"")))
}

fn stop(s: &mut Session, p: &Value) -> Result<Value> {
    let rec = s.voiceover.rec.clone().ok_or_else(|| bad("audio.voiceover.stop", "no voice-over is recording"))?;
    if bool_p(p, "discard").unwrap_or(false) {
        s.voiceover.input().stop();
        s.voiceover.rec = None;
        s.voiceover.pending_stop = None;
        s.voiceover.captured.clear();
        return Ok(json!({"recording": false, "placed": false}));
    }
    let t_stop = s.voiceover.pending_stop.unwrap_or_else(|| time_p(s, p, "").unwrap_or(s.playhead()));
    let sr = rec.format.sample_rate.max(1) as i64;
    let c0 = rec.capture_start.to_units_floor(sr);
    let r0 = rec.record_start.to_units_floor(sr);
    let end = rec.punch_out.map_or(t_stop, |o| o.min(t_stop)).to_units_floor(sr);
    // read the input up to the end of the take
    let want = end.saturating_sub(c0).max(0) as usize;
    if want.saturating_mul(usize::from(rec.format.channels).max(1)) > 268_435_456 {
        return Err(bad("audio.voiceover.stop", "recording exceeds the input buffer limit"));
    }
    let have = s.voiceover.captured.len();
    s.voiceover.captured.try_reserve(want.saturating_sub(have)).map_err(|e| bad("audio.voiceover.stop", format!("unable to allocate the recording: {e}")))?;
    s.voiceover.pending_stop = Some(t_stop);
    let input = s.voiceover.input();
    input.stop();
    let got = input.read(want.saturating_sub(have));
    let input_error = input.error();
    let ch = rec.channel as usize;
    if let Some(c) = got.get(ch).or(got.first()) {
        s.voiceover.captured.extend_from_slice(c);
    }
    if let Some(error) = input_error {
        return Err(bad("audio.voiceover.stop", format!("input failed: {error}")));
    }
    s.voiceover.captured.resize(want, 0.0);
    let skip = r0.saturating_sub(c0).clamp(0, want as i64) as usize;
    let samples = s.voiceover.captured.get(skip..).ok_or_else(|| bad("audio.voiceover.stop", "invalid recording trim range"))?;
    if samples.is_empty() {
        s.voiceover.rec = None;
        s.voiceover.pending_stop = None;
        s.voiceover.captured.clear();
        return Ok(json!({"recording": false, "placed": false}));
    }
    let sample_count = samples.len();
    let sequence = s.project.sequence(rec.seq).ok_or(EngineError::NoSequence)?;
    if !sequence.audio_tracks.iter().any(|track| track.id == rec.track && !track.locked) {
        return Err(bad("audio.voiceover.stop", "the record track was deleted or locked; restore it before saving the take"));
    }
    // write the file
    let dir = record_dir(s, p);
    let base = s.prefs.voice_over.name.clone();
    let path = unique_wav_path(s, &dir, &base)?;
    if !cfg!(target_arch = "wasm32") {
        std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
    }
    let samples = s.voiceover.captured.get(skip..).ok_or_else(|| bad("audio.voiceover.stop", "invalid recording trim range"))?;
    let bytes = write_wav_f32(samples, sr as u32);
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    // import + place as one undo step
    let n0 = s.history.undo.len();
    let item = crate::commands::import_bytes(s, &path, bytes.into(), None)?;
    let dur = Tick::from_units(sample_count as i64, sr);
    let at = rec.record_start;
    let track = rec.track;
    let rate = s.project.sequence(rec.seq).map(|q| q.settings.frame_rate).unwrap_or_default();
    let clip = s.edit("Record Voice-over", |pr, _| {
        let mut it = pr
            .make_track_item(item, TrackKind::Audio, at, TimeRange::new(Tick::ZERO, dur), rate)
            .ok_or(EngineError::Other("recording not importable".into()))?;
        it.duration = dur;
        let id = it.id;
        let mut next = pr.next_id;
        let seq = pr.sequence_mut(rec.seq).ok_or(EngineError::NoSequence)?;
        if !seq.audio_tracks.iter().any(|t| t.id == track) {
            return Err(bad("audio.voiceover.stop", "the record track was deleted"));
        }
        let durations = |_: filmcraft_project::ItemId| -> Option<Tick> { None };
        let mut ctx =
            filmcraft_edit::EditCtx { next_id: &mut next, media_duration: &durations, media_start: &|_| Tick::ZERO, min_duration: rate.frame_duration() };
        filmcraft_edit::overwrite(seq, vec![(track, it)], &mut ctx)?;
        seq.check().map_err(EngineError::Other)?;
        pr.next_id = next;
        Ok(id)
    });
    crate::clip_ops::collapse_history(s, n0, "Record Voice-over");
    let clip = clip?;
    let track_label = crate::mixer::strip_label(s.project.sequence(rec.seq).ok_or(EngineError::NoSequence)?, track);
    s.voiceover.rec = None;
    s.voiceover.pending_stop = None;
    s.voiceover.captured.clear();
    Ok(json!({
        "recording": false,
        "placed": true,
        "path": path,
        "item": item.0,
        "clip": clip.0,
        "track": track_label,
        "start": at.0,
        "duration": dur.0,
        "samples": sample_count,
        "sampleRate": sr,
    }))
}

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "audio.voiceover.settings",
            "Voice-Over Record Settings",
            r#"{"source":str?,"inputChannel":n?,"name":str?,"countdownSoundCues":bool?,"prerollSeconds":f64?,"postrollSeconds":f64?}"#,
            crate::commands::always,
            settings,
            true,
        ),
        spec("audio.voiceover.start", "Start Voice-over Recording", r#"{"track":"A1"|id?,"time":ticks?,"preroll":seconds?}"#, can_start, start, true),
        spec("audio.voiceover.sync", "Sync Voice-over Capture", r#"{"time":ticks?}"#, is_recording, sync, true),
        spec("audio.voiceover.stop", "Stop Voice-over Recording", r#"{"time":ticks?,"dir":str?,"discard":bool?}"#, is_recording, stop, true),
    ]
}

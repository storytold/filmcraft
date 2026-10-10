//! File ▸ Export ▸ AAF… / OMF… and File ▸ Import of AAF / OMF documents.
//!
//! The interchange crate writes the documents; this module prepares their audio essence the way
//! Premiere's AAF and OMF export dialogs offer it: embedded in the document or written as separate
//! WAV / AIFF files next to it, at a chosen sample rate and bit depth, trimmed to the used ranges
//! plus handles (or whole), with clip effects rendered in, broken out to mono; and for AAF an
//! optional video mixdown rendered to one file.
//!
//! `file.exportAaf {path, sequence?, mixdownVideo?, breakoutToMono?, audio?: "embedded"|"separate"|
//! "linked", audioFormat?: "wav"|"aiff"|"mxf" (OP-Atom, AAF only), sampleRate?, bitDepth?, trimAudio?, handles? (frames),
//! renderAudioEffects?, smallSectors?}`
//!
//! `file.exportOmf {path, sequence?, audio?: "embedded"|"separate", audioFormat?, sampleRate?,
//! bitDepth?, trimAudio? (default on), handles?, renderAudioEffects?, breakoutToMono?}`
//!
//! `file.importAaf {path}` (also `file.import` of `.aaf` / `.omf` files).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use filmcraft_interchange::essence::{AudioEssence, AudioNeed, EssenceData, EssenceKey, MediaOptions, MixdownVideo, NeedOptions, NestNeeds, audio_needs};
use filmcraft_project::{ClipId, ItemId, Project, Sequence, SequenceSettings, Track, TrackItem, TrackKind};
use filmcraft_time::{Tick, TimeRange};

use crate::commands::{bad, bool_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

/// Where exported audio goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    /// Inside the document.
    Embedded,
    /// Separate WAV / AIFF files next to the document.
    Separate,
    /// The original media files (AAF only; untrimmed, unrendered).
    Linked,
}

/// The audio settings of the AAF / OMF export dialogs.
#[derive(Clone, Debug)]
pub struct AudioPlan {
    pub mode: AudioMode,
    pub aiff: bool,
    /// Separate files as OP-Atom MXF (AAF only; Avid-style consolidated media).
    pub mxf: bool,
    pub sample_rate: u32,
    pub bits: u16,
    pub trim: bool,
    pub handles: Tick,
    pub render_effects: bool,
    pub breakout: bool,
    /// Nested sequences on the audio tracks. OMF: their sound is rendered into the export, as
    /// Premiere Pro does. AAF: they are written as compositions, so the media of the clips inside
    /// them is prepared like the rest.
    pub nests: NestNeeds,
}

fn plan_from(s: &Session, p: &Value, seq: ItemId, cmd: &str, omf: bool) -> Result<AudioPlan> {
    let q = s.project.sequence(seq).ok_or(EngineError::NoSequence)?;
    let mode = match str_p(p, "audio").unwrap_or("embedded") {
        "embedded" | "embed" | "encapsulate" => AudioMode::Embedded,
        "separate" | "separateAudio" => AudioMode::Separate,
        "linked" | "link" if !omf => AudioMode::Linked,
        other => return Err(bad(cmd, format!("unknown audio mode {other:?} (embedded, separate{})", if omf { "" } else { " or linked" }))),
    };
    let (aiff, mxf) = match str_p(p, "audioFormat").unwrap_or("wav") {
        "wav" | "bwf" | "broadcastWave" => (false, false),
        "aiff" | "aif" => (true, false),
        "mxf" | "opatom" if !omf => (false, true),
        other => return Err(bad(cmd, format!("unknown audio format {other:?} (wav, aiff{})", if omf { "" } else { " or mxf" }))),
    };
    let sample_rate = u64_p(p, "sampleRate").map(|r| r as u32).unwrap_or(q.settings.sample_rate.max(1));
    if !(8_000..=192_000).contains(&sample_rate) {
        return Err(bad(cmd, "sampleRate must be 8000–192000"));
    }
    let bits = u64_p(p, "bitDepth").unwrap_or(16) as u16;
    if bits != 16 && bits != 24 {
        return Err(bad(cmd, "bitDepth must be 16 or 24"));
    }
    let frames = u64_p(p, "handles").unwrap_or(if omf { 30 } else { 0 }) as i64;
    Ok(AudioPlan {
        mode,
        aiff,
        mxf,
        sample_rate,
        bits,
        trim: bool_p(p, "trimAudio").unwrap_or(omf),
        handles: q.settings.frame_rate.tick_of(frames),
        render_effects: bool_p(p, "renderAudioEffects").unwrap_or(false),
        breakout: bool_p(p, "breakoutToMono").unwrap_or(false),
        nests: if omf { NestNeeds::Render } else { NestNeeds::Inside },
    })
}

fn quantize(planar: &[&Vec<f32>], bits: u16) -> Vec<u8> {
    let n = planar.first().map_or(0, |c| c.len());
    let bps = if bits >= 24 { 3 } else { 2 };
    let mut out = Vec::with_capacity(n * planar.len() * bps);
    for i in 0..n {
        for c in planar {
            let x = c[i].clamp(-1.0, 1.0);
            if bps == 3 {
                out.extend_from_slice(&((x * 8_388_607.0).round() as i32).to_le_bytes()[..3]);
            } else {
                out.extend_from_slice(&((x * 32_767.0).round() as i16).to_le_bytes());
            }
        }
    }
    out
}

fn dequantize(pcm: &[u8], channels: usize, bits: u16) -> Vec<Vec<f32>> {
    let bps = bits.div_ceil(8) as usize;
    let ch = channels.max(1);
    let n = pcm.len() / (bps * ch).max(1);
    let mut out = vec![Vec::with_capacity(n); ch];
    for i in 0..n {
        for (c, o) in out.iter_mut().enumerate() {
            let s = &pcm[(i * ch + c) * bps..(i * ch + c + 1) * bps];
            let v = match bps {
                1 => (s[0] as f32 - 128.0) / 128.0,
                2 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0,
                3 => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
                _ => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            };
            o.push(v);
        }
    }
    out
}

/// Decode a media range at `sample_rate` (planar).
fn decode(s: &Session, need: &AudioNeed, sample_rate: u32) -> Result<Vec<Vec<f32>>> {
    let src = s
        .media
        .full_res_source(&s.project, need.item, &*s.services)
        .ok_or_else(|| EngineError::Other(format!("media {} is offline", need.path.as_deref().unwrap_or("?"))))?;
    let start = need.start.to_units_floor(sample_rate as i64);
    let frames = (need.end.to_units_floor(sample_rate as i64) - start).max(0) as usize;
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(frames); need.channels.max(1) as usize];
    let chunk = sample_rate as usize * 10;
    let mut done = 0usize;
    while done < frames {
        let n = chunk.min(frames - done);
        let b = src.audio_stream(need.audio_stream, start + done as i64, n, sample_rate).map_err(|e| EngineError::Other(e.to_string()))?;
        for (c, o) in out.iter_mut().enumerate() {
            match b.channels.get(c).or(b.channels.first()) {
                Some(x) => o.extend(x.iter().copied().chain(std::iter::repeat(0.0)).take(n)),
                None => o.extend(std::iter::repeat_n(0.0, n)),
            }
        }
        done += n;
    }
    Ok(out)
}

/// Render one clip (with its effects, gain and volume) over a media range, through the export
/// pipeline's audio mixer.
fn render_clip(s: &Session, seq: ItemId, need: &AudioNeed, sample_rate: u32, bits: u16) -> Result<Vec<Vec<f32>>> {
    let EssenceKey::Clip(clip_id) = need.key else { return decode(s, need, sample_rate) };
    let (q, track, c) = audio_clip(&s.project, seq, clip_id).ok_or_else(|| EngineError::Other("clip not found".into()))?;
    let c = c.clone();
    let mut p: Project = (*s.project).clone();
    let settings = SequenceSettings { sample_rate, ..q.settings.clone() };
    let rs = p.new_sequence("AAF audio render", settings, 0, 1, None);
    let dur = need.end - need.start;
    let mut c = c;
    c.start = Tick::ZERO;
    c.source_in = need.start;
    c.duration = dur;
    c.speed = 1.0;
    c.reverse = false;
    c.link = None;
    let channels = match track.channels {
        filmcraft_project::AudioChannels::Mono => 1,
        filmcraft_project::AudioChannels::Surround51 => 6,
        _ => 2,
    };
    if let Some(t) = p.sequence_mut(rs).and_then(|r| r.audio_tracks.first_mut()) {
        t.channels = track.channels;
        t.items = vec![c];
    }
    let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let sink_buf = buf.clone();
    let mut settings = filmcraft_export::ExportSettings {
        format: filmcraft_export::Format::Wav,
        path: "render.wav".into(),
        range: Some(TimeRange::new(Tick::ZERO, dur)),
        part_of_batch: true,
        sink: Some(filmcraft_export::OutputSink(Arc::new(move |_: &str, d: Vec<u8>| {
            *sink_buf.lock().unwrap_or_else(|e| e.into_inner()) = d;
            Ok(())
        }))),
        ..Default::default()
    };
    settings.audio.sample_rate = Some(sample_rate);
    settings.audio.channels = channels;
    settings.audio.bits = bits;
    let project = Arc::new(p);
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    filmcraft_export::export(&project, rs, &settings, &provider, &Default::default()).map_err(|e| EngineError::Other(e.to_string()))?;
    let wav = std::mem::take(&mut *buf.lock().unwrap_or_else(|e| e.into_inner()));
    let (pcm, ch, _, b) = filmcraft_interchange::wav::parse_wav(&wav).ok_or_else(|| EngineError::Other("audio render produced no WAVE data".into()))?;
    Ok(dequantize(&pcm, ch as usize, b))
}

/// The audio clip `clip` with its track and sequence: in `seq`, or in a sequence nested in it
/// (clip ids are unique in a project).
fn audio_clip(p: &Project, seq: ItemId, clip: ClipId) -> Option<(&Sequence, &Track, &TrackItem)> {
    let first = p.sequence(seq).into_iter();
    let others = p.sequences().filter(|i| i.id != seq).filter_map(|i| p.sequence(i.id));
    for q in first.chain(others) {
        if let Some((t, c)) = q.tracks(TrackKind::Audio).iter().find_map(|t| t.items.iter().find(|c| c.id == clip).map(|c| (t, c))) {
            return Some((q, t, c));
        }
    }
    None
}

fn safe_name(s: &str) -> String {
    let n: String = s.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
    let n = n.trim().trim_matches('.').to_string();
    if n.is_empty() { "audio".into() } else { n }
}

fn ensure_dir(s: &Session, dir: &str) {
    if !s.services.export_in_memory() && !dir.is_empty() {
        let _ = std::fs::create_dir_all(dir);
    }
}

fn check_linked_streams(s: &Session, seq: ItemId, plan: &AudioPlan) -> Result<()> {
    if plan.mode == AudioMode::Linked && !plan.trim && !plan.render_effects {
        let needs = audio_needs(&s.project, seq, &NeedOptions { nests: plan.nests, ..Default::default() });
        if needs.iter().any(|need| need.audio_stream > 0) {
            return Err(EngineError::Other("linked AAF media cannot select container audio streams; choose embedded or separate audio".into()));
        }
    }
    Ok(())
}

/// Prepare the audio essence of an export; returns the essence and the files written.
pub fn prepare_audio(s: &Session, seq: ItemId, plan: &AudioPlan, media_dir: &str) -> Result<(Vec<AudioEssence>, Vec<String>)> {
    check_linked_streams(s, seq, plan)?;
    if plan.mode == AudioMode::Linked && !plan.trim && !plan.render_effects {
        return Ok((Vec::new(), Vec::new()));
    }
    let needs = audio_needs(
        &s.project,
        seq,
        &NeedOptions { handles: plan.handles, per_clip: plan.render_effects, whole_media: !plan.trim && !plan.render_effects, nests: plan.nests },
    );
    let mut essence = Vec::new();
    let mut files = Vec::new();
    let mut used = std::collections::HashSet::new();
    for need in &needs {
        // a nested sequence has no file to read: its sound is mixed, with what is on the clip
        let nest = s.project.sequence(need.item).is_some();
        let planar = if plan.render_effects || nest { render_clip(s, seq, need, plan.sample_rate, plan.bits)? } else { decode(s, need, plan.sample_rate)? };
        let frames = planar.first().map_or(0, Vec::len) as u64;
        let groups: Vec<(Option<u32>, Vec<&Vec<f32>>)> =
            if plan.breakout { planar.iter().enumerate().map(|(c, x)| (Some(c as u32), vec![x])).collect() } else { vec![(None, planar.iter().collect())] };
        let base = match need.key {
            EssenceKey::Clip(c) => {
                let name = audio_clip(&s.project, seq, c).map(|(_, _, ti)| ti.name.clone()).unwrap_or_default();
                format!("{} {}", safe_name(&name), c.0)
            }
            EssenceKey::Media(_) | EssenceKey::MediaStream { .. } => safe_name(
                need.path
                    .as_deref()
                    .map(|p| std::path::Path::new(p).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default())
                    .as_deref()
                    .unwrap_or("audio"),
            ),
        };
        for (ch, chans) in groups {
            let pcm = quantize(&chans, plan.bits);
            let n = chans.len() as u32;
            let data = match plan.mode {
                AudioMode::Embedded => EssenceData::Embedded(pcm),
                AudioMode::Separate | AudioMode::Linked => {
                    let ext = if plan.mxf {
                        "mxf"
                    } else if plan.aiff {
                        "aif"
                    } else {
                        "wav"
                    };
                    let stem = match ch {
                        Some(c) => format!("{base}_A{}", c + 1),
                        None => base.clone(),
                    };
                    let mut name = format!("{stem}.{ext}");
                    let mut k = 1;
                    while !used.insert(name.to_ascii_lowercase()) {
                        k += 1;
                        name = format!("{stem} ({k}).{ext}");
                    }
                    let path = if media_dir.is_empty() { name.clone() } else { format!("{media_dir}/{name}") };
                    let bytes = if plan.mxf {
                        let bps = plan.bits.div_ceil(8) as usize;
                        let samples: Vec<i32> = pcm
                            .chunks_exact(bps)
                            .map(|b| if bps == 3 { i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8 } else { i16::from_le_bytes([b[0], b[1]]) as i32 })
                            .collect();
                        let opts = filmcraft_mxf::OpAtomPcm {
                            sample_rate: plan.sample_rate,
                            bits: plan.bits as u32,
                            channels: n,
                            edit_rate: None,
                            ids: filmcraft_mxf::PackageIds::from_seed(&path, &name),
                            timecode: None,
                        };
                        filmcraft_mxf::write_opatom_pcm(&opts, &samples).map_err(|e| EngineError::Other(format!("{path}: {e}")))?
                    } else if plan.aiff {
                        filmcraft_interchange::wav::aiff_file(&pcm, n as u16, plan.sample_rate, plan.bits)
                    } else {
                        filmcraft_interchange::wav::wav_file(&pcm, n as u16, plan.sample_rate, plan.bits)
                    };
                    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
                    files.push(path.clone());
                    EssenceData::File { path }
                }
            };
            essence.push(AudioEssence {
                key: need.key,
                channel: ch,
                start: need.start,
                frames,
                sample_rate: plan.sample_rate,
                bits: plan.bits,
                channels: n,
                data,
                effects_rendered: plan.render_effects || nest,
            });
        }
    }
    Ok((essence, files))
}

/// Render the video of `seq` to one file for an AAF video mixdown.
fn mixdown(s: &Session, seq: ItemId, path: &str) -> Result<MixdownVideo> {
    let q = s.project.sequence(seq).ok_or(EngineError::NoSequence)?;
    let dur = q.duration();
    if dur <= Tick::ZERO {
        return Err(EngineError::Other("the sequence is empty".into()));
    }
    let format = if path.to_ascii_lowercase().ends_with(".mxf") {
        filmcraft_export::Format::from_name("mxf-opatom").unwrap_or(filmcraft_export::Format::DnxHr)
    } else {
        filmcraft_export::Format::DnxHr
    };
    let mut settings = filmcraft_export::ExportSettings {
        format,
        path: path.to_string(),
        range: Some(TimeRange::new(Tick::ZERO, dur)),
        include_audio: false,
        part_of_batch: true,
        dnx_profile: "hq".into(),
        ..Default::default()
    };
    if s.services.export_in_memory() {
        let services = s.services.clone();
        settings.sink = Some(filmcraft_export::OutputSink(Arc::new(move |p: &str, d: Vec<u8>| services.write_file(p, &d))));
    }
    let project = s.project.clone();
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    filmcraft_export::export(&project, seq, &settings, &provider, &Default::default()).map_err(|e| EngineError::Other(format!("video mixdown: {e}")))?;
    Ok(MixdownVideo { path: path.to_string(), start: Tick::ZERO, duration: dur, width: q.settings.width, height: q.settings.height })
}

fn report_json(r: &filmcraft_interchange::Report) -> Vec<String> {
    r.entries.iter().map(|e| if e.count > 1 { format!("{} (×{})", e.message, e.count) } else { e.message.clone() }).collect()
}

fn split(path: &str) -> (String, String) {
    let p = std::path::Path::new(path);
    let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    let stem = p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Export".into());
    (dir, stem)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// `file.exportAaf`
pub fn export_aaf(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.exportAaf";
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`"))?.to_string();
    let seq = crate::export_tools::sequence_param(s, p, cmd)?;
    let plan = plan_from(s, p, seq, cmd, false)?;
    check_linked_streams(s, seq, &plan)?;
    let (dir, stem) = split(&path);
    let media_dir = join(&dir, &format!("{stem} Media"));
    let mut files = Vec::new();
    if plan.mode != AudioMode::Embedded || bool_p(p, "mixdownVideo").unwrap_or(false) {
        ensure_dir(s, &media_dir);
    }
    let (essence, written) = prepare_audio(s, seq, &plan, &media_dir)?;
    files.extend(written);
    let mixdown_video = if bool_p(p, "mixdownVideo").unwrap_or(false) {
        let ext = if str_p(p, "mixdownFormat") == Some("mxf") { "mxf" } else { "mov" };
        let m = mixdown(s, seq, &join(&media_dir, &format!("{} Video Mixdown.{ext}", safe_name(&stem))))?;
        files.push(m.path.clone());
        Some(m)
    } else {
        None
    };
    let opts = filmcraft_interchange::aaf::AafOptions {
        name: s.project.item(seq).map(|i| i.name.clone()),
        media: MediaOptions { breakout_to_mono: plan.breakout, audio_only: false, essence, mixdown_video },
        small_sectors: bool_p(p, "smallSectors").unwrap_or(false),
    };
    let (bytes, report) = filmcraft_interchange::aaf::export(&s.project, seq, &opts).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": bytes.len(), "mediaFiles": files, "report": report_json(&report)}))
}

/// `file.exportOmf`
pub fn export_omf(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.exportOmf";
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`"))?.to_string();
    let seq = crate::export_tools::sequence_param(s, p, cmd)?;
    let plan = plan_from(s, p, seq, cmd, true)?;
    let (dir, stem) = split(&path);
    let media_dir = join(&dir, &format!("{stem} Audio Files"));
    if plan.mode == AudioMode::Separate {
        ensure_dir(s, &media_dir);
    }
    let (essence, files) = prepare_audio(s, seq, &plan, &media_dir)?;
    let opts = filmcraft_interchange::omf::OmfOptions {
        name: str_p(p, "title").map(str::to_string).or_else(|| s.project.item(seq).map(|i| i.name.clone())),
        media: MediaOptions { breakout_to_mono: plan.breakout, audio_only: true, essence, mixdown_video: None },
    };
    let (bytes, report) = filmcraft_interchange::omf::export(&s.project, seq, &opts).map_err(|e| EngineError::Other(e.to_string()))?;
    if bytes.len() as u64 > i32::MAX as u64 {
        return Err(EngineError::Other("the OMF file would exceed 2 GB; use separate audio files or trim the audio".into()));
    }
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": bytes.len(), "mediaFiles": files, "report": report_json(&report)}))
}

/// `file.importAaf` (and OMF): import a document as sequences.
pub fn import_document(s: &mut Session, p: &Value, cmd: &str) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`"))?.to_string();
    let bytes = s.services.read_file(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let format = crate::interchange::detect(&path, &bytes).filter(|f| matches!(f, filmcraft_interchange::Format::Aaf | filmcraft_interchange::Format::Omf));
    let format = format.ok_or_else(|| bad(cmd, format!("{path} is not an AAF or OMF file")))?;
    crate::interchange::import(s, &path, &bytes, format)
}

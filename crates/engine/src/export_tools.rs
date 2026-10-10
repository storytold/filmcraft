//! Export mode for every client: export presets (built-in + the user's), building
//! [`ExportSettings`] from a preset plus overrides, the export queue and Quick Export.
//!
//! - **Presets.** Built-ins come from [`filmcraft_export::presets`]; user presets and favourites
//!   persist in `<data dir>/export-presets.json`
//!   (`{"format": "filmcraft.export-presets", "version": 1, "presets": […], "favorites": […]}`); the
//!   same format is used to import / export preset files. A user preset shadows a built-in of the
//!   same name. Names compare case- and punctuation-insensitively.
//! - **Settings.** Every export command takes `preset` (a name), `settings` (an
//!   [`ExportSettings`] JSON object in camelCase, merged over the preset) and a few flat
//!   overrides (`format`, `width`/`height`, `fps`, `bitrateKbps`, …), plus `path`, `sequence` and
//!   `range` (`entire` | `inOut` | `workArea` | `custom` with `start…` / `end…` times).
//! - **Queue** (session state, not saved with the project). `export.queue.add` snapshots the
//!   project and resolves the settings; `export.queue.start` encodes the ready items one after
//!   another as ordinary background jobs ([`crate::Job`], visible in `jobs.list`); the frontend's
//!   per-frame [`Session::poll_persistence`] (or `export.queue.list`) advances it.
//! - **Quick Export** (`export.quick`): the active sequence with a preset (the last one used,
//!   initially Match Source – Adaptive High Bitrate) to a default path next to the project.
//!
//! Commands: `export.presets.{list,get,save,delete,favorite,import,export}`, `export.formats`,
//! `export.resolve`, `export.queue.{add,list,start,stop,cancel,retry,remove,move,clear}`,
//! `export.quick`; `file.exportMedia` uses the same settings builder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_export::presets::{DEFAULT_PRESET, preset_key};
use filmcraft_export::{BitrateMode, ExportPreset, ExportSettings, Format, GpuRendering, HardwareEncoding};
use filmcraft_project::{ItemId, Project};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Event, Result, Session};

pub const FILE_FORMAT: &str = "filmcraft.export-presets";
const LIBRARY_FILE: &str = "export-presets.json";

// ---------------------------------------------------------------------------------------------
// preset library
// ---------------------------------------------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct PresetFile {
    format: String,
    version: u32,
    #[serde(default)]
    presets: Vec<ExportPreset>,
    #[serde(default)]
    favorites: Vec<String>,
}

/// User export presets and favourites (+ where they persist).
#[derive(Default)]
pub struct ExportPresetLibrary {
    pub user: Vec<ExportPreset>,
    /// Favourite preset names (built-in or user).
    pub favorites: Vec<String>,
    dir: Option<PathBuf>,
}

impl ExportPresetLibrary {
    /// Persist in `data_dir` (and load what is there).
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.to_path_buf());
        let path = data_dir.join(LIBRARY_FILE);
        if let Ok(bytes) = std::fs::read(&path) {
            match parse_file(&bytes) {
                Ok(f) => {
                    self.user = f.presets;
                    self.favorites = f.favorites;
                }
                Err(e) => log::warn!("{}: {e}", path.display()),
            }
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn persist(&self) -> std::result::Result<(), String> {
        let Some(d) = &self.dir else { return Ok(()) };
        let _ = std::fs::create_dir_all(d);
        filmcraft_format::atomic_write(&d.join(LIBRARY_FILE), &file_bytes(&self.user, &self.favorites)).map_err(|e| e.to_string())
    }

    /// Built-in and user presets (user presets shadow built-ins of the same name).
    pub fn all(&self) -> Vec<ExportPreset> {
        let mut v: Vec<ExportPreset> =
            filmcraft_export::builtin_presets().into_iter().filter(|b| !self.user.iter().any(|u| preset_key(&u.name) == preset_key(&b.name))).collect();
        v.extend(self.user.iter().cloned());
        v
    }

    pub fn find(&self, name: &str) -> Option<ExportPreset> {
        let k = preset_key(name);
        self.user.iter().find(|p| preset_key(&p.name) == k).cloned().or_else(|| filmcraft_export::presets::find_builtin(name))
    }

    pub fn is_favorite(&self, name: &str) -> bool {
        let k = preset_key(name);
        self.favorites.iter().any(|f| preset_key(f) == k)
    }
}

fn file_bytes(presets: &[ExportPreset], favorites: &[String]) -> Vec<u8> {
    let f = PresetFile { format: FILE_FORMAT.into(), version: 1, presets: presets.to_vec(), favorites: favorites.to_vec() };
    serde_json::to_vec_pretty(&f).unwrap_or_default()
}

fn parse_file(bytes: &[u8]) -> std::result::Result<PresetFile, String> {
    let f: PresetFile = serde_json::from_slice(bytes).map_err(|e| format!("not an export preset file: {e}"))?;
    if f.format != FILE_FORMAT {
        return Err(format!("not an export preset file (format `{}`)", f.format));
    }
    if f.version > 1 {
        return Err(format!("export preset file version {} is newer than this build reads (1)", f.version));
    }
    if let Some(p) = f.presets.iter().find(|p| p.name.trim().is_empty()) {
        return Err(format!("a preset has no name ({:?})", p.category));
    }
    Ok(f)
}

// ---------------------------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------------------------

/// `sampleRate` → `sample_rate`.
fn camel_to_snake(k: &str) -> String {
    let mut out = String::with_capacity(k.len() + 2);
    for c in k.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Deep-merge `patch` into `base` (objects merge, everything else replaces).
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                // Some nested settings structs serialize snake_case (`audio.sample_rate`); accept the documented camelCase
                // spelling for them too instead of adding a second, ignored key next to the default.
                let snake = camel_to_snake(k);
                let key = if !b.contains_key(k) && b.contains_key(&snake) { snake } else { k.clone() };
                merge(b.entry(key).or_insert(Value::Null), v);
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// A frame rate from a number of frames per second (29.97 → 30000/1001) or "num/den".
pub fn parse_rate(v: &Value) -> Option<FrameRate> {
    if let Some(s) = v.as_str() {
        if let Some((n, d)) = s.split_once('/') {
            return Some(FrameRate::new(n.trim().parse().ok()?, d.trim().parse().ok()?));
        }
        return parse_rate(&json!(s.trim().parse::<f64>().ok()?));
    }
    let f = v.as_f64().filter(|f| *f > 0.0 && *f <= 1000.0)?;
    let ntsc = [(23.976, FrameRate::FPS_23_976), (29.97, FrameRate::FPS_29_97), (59.94, FrameRate::FPS_59_94), (119.88, FrameRate::FPS_119_88)];
    if let Some((_, r)) = ntsc.iter().find(|(x, _)| (f - x).abs() < 0.01) {
        return Some(*r);
    }
    if (f - f.round()).abs() < 1e-6 { Some(FrameRate::new(f.round() as i64, 1)) } else { Some(FrameRate::new((f * 1000.0).round() as i64, 1000)) }
}

/// The preset named in `p` (or None), and the settings: preset ⊕ `settings` ⊕ flat overrides.
pub fn settings_from_params(s: &Session, p: &Value, cmd: &str) -> Result<(Option<ExportPreset>, ExportSettings)> {
    let preset = match str_p(p, "preset") {
        Some(name) => Some(s.export_presets.find(name).ok_or_else(|| bad(cmd, format!("no export preset named `{name}` (see export.presets.list)")))?),
        None => None,
    };
    let mut settings = preset.as_ref().map(|x| x.settings.clone()).unwrap_or_default();
    if preset.is_none()
        && let Some(f) = str_p(p, "format")
    {
        settings.format = Format::from_name(f).ok_or_else(|| bad(cmd, format!("unknown format `{f}`")))?;
    }
    if let Some(patch) = p.get("settings").filter(|v| v.is_object()) {
        let mut v = serde_json::to_value(&settings).map_err(|e| EngineError::Other(e.to_string()))?;
        merge(&mut v, patch);
        settings = serde_json::from_value(v).map_err(|e| bad(cmd, format!("settings: {e}")))?;
    }
    // flat overrides (the original file.exportMedia parameters and a few common ones)
    if let Some(f) = str_p(p, "format") {
        settings.format = Format::from_name(f).ok_or_else(|| bad(cmd, format!("unknown format `{f}`")))?;
    }
    if let Some(v) = f64_p(p, "scale") {
        settings.scale = v as f32;
    }
    if let Some(v) = bool_p(p, "audio") {
        settings.include_audio = v;
    }
    if let Some(v) = u64_p(p, "quality") {
        settings.quality = v.min(100) as u8;
    }
    if let Some(v) = crate::commands::checked_u32_p(p, "bitrateKbps", cmd)? {
        settings.bitrate_kbps = v;
        settings.adaptive_bitrate = None;
    }
    if let Some(v) = crate::commands::checked_u32_p(p, "maxBitrateKbps", cmd)? {
        settings.max_bitrate_kbps = Some(v);
    }
    if let Some(v) = str_p(p, "bitrateMode") {
        settings.bitrate_mode = serde_json::from_value(json!(v)).map_err(|_| bad(cmd, "bitrateMode: cbr | vbr1Pass | vbr2Pass | crf"))?;
    }
    if let Some(v) = f64_p(p, "crf") {
        settings.crf = v as f32;
        if p.get("bitrateMode").is_none() {
            settings.bitrate_mode = BitrateMode::Crf;
        }
    }
    if let Some(v) = crate::commands::checked_u32_p(p, "keyframeDistance", cmd)? {
        settings.keyframe_distance = Some(v);
    }
    if let Some(v) = p.get("hardwareEncoding") {
        settings.hardware_encoding = match v {
            Value::Bool(on) => {
                if *on {
                    HardwareEncoding::Auto
                } else {
                    HardwareEncoding::Off
                }
            }
            other => serde_json::from_value(other.clone()).map_err(|_| bad(cmd, "hardwareEncoding: off | auto"))?,
        };
    }
    if let Some(v) = p.get("gpuRendering") {
        settings.gpu_rendering = match v {
            Value::Bool(on) => {
                if *on {
                    GpuRendering::Auto
                } else {
                    GpuRendering::Off
                }
            }
            other => serde_json::from_value(other.clone()).map_err(|_| bad(cmd, "gpuRendering: off | auto"))?,
        };
    }
    if let Some(v) = bool_p(p, "burnCaptions") {
        settings.burn_captions = v;
    }
    if let Some(v) = str_p(p, "proresProfile") {
        settings.prores_profile = v.to_string();
    }
    if let Some(v) = str_p(p, "dnxProfile") {
        settings.dnx_profile = v.to_string();
    }
    if let Some(v) = str_p(p, "apvProfile") {
        settings.apv_profile = v.to_string();
    }
    if let Some(v) = str_p(p, "mxfVideoCodec") {
        settings.mxf_video_codec = serde_json::from_value(json!(v)).map_err(|_| bad(cmd, "mxfVideoCodec: dnxhr | proRes | h264"))?;
    }
    if let Some(v) = bool_p(p, "sdr") {
        settings.sdr = v;
    }
    if let Some(v) = bool_p(p, "alpha") {
        settings.alpha = v;
    }
    match (crate::commands::checked_u32_p(p, "width", cmd)?, crate::commands::checked_u32_p(p, "height", cmd)?) {
        (Some(w), Some(h)) => settings.frame_size = Some((w, h)),
        (None, None) => {}
        _ => return Err(bad(cmd, "provide both width and height")),
    }
    if let Some(v) = p.get("fps") {
        settings.frame_rate = Some(parse_rate(v).ok_or_else(|| bad(cmd, "fps: a number (29.97) or \"num/den\""))?);
    }
    if let Some(v) = str_p(p, "captionSidecar") {
        settings.caption_sidecar = (!v.is_empty() && v != "none").then(|| v.to_ascii_lowercase());
    }
    if let Some(v) = f64_p(p, "loudnessLufs") {
        settings.effects.loudness.enabled = true;
        settings.effects.loudness.target_lufs = v;
    }
    settings.validate().map_err(|e| bad(cmd, e.to_string()))?;
    Ok((preset, settings))
}

/// The sequence an export command targets (`sequence` param, else the active one).
pub fn sequence_param(s: &Session, p: &Value, cmd: &str) -> Result<ItemId> {
    match u64_p(p, "sequence") {
        Some(id) => {
            let id = ItemId(id);
            s.project.sequence(id).map(|_| id).ok_or_else(|| bad(cmd, "no such sequence"))
        }
        None => s.state.active_sequence.ok_or(EngineError::NoSequence),
    }
}

/// Export ▸ Range: `entire` | `inOut` | `workArea` | `custom` (`start…` / `end…` times).
/// None = the default (In/Out when set, else the whole sequence); bare `start…` / `end…` times
/// without a `range` mean `custom`.
pub fn range_param(s: &Session, project: &Project, seq: ItemId, v: Option<&Value>, p: &Value, cmd: &str) -> Result<Option<TimeRange>> {
    let q = project.sequence(seq).ok_or(EngineError::NoSequence)?;
    q.check_bounds().map_err(|e| bad(cmd, e))?;
    let fd = q.settings.frame_rate.frame_duration();
    let whole = TimeRange::from_bounds(Tick::ZERO, q.duration().max(fd));
    let mode = match v {
        None | Some(Value::Null) if time_p(s, p, "start").is_some() || time_p(s, p, "end").is_some() => "custom".to_string(),
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(m)) => m.to_ascii_lowercase().replace(['-', '_', ' ', '/'], ""),
        Some(o @ Value::Object(_)) => return range_param(s, project, seq, o.get("mode").or(Some(&json!("custom"))), o, cmd),
        Some(other) => return Err(bad(cmd, format!("range: unexpected {other}"))),
    };
    let range = match mode.as_str() {
        "entire" | "entiresequence" | "sequence" | "all" => whole,
        "inout" | "sequenceinout" => filmcraft_export::export_range(project, seq, &ExportSettings::default()).map_err(|e| bad(cmd, e.to_string()))?,
        "workarea" => q.work_area.ok_or_else(|| bad(cmd, "the sequence has no work area"))?,
        "custom" => {
            let a = time_p(s, p, "start").ok_or_else(|| bad(cmd, "custom range: need start (startTime / startSeconds / startFrame / startTimecode)"))?;
            let b = time_p(s, p, "end").ok_or_else(|| bad(cmd, "custom range: need end (endTime / endSeconds / endFrame / endTimecode)"))?;
            if b <= a {
                return Err(bad(cmd, "custom range: end must be after start"));
            }
            let duration = b.0.checked_sub(a.0).ok_or_else(|| bad(cmd, "custom range: duration overflows"))?;
            TimeRange::new(a, Tick(duration))
        }
        other => return Err(bad(cmd, format!("range `{other}`: entire | inOut | workArea | custom"))),
    };
    filmcraft_export::validate_range(range).map_err(|e| bad(cmd, e.to_string()))?;
    Ok(Some(range))
}

/// Where exports go when no path is given: next to the saved project, else ~/Movies, else the
/// home directory, else the temporary directory.
pub fn default_export_dir(s: &Session) -> PathBuf {
    if let Some(d) = s.path.as_deref().and_then(|p| Path::new(p).parent()).filter(|d| !d.as_os_str().is_empty()) {
        return d.to_path_buf();
    }
    if cfg!(target_arch = "wasm32") {
        // the web build's virtual file table: written files are offered as downloads
        return PathBuf::from("/exports");
    }
    if let Some(h) = crate::media_browser::std_home_dir() {
        let movies = Path::new(&h).join("Movies");
        return if movies.is_dir() { movies } else { PathBuf::from(h) };
    }
    crate::temp_dir()
}

fn file_safe(n: &str) -> String {
    let s: String = n.chars().map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "Export".into() } else { s }
}

/// `path` with the settings' extension (added when missing; image sequences keep theirs).
fn with_extension(path: &str, settings: &ExportSettings) -> String {
    let ext = settings.extension();
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case(ext) || (settings.format == Format::TiffSequence && e.eq_ignore_ascii_case("tiff")) => path.to_string(),
        Some(_) if settings.is_image_sequence() || matches!(settings.format, Format::Aiff) => format!("{}.{ext}", path.rsplit_once('.').map_or(path, |x| x.0)),
        _ => format!("{path}.{ext}"),
    }
}

/// `~/` expanded to the home directory (`USERPROFILE` on Windows). On Windows `/` becomes `\`:
/// after a `\\?\` prefix (what `canonicalize` returns) `/` is not a separator.
pub fn expand_home(p: &str) -> String {
    let p = match (p.strip_prefix("~/"), crate::media_browser::std_home_dir()) {
        (Some(rest), Some(h)) => format!("{h}/{rest}"),
        _ => p.to_string(),
    };
    if cfg!(windows) { p.replace('/', "\\") } else { p }
}

/// `x` names a folder: it ends in a separator or already exists as one.
fn is_folder(x: &str) -> bool {
    x.ends_with(['/', '\\']) || Path::new(x).is_dir()
}

/// Output path for `seq`: the `path` param (a file, or a directory ending in `/` or existing), else
/// `<default dir>/<sequence name>.<ext>`; `suffix` distinguishes several items.
fn output_path(s: &Session, p: &Value, seq: ItemId, settings: &ExportSettings, suffix: Option<usize>) -> String {
    let name = s.project.item(seq).map(|i| i.name.clone()).unwrap_or_else(|| "Sequence".into());
    let base = match str_p(p, "path").map(expand_home) {
        Some(x) if is_folder(&x) => Path::new(&x).join(file_safe(&name)).to_string_lossy().to_string(),
        Some(x) => x,
        None => default_export_dir(s).join(file_safe(&name)).to_string_lossy().to_string(),
    };
    let path = with_extension(&base, settings);
    match suffix {
        Some(n) if n > 0 => match path.rsplit_once('.') {
            Some((a, e)) => format!("{a}_{}.{e}", n + 1),
            None => format!("{path}_{}", n + 1),
        },
        _ => path,
    }
}

/// Write the caption sidecar (`settings.caption_sidecar`) next to `path` for `range` of `seq`.
fn write_sidecar(s: &Session, project: &Project, seq: ItemId, settings: &ExportSettings) -> Result<Option<String>> {
    let Some(kind) = settings.caption_sidecar.as_deref() else { return Ok(None) };
    let q = project.sequence(seq).ok_or(EngineError::NoSequence)?;
    let Some(track) = q.caption_tracks.first() else { return Ok(None) };
    let format = filmcraft_captions::Format::from_name(kind).ok_or_else(|| bad("export", format!("caption sidecar `{kind}`: srt | vtt")))?;
    let range = filmcraft_export::export_range(project, seq, settings).map_err(|e| EngineError::Other(e.to_string()))?;
    let doc = filmcraft_captions::document_from_track(track, range.start);
    let bytes =
        filmcraft_captions::write(&doc, format, filmcraft_captions::WriteOptions { drop_frame: q.settings.drop_frame, rate: Some(q.settings.frame_rate) });
    let ext = if format == filmcraft_captions::Format::WebVtt { "vtt" } else { kind };
    let path = format!("{}.{ext}", settings.path.rsplit_once('.').map_or(settings.path.as_str(), |x| x.0));
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(Some(path))
}

/// Run an export of `seq` in `project` as a background job (or now, with `wait`); returns the job id.
pub fn spawn_export(s: &mut Session, project: Arc<Project>, seq: ItemId, mut settings: ExportSettings, label: String, wait: bool) -> Result<u64> {
    let format = settings.format;
    if !filmcraft_export::available(format) {
        return Err(EngineError::Other(format!("{} export is not available (no encoder registered)", format.label())));
    }
    if s.services.export_in_memory() {
        let services = s.services.clone();
        settings.sink = Some(filmcraft_export::OutputSink(Arc::new(move |path: &str, data: Vec<u8>| services.write_file(path, &data))));
    }
    if let Some(dir) = Path::new(&settings.path).parent().filter(|d| !d.as_os_str().is_empty() && !s.services.export_in_memory()) {
        let _ = std::fs::create_dir_all(dir);
    }
    write_sidecar(s, &project, seq, &settings)?;
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    // Export always renders full-resolution media, whatever the proxy toggle says.
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    let (prog, res) = (job.progress.clone(), job.result.clone());
    if cfg!(target_arch = "wasm32") && !wait && filmcraft_export::stepped(format) {
        // No threads: the host advances the export between UI frames (`Session::pump_jobs`).
        let mut exporter = filmcraft_export::Exporter::new(project, seq, &settings, &prog).map_err(|e| EngineError::Other(e.to_string()))?;
        exporter.set_batch(1);
        s.jobs.push(job);
        s.stepped.push(crate::SteppedJob { job: id, exporter, provider, progress: prog, result: res });
        return Ok(id);
    }
    let run = move || {
        let r = filmcraft_export::export(&project, seq, &settings, &provider, &prog).map_err(|e| e.to_string());
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
            prog.finished.store(true, Ordering::Relaxed);
        }
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if wait || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-export".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(id)
}

/// Last-resort guard for a background job body (export, previews, proxies, …): a panic marks the
/// job failed with an error instead of killing its thread and leaving the job "running" forever.
/// The panic hook has already logged where it happened.
pub(crate) fn guard_job(
    progress: Arc<filmcraft_export::Progress>,
    result: Arc<std::sync::Mutex<Option<std::result::Result<filmcraft_export::Report, String>>>>,
    run: impl FnOnce() + Send + 'static,
) -> impl FnOnce() + Send + 'static {
    move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err() {
            let msg = "internal error (see the crash log); the job was stopped".to_string();
            *progress.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(msg.clone());
            progress.finished.store(true, Ordering::Relaxed);
            *result.lock().unwrap_or_else(|x| x.into_inner()) = Some(Err(msg));
        }
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// `file.exportMedia`: export the active (or `sequence`) sequence now.
pub fn export_media(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.exportMedia";
    let seq = sequence_param(s, p, cmd)?;
    let (preset, mut settings) = settings_from_params(s, p, cmd)?;
    if str_p(p, "path").is_none() {
        return Err(bad(cmd, "need `path`"));
    }
    let project = s.project.clone();
    settings.range = range_param(s, &project, seq, p.get("range"), p, cmd)?;
    settings.path = output_path(s, p, seq, &settings, None);
    let wait = bool_p(p, "wait").unwrap_or(false);
    let path = settings.path.clone();
    let id = spawn_export(s, project, seq, settings, format!("Export {}", file_name(&path)), wait)?;
    let mut out = json!({"job": id, "path": path});
    if let Some(pr) = preset {
        out["preset"] = json!(pr.name);
    }
    if wait && let Some(j) = s.jobs.iter().find(|j| j.id == id) {
        let v = j.to_json();
        if let Some(e) = v["result"]["error"].as_str() {
            return Err(EngineError::Other(format!("export failed: {e}")));
        }
        out["result"] = v["result"].clone();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// queue
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueStatus {
    Ready,
    Encoding,
    Done,
    Failed,
    Cancelled,
}

/// One queued export.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: u64,
    pub sequence: ItemId,
    pub sequence_name: String,
    /// Preset the settings came from ("" = custom).
    pub preset: String,
    pub settings: ExportSettings,
    pub status: QueueStatus,
    /// The background job while encoding / after it ran.
    pub job: Option<u64>,
    pub error: Option<String>,
    /// The project as it was when the item was queued.
    #[serde(skip)]
    pub project: Arc<Project>,
}

/// The export queue (session state).
#[derive(Default)]
pub struct ExportQueue {
    pub items: Vec<QueueItem>,
    /// Started: ready items are encoded one after another.
    pub running: bool,
    next_id: u64,
    /// Quick Export's preset (the last one used).
    pub quick_preset: Option<String>,
}

impl ExportQueue {
    pub fn is_active(&self) -> bool {
        self.items.iter().any(|i| i.status == QueueStatus::Encoding) || (self.running && self.items.iter().any(|i| i.status == QueueStatus::Ready))
    }
}

fn item_json(s: &Session, it: &QueueItem) -> Value {
    let job = it.job.and_then(|id| s.jobs.iter().find(|j| j.id == id));
    let (progress, status_text) = match job {
        Some(j) => (j.progress.fraction() as f64, j.progress.status.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        None => (if it.status == QueueStatus::Done { 1.0 } else { 0.0 }, String::new()),
    };
    json!({
        "id": it.id,
        "sequence": it.sequence.0,
        "sequenceName": it.sequence_name,
        "preset": it.preset,
        "format": it.settings.format.id(),
        "path": it.settings.path,
        "range": it.settings.range,
        "status": it.status,
        "progress": progress,
        "etaSeconds": job.filter(|_| it.status == QueueStatus::Encoding).and_then(|j| j.progress.eta()).map(|d| d.as_secs_f64()),
        "statusText": status_text,
        "job": it.job,
        "error": it.error,
    })
}

/// Advance the queue: settle finished encodes, start the next ready item. `wait` encodes all of
/// them now (blocking).
pub fn pump_queue(s: &mut Session, wait: bool) {
    loop {
        // settle
        let mut finished_any = false;
        for i in 0..s.export_queue.items.len() {
            let it = &s.export_queue.items[i];
            if it.status != QueueStatus::Encoding {
                continue;
            }
            let Some(j) = it.job.and_then(|id| s.jobs.iter().find(|j| j.id == id)) else { continue };
            let res = j.result.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let Some(r) = res else { continue };
            let it = &mut s.export_queue.items[i];
            match r {
                Ok(_) => it.status = QueueStatus::Done,
                Err(e) if e == "cancelled" => it.status = QueueStatus::Cancelled,
                Err(e) => {
                    it.status = QueueStatus::Failed;
                    it.error = Some(e);
                }
            }
            finished_any = true;
        }
        if s.export_queue.items.iter().any(|i| i.status == QueueStatus::Encoding) || !s.export_queue.running {
            return;
        }
        let Some(i) = s.export_queue.items.iter().position(|i| i.status == QueueStatus::Ready) else {
            s.export_queue.running = false;
            if finished_any || !wait {
                let failed = s.export_queue.items.iter().filter(|i| i.status == QueueStatus::Failed).count();
                let message = if failed > 0 { format!("Export queue finished ({failed} failed)") } else { "Export queue finished".into() };
                s.events.push(Event::Toast { message, error: failed > 0 });
            }
            return;
        };
        let it = s.export_queue.items[i].clone();
        let label = format!("Queue: {} → {}", it.sequence_name, file_name(&it.settings.path));
        match spawn_export(s, it.project.clone(), it.sequence, it.settings.clone(), label, wait) {
            Ok(job) => {
                let x = &mut s.export_queue.items[i];
                x.status = QueueStatus::Encoding;
                x.job = Some(job);
                x.error = None;
            }
            Err(e) => {
                let x = &mut s.export_queue.items[i];
                x.status = QueueStatus::Failed;
                x.error = Some(e.to_string());
            }
        }
        if !wait {
            return;
        }
    }
}

fn queue_item_p(s: &Session, p: &Value, cmd: &str) -> Result<usize> {
    let id = u64_p(p, "id").ok_or_else(|| bad(cmd, "need `id` (see export.queue.list)"))?;
    s.export_queue.items.iter().position(|i| i.id == id).ok_or_else(|| bad(cmd, format!("no queued export {id}")))
}

fn queue_add(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.queue.add";
    let (preset, settings) = settings_from_params(s, p, cmd)?;
    let seqs: Vec<ItemId> = match p.get("sequences").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(ItemId).collect(),
        None => vec![sequence_param(s, p, cmd)?],
    };
    if seqs.is_empty() {
        return Err(bad(cmd, "no sequences"));
    }
    for q in &seqs {
        if s.project.sequence(*q).is_none() {
            return Err(bad(cmd, format!("{} is not a sequence", q.0)));
        }
    }
    let ranges: Vec<Option<&Value>> = match p.get("ranges").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => a.iter().map(Some).collect(),
        _ => vec![p.get("range")],
    };
    let project = s.project.clone();
    let many = seqs.len() * ranges.len() > 1;
    let mut ids = Vec::new();
    let mut n = 0usize;
    let mut items = Vec::new();
    for &seq in &seqs {
        for r in &ranges {
            let mut st = settings.clone();
            let times = match r {
                Some(v) if v.is_object() => *v,
                _ => p,
            };
            st.range = range_param(s, &project, seq, *r, times, cmd)?;
            // several ranges of one sequence get numbered names; several sequences their own names
            let suffix = if many && (ranges.len() > 1 || str_p(p, "path").is_some_and(|x| !is_folder(&expand_home(x)))) { Some(n) } else { None };
            st.path = output_path(s, p, seq, &st, suffix);
            n += 1;
            items.push((seq, st));
        }
    }
    for (seq, st) in items {
        s.export_queue.next_id += 1;
        let id = s.export_queue.next_id;
        let name = s.project.item(seq).map(|i| i.name.clone()).unwrap_or_default();
        s.export_queue.items.push(QueueItem {
            id,
            sequence: seq,
            sequence_name: name,
            preset: preset.as_ref().map(|x| x.name.clone()).unwrap_or_default(),
            settings: st,
            status: QueueStatus::Ready,
            job: None,
            error: None,
            project: project.clone(),
        });
        ids.push(id);
    }
    if bool_p(p, "start").unwrap_or(false) {
        s.export_queue.running = true;
        pump_queue(s, bool_p(p, "wait").unwrap_or(false));
    }
    Ok(json!({"added": ids, "items": queue_list_json(s)}))
}

fn queue_list_json(s: &Session) -> Value {
    Value::Array(s.export_queue.items.iter().map(|i| item_json(s, i)).collect())
}

fn queue_list(s: &mut Session, _: &Value) -> Result<Value> {
    pump_queue(s, false);
    Ok(json!({"running": s.export_queue.running, "items": queue_list_json(s)}))
}

fn queue_start(s: &mut Session, p: &Value) -> Result<Value> {
    if !s.export_queue.items.iter().any(|i| matches!(i.status, QueueStatus::Ready | QueueStatus::Encoding)) {
        return Err(bad("export.queue.start", "nothing to export: the queue has no ready items"));
    }
    s.export_queue.running = true;
    pump_queue(s, bool_p(p, "wait").unwrap_or(false));
    Ok(json!({"running": s.export_queue.running, "items": queue_list_json(s)}))
}

fn queue_stop(s: &mut Session, _: &Value) -> Result<Value> {
    // the encode in progress finishes; nothing new starts
    s.export_queue.running = false;
    Ok(json!({"running": false, "items": queue_list_json(s)}))
}

fn cancel_item(s: &mut Session, i: usize) {
    let it = &mut s.export_queue.items[i];
    match it.status {
        QueueStatus::Encoding => {
            if let Some(j) = it.job.and_then(|id| s.jobs.iter().find(|j| j.id == id)) {
                j.progress.cancel.store(true, Ordering::Relaxed);
            }
            // settled to Cancelled by `pump_queue` when the job stops
        }
        QueueStatus::Ready => it.status = QueueStatus::Cancelled,
        _ => {}
    }
}

fn queue_cancel(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("id").is_some() {
        let i = queue_item_p(s, p, "export.queue.cancel")?;
        cancel_item(s, i);
    } else {
        s.export_queue.running = false;
        for i in 0..s.export_queue.items.len() {
            cancel_item(s, i);
        }
    }
    // wait for a cancelled encode to stop (there is no encode thread, clock or sleep on wasm32)
    if bool_p(p, "wait").unwrap_or(false) && !cfg!(target_arch = "wasm32") {
        let t0 = web_time::Instant::now();
        while s.export_queue.items.iter().any(|i| i.status == QueueStatus::Encoding) && t0.elapsed() < std::time::Duration::from_secs(60) {
            let running = s.export_queue.running;
            s.export_queue.running = false;
            pump_queue(s, false);
            s.export_queue.running = running;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    Ok(json!({"items": queue_list_json(s)}))
}

fn queue_retry(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.queue.retry";
    let i = queue_item_p(s, p, cmd)?;
    let it = &mut s.export_queue.items[i];
    if matches!(it.status, QueueStatus::Encoding | QueueStatus::Ready) {
        return Err(bad(cmd, "only finished, failed or cancelled exports can be retried"));
    }
    it.status = QueueStatus::Ready;
    it.error = None;
    it.job = None;
    if bool_p(p, "start").unwrap_or(false) {
        s.export_queue.running = true;
        pump_queue(s, bool_p(p, "wait").unwrap_or(false));
    }
    Ok(json!({"items": queue_list_json(s)}))
}

fn queue_remove(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.queue.remove";
    let i = queue_item_p(s, p, cmd)?;
    if s.export_queue.items[i].status == QueueStatus::Encoding {
        return Err(bad(cmd, "cancel the export before removing it"));
    }
    s.export_queue.items.remove(i);
    Ok(json!({"items": queue_list_json(s)}))
}

fn queue_move(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.queue.move";
    let i = queue_item_p(s, p, cmd)?;
    let n = s.export_queue.items.len() as i64;
    let to = match (p.get("to").and_then(Value::as_i64), p.get("by").and_then(Value::as_i64)) {
        (Some(t), _) => t,
        (None, Some(b)) => (i as i64).saturating_add(b),
        _ => return Err(bad(cmd, "need `to` (index) or `by` (±n)")),
    }
    .clamp(0, n - 1) as usize;
    let it = s.export_queue.items.remove(i);
    s.export_queue.items.insert(to, it);
    Ok(json!({"items": queue_list_json(s)}))
}

fn queue_clear(s: &mut Session, p: &Value) -> Result<Value> {
    let all = bool_p(p, "all").unwrap_or(false);
    if all && s.export_queue.items.iter().any(|i| i.status == QueueStatus::Encoding) {
        return Err(bad("export.queue.clear", "an export is encoding; cancel it first"));
    }
    s.export_queue.items.retain(|i| !all && matches!(i.status, QueueStatus::Ready | QueueStatus::Encoding));
    Ok(json!({"items": queue_list_json(s)}))
}

// ---------------------------------------------------------------------------------------------
// quick export, resolve, presets commands
// ---------------------------------------------------------------------------------------------

fn quick(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.quick";
    let name = str_p(p, "preset").map(str::to_string).or_else(|| s.export_queue.quick_preset.clone()).unwrap_or_else(|| DEFAULT_PRESET.to_string());
    let mut q = p.clone();
    if !q.is_object() {
        q = json!({});
    }
    q["preset"] = json!(name);
    let seq = sequence_param(s, &q, cmd)?;
    let (preset, mut settings) = settings_from_params(s, &q, cmd)?;
    let project = s.project.clone();
    settings.range = range_param(s, &project, seq, q.get("range"), &q, cmd)?;
    settings.path = output_path(s, &q, seq, &settings, None);
    let wait = bool_p(p, "wait").unwrap_or(false);
    let path = settings.path.clone();
    let id = spawn_export(s, project, seq, settings, format!("Quick Export {}", file_name(&path)), wait)?;
    let used = preset.map(|x| x.name).unwrap_or(name);
    s.export_queue.quick_preset = Some(used.clone());
    Ok(json!({"job": id, "path": path, "preset": used}))
}

fn resolve(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.resolve";
    let seq = sequence_param(s, p, cmd)?;
    let (preset, mut settings) = settings_from_params(s, p, cmd)?;
    let project = s.project.clone();
    settings.range = range_param(s, &project, seq, p.get("range"), p, cmd)?;
    settings.path = output_path(s, p, seq, &settings, None);
    let q = project.sequence(seq).ok_or(EngineError::NoSequence)?;
    let range = filmcraft_export::export_range(&project, seq, &settings).map_err(|e| EngineError::Other(e.to_string()))?;
    let (w, h, rate, sr) = (q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate);
    let r = settings.resolve(w, h, rate, sr);
    let (f0, f1) = filmcraft_export::frame_span(r.rate, range);
    let summary = settings.summary(w, h, rate, sr, range.duration);
    Ok(json!({
        "preset": preset.map(|x| x.name),
        "settings": settings,
        "range": {"start": range.start.0, "duration": range.duration.0, "seconds": range.duration.seconds()},
        "frames": f1 - f0,
        "output": {"width": r.width, "height": r.height, "fps": r.rate.label(), "sampleRate": r.sample_rate, "channels": r.channels,
                   "targetKbps": r.target_kbps, "maxKbps": r.max_kbps, "keyframeDistance": r.keyint},
        "summary": summary,
        "source": format!("Sequence, {}x{}, {} fps, {} Hz", w, h, rate.label(), sr),
    }))
}

fn preset_json(s: &Session, pr: &ExportPreset, full: bool) -> Value {
    let mut v = json!({
        "name": pr.name,
        "category": pr.category,
        "description": pr.description,
        "builtin": pr.builtin,
        "favorite": s.export_presets.is_favorite(&pr.name),
        "format": pr.settings.format.id(),
        "formatLabel": pr.settings.format.label(),
        "extension": pr.settings.extension(),
        "frameSize": pr.settings.frame_size,
    });
    if full {
        v["settings"] = serde_json::to_value(&pr.settings).unwrap_or_default();
    }
    v
}

fn presets_list(s: &mut Session, p: &Value) -> Result<Value> {
    let query = str_p(p, "query").unwrap_or("").to_ascii_lowercase();
    let cat = str_p(p, "category").map(str::to_ascii_lowercase);
    let fav = bool_p(p, "favorites").unwrap_or(false);
    let fmt = str_p(p, "format").and_then(Format::from_name);
    let v: Vec<Value> = s
        .export_presets
        .all()
        .iter()
        .filter(|x| {
            query.is_empty()
                || x.name.to_ascii_lowercase().contains(&query)
                || x.description.to_ascii_lowercase().contains(&query)
                || x.category.to_ascii_lowercase().contains(&query)
        })
        .filter(|x| cat.as_ref().is_none_or(|c| x.category.to_ascii_lowercase() == *c))
        .filter(|x| !fav || s.export_presets.is_favorite(&x.name))
        .filter(|x| fmt.is_none_or(|f| x.settings.format == f))
        .map(|x| preset_json(s, x, false))
        .collect();
    Ok(json!({"presets": v, "default": DEFAULT_PRESET}))
}

fn presets_get(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").ok_or_else(|| bad("export.presets.get", "need `name`"))?;
    let pr = s.export_presets.find(name).ok_or_else(|| bad("export.presets.get", format!("no export preset named `{name}`")))?;
    Ok(preset_json(s, &pr, true))
}

fn presets_save(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.presets.save";
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "need a `name`"))?.to_string();
    if filmcraft_export::presets::find_builtin(&name).is_some() {
        return Err(bad(cmd, format!("`{name}` is a built-in preset; choose another name")));
    }
    // the settings: `settings` (over `from`, a preset to start from), plus flat overrides
    let mut q = p.clone();
    if let Some(from) = str_p(p, "from") {
        q["preset"] = json!(from);
    } else if let Some(o) = q.as_object_mut() {
        o.remove("preset");
    }
    let (_, mut settings) = settings_from_params(s, &q, cmd)?;
    settings.path.clear();
    settings.range = None;
    let k = preset_key(&name);
    let existing = s.export_presets.user.iter().position(|x| preset_key(&x.name) == k);
    if existing.is_some() && !bool_p(p, "overwrite").unwrap_or(true) {
        return Err(bad(cmd, format!("a preset named `{name}` exists")));
    }
    let pr = ExportPreset {
        name: name.clone(),
        category: str_p(p, "category").unwrap_or("User").to_string(),
        description: str_p(p, "description").map(str::to_string).unwrap_or_else(|| format!("{} (custom)", settings.format.label())),
        settings,
        builtin: false,
    };
    match existing {
        Some(i) => s.export_presets.user[i] = pr,
        None => s.export_presets.user.push(pr),
    }
    s.export_presets.persist().map_err(EngineError::Other)?;
    Ok(json!({"name": name}))
}

fn presets_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.presets.delete";
    let name = str_p(p, "name").ok_or_else(|| bad(cmd, "need `name`"))?;
    let k = preset_key(name);
    let n = s.export_presets.user.len();
    s.export_presets.user.retain(|x| preset_key(&x.name) != k);
    if s.export_presets.user.len() == n {
        return Err(bad(
            cmd,
            if filmcraft_export::presets::find_builtin(name).is_some() {
                format!("`{name}` is built in and cannot be deleted")
            } else {
                format!("no user preset named `{name}`")
            },
        ));
    }
    s.export_presets.favorites.retain(|f| preset_key(f) != k);
    s.export_presets.persist().map_err(EngineError::Other)?;
    Ok(json!({"deleted": name}))
}

fn presets_favorite(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.presets.favorite";
    let name = str_p(p, "name").ok_or_else(|| bad(cmd, "need `name`"))?;
    let pr = s.export_presets.find(name).ok_or_else(|| bad(cmd, format!("no export preset named `{name}`")))?;
    let on = bool_p(p, "favorite").unwrap_or(!s.export_presets.is_favorite(&pr.name));
    let k = preset_key(&pr.name);
    s.export_presets.favorites.retain(|f| preset_key(f) != k);
    if on {
        s.export_presets.favorites.push(pr.name.clone());
    }
    s.export_presets.persist().map_err(EngineError::Other)?;
    Ok(json!({"name": pr.name, "favorite": on}))
}

fn presets_import(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.presets.import";
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`"))?;
    let bytes = s.services.read_file(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let f = parse_file(&bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let mut names = Vec::new();
    for mut pr in f.presets {
        if filmcraft_export::presets::find_builtin(&pr.name).is_some() {
            pr.name = format!("{} (imported)", pr.name);
        }
        pr.builtin = false;
        let k = preset_key(&pr.name);
        s.export_presets.user.retain(|x| preset_key(&x.name) != k);
        names.push(pr.name.clone());
        s.export_presets.user.push(pr);
    }
    s.export_presets.persist().map_err(EngineError::Other)?;
    Ok(json!({"imported": names}))
}

fn presets_export(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "export.presets.export";
    let path = str_p(p, "path").ok_or_else(|| bad(cmd, "need `path`"))?;
    let list: Vec<ExportPreset> = match p.get("names").and_then(Value::as_array) {
        Some(names) => names
            .iter()
            .filter_map(Value::as_str)
            .map(|n| s.export_presets.find(n).ok_or_else(|| bad(cmd, format!("no export preset named `{n}`"))))
            .collect::<Result<_>>()?,
        None => s.export_presets.user.clone(),
    };
    let favs: Vec<String> = s.export_presets.favorites.iter().filter(|f| list.iter().any(|x| preset_key(&x.name) == preset_key(f))).cloned().collect();
    s.services.write_file(path, &file_bytes(&list, &favs)).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "presets": list.len()}))
}

fn formats(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(Value::Array(
        Format::ALL
            .iter()
            .map(|f| {
                let st = ExportSettings { format: *f, ..Default::default() };
                json!({"id": f.id(), "label": f.label(), "extension": f.extension(), "video": st.has_video(), "audio": st.has_audio() || !st.has_video(),
                       "imageSequence": st.is_image_sequence(), "available": filmcraft_export::available(*f)})
            })
            .collect(),
    ))
}

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: Run,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("export.presets.list", "List Export Presets", r#"{"query":str?,"category":str?,"format":str?,"favorites":bool?}"#, always, presets_list, false),
        spec("export.presets.get", "Get Export Preset", r#"{"name":str}"#, always, presets_get, false),
        spec(
            "export.presets.save",
            "Save Export Preset",
            r#"{"name":str,"from":str?,"settings":ExportSettings?,"category":str?,"description":str?,"overwrite":bool=true, …flat overrides}"#,
            always,
            presets_save,
            true,
        ),
        spec("export.presets.delete", "Delete Export Preset", r#"{"name":str}"#, always, presets_delete, true),
        spec("export.presets.favorite", "Favorite Export Preset", r#"{"name":str,"favorite":bool?}"#, always, presets_favorite, true),
        spec("export.presets.import", "Import Export Presets", r#"{"path":str}"#, always, presets_import, true),
        spec("export.presets.export", "Export Export Presets", r#"{"path":str,"names":[str]?}"#, always, presets_export, false),
        spec("export.formats", "List Export Formats", "{}", always, formats, false),
        spec("export.resolve", "Resolve Export Settings", "{…settings params, \"path\":str?}", has_seq, resolve, false),
        spec(
            "export.queue.add",
            "Send to Export Queue",
            r#"{…settings params,"path":str|dir?,"sequences":[id]?,"ranges":[range]?,"start":bool?,"wait":bool?}"#,
            has_seq,
            queue_add,
            true,
        ),
        spec("export.queue.list", "List Export Queue", "{}", always, queue_list, false),
        spec("export.queue.start", "Start Export Queue", r#"{"wait":bool=false}"#, always, queue_start, true),
        spec("export.queue.stop", "Stop Export Queue", "{}", always, queue_stop, true),
        spec("export.queue.cancel", "Cancel Queued Export", r#"{"id":id?,"wait":bool?}"#, always, queue_cancel, true),
        spec("export.queue.retry", "Retry Queued Export", r#"{"id":id,"start":bool?,"wait":bool?}"#, always, queue_retry, true),
        spec("export.queue.remove", "Remove Queued Export", r#"{"id":id}"#, always, queue_remove, true),
        spec("export.queue.move", "Reorder Queued Export", r#"{"id":id,"to":index?,"by":int?}"#, always, queue_move, true),
        spec("export.queue.clear", "Clear Finished Exports", r#"{"all":bool=false}"#, always, queue_clear, true),
        spec("export.quick", "Quick Export", r#"{"preset":str?,"path":str?,"wait":bool?, …settings params}"#, has_seq, quick, true),
    ]
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    /// A panicking job body must leave the job finished with an error, not "running" forever.
    #[test]
    fn panicking_job_is_reported_as_failed() {
        let progress: Arc<filmcraft_export::Progress> = Arc::default();
        let result: Arc<std::sync::Mutex<Option<std::result::Result<filmcraft_export::Report, String>>>> = Arc::default();
        let run = guard_job(progress.clone(), result.clone(), || panic!("job bug"));
        std::thread::spawn(run).join().expect("the guard catches the panic");
        assert!(progress.finished.load(Ordering::Relaxed));
        assert!(progress.error.lock().unwrap().is_some());
        assert!(matches!(&*result.lock().unwrap(), Some(Err(_))));
    }
}

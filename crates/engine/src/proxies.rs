//! Proxies and ingest (Clip ▸ Proxy ▸ Create / Attach / Reconnect Full Resolution, Toggle Proxies,
//! Project Settings ▸ Ingest Settings).
//!
//! - **Create.** A background job transcodes each clip with our own encoders (ProRes 422 Proxy /
//!   LT or H.264 at ¼ or ½ size, audio included) and attaches the results when it finishes
//!   ([`poll`] applies finished jobs; the frontend calls it every frame through
//!   `Session::poll_persistence`).
//! - **Attach.** A proxy must have the original's duration (±1 frame) and frame rate; its frame
//!   size and aspect may differ.
//! - **Toggle.** `media.toggleProxies` flips Preferences ▸ Media ▸ Enable proxies; the media pool
//!   then hands proxies to monitors and playback. Export always uses full resolution.
//! - **Ingest.** With ingest enabled, `file.import` copies (verified by fingerprint), transcodes
//!   and/or creates proxies.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use filmcraft_export::{ExportSettings, Format, Progress, Report};
use filmcraft_media::{MediaKind, SharedSource};
use filmcraft_project::{IngestAction, ItemId, ItemKind, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde::Serialize;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

/// A proxy / transcode preset.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    #[serde(skip)]
    pub format: Format,
    /// Frame size relative to the original.
    pub scale: f32,
    /// ProRes flavour (`proxy`, `lt`, `hq`) for ProRes presets.
    pub prores: &'static str,
    pub extension: &'static str,
    /// Offered for proxies (else only for transcodes).
    pub proxy: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "prores_proxy_quarter",
        label: "ProRes 422 Proxy, ¼ size",
        format: Format::ProRes,
        scale: 0.25,
        prores: "proxy",
        extension: "mov",
        proxy: true,
    },
    Preset { id: "prores_proxy_half", label: "ProRes 422 Proxy, ½ size", format: Format::ProRes, scale: 0.5, prores: "proxy", extension: "mov", proxy: true },
    Preset { id: "prores_lt_half", label: "ProRes 422 LT, ½ size", format: Format::ProRes, scale: 0.5, prores: "lt", extension: "mov", proxy: true },
    Preset { id: "h264_quarter", label: "H.264, ¼ size", format: Format::H264, scale: 0.25, prores: "", extension: "mp4", proxy: true },
    Preset { id: "h264_half", label: "H.264, ½ size", format: Format::H264, scale: 0.5, prores: "", extension: "mp4", proxy: true },
    Preset { id: "prores_lt", label: "ProRes 422 LT (full size)", format: Format::ProRes, scale: 1.0, prores: "lt", extension: "mov", proxy: false },
    Preset { id: "prores_hq", label: "ProRes 422 HQ (full size)", format: Format::ProRes, scale: 1.0, prores: "hq", extension: "mov", proxy: false },
    Preset { id: "h264", label: "H.264 (full size)", format: Format::H264, scale: 1.0, prores: "", extension: "mp4", proxy: false },
];

pub const DEFAULT_PROXY_PRESET: &str = "prores_proxy_quarter";
pub const DEFAULT_TRANSCODE_PRESET: &str = "prores_lt";

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// What to do with a finished job's outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDone {
    /// Attach outputs as proxies.
    AttachProxies,
    /// Point the clips at the outputs (ingest transcode).
    ReplaceMedia,
    /// Nothing (the job's own result is the product, e.g. Project Manager).
    Nothing,
}

/// A media job whose outputs still have to be applied to the project.
pub struct PendingJob {
    pub job: u64,
    pub on_done: OnDone,
    /// (item, output path) for each finished output.
    pub outputs: Arc<Mutex<Vec<(ItemId, String)>>>,
}

/// Transcode `range` (whole media when None) of one media clip to `out` with `preset`, blocking.
/// `progress.total` is the caller's; this adds one per frame to `progress.done`.
pub fn transcode(
    clip: &MediaClip,
    name: &str,
    src: SharedSource,
    range: Option<TimeRange>,
    out: &str,
    preset: &Preset,
    progress: &Progress,
) -> std::result::Result<Report, String> {
    let info = &clip.info;
    let rate = clip.frame_rate();
    let mut st = SequenceSettings::default();
    if let Some(v) = &info.video {
        st.width = v.width;
        st.height = v.height;
    }
    st.frame_rate = rate;
    st.sample_rate = info.audio().map_or(48_000, |a| a.sample_rate.max(8000));
    let mut p = Project::new("transcode");
    let mut c = clip.clone();
    c.offline = false;
    c.proxy = None;
    let item = p.add_item(name, filmcraft_project::Label::Iris, ItemKind::Media(c), None);
    let has_v = info.video.is_some();
    let has_a = info.has_audio();
    let seq = p.new_sequence("transcode", st, usize::from(has_v), usize::from(has_a), None);
    let range = range.unwrap_or(TimeRange { start: Tick::ZERO, duration: info.duration });
    let mut placed = Vec::new();
    if has_v && let Some(ti) = p.make_track_item(item, TrackKind::Video, Tick::ZERO, range, rate) {
        placed.push((TrackKind::Video, ti));
    }
    if has_a && let Some(ti) = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, rate) {
        placed.push((TrackKind::Audio, ti));
    }
    let q = p.sequence_mut(seq).ok_or("no sequence")?;
    for (k, ti) in placed {
        let t = if k == TrackKind::Video { &mut q.video_tracks[0] } else { &mut q.audio_tracks[0] };
        t.items.push(ti);
    }
    let format = if has_v { preset.format } else { Format::Wav };
    let settings = ExportSettings {
        format,
        path: out.to_string(),
        scale: preset.scale,
        include_audio: has_a,
        quality: 85,
        bitrate_kbps: ((info.video.as_ref().map_or(1920 * 1080, |v| v.width * v.height) as f64 * (preset.scale * preset.scale) as f64) / 200.0).max(800.0)
            as u32,
        part_of_batch: true,
        prores_profile: preset.prores.to_string(),
        ..Default::default()
    };
    let project = Arc::new(p);
    let provider = move |id: ItemId| (id == item).then(|| src.clone());
    filmcraft_export::export(&project, seq, &settings, &provider, progress).map_err(|e| e.to_string())
}

/// Frames a transcode of `clip` will write (for progress totals).
pub fn frame_count(clip: &MediaClip, range: Option<TimeRange>) -> u64 {
    let rate = clip.frame_rate();
    let d = range.map_or(clip.info.duration, |r| r.duration);
    if clip.info.video.is_none() { 1 } else { rate.frame_at(d).max(1) as u64 }
}

/// `<dir>/<stem><suffix>.<ext>`, numbered when the name is taken.
pub fn unique_path(dir: &Path, stem: &str, suffix: &str, ext: &str, taken: &[String]) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{stem}{suffix}.{ext}") } else { format!("{stem}{suffix} {n}.{ext}") };
        let p = dir.join(name);
        if !p.exists() && !taken.contains(&p.to_string_lossy().into_owned()) {
            return p;
        }
        n += 1;
    }
}

fn media_path(p: &Project, item: ItemId) -> Option<(MediaClip, String, String)> {
    let it = p.item(item)?;
    let m = it.as_media()?;
    match &m.media {
        MediaRef::File { path } => Some((m.clone(), path.clone(), it.name.clone())),
        MediaRef::Generator(_) => None,
    }
}

fn items_param(s: &Session, p: &Value) -> Vec<ItemId> {
    match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(ItemId).collect(),
        None => u64_p(p, "item").map(|i| vec![ItemId(i)]).unwrap_or_else(|| s.state.project_selection.clone()),
    }
}

/// Start a background transcode job over `work` (item, output path, preset). Returns the job id.
pub fn start_job(s: &mut Session, label: String, work: Vec<(ItemId, String, &'static Preset)>, on_done: OnDone, wait: bool) -> Result<u64> {
    let mut tasks = Vec::new();
    let mut total = 0;
    for (item, out, pr) in work {
        let (clip, _, name) = media_path(&s.project, item).ok_or_else(|| bad("media.createProxies", format!("item {} is not file media", item.0)))?;
        let src = s.media.full_res_source(&s.project, item, &*s.services).ok_or_else(|| EngineError::Other(format!("{name}: no source")))?;
        if s.media.offline_status(item).is_some() {
            return Err(EngineError::Other(format!("{name} is offline; link it first")));
        }
        total += frame_count(&clip, None);
        tasks.push((item, clip, name, src, out, pr));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(total, std::sync::atomic::Ordering::Relaxed);
    let outputs: Arc<Mutex<Vec<(ItemId, String)>>> = Arc::default();
    let (prog, res, outs) = (job.progress.clone(), job.result.clone(), outputs.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        let mut bytes = 0;
        let mut err = None;
        let n = tasks.len();
        for (k, (item, clip, name, src, out, pr)) in tasks.into_iter().enumerate() {
            if prog.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                err = Some("cancelled".to_string());
                break;
            }
            *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("{name} ({} of {n})", k + 1);
            if let Some(d) = Path::new(&out).parent() {
                let _ = std::fs::create_dir_all(d);
            }
            match transcode(&clip, &name, src, None, &out, pr, &prog) {
                Ok(r) => {
                    bytes += r.bytes;
                    outs.lock().unwrap_or_else(|e| e.into_inner()).push((item, out));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&out);
                    err = Some(format!("{name}: {e}"));
                    break;
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let frames = prog.done.load(std::sync::atomic::Ordering::Relaxed);
        let r = match err {
            Some(e) => {
                *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
                Err(e)
            }
            None => Ok(Report { path: String::new(), frames, seconds: secs, bytes, render_fps: frames as f64 / secs.max(1e-6), extra_files: Vec::new() }),
        };
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = match &r {
            Ok(_) => format!("Done in {secs:.1}s"),
            Err(e) => e.clone(),
        };
        prog.finished.store(true, std::sync::atomic::Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.media_jobs.push(PendingJob { job: id, on_done, outputs });
    if wait || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
    } else {
        std::thread::Builder::new().name("filmcraft-proxies".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(id)
}

/// Apply the outputs of finished media jobs (attach proxies, switch to transcodes).
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let finished: Vec<usize> = s
        .media_jobs
        .iter()
        .enumerate()
        .filter(|(_, pj)| s.jobs.iter().find(|j| j.id == pj.job).is_none_or(|j| j.progress.finished.load(Ordering::Relaxed)))
        .map(|(i, _)| i)
        .collect();
    for i in finished.into_iter().rev() {
        let pj = s.media_jobs.remove(i);
        let outs = std::mem::take(&mut *pj.outputs.lock().unwrap_or_else(|e| e.into_inner()));
        if outs.is_empty() {
            continue;
        }
        let r = match pj.on_done {
            OnDone::AttachProxies => attach(s, &outs, false).map(|_| ()),
            OnDone::ReplaceMedia => replace_media(s, &outs),
            OnDone::Nothing => Ok(()),
        };
        if let Err(e) = r {
            s.error_toast("job", e.to_string());
        }
    }
}

/// Check a proxy file against its clip: duration within one frame, same frame rate.
fn check_proxy(s: &Session, item: ItemId, path: &str) -> Result<()> {
    let (clip, _, name) = media_path(&s.project, item).ok_or_else(|| bad("media.attachProxies", format!("item {} is not file media", item.0)))?;
    let src = s.media.open_file(path, &*s.services).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let pi = src.info();
    let fd = clip.info.frame_rate().frame_duration();
    if (pi.duration - clip.info.duration).0.abs() > fd.0 {
        return Err(EngineError::Other(format!(
            "{}: the proxy is {:.3}s long but {name} is {:.3}s",
            Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            pi.duration.seconds(),
            clip.info.duration.seconds()
        )));
    }
    if let (Some(a), Some(b)) = (&pi.video, &clip.info.video)
        && a.frame_rate != b.frame_rate
    {
        return Err(EngineError::Other(format!("the proxy runs at {} fps but {name} at {} fps", a.frame_rate.label(), b.frame_rate.label())));
    }
    if pi.video.is_none() && clip.info.video.is_some() {
        return Err(EngineError::Other("the proxy has no video".into()));
    }
    Ok(())
}

/// Attach proxies (checked unless `force`). One undo step.
pub fn attach(s: &mut Session, pairs: &[(ItemId, String)], force: bool) -> Result<Value> {
    if !force {
        for (i, p) in pairs {
            check_proxy(s, *i, p)?;
        }
    }
    let pairs = pairs.to_vec();
    s.edit("Attach Proxies", |proj, _| {
        for (i, p) in &pairs {
            if let Some(m) = proj.item_mut(*i).and_then(|it| it.as_media_mut()) {
                m.proxy = Some(MediaRef::File { path: p.clone() });
            }
        }
        Ok(())
    })?;
    Ok(json!({"attached": pairs.iter().map(|(i, p)| json!({"item": i.0, "path": p})).collect::<Vec<_>>()}))
}

fn replace_media(s: &mut Session, outs: &[(ItemId, String)]) -> Result<()> {
    for (i, p) in outs {
        let q = json!({"item": i.0, "path": p, "force": true, "relinkOthers": false});
        crate::relink::relink(s, &q)?;
    }
    Ok(())
}

/// Where proxies of `media` go: the chosen folder, else `<media dir>/Proxies`.
pub fn proxy_dir(media: &str, dest: Option<&str>) -> PathBuf {
    match dest {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => Path::new(media).parent().unwrap_or(Path::new(".")).join("Proxies"),
    }
}

/// `media.createProxies {items?, preset?, destination?, wait?}`.
pub fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let pr = preset(str_p(p, "preset").unwrap_or(DEFAULT_PROXY_PRESET)).ok_or_else(|| bad("media.createProxies", "unknown preset (see media.proxyPresets)"))?;
    let items = items_param(s, p);
    // default: Project Settings ▸ Scratch Disks ▸ Captured and Generated, else next to the media
    let dest = str_p(p, "destination")
        .map(str::to_string)
        .or_else(|| s.project.settings.scratch.captured.clone().filter(|d| !d.is_empty()).map(|d| format!("{d}/Proxies")));
    let mut work = Vec::new();
    let mut taken = Vec::new();
    let mut skipped = Vec::new();
    for i in items {
        let Some((clip, path, name)) = media_path(&s.project, i) else {
            skipped.push(json!({"item": i.0, "reason": "not file media"}));
            continue;
        };
        if !matches!(clip.info.kind, MediaKind::Movie) || clip.info.video.is_none() {
            skipped.push(json!({"item": i.0, "reason": format!("{name} has no video to proxy")}));
            continue;
        }
        let stem = Path::new(&path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
        let out = unique_path(&proxy_dir(&path, dest.as_deref()), &stem, "_Proxy", pr.extension, &taken).to_string_lossy().into_owned();
        taken.push(out.clone());
        work.push((i, out, pr));
    }
    if work.is_empty() {
        return Err(EngineError::Other(format!("nothing to proxy: {}", skipped.iter().filter_map(|v| v["reason"].as_str()).collect::<Vec<_>>().join("; "))));
    }
    let outputs: Vec<Value> = work.iter().map(|(i, o, _)| json!({"item": i.0, "path": o})).collect();
    let n = work.len();
    let label = format!("Create Proxies ({n} clip{})", if n == 1 { "" } else { "s" });
    let attach = bool_p(p, "attach").unwrap_or(true);
    let job = start_job(s, label, work, if attach { OnDone::AttachProxies } else { OnDone::Nothing }, bool_p(p, "wait").unwrap_or(false))?;
    Ok(json!({"job": job, "preset": pr.id, "outputs": outputs, "skipped": skipped}))
}

/// Ingest freshly imported items per the project's ingest settings.
/// Where Project Settings ▸ Ingest copies the file at `path`. `None`: ingest does not copy, or
/// the file is already there.
pub(crate) fn ingest_copy_path(ing: &filmcraft_project::IngestSettings, path: &str) -> Option<PathBuf> {
    if !ing.enabled || !matches!(ing.action, IngestAction::Copy | IngestAction::CopyAndCreateProxies) {
        return None;
    }
    let dir = match &ing.destination {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => Path::new(path).parent().unwrap_or(Path::new(".")).join("Ingested Media"),
    };
    let out = dir.join(Path::new(path).file_name()?);
    (Path::new(path) != out).then_some(out)
}

pub fn ingest(s: &mut Session, items: &[ItemId]) -> Result<Value> {
    let ing = s.project.settings.ingest.clone();
    if !ing.enabled || items.is_empty() {
        return Ok(Value::Null);
    }
    let mut copied = Vec::new();
    let copy = matches!(ing.action, IngestAction::Copy | IngestAction::CopyAndCreateProxies);
    if copy {
        for &i in items {
            let Some((_, path, _)) = media_path(&s.project, i) else { continue };
            let Some(out) = ingest_copy_path(&ing, &path) else { continue };
            let dir = out.parent().unwrap_or(Path::new(".")).to_path_buf();
            let fname = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{}: {e}", dir.display())))?;
            std::fs::copy(&path, &out).map_err(|e| EngineError::Other(format!("copying {fname}: {e}")))?;
            let out_s = out.to_string_lossy().into_owned();
            // verify the copy before switching to it
            let a = crate::relink::identity_of(&*s.services, &path).map_err(|e| EngineError::Other(e.to_string()))?;
            let b = crate::relink::identity_of(&*s.services, &out_s).map_err(|e| EngineError::Other(e.to_string()))?;
            if a != b {
                return Err(EngineError::Other(format!("the copy of {fname} doesn't match the original")));
            }
            crate::relink::relink(s, &json!({"item": i.0, "path": out_s, "relinkOthers": false}))?;
            copied.push(json!({"item": i.0, "path": out_s}));
        }
    }
    let mut job = Value::Null;
    match ing.action {
        IngestAction::CreateProxies | IngestAction::CopyAndCreateProxies => {
            let p = json!({"items": items.iter().map(|i| i.0).collect::<Vec<_>>(), "preset": if ing.preset.is_empty() { DEFAULT_PROXY_PRESET } else { ing.preset.as_str() }, "destination": ing.destination});
            if items.iter().any(|i| s.project.item(*i).and_then(|x| x.as_media()).is_some_and(|m| m.info.video.is_some() && m.info.kind == MediaKind::Movie)) {
                job = create(s, &p)?;
            }
        }
        IngestAction::Transcode => {
            let pr =
                preset(if ing.preset.is_empty() { DEFAULT_TRANSCODE_PRESET } else { ing.preset.as_str() }).ok_or_else(|| bad("ingest", "unknown preset"))?;
            let mut work = Vec::new();
            for &i in items {
                let Some((clip, path, name)) = media_path(&s.project, i) else { continue };
                if clip.info.video.is_none() {
                    continue;
                }
                let dir = match &ing.destination {
                    Some(d) if !d.is_empty() => PathBuf::from(d),
                    _ => Path::new(&path).parent().unwrap_or(Path::new(".")).join("Ingested Media"),
                };
                let stem = Path::new(&path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
                work.push((i, unique_path(&dir, &stem, "", pr.extension, &[]).to_string_lossy().into_owned(), pr));
            }
            if !work.is_empty() {
                let id = start_job(s, format!("Ingest: transcode {} clip(s)", work.len()), work, OnDone::ReplaceMedia, false)?;
                job = json!({"job": id});
            }
        }
        IngestAction::Copy => {}
    }
    Ok(json!({"copied": copied, "job": job}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "media.createProxies",
            label: "Create Proxies…",
            menu: &["Clip", "Proxy"],
            shortcut: None,
            params: r#"{"items":[id]?,"preset":"prores_proxy_quarter|prores_proxy_half|prores_lt_half|h264_quarter|h264_half","destination":str?,"attach":bool=true,"wait":bool=false}"#,
            enabled: has_file_media,
            run: create,
            journal: true,
        },
        CommandSpec {
            id: "media.attachProxies",
            label: "Attach Proxies…",
            menu: &["Clip", "Proxy"],
            shortcut: None,
            params: r#"{"item":id,"path":str}|{"items":[id],"paths":[str]},"force":bool=false"#,
            enabled: has_file_media,
            run: |s, p| {
                let pairs: Vec<(ItemId, String)> = match (p.get("items").and_then(Value::as_array), p.get("paths").and_then(Value::as_array)) {
                    (Some(i), Some(ps)) => i.iter().zip(ps).filter_map(|(i, p)| Some((ItemId(i.as_u64()?), p.as_str()?.to_string()))).collect(),
                    _ => vec![(
                        u64_p(p, "item").map(ItemId).ok_or_else(|| bad("media.attachProxies", "need `item`"))?,
                        str_p(p, "path").ok_or_else(|| bad("media.attachProxies", "need `path`"))?.to_string(),
                    )],
                };
                attach(s, &pairs, bool_p(p, "force").unwrap_or(false))
            },
            journal: true,
        },
        CommandSpec {
            id: "media.detachProxies",
            label: "Detach Proxies",
            menu: &["Clip", "Proxy"],
            shortcut: None,
            params: r#"{"items":[id]?}"#,
            enabled: has_file_media,
            run: |s, p| {
                let items = items_param(s, p);
                let n = s.edit("Detach Proxies", |proj, _| {
                    let mut n = 0;
                    for i in &items {
                        if let Some(m) = proj.item_mut(*i).and_then(|it| it.as_media_mut())
                            && m.proxy.take().is_some()
                        {
                            n += 1;
                        }
                    }
                    Ok(n)
                })?;
                Ok(json!({"detached": n}))
            },
            journal: true,
        },
        CommandSpec {
            id: "media.reconnectFullRes",
            label: "Reconnect Full Resolution Media…",
            menu: &["Clip", "Proxy"],
            shortcut: None,
            params: r#"{"item":id,"path":str}"#,
            enabled: has_file_media,
            run: |s, p| {
                let item = u64_p(p, "item").map(ItemId).ok_or_else(|| bad("media.reconnectFullRes", "need `item`"))?;
                if s.project.item(item).and_then(|i| i.as_media()).is_none_or(|m| m.proxy.is_none()) {
                    return Err(EngineError::Other("the clip has no proxy attached".into()));
                }
                let mut q = p.clone();
                q["match"] = json!({"fileName": false, "extension": false});
                q["relinkOthers"] = json!(false);
                crate::relink::relink(s, &q)
            },
            journal: true,
        },
        CommandSpec {
            id: "media.toggleProxies",
            label: "Toggle Proxies",
            menu: &["View"],
            shortcut: None,
            params: r#"{"enabled":bool?}"#,
            enabled: always,
            run: |s, p| {
                let on = bool_p(p, "enabled").unwrap_or(!s.prefs.media.enable_proxies);
                let mut next = s.prefs.clone();
                next.media.enable_proxies = on;
                s.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
                let attached = s.project.items.values().filter(|i| i.as_media().is_some_and(|m| m.proxy.is_some())).count();
                Ok(json!({"enabled": on, "clipsWithProxies": attached}))
            },
            journal: false,
        },
        CommandSpec {
            id: "media.proxyPresets",
            label: "Proxy Presets",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |_, _| Ok(serde_json::to_value(PRESETS).unwrap_or_default()),
            journal: false,
        },
        CommandSpec {
            id: "project.ingestSettings",
            label: "Ingest Settings…",
            menu: &[],
            shortcut: None,
            params: r#"{"enabled":bool?,"action":"copy|transcode|createProxies|copyAndCreateProxies"?,"destination":str?,"preset":str?}"#,
            enabled: always,
            run: |s, p| {
                let mut ing = s.project.settings.ingest.clone();
                if let Some(b) = bool_p(p, "enabled") {
                    ing.enabled = b;
                }
                if let Some(a) = p.get("action") {
                    ing.action = serde_json::from_value(a.clone()).map_err(|e| bad("project.ingestSettings", e.to_string()))?;
                }
                if let Some(d) = p.get("destination") {
                    ing.destination = d.as_str().filter(|d| !d.is_empty()).map(str::to_string);
                }
                if let Some(pr) = str_p(p, "preset") {
                    if !pr.is_empty() && preset(pr).is_none() {
                        return Err(bad("project.ingestSettings", format!("unknown preset `{pr}`")));
                    }
                    ing.preset = pr.to_string();
                }
                if ing != s.project.settings.ingest {
                    let v = ing.clone();
                    s.edit("Ingest Settings", |proj, _| {
                        proj.settings.ingest = v;
                        Ok(())
                    })?;
                }
                Ok(serde_json::to_value(&s.project.settings.ingest).unwrap_or_default())
            },
            journal: true,
        },
    ]
}

fn has_file_media(s: &Session) -> std::result::Result<(), String> {
    if s.project.items.values().any(|i| i.as_media().is_some_and(|m| matches!(m.media, MediaRef::File { .. }))) {
        Ok(())
    } else {
        Err("the project has no file-based media".into())
    }
}

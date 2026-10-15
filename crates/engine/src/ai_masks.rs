//! Experimental local AI masking. Commands own selection/settings, workers own inference, and
//! only a complete, validated result is committed as one undo step. Rendering never calls a model.

use std::sync::{Arc, Mutex, atomic::Ordering};

use filmcraft_project::{ClipId, ItemId, Mask, MaskPath, ParamValue, TrackItem, TrackKind, raster_mask::RasterMask};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    EngineError, Result, Session,
    commands::{CommandSpec, bad, bool_p, clip_p, f64_p, has_seq, str_p, u64_p},
};

#[cfg(all(feature = "experimental-ai-rotoscope", not(target_arch = "wasm32")))]
mod bridge;
mod display;
#[cfg(all(feature = "experimental-ai-rotoscope", not(target_arch = "wasm32")))]
mod native;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    #[default]
    Off,
    Foreground,
    Background,
    Refine,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefineView {
    #[default]
    Region,
    Alpha,
    Composite,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub time: Tick,
    pub x: f64,
    pub y: f64,
    pub kind: Pick,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub clip: ClipId,
    pub pick: Pick,
    pub quality: bool,
    pub frames: u32,
    pub band: u32,
    pub points: Vec<Point>,
    #[serde(default)]
    pub view: RefineView,
}

pub struct InputFrame {
    pub time: Tick,
    pub rgba: Vec<u8>,
}
pub struct Input {
    pub width: u32,
    pub height: u32,
    pub end: Tick,
    pub fps: String,
    pub frames: Vec<InputFrame>,
    pub points: Vec<Point>,
    pub quality: bool,
    pub band: u32,
    /// Existing single-frame alpha for refining a propagated frame without another Include click.
    pub base_alpha: Option<Vec<u16>>,
}

pub trait Backend: Send + Sync {
    fn infer(&self, input: &Input, progress: &filmcraft_export::Progress) -> std::result::Result<RasterMask, String>;
}

pub fn default_backend() -> Option<Arc<dyn Backend>> {
    #[cfg(all(feature = "experimental-ai-rotoscope", not(target_arch = "wasm32")))]
    {
        // The reference server is an explicitly selected development oracle only.
        if std::env::var("FILMCRAFT_AI_ROTO_BACKEND").as_deref() == Ok("python-reference") { bridge::from_env() } else { Some(native::from_env()) }
    }
    #[cfg(not(all(feature = "experimental-ai-rotoscope", not(target_arch = "wasm32"))))]
    {
        None
    }
}

pub struct Pending {
    job: u64,
    seq: ItemId,
    clip: ClipId,
    snapshot: TrackItem,
    result: Arc<Mutex<Option<RasterMask>>>,
    preview: bool,
    draft: Draft,
}

/// A transient Program-monitor result. Never enters the document, save file or undo stack.
pub struct Preview {
    pub job: u64,
    pub clip: ClipId,
    seq: ItemId,
    base_revision: u64,
    project: Arc<filmcraft_project::Project>,
    excluded_project: Arc<filmcraft_project::Project>,
    refined_project: Arc<filmcraft_project::Project>,
    alpha_project: Arc<filmcraft_project::Project>,
    composite_project: Arc<filmcraft_project::Project>,
    mask: Arc<RasterMask>,
}

pub fn monitor_preview(s: &Session, seq: ItemId, time: Tick) -> Option<(Arc<filmcraft_project::Project>, u64)> {
    let preview = s.ai_mask_preview.as_ref()?;
    if preview.seq != seq || preview.base_revision != s.revision || !s.state.selection.contains(&preview.clip) {
        return None;
    }
    let item = s.project.sequence(seq)?.find_item(preview.clip)?.1;
    let source_time = item.source_time_at(time);
    if time < item.start || time >= item.end() || source_time < preview.mask.frames.first()?.time || source_time >= preview.mask.end {
        return None;
    }
    let kind = s.state.ai_mask.as_ref().filter(|d| d.clip == preview.clip).map(|d| d.pick).unwrap_or_default();
    let (project, variant) = match kind {
        Pick::Background => (&preview.excluded_project, 1),
        Pick::Refine => match s.state.ai_mask.as_ref().map(|d| d.view).unwrap_or_default() {
            RefineView::Region => (&preview.refined_project, 2),
            RefineView::Alpha => (&preview.alpha_project, 3),
            RefineView::Composite => (&preview.composite_project, 4),
        },
        _ => (&preview.project, 0),
    };
    // Tool changes redraw the cached alpha immediately, without another model job.
    // The display variant must also distinguish the monitor's cached frame key.
    Some((project.clone(), s.revision ^ 0xa17a_0000_0000_0000 ^ preview.job.rotate_left(17) ^ variant))
}

fn has_full_job(s: &Session, clip: ClipId) -> bool {
    s.ai_mask_jobs
        .iter()
        .any(|j| j.clip == clip && !j.preview && s.jobs.iter().find(|job| job.id == j.job).is_some_and(|job| !job.progress.cancel.load(Ordering::Relaxed)))
}

fn cancel_previews(s: &mut Session) {
    cancel_preview_jobs(s);
    s.ai_mask_preview = None;
}

fn cancel_preview_jobs(s: &mut Session) {
    for pending in s.ai_mask_jobs.iter().filter(|j| j.preview) {
        if let Some(job) = s.jobs.iter().find(|j| j.id == pending.job) {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// History restores the draft synchronously; stale jobs must never replace its preview.
pub(crate) fn history_restored(s: &mut Session) {
    for pending in &s.ai_mask_jobs {
        if let Some(job) = s.jobs.iter().find(|job| job.id == pending.job) {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
    cancel_previews(s);
    if s.ai_mask_backend.is_some()
        && can_preview_current(s)
        && let Err(error) = preview(s, &json!({}))
    {
        s.error_toast("masks.ai.preview", error.to_string());
    }
}

fn can_preview_current(s: &Session) -> bool {
    let Some(draft) = s.state.ai_mask.as_ref() else { return false };
    let ph = s.playhead();
    s.active_sequence().and_then(|q| q.find_item(draft.clip)).is_some_and(|(_, it)| {
        ph >= it.start
            && ph < it.end()
            && s.source(it.item).is_some_and(|src| {
                let rate = src.info().frame_rate();
                let current = rate.frame_at(it.source_time_at(ph));
                draft.points.iter().any(|point| point.kind == Pick::Foreground && rate.frame_at(point.time) == current)
                    || (draft.pick == Pick::Refine
                        && it
                            .effects
                            .iter()
                            .flat_map(|effect| &effect.masks)
                            .filter_map(|mask| mask.raster.as_ref())
                            .any(|mask| mask.alpha_at(it.source_time_at(ph)).is_some()))
            })
    })
}

pub fn active_job(s: &Session, clip: ClipId) -> Option<&crate::Job> {
    let pending = s.ai_mask_jobs.iter().rev().find(|j| j.clip == clip)?;
    s.jobs.iter().find(|j| j.id == pending.job)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    [
        (
            "masks.ai.select",
            "AI Rotoscope…",
            r#"{"clip":id?,"kind":"foreground|background|refine|off"?,"clear":bool?}"#,
            select as fn(&mut Session, &Value) -> Result<Value>,
        ),
        ("masks.ai.settings", "AI Rotoscope Settings", r#"{"mode":"fast|quality"?,"frames":1..120?,"band":1..32?,"view":"region|alpha|composite"?}"#, settings),
        ("masks.ai.point", "AI Mask Point", r#"{"x":px,"y":px,"kind":"foreground|background|refine"?,"preview":bool?=true,"stroke":uint?}"#, point),
        ("masks.ai.removePoint", "Remove AI Mask Point", r#"{"point":index,"preview":bool?=true}"#, remove_point),
        ("masks.ai.generate", "Generate AI Mask", r#"{"wait":bool?}"#, generate),
        ("masks.ai.preview", "Preview AI Mask", r#"{"wait":bool?}"#, preview),
    ]
    .into_iter()
    .map(|(id, label, params, run)| CommandSpec {
        id,
        label,
        params,
        run,
        menu: if id == "masks.ai.select" { &["Clip"] } else { &[] },
        shortcut: None,
        enabled: has_seq,
        journal: true,
    })
    .collect()
}

fn selected_clip(s: &Session, p: &Value) -> Result<ClipId> {
    let clip = clip_p(p, "clip").or_else(|| s.state.selection.first().copied()).ok_or_else(|| bad("masks.ai.select", "select a video clip"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = seq.find_item(clip).ok_or_else(|| bad("masks.ai.select", "clip is gone"))?;
    if seq.track(track).is_none_or(|t| t.kind != TrackKind::Video || t.locked) || s.project.resolve_media(it.item).is_none() {
        return Err(bad("masks.ai.select", "select an unlocked video clip with source media"));
    }
    Ok(clip)
}

fn pick(value: &str) -> Result<Pick> {
    match value {
        "off" => Ok(Pick::Off),
        "foreground" => Ok(Pick::Foreground),
        "background" => Ok(Pick::Background),
        "refine" => Ok(Pick::Refine),
        _ => Err(bad("masks.ai.select", "unknown selection kind")),
    }
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = selected_clip(s, p)?;
    if has_full_job(s, clip) {
        return Err(bad("masks.ai.select", "wait for or cancel the current mask job"));
    }
    let kind = pick(str_p(p, "kind").unwrap_or("foreground"))?;
    let mut draft = s.state.ai_mask.clone().filter(|d| d.clip == clip).unwrap_or(Draft {
        clip,
        pick: kind,
        quality: false,
        frames: 30,
        band: 8,
        points: Vec::new(),
        view: RefineView::Region,
    });
    draft.pick = kind;
    if kind == Pick::Refine {
        draft.quality = true;
    }
    let clear = bool_p(p, "clear").unwrap_or(false);
    if clear {
        draft.points.clear();
        cancel_previews(s);
    }
    s.state.selected_mask = None;
    s.state.selected_effect = None;
    if clear {
        s.edit_ai_draft("Clear AI points", None, draft.clone());
    } else {
        s.state.ai_mask = Some(draft.clone());
    }
    if kind == Pick::Refine
        && !clear
        && s.ai_mask_backend.is_some()
        && s.ai_mask_preview.as_ref().is_none_or(|preview| !preview.mask.quality)
        && can_preview_current(s)
        && let Err(error) = preview(s, &json!({}))
    {
        s.error_toast("masks.ai.preview", error.to_string());
    }
    Ok(json!(draft))
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    let mut draft = s.state.ai_mask.clone().ok_or_else(|| bad("masks.ai.settings", "select an object first"))?;
    if has_full_job(s, draft.clip) {
        return Err(bad("masks.ai.settings", "wait for or cancel the current mask job"));
    }
    if let Some(mode) = p.get("mode") {
        draft.quality = match mode.as_str() {
            Some("fast") => false,
            Some("quality") => true,
            _ => return Err(bad("masks.ai.settings", "mode must be fast or quality")),
        };
        if !draft.quality && draft.pick == Pick::Refine {
            draft.pick = Pick::Foreground;
        }
    }
    if let Some(view) = p.get("view") {
        draft.view = match view.as_str() {
            Some("region") => RefineView::Region,
            Some("alpha") => RefineView::Alpha,
            Some("composite") => RefineView::Composite,
            _ => return Err(bad("masks.ai.settings", "view must be region, alpha or composite")),
        };
    }
    for (name, target, max) in [("frames", &mut draft.frames, 120), ("band", &mut draft.band, 32)] {
        if p.get(name).is_some() {
            let n = u64_p(p, name).filter(|n| (1..=max).contains(n)).ok_or_else(|| bad("masks.ai.settings", format!("{name} must be 1..{max}")))?;
            *target = n as u32;
        }
    }
    s.state.ai_mask = Some(draft.clone());
    if (p.get("mode").is_some() || p.get("band").is_some()) && !draft.points.is_empty() && s.ai_mask_preview.is_some() {
        cancel_previews(s);
        if let Err(error) = preview(s, &json!({})) {
            s.error_toast("masks.ai.preview", error.to_string());
        }
    }
    Ok(json!(draft))
}

fn point(s: &mut Session, p: &Value) -> Result<Value> {
    let draft = s.state.ai_mask.as_ref().ok_or_else(|| bad("masks.ai.point", "select an object first"))?;
    let clip = draft.clip;
    if has_full_job(s, clip) {
        return Err(bad("masks.ai.point", "wait for or cancel the current mask job"));
    }
    let kind = str_p(p, "kind").map(pick).transpose()?.unwrap_or(draft.pick);
    if kind == Pick::Off {
        return Err(bad("masks.ai.point", "enable a selection tool first"));
    }
    let x = f64_p(p, "x").filter(|v| v.is_finite()).ok_or_else(|| bad("masks.ai.point", "x must be finite"))?;
    let y = f64_p(p, "y").filter(|v| v.is_finite()).ok_or_else(|| bad("masks.ai.point", "y must be finite"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("masks.ai.point", "clip is gone"))?;
    let t = s.playhead();
    let src = s.source(it.item).ok_or_else(|| bad("masks.ai.point", "source media unavailable"))?;
    let video = src.info().video.as_ref().ok_or_else(|| bad("masks.ai.point", "no video stream"))?;
    if t < it.start || t >= it.end() || x < 0.0 || y < 0.0 || x >= video.width as f64 || y >= video.height as f64 || draft.points.len() >= 512 {
        return Err(bad("masks.ai.point", "point is outside this clip/frame or the 512-point budget"));
    }
    let time = it.source_time_at(t);
    let stroke = p.get("stroke").map(|n| n.as_u64().ok_or_else(|| bad("masks.ai.point", "stroke must be an unsigned integer"))).transpose()?;
    let key = stroke.filter(|_| kind == Pick::Refine).map(|stroke| format!("ai-refine:{}:{}:{stroke}", clip.0, time.0));
    let mut draft = draft.clone();
    if kind == Pick::Refine {
        draft.quality = true;
    }
    draft.points.push(Point { time, x, y, kind });
    let points = draft.points.len();
    s.edit_ai_draft(
        match kind {
            Pick::Refine => "Refine AI edge",
            Pick::Background => "Add AI Exclude point",
            _ => "Add AI Include point",
        },
        key.as_deref(),
        draft,
    );
    // Include/Exclude clicks give feedback on this frame before full-clip tracking.
    // During a brush drag the UI defers inference until release. A click or completed
    // stroke runs Quality automatically; painted pixels guide ViTMatte, never fill alpha.
    if kind == Pick::Refine {
        cancel_preview_jobs(s);
    }
    if s.ai_mask_backend.is_some()
        && bool_p(p, "preview").unwrap_or(true)
        && let Err(error) = preview(s, &json!({}))
    {
        s.error_toast("masks.ai.preview", error.to_string());
    }
    Ok(json!({"points": points, "time": time.0}))
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    generate(s, &json!({"preview":true, "wait":bool_p(p, "wait").unwrap_or(false)}))
}

fn remove_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "masks.ai.removePoint";
    let draft = s.state.ai_mask.as_ref().ok_or_else(|| bad(CMD, "select an object first"))?;
    let clip = draft.clip;
    if has_full_job(s, clip) {
        return Err(bad(CMD, "wait for or cancel the current mask job"));
    }
    selected_clip(s, &json!({"clip":clip.0}))?;
    let index = p
        .get("point")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&i| draft.points.get(i).is_some())
        .ok_or_else(|| bad(CMD, "point must be an existing zero-based point index"))?;
    let mut draft = draft.clone();
    draft.points.remove(index);
    let count = draft.points.len();
    s.edit_ai_draft("Remove AI point", None, draft);
    // An old preview must disappear even when this removes the last Include point.
    // In-flight previews are cancelled so their results cannot restore a deleted point.
    cancel_previews(s);
    if can_preview_current(s)
        && s.ai_mask_backend.is_some()
        && bool_p(p, "preview").unwrap_or(true)
        && let Err(error) = preview(s, &json!({}))
    {
        s.error_toast(CMD, error.to_string());
    }
    Ok(json!({"removed":index,"points":count}))
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let mut draft = s.state.ai_mask.clone().ok_or_else(|| bad("masks.ai.generate", "select an object first"))?;
    let request_draft = draft.clone();
    let preview = bool_p(p, "preview").unwrap_or(false);
    if !(1..=120).contains(&draft.frames) || !(1..=32).contains(&draft.band) || draft.points.len() > 512 {
        return Err(bad("masks.ai.generate", "invalid frame, edge-band or point budget"));
    }
    selected_clip(s, &json!({"clip": draft.clip.0}))?;
    if has_full_job(s, draft.clip) {
        return Err(bad("masks.ai.generate", "mask generation is already running"));
    }
    if !preview && !draft.points.iter().any(|p| p.kind == Pick::Foreground) {
        return Err(bad("masks.ai.generate", "add at least one foreground point in the Program monitor"));
    }
    let backend = s
        .ai_mask_backend
        .clone()
        .ok_or_else(|| EngineError::Other("AI inference worker unavailable. Start experiments/rotoscope/run-editor.sh for this experimental build.".into()))?;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let (_, item) = s.active_sequence().and_then(|q| q.find_item(draft.clip)).ok_or_else(|| bad("masks.ai.generate", "clip is gone"))?;
    let snapshot = item.clone();
    let source = s.source(item.item).ok_or_else(|| bad("masks.ai.generate", "source unavailable"))?;
    let video = source.info().video.as_ref().ok_or_else(|| bad("masks.ai.generate", "no video stream"))?.clone();
    let rate = video.frame_rate;
    if rate.num <= 0
        || rate.den <= 0
        || rate.num > 1_000_000
        || rate.den > 1_000_000
        || item.source_in.0 < 0
        || item.duration.0 <= 0
        || !item.speed.is_finite()
        || item.speed == 0.0
        || video.width == 0
        || video.height == 0
    {
        return Err(bad("masks.ai.generate", "invalid source frame rate/time/speed"));
    }
    let first = rate.frame_at(if preview { item.source_time_at(s.playhead()) } else { item.source_in });
    let mut base_mask = None;
    if preview {
        if s.playhead() < item.start || s.playhead() >= item.end() {
            return Err(bad("masks.ai.preview", "move the playhead inside the clip"));
        }
        draft.points.retain(|p| rate.frame_at(p.time) == first);
        draft.frames = 1;
        if !draft.points.iter().any(|p| p.kind == Pick::Foreground) {
            if draft.quality && draft.pick == Pick::Refine {
                let time = item.source_time_at(s.playhead());
                base_mask = item
                    .effect("opacity")
                    .into_iter()
                    .flat_map(|effect| &effect.masks)
                    .filter_map(|mask| mask.raster.as_ref())
                    .find(|mask| mask.alpha_at(time).is_some())
                    .cloned();
            }
            if base_mask.is_none() {
                return Err(bad("masks.ai.preview", "add an Include point on this frame, or generate a selection before refining it"));
            }
        }
    }
    let source_span = (item.duration.0 as f64 * item.speed.abs()).ceil();
    if !source_span.is_finite() || source_span < 1.0 || source_span >= i64::MAX as f64 {
        return Err(bad("masks.ai.generate", "clip time overflow"));
    }
    let source_end = item.source_in.0.checked_add(source_span as i64).map(Tick).ok_or_else(|| bad("masks.ai.generate", "clip time overflow"))?;
    let fd = rate.frame_duration().0;
    if fd <= 0 || first < 0 || !item.speed.is_finite() || item.speed == 0.0 {
        return Err(bad("masks.ai.generate", "invalid source frame rate/time/speed"));
    }
    let available = ((source_end.0.saturating_sub(rate.tick_of(first).0).saturating_add(fd - 1)) / fd).max(1);
    let count = available.min(draft.frames as i64) as usize;
    let times: Vec<Tick> = (0..count).map(|n| rate.tick_of(first.saturating_add(n as i64))).collect();
    let end = rate.tick_of(first.saturating_add(count as i64));
    if draft.points.iter().any(|p| {
        p.time < rate.tick_of(first)
            || p.time >= end
            || !p.x.is_finite()
            || !p.y.is_finite()
            || p.x < 0.0
            || p.y < 0.0
            || p.x >= video.width as f64
            || p.y >= video.height as f64
            || p.kind == Pick::Off
    }) {
        return Err(bad("masks.ai.generate", "prompts must fall in the processed range (the first Frames frames of this clip)"));
    }
    let scale = (720.0 / video.width.max(video.height).max(1) as f64).min(1.0);
    let width = ((video.width as f64 * scale).round() as u32).max(1);
    let height = ((video.height as f64 * scale).round() as u32).max(1);
    let n = (width as usize).checked_mul(height as usize).ok_or_else(|| bad("masks.ai.generate", "dimensions overflow"))?;
    if n.checked_mul(count).is_none_or(|n| n > 40_000_000) {
        return Err(bad("masks.ai.generate", "reduce Frames: the input exceeds the pixel budget"));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| bad("masks.ai.generate", "job id overflow"))?;
    if preview && draft.pick == Pick::Refine {
        cancel_preview_jobs(s);
    } else {
        cancel_previews(s);
    }
    let job = crate::Job {
        id,
        label: format!("AI Rotoscope{} ({})", if preview { " Preview" } else { "" }, if draft.quality { "Quality" } else { "Fast" }),
        progress: Default::default(),
        result: Default::default(),
    };
    job.progress.total.store((count * 3) as u64, Ordering::Relaxed);
    let output: Arc<Mutex<Option<RasterMask>>> = Arc::default();
    let (progress, result, out) = (job.progress.clone(), job.result.clone(), output.clone());
    let work = move || {
        let start = web_time::Instant::now();
        let compute = || -> std::result::Result<RasterMask, String> {
            let mut frames = Vec::with_capacity(count);
            for (i, time) in times.iter().enumerate() {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err("Cancelled: nothing was changed".into());
                }
                let f = source.video_frame(filmcraft_media::FrameRequest { time: *time, scale: scale as f32 }).map_err(|e| e.to_string())?;
                let fw = f.width as usize;
                let fh = f.height as usize;
                if fw == 0 || fh == 0 || fw.checked_mul(fh).is_none_or(|n| n > 16_777_216) {
                    return Err("decoded frame exceeds the dimension budget".into());
                }
                let pixels = f.to_rgba8();
                if fw.checked_mul(fh).and_then(|n| n.checked_mul(4)) != Some(pixels.len()) {
                    return Err("invalid decoded frame length".into());
                }
                let mut rgba = Vec::with_capacity(n * 4);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let sy = y * fh / height as usize;
                        let sx = x * fw / width as usize;
                        let offset = (sy * fw + sx) * 4;
                        let pixel = pixels.get(offset..offset + 4).ok_or("frame sample out of bounds")?;
                        rgba.extend_from_slice(pixel);
                    }
                }
                frames.push(InputFrame { time: *time, rgba });
                progress.done.store((i + 1) as u64, Ordering::Relaxed);
                *lock(&progress.status) = format!("Preparing frame {}/{}", i + 1, count);
            }
            let points = draft
                .points
                .iter()
                .map(|p| Point {
                    time: p.time,
                    x: p.x * width as f64 / video.width.max(1) as f64,
                    y: p.y * height as f64 / video.height.max(1) as f64,
                    kind: p.kind,
                })
                .collect();
            let base_alpha = base_mask
                .as_ref()
                .map(|mask| {
                    mask.validate()?;
                    let alpha = mask.alpha_at(rate.tick_of(first)).ok_or("Existing alpha does not cover this source frame")?;
                    let resized = filmcraft_rotoscope::pixels::bilinear(&alpha, mask.width as usize, mask.height as usize, width as usize, height as usize)?;
                    Ok::<_, String>(resized.into_iter().map(|value| (value.clamp(0.0, 1.0) * 65535.0).round_ties_even() as u16).collect())
                })
                .transpose()?;
            let input =
                Input { width, height, end, fps: format!("{}/{}", rate.num, rate.den), frames, points, quality: draft.quality, band: draft.band, base_alpha };
            let mask = backend.infer(&input, &progress)?;
            mask.validate()?;
            if mask.width != width
                || mask.height != height
                || mask.frames.len() != count
                || mask.end != end
                || mask.quality != draft.quality
                || mask.frames.iter().zip(&input.frames).any(|(a, b)| a.time != b.time)
            {
                return Err("inference returned alpha for the wrong frames or dimensions".into());
            }
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("Cancelled: nothing was changed".into());
            }
            Ok(mask)
        };
        let computed = compute();
        let seconds = start.elapsed().as_secs_f64();
        let report = match computed {
            Ok(mask) => {
                *lock(&out) = Some(mask);
                *lock(&progress.status) = "AI mask complete".into();
                progress.done.store((count * 3) as u64, Ordering::Relaxed);
                Ok(filmcraft_export::Report {
                    path: String::new(),
                    frames: count as u64,
                    seconds,
                    bytes: 0,
                    render_fps: count as f64 / seconds.max(1e-6),
                    extra_files: Vec::new(),
                })
            }
            Err(error) => {
                *lock(&progress.status) = error.clone();
                Err(error)
            }
        };
        *lock(&result) = Some(report);
        progress.finished.store(true, Ordering::Relaxed);
    };
    let work = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), work);
    let wait = bool_p(p, "wait").unwrap_or(false);
    #[cfg(not(target_arch = "wasm32"))]
    if !wait {
        std::thread::Builder::new().name("filmcraft-ai-mask".into()).spawn(work).map_err(|e| EngineError::Other(e.to_string()))?;
    } else {
        work();
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = wait;
        work();
    }
    s.jobs.push(job);
    s.ai_mask_jobs.push(Pending { job: id, seq, clip: draft.clip, snapshot, result: output, preview, draft: request_draft });
    if wait {
        poll(s);
    }
    Ok(json!({"job": id, "frames": count, "preview":preview, "sourceStart": rate.tick_of(first).0, "sourceEnd": end.0}))
}

pub fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.ai_mask_jobs.len() {
        let pending = &s.ai_mask_jobs[i];
        let job = s.jobs.iter().find(|j| j.id == pending.job);
        if job.is_some_and(|j| !j.progress.finished.load(Ordering::Relaxed)) {
            i += 1;
            continue;
        }
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        let pending = s.ai_mask_jobs.remove(i);
        let Some(mask) = lock(&pending.result).take().filter(|_| !cancelled) else {
            let error = s.jobs.iter().find(|j| j.id == pending.job).and_then(|j| lock(&j.result).as_ref().and_then(|r| r.as_ref().err()).cloned());
            if !cancelled && let Some(error) = error {
                s.error_toast("masks.ai.generate", error);
            }
            continue;
        };
        let current = s.project.sequence(pending.seq).and_then(|q| q.find_item(pending.clip)).map(|(_, it)| it);
        let same_prompts = s.state.ai_mask.as_ref().is_some_and(|draft| {
            draft.clip == pending.clip && draft.quality == pending.draft.quality && draft.band == pending.draft.band && draft.points == pending.draft.points
        });
        if pending.preview && current == Some(&pending.snapshot) && same_prompts {
            let mask = Arc::new(mask);
            let mut project = (*s.project).clone();
            if let Some((_, item)) = project.sequence_mut(pending.seq).and_then(|q| q.find_item_mut(pending.clip))
                && let Some(tint) = filmcraft_project::find_effect("tint")
            {
                if let Some(opacity) = item.effect_mut("opacity") {
                    opacity.masks.retain(|m| m.raster.is_none());
                }
                // Preview the selection over the original scene, rather than applying a cutout.
                // A masked standard Tint reuses the renderer's premultiplied-alpha reference
                // path and Motion transform; no display-only colour effect is saved.
                let mut overlay = tint.instance();
                for key in ["black", "white"] {
                    if let Some(param) = overlay.param_mut(key) {
                        param.value = ParamValue::Color([0.08, 0.9, 0.45, 1.0]);
                    }
                }
                if let Some(amount) = overlay.param_mut("amount") {
                    amount.value = ParamValue::Float(45.0);
                }
                let mut m = Mask::new("AI preview", MaskPath::default());
                m.raster = Some(mask.clone());
                overlay.masks.push(m);
                let mut excluded = overlay.clone();
                for key in ["black", "white"] {
                    if let Some(param) = excluded.param_mut(key) {
                        param.value = ParamValue::Color([1.0, 0.12, 0.12, 1.0]);
                    }
                }
                if let Some(m) = excluded.masks.first_mut() {
                    m.inverted = true;
                }
                let refined = display::refine_views(&project, pending.seq, pending.clip, &mask, &pending.draft);
                let Some((_, item)) = project.sequence_mut(pending.seq).and_then(|q| q.find_item_mut(pending.clip)) else { continue };
                item.effects.push(overlay);
                let mut excluded_project = project.clone();
                if let Some((_, item)) = excluded_project.sequence_mut(pending.seq).and_then(|q| q.find_item_mut(pending.clip)) {
                    // Place the red tint before the green tint at soft alpha edges.
                    let green_index = item.effects.len().saturating_sub(1);
                    item.effects.insert(green_index, excluded);
                }
                let refined = match refined {
                    Ok(views) => views,
                    Err(error) => {
                        s.error_toast("masks.ai.preview", error);
                        continue;
                    }
                };
                s.ai_mask_preview = Some(Preview {
                    job: pending.job,
                    clip: pending.clip,
                    seq: pending.seq,
                    base_revision: s.revision,
                    project: Arc::new(project),
                    excluded_project: Arc::new(excluded_project),
                    refined_project: Arc::new(refined.region),
                    alpha_project: Arc::new(refined.alpha),
                    composite_project: Arc::new(refined.composite),
                    mask,
                });
            }
            continue;
        }
        if pending.preview {
            continue;
        }
        let apply = if current != Some(&pending.snapshot) {
            Err(EngineError::Other("clip changed during AI inference; regenerate its mask".into()))
        } else {
            s.edit("AI Rotoscope", |project, state| {
                let (_, item) =
                    project.sequence_mut(pending.seq).and_then(|q| q.find_item_mut(pending.clip)).ok_or_else(|| bad("masks.ai.generate", "clip is gone"))?;
                let ei = item.effects.iter().position(|e| e.effect == "opacity").ok_or_else(|| bad("masks.ai.generate", "clip has no Opacity effect"))?;
                let effect = item.effects.get_mut(ei).ok_or_else(|| bad("masks.ai.generate", "Opacity effect is gone"))?;
                effect.masks.retain(|m| m.raster.is_none());
                let mut m = Mask::new(if mask.quality { "AI Rotoscope (Quality)" } else { "AI Rotoscope (Fast)" }, MaskPath::default());
                m.feather.value = ParamValue::Float(0.0);
                m.raster = Some(Arc::new(mask));
                let mi = effect.masks.len();
                effect.masks.push(m);
                state.selected_mask = Some(crate::masks::MaskSel { clip: pending.clip, effect: ei, mask: mi });
                if let Some(d) = state.ai_mask.as_mut().filter(|d| d.clip == pending.clip) {
                    d.pick = Pick::Off;
                }
                Ok(())
            })
        };
        if let Err(error) = apply {
            if let Some(job) = s.jobs.iter().find(|j| j.id == pending.job) {
                *lock(&job.result) = Some(Err(error.to_string()));
            }
            s.error_toast("masks.ai.generate", error.to_string());
        } else {
            s.ai_mask_preview = None;
        }
    }
}

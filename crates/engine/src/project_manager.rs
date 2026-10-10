//! File ▸ Project Manager…: make a self-contained copy of a project.
//!
//! - **Collect Files and Copy to New Location** copies every media file the chosen sequences use
//!   (optionally their proxies and render previews) into one folder, and writes the project there
//!   with its paths pointing at the copies.
//! - **Consolidate and Transcode** writes only the used part of each clip (plus handles) with a
//!   transcode preset, and re-bases the clips' media time so every edit, keyframe and marker stays
//!   where it was.
//!
//! Both can exclude unused clips. A dry run reports the disk space before and after. The copy
//! runs as a background job; the project file is written last, with the outputs' fingerprints.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use filmcraft_media::MediaKind;
use filmcraft_project::{ItemId, ItemKind, MediaRef, Project};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, str_p, u64_p};
use crate::proxies::{Preset, preset, unique_path};
use crate::{EngineError, Result, Session};

/// Media time used per item (min, max) across the sequences, following nested sequences.
pub fn used_ranges(p: &Project, seqs: &[ItemId]) -> (BTreeMap<ItemId, (Tick, Tick)>, BTreeSet<ItemId>) {
    let mut ranges: BTreeMap<ItemId, (Tick, Tick)> = BTreeMap::new();
    let mut used: BTreeSet<ItemId> = BTreeSet::new();
    let mut stack: Vec<ItemId> = seqs.to_vec();
    while let Some(sid) = stack.pop() {
        if !used.insert(sid) {
            continue;
        }
        let Some(seq) = p.sequence(sid) else { continue };
        for t in seq.all_tracks() {
            for ti in &t.items {
                let mut target = ti.item;
                match p.item(ti.item).map(|i| &i.kind) {
                    Some(ItemKind::Sequence(_)) => {
                        stack.push(ti.item);
                        continue;
                    }
                    Some(ItemKind::Subclip { parent, .. }) => {
                        used.insert(ti.item);
                        target = *parent;
                        // Loaded projects can nest subclips; keep every link up to the media (bounded like `resolve_media`).
                        let mut up = *parent;
                        for _ in 0..16 {
                            used.insert(up);
                            match p.item(up).map(|i| &i.kind) {
                                Some(ItemKind::Subclip { parent, .. }) => up = *parent,
                                _ => break,
                            }
                        }
                    }
                    Some(_) => {}
                    None => continue,
                }
                used.insert(target);
                let a = ti.source_time_at(ti.start);
                let b = ti.source_time_at(ti.end() - Tick(1));
                let (lo, hi) = (a.min(b), a.max(b) + Tick(1));
                let e = ranges.entry(target).or_insert((lo, hi));
                e.0 = e.0.min(lo);
                e.1 = e.1.max(hi);
            }
        }
    }
    (ranges, used)
}

struct Work {
    item: ItemId,
    src_path: String,
    out: String,
    /// Consolidate: the media range to write; None = copy the file.
    range: Option<TimeRange>,
    proxy: Option<(String, String)>,
}

/// Plan, estimate and (unless `dryRun`) run the Project Manager.
pub fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let dest = str_p(p, "destination").ok_or_else(|| bad("file.projectManager", "need `destination`"))?.to_string();
    let consolidate = match str_p(p, "mode").unwrap_or("collect") {
        "collect" => false,
        "consolidate" | "transcode" => true,
        m => return Err(bad("file.projectManager", format!("unknown mode `{m}` (collect | consolidate)"))),
    };
    let exclude_unused = bool_p(p, "excludeUnused").unwrap_or(true);
    let handles = u64_p(p, "handles").unwrap_or(30) as i64;
    let include_proxies = bool_p(p, "includeProxies").unwrap_or(!consolidate) && !consolidate;
    let include_previews = bool_p(p, "includePreviews").unwrap_or(false);
    let pr: &'static Preset =
        preset(str_p(p, "preset").unwrap_or(crate::proxies::DEFAULT_TRANSCODE_PRESET)).ok_or_else(|| bad("file.projectManager", "unknown preset"))?;
    let seqs: Vec<ItemId> = match p.get("sequences").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(ItemId).collect(),
        None => s.project.sequences().map(|i| i.id).collect(),
    };
    if seqs.iter().any(|q| s.project.sequence(*q).is_none()) {
        return Err(bad("file.projectManager", "`sequences` must list sequence ids"));
    }
    let (ranges, used) = used_ranges(&s.project, &seqs);
    let name = str_p(p, "projectName")
        .map(str::to_string)
        .or_else(|| s.path.as_deref().and_then(|x| Path::new(x).file_stem()).map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| s.project.name.clone());
    let dest_dir = PathBuf::from(&dest);
    let project_path = dest_dir.join(format!("{name}.fcproj")).to_string_lossy().into_owned();

    // ---- the new project
    let mut proj = (*s.project).clone();
    if exclude_unused {
        let keep: BTreeSet<ItemId> = proj
            .items
            .values()
            .filter(|i| {
                used.contains(&i.id)
                    || matches!(i.kind, ItemKind::Graphic { .. })
                    || (!matches!(i.kind, ItemKind::Media(_) | ItemKind::Subclip { .. } | ItemKind::Sequence(_)))
            })
            .map(|i| i.id)
            .collect();
        let drop: Vec<ItemId> = proj.items.keys().filter(|i| !keep.contains(i)).copied().collect();
        for i in drop {
            proj.items.remove(&i);
            proj.root.remove_item(i);
        }
    }
    // ---- work list and estimate
    let mut work = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    let mut original_bytes = 0u64;
    let mut result_bytes = 0u64;
    let fd_handles = |rate: filmcraft_time::FrameRate| Tick(rate.frame_duration().0 * handles);
    let ids: Vec<ItemId> = proj.items.keys().copied().collect();
    for id in ids {
        let Some(m) = proj.item(id).and_then(|i| i.as_media()).cloned() else { continue };
        let MediaRef::File { path } = &m.media else { continue };
        let size = s.services.file_size(path).unwrap_or(0);
        original_bytes += size;
        let file = Path::new(path);
        let stem = file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("media-{}", id.0));
        let ext = file.extension().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let transcode = consolidate && matches!(m.info.kind, MediaKind::Movie | MediaKind::AudioOnly) && ranges.contains_key(&id);
        let (out, range) = if transcode {
            let rate = m.frame_rate();
            let (lo, hi) = ranges[&id];
            let a = rate.snap((lo - fd_handles(rate)).max(Tick::ZERO));
            let b = (rate.snap(hi + fd_handles(rate) + rate.frame_duration() - Tick(1))).min(m.info.duration).max(a + rate.frame_duration());
            let ext = if m.info.video.is_some() { pr.extension } else { "wav" };
            let out = unique_path(&dest_dir, &stem, "", ext, &taken).to_string_lossy().into_owned();
            result_bytes += estimate_bytes(&m, b - a, pr);
            (out, Some(TimeRange::from_bounds(a, b)))
        } else {
            result_bytes += size;
            (unique_path(&dest_dir, &stem, "", &ext, &taken).to_string_lossy().into_owned(), None)
        };
        taken.push(out.clone());
        let proxy = match (&m.proxy, include_proxies) {
            (Some(MediaRef::File { path: pp }), true) if s.services.file_size(pp).is_ok() => {
                let pf = Path::new(pp);
                let ps = pf.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let pe = pf.extension().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let o = unique_path(&dest_dir.join("Proxies"), &ps, "", &pe, &taken).to_string_lossy().into_owned();
                taken.push(o.clone());
                let b = s.services.file_size(pp).unwrap_or(0);
                original_bytes += b;
                result_bytes += b;
                Some((pp.clone(), o))
            }
            _ => None,
        };
        work.push(Work { item: id, src_path: path.clone(), out, range, proxy });
    }
    let previews = s.path.as_deref().map(crate::previews::dir_for_project).filter(|d| include_previews && d.is_dir());
    if let Some(d) = &previews {
        let b = dir_size(d);
        original_bytes += b;
        result_bytes += b;
    }
    let files: Vec<Value> = work
        .iter()
        .map(|w| json!({"item": w.item.0, "from": w.src_path, "to": w.out, "trim": w.range.map(|r| json!({"start": r.start.0, "end": r.end().0}))}))
        .collect();
    let summary = json!({
        "project": project_path,
        "mode": if consolidate { "consolidate" } else { "collect" },
        "originalBytes": original_bytes,
        "resultBytes": result_bytes,
        "files": files,
        "excluded": s.project.items.len() - proj.items.len(),
    });
    if bool_p(p, "dryRun").unwrap_or(false) {
        return Ok(summary);
    }
    if Path::new(&project_path).exists() && !bool_p(p, "overwrite").unwrap_or(false) {
        return Err(EngineError::Other(format!("{project_path} already exists")));
    }
    std::fs::create_dir_all(&dest_dir).map_err(|e| EngineError::Other(format!("{dest}: {e}")))?;

    // ---- the job
    let sources: BTreeMap<ItemId, filmcraft_media::SharedSource> =
        work.iter().filter(|w| w.range.is_some()).filter_map(|w| s.media.full_res_source(&s.project, w.item, &*s.services).map(|src| (w.item, src))).collect();
    if let Some(w) = work.iter().find(|w| w.range.is_some() && s.media.offline_status(w.item).is_some()) {
        return Err(EngineError::Other(format!("{} is offline; link it before consolidating", w.src_path)));
    }
    let total: u64 = work
        .iter()
        .map(|w| w.range.map_or(1, |r| proj.item(w.item).and_then(|i| i.as_media()).map_or(1, |m| crate::proxies::frame_count(m, Some(r)))))
        .sum::<u64>()
        + 1;
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: format!("Project Manager: {name}"), progress: Default::default(), result: Default::default() };
    job.progress.total.store(total, Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let prev_dst = crate::previews::dir_for_project(&project_path);
    let services = s.services.clone();
    let run = move || {
        let t0 = web_time::Instant::now();
        let r = execute(proj, work, sources, previews.map(|d| (d, prev_dst)), &project_path, pr, &prog, &*services);
        let secs = t0.elapsed().as_secs_f64();
        let r = r.map(|bytes| filmcraft_export::Report {
            path: project_path.clone(),
            frames: prog.done.load(Ordering::Relaxed),
            seconds: secs,
            bytes,
            render_fps: 0.0,
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = match &r {
            Ok(_) => format!("Done in {secs:.1}s"),
            Err(e) => e.clone(),
        };
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-project-manager".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    let mut out = summary;
    out["job"] = json!(id);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn execute(
    mut proj: Project,
    work: Vec<Work>,
    sources: BTreeMap<ItemId, filmcraft_media::SharedSource>,
    previews: Option<(PathBuf, PathBuf)>,
    project_path: &str,
    pr: &Preset,
    prog: &filmcraft_export::Progress,
    services: &dyn crate::Services,
) -> std::result::Result<u64, String> {
    let mut bytes = 0;
    let openers = filmcraft_codecs::openers();
    for w in &work {
        if prog.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("{}", Path::new(&w.out).file_name().map(|n| n.to_string_lossy()).unwrap_or_default());
        if let Some(d) = Path::new(&w.out).parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        match w.range {
            None => {
                bytes += std::fs::copy(&w.src_path, &w.out).map_err(|e| format!("copying {}: {e}", w.src_path))?;
                prog.done.fetch_add(1, Ordering::Relaxed);
            }
            Some(r) => {
                let m = proj.item(w.item).and_then(|i| i.as_media()).cloned().ok_or("item vanished")?;
                let name = proj.item(w.item).map(|i| i.name.clone()).unwrap_or_default();
                let src = sources.get(&w.item).cloned().ok_or_else(|| format!("{name}: no source"))?;
                let rep = crate::proxies::transcode(&m, &name, src, Some(r), &w.out, pr, prog)?;
                bytes += rep.bytes;
                // the clip now starts at the range start: shift its uses, take the new file's info
                proj.shift_media_time(w.item, r.start);
                let b = std::fs::read(&w.out).map_err(|e| format!("{}: {e}", w.out))?;
                let fname = Path::new(&w.out).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let info = filmcraft_media::open_bytes(&fname, b.into(), &openers).map_err(|e| format!("{}: {e}", w.out))?.info().clone();
                if let Some(mm) = proj.item_mut(w.item).and_then(|i| i.as_media_mut()) {
                    mm.info = info;
                    mm.proxy = None;
                }
            }
        }
        if let Some((from, to)) = &w.proxy {
            bytes += std::fs::create_dir_all(Path::new(to).parent().unwrap_or(Path::new(".")))
                .and_then(|_| std::fs::copy(from, to))
                .map_err(|e| format!("copying {from}: {e}"))?;
        }
        let identity = crate::relink::identity_of(services, &w.out).ok();
        if let Some(mm) = proj.item_mut(w.item).and_then(|i| i.as_media_mut()) {
            mm.media = MediaRef::File { path: w.out.clone() };
            mm.offline = false;
            mm.identity = identity;
            mm.proxy = match (&w.proxy, w.range) {
                (Some((_, to)), None) => Some(MediaRef::File { path: to.clone() }),
                _ => None,
            };
        }
    }
    if let Some((from, to)) = previews {
        bytes += copy_dir(&from, &to).map_err(|e| format!("copying previews: {e}"))?;
    }
    let data = filmcraft_format::encode(&proj, false);
    filmcraft_format::atomic_write(Path::new(project_path), &data).map_err(|e| format!("{project_path}: {e}"))?;
    prog.done.fetch_add(1, Ordering::Relaxed);
    Ok(bytes + data.len() as u64)
}

fn estimate_bytes(m: &filmcraft_project::MediaClip, dur: Tick, pr: &Preset) -> u64 {
    let secs = dur.seconds().max(0.0);
    let audio = m.info.audio().map_or(0.0, |a| a.sample_rate as f64 * 2.0 * 2.0 * secs);
    let Some(v) = &m.info.video else { return audio as u64 };
    let fps = v.frame_rate.num as f64 / v.frame_rate.den.max(1) as f64;
    let px = (v.width * v.height) as f64 * (pr.scale * pr.scale) as f64;
    let mbps = match pr.format {
        filmcraft_export::Format::ProRes => filmcraft_export::prores_profile(pr.prores).nominal_mbps_1080p30() * px / (1920.0 * 1080.0) * fps / 29.97,
        _ => (px / 200.0).max(800.0) / 1000.0,
    };
    (mbps * 1e6 / 8.0 * secs + audio) as u64
}

fn dir_size(d: &Path) -> u64 {
    std::fs::read_dir(d)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| if e.path().is_dir() { dir_size(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) }).sum())
        .unwrap_or(0)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<u64> {
    std::fs::create_dir_all(to)?;
    let mut n = 0;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            n += copy_dir(&p, &to.join(e.file_name()))?;
        } else {
            n += std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(n)
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "file.projectManager",
        label: "Project Manager…",
        menu: &["File"],
        shortcut: None,
        params: r#"{"destination":str,"mode":"collect|consolidate","sequences":[id]?,"excludeUnused":bool=true,"handles":frames=30,"preset":"prores_lt|prores_hq|h264|…","includeProxies":bool,"includePreviews":bool=false,"projectName":str?,"dryRun":bool=false,"overwrite":bool=false,"wait":bool=false}"#,
        enabled: |s| if s.project.sequences().next().is_some() { Ok(()) } else { Err("the project has no sequences".into()) },
        run,
        journal: false,
    }]
}

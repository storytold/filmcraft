//! Saved-project links. The document model and worker lifecycle are provider-neutral; only the
//! EffectCraft adapter knows `.ecproj` or CLI arguments. No executable comes from a project file.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use filmcraft_export::Progress;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_project::{ItemId, Label, MediaRef, project_link::ProjectLink};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::commands::{CommandSpec, always, bad, str_p, u64_p};
use crate::{EngineError, Result, Services, Session};

const MAX_PROJECT: u64 = 16 * 1024 * 1024;
const MAX_LINKS: usize = 128;
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Object {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rate: filmcraft_time::FrameRate,
    pub duration: filmcraft_time::Tick,
}

/// A future PhotoCraft/VectorCraft adapter implements this contract without changing stored links.
/// Render at source resolution with alpha. A still adapter can return one-frame media; an animated
/// adapter returns source-time media starting at zero. Preview and export use this same output.
pub trait ProjectAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn extension(&self) -> &'static str {
        "mov"
    }
    fn objects(&self, services: &dyn Services, path: &str) -> std::result::Result<Vec<Object>, String>;
    fn revision(&self, services: &dyn Services, link: &ProjectLink) -> std::result::Result<String, String>;
    fn render(&self, link: &ProjectLink, output: &Path, progress: &Progress) -> std::result::Result<(), String>;
}

/// Installed by a trusted user action, never deserialized from a FilmCraft document.
pub struct EffectCraft {
    pub executable: PathBuf,
}

fn document(services: &dyn Services, path: &str) -> std::result::Result<Value, String> {
    if services.file_size(path).map_err(|e| e.to_string())? > MAX_PROJECT {
        return Err("linked project exceeds the 16 MiB prototype limit".into());
    }
    let bytes = services.read_range(path, 0, MAX_PROJECT as usize + 1).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT {
        return Err("linked project grew beyond the size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid linked project: {e}"))
}

fn hash_file(services: &dyn Services, path: &str, hash: &mut Sha256) -> std::result::Result<(), String> {
    let size = services.file_size(path).map_err(|e| format!("{path}: {e}"))?;
    if size > 4 * 1024 * 1024 * 1024_u64 {
        return Err("linked dependency exceeds the 4 GiB prototype limit".into());
    }
    hash.update(size.to_le_bytes());
    let mut offset = 0;
    while offset < size {
        let n = (size - offset).min(256 * 1024) as usize;
        let bytes = services.read_range(path, offset, n).map_err(|e| e.to_string())?;
        if bytes.len() != n {
            return Err(format!("{path}: dependency changed while reading"));
        }
        hash.update(&bytes);
        offset += n as u64;
    }
    Ok(())
}

impl ProjectAdapter for EffectCraft {
    fn id(&self) -> &'static str {
        "effectcraft"
    }

    fn objects(&self, services: &dyn Services, path: &str) -> std::result::Result<Vec<Object>, String> {
        let doc = document(services, path)?;
        if doc.get("schema").and_then(Value::as_u64) != Some(2) {
            return Err("unsupported EffectCraft schema (this adapter supports schema 2)".into());
        }
        let items = doc.get("items").and_then(Value::as_object).ok_or("EffectCraft project has no items")?;
        if items.len() > 100_000 {
            return Err("too many EffectCraft items".into());
        }
        let mut objects = Vec::new();
        for (id, item) in items {
            let Some(kind) = item.get("kind").filter(|k| k.get("type").and_then(Value::as_str) == Some("Comp")) else { continue };
            let width = kind.get("width").and_then(Value::as_u64).filter(|n| (1..=16_384).contains(n)).ok_or("invalid composition width")?;
            let height = kind.get("height").and_then(Value::as_u64).filter(|n| (1..=16_384).contains(n)).ok_or("invalid composition height")?;
            if width.checked_mul(height).is_none_or(|pixels| pixels > 16_777_216) {
                return Err("composition exceeds the 16-megapixel prototype limit".into());
            }
            let rate: filmcraft_time::FrameRate =
                serde_json::from_value(kind.get("frame_rate").cloned().ok_or("missing composition rate")?).map_err(|e| e.to_string())?;
            if rate.num <= 0 || rate.den <= 0 || rate.num > 240_000 || rate.den > 100_000 || rate.den.checked_mul(240).is_none_or(|max| rate.num > max) {
                return Err("invalid composition frame rate".into());
            }
            let duration = kind
                .get("duration")
                .and_then(Value::as_i64)
                .filter(|d| *d > 0 && *d <= filmcraft_time::TICKS_PER_SECOND * 3600)
                .ok_or("composition must be between zero and one hour")?;
            if id.parse::<u64>().is_err() {
                return Err("invalid composition ID".into());
            }
            objects.push(Object {
                id: id.clone(),
                name: item.get("name").and_then(Value::as_str).unwrap_or("Composition").into(),
                width: width as u32,
                height: height as u32,
                rate,
                duration: filmcraft_time::Tick(duration),
            });
            if objects.len() > 256 {
                return Err("too many compositions (prototype limit: 256)".into());
            }
        }
        Ok(objects)
    }

    fn revision(&self, services: &dyn Services, link: &ProjectLink) -> std::result::Result<String, String> {
        if link.version != VERSION {
            return Err("unsupported linked-project version".into());
        }
        if !self.objects(services, &link.project_path)?.iter().any(|o| o.id == link.object) {
            return Err("linked composition was deleted; relink the object".into());
        }
        let doc = document(services, &link.project_path)?;
        let mut hash = Sha256::new();
        hash.update(b"filmcraft-link-v1/full-resolution/prores4444/rgba/no-audio");
        hash.update(link.object.as_bytes());
        hash.update(link.cache_generation.to_le_bytes());
        // The renderer revision matters too: an installed update must not reuse old pictures.
        let meta = std::fs::metadata(&self.executable).map_err(|e| format!("EffectCraft renderer: {e}"))?;
        hash.update(self.executable.to_string_lossy().as_bytes());
        hash.update(meta.len().to_le_bytes());
        hash.update(format!("{:?}", meta.modified().ok()).as_bytes());
        hash_file(services, &link.project_path, &mut hash)?;
        let items = doc.get("items").and_then(Value::as_object).ok_or("missing items")?;
        let mut dependency_bytes = 0_u64;
        for item in items.values() {
            let Some(k) = item.get("kind").filter(|k| k.get("type").and_then(Value::as_str) == Some("Footage")) else { continue };
            if k.get("kind").and_then(Value::as_str) == Some("Sequence") {
                return Err("image-sequence dependencies are not supported yet; bake this composition".into());
            }
            if let Some(path) = k.get("path").and_then(Value::as_str) {
                let path = if Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    Path::new(&link.project_path).parent().unwrap_or(Path::new(".")).join(path)
                };
                let path = path.to_string_lossy();
                dependency_bytes = dependency_bytes.checked_add(services.file_size(&path).map_err(|e| e.to_string())?).ok_or("dependency size overflow")?;
                if dependency_bytes > 512 * 1024 * 1024 {
                    return Err("composition dependencies exceed the 512 MiB prototype budget; bake this composition".into());
                }
                hash.update(path.as_bytes());
                hash_file(services, &path, &mut hash)?;
            }
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn render(&self, link: &ProjectLink, output: &Path, progress: &Progress) -> std::result::Result<(), String> {
        use std::process::{Command, Stdio};
        let mut command = Command::new(&self.executable);
        command
            .args(["render", "--project", &link.project_path, "--comp", &link.object, "--out"])
            .arg(output)
            .args(["--format", "prores", "--prores", "4444", "--channels", "rgba", "--audio", "off"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().map_err(|e| format!("could not start EffectCraft renderer: {e}"))?;
        let start = web_time::Instant::now();
        loop {
            let oversized = std::fs::metadata(output).is_ok_and(|m| m.len() > 1024 * 1024 * 1024);
            if progress.cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(600) || oversized {
                let _ = child.kill();
                let _ = child.wait();
                return Err("linked render cancelled, exceeded ten minutes or exceeded 1 GiB; last valid cache retained".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() { Ok(()) } else { Err(format!("EffectCraft renderer exited with {status}; check the project in EffectCraft")) };
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.to_string());
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn render(&self, _: &ProjectLink, _: &Path, _: &Progress) -> std::result::Result<(), String> {
        Err("linked rendering requires a native desktop host".into())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: String,
    pub message: String,
}
type Outputs = Vec<(ItemId, ProjectLink, std::result::Result<(String, String), String>)>;
type Slot = Arc<Mutex<Option<Outputs>>>;

#[derive(Default)]
pub struct Runtime {
    pub adapters: BTreeMap<String, Arc<dyn ProjectAdapter>>,
    pub status: BTreeMap<ItemId, Status>,
    worker: Option<(Slot, Arc<Progress>)>,
    last_scan: Option<web_time::Instant>,
    paused: bool,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some((_, p)) = &self.worker {
            p.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// A final export must never silently use the last-good picture of a failed or changed link.
/// Background previews remain available while that link is stale or offline.
/// Capture session-owned adapter authorization; perform fresh content checks on the export worker.
pub fn export_preflight(s: &Session) -> impl FnOnce(&filmcraft_project::Project, ItemId) -> Result<()> + Send + 'static {
    let adapters = s.project_links.adapters.clone();
    let services = s.services.clone();
    let media = s.media.clone();
    move |project, seq| validate_export_inner(&adapters, &*services, &media, project, seq)
}

#[cfg(test)]
fn validate_export(s: &Session, project: &filmcraft_project::Project, seq: ItemId) -> Result<()> {
    validate_export_inner(&s.project_links.adapters, &*s.services, &s.media, project, seq)
}

fn validate_export_inner(
    adapters: &BTreeMap<String, Arc<dyn ProjectAdapter>>,
    services: &dyn Services,
    media: &crate::media_pool::MediaPool,
    project: &filmcraft_project::Project,
    seq: ItemId,
) -> Result<()> {
    let used = crate::project_tools::closure(project, &[seq]);
    let fresh = crate::media_pool::MediaPool::default();
    for (id, link) in project.project_links.iter().filter(|(id, _)| used.contains(id)) {
        if link.baked || project.item(*id).is_none() {
            continue;
        }
        let adapter = adapters
            .get(&link.provider)
            .ok_or_else(|| EngineError::Other(format!("Linked object {}: enable its renderer or bake it before exporting", link.object_name)))?;
        let revision = adapter.revision(services, link).map_err(|e| EngineError::Other(format!("Linked object {}: {e}", link.object_name)))?;
        if revision != link.revision {
            return Err(EngineError::Other(format!("Linked object {} has changed: wait for background refresh before exporting", link.object_name)));
        }
        let Some(MediaRef::File { path }) = project.item(*id).and_then(|i| i.as_media()).map(|m| &m.media) else {
            return Err(EngineError::Other(format!("Linked object {} has no render cache; refresh before exporting", link.object_name)));
        };
        let path = media.follow(path).map_err(|e| EngineError::Other(format!("Linked object {} cache path is not followed: {e}", link.object_name)))?;
        // Reopen the published file rather than trusting an already-cached decoder or its existence.
        let source = fresh
            .open_file(&path, services)
            .map_err(|e| EngineError::Other(format!("Linked object {} has an invalid render cache: {e}; refresh before exporting", link.object_name)))?;
        if source.info().video.is_none() {
            return Err(EngineError::Other(format!("Linked object {} render cache has no video; refresh before exporting", link.object_name)));
        }
    }
    Ok(())
}

pub fn configure(s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(bad("linkedProject.configure", "native desktop only; use a baked fallback"));
    }
    let path = str_p(p, "executable").ok_or_else(|| bad("linkedProject.configure", "select the installed effectcraft-cli executable"))?;
    let path = std::fs::canonicalize(path).map_err(|e| EngineError::Other(e.to_string()))?;
    if !path.is_file() {
        return Err(bad("linkedProject.configure", "renderer is not a file"));
    }
    s.project_links.adapters.insert("effectcraft".into(), Arc::new(EffectCraft { executable: path }));
    s.project_links.last_scan = None;
    s.project_links.paused = false;
    Ok(json!({"provider":"effectcraft","enabled":true}))
}

pub fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let provider = str_p(p, "provider").unwrap_or("effectcraft");
    let adapter = s.project_links.adapters.get(provider).ok_or_else(|| bad("linkedProject.inspect", "configure the installed renderer first"))?;
    let path = str_p(p, "path").ok_or_else(|| bad("linkedProject.inspect", "need path"))?;
    adapter.objects(&*s.services, path).map(|objects| json!({"objects":objects})).map_err(EngineError::Other)
}

pub fn import(s: &mut Session, p: &Value) -> Result<Value> {
    if s.project.project_links.len() >= MAX_LINKS {
        return Err(bad("linkedProject.import", "at most 128 linked objects per prototype project"));
    }
    let path = str_p(p, "path").ok_or_else(|| bad("linkedProject.import", "need path"))?.to_string();
    let provider = str_p(p, "provider").unwrap_or("effectcraft").to_string();
    let adapter = s.project_links.adapters.get(&provider).ok_or_else(|| bad("linkedProject.import", "configure the installed renderer first"))?;
    let objects = adapter.objects(&*s.services, &path).map_err(EngineError::Other)?;
    let object = match str_p(p, "object") {
        Some(id) => objects.iter().find(|o| o.id == id),
        None if objects.len() == 1 => objects.first(),
        _ => None,
    }
    .ok_or_else(|| bad("linkedProject.import", "choose an object ID using linkedProject.inspect"))?;
    let src = GeneratorSource::new(filmcraft_media::Generator::TransparentVideo, object.width, object.height, object.rate, object.duration);
    let pool = s.media.clone();
    let link = ProjectLink {
        version: VERSION,
        provider,
        project_path: path,
        object: object.id.clone(),
        object_name: object.name.clone(),
        revision: String::new(),
        cache_generation: 0,
        baked: false,
    };
    let bin = u64_p(p, "bin").map(filmcraft_project::BinId);
    let id = s.edit("Import Linked Project", |pr, _| {
        let id = crate::demo::add_generator(pr, &pool, src, &object.name, Label::Purple, bin);
        pr.project_links.insert(id, link);
        Ok(id)
    })?;
    s.project_links.last_scan = None;
    s.project_links.paused = false;
    poll(s);
    Ok(json!({"items":[id.0],"linked":true}))
}

fn selected(s: &Session, p: &Value) -> Result<ItemId> {
    u64_p(p, "item")
        .map(ItemId)
        .or_else(|| s.state.project_selection.first().copied())
        .filter(|id| s.project.project_links.contains_key(id))
        .ok_or_else(|| bad("linkedProject", "select a linked project item"))
}

fn bake(s: &mut Session, p: &Value) -> Result<Value> {
    let id = selected(s, p)?;
    let restore = p.get("restore").and_then(Value::as_bool).unwrap_or(false);
    let mut destination = None;
    if !restore {
        let link = s.project.project_links.get(&id).ok_or_else(|| bad("linkedProject.bake", "missing link"))?;
        if link.revision.is_empty() || !s.project.item(id).and_then(|i| i.as_media()).is_some_and(|m| matches!(m.media, MediaRef::File { .. })) {
            return Err(bad("linkedProject.bake", "wait for a successful render before baking"));
        }
        let target = str_p(p, "path").ok_or_else(|| bad("linkedProject.bake", "choose a new .mov destination for the baked file"))?;
        if !Path::new(target).extension().is_some_and(|e| e.eq_ignore_ascii_case("mov")) {
            return Err(bad("linkedProject.bake", "destination must end in .mov"));
        }
        let source = s
            .project
            .item(id)
            .and_then(|i| i.as_media())
            .and_then(|m| match &m.media {
                MediaRef::File { path } => Some(path.clone()),
                _ => None,
            })
            .ok_or_else(|| bad("linkedProject.bake", "no rendered source"))?;
        if s.services.file_size(&source).map_err(|e| EngineError::Other(e.to_string()))? > 128 * 1024 * 1024 {
            return Err(bad("linkedProject.bake", "prototype bake is limited to 128 MiB"));
        }
        let bytes = s.services.read_range(&source, 0, 128 * 1024 * 1024 + 1).map_err(|e| EngineError::Other(e.to_string()))?;
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(bad("linkedProject.bake", "cache exceeds bake limit"));
        }
        s.services.write_file_new(target, &bytes).map_err(|e| EngineError::Other(format!("bake: {e}")))?;
        destination = Some(target.to_string());
    }
    s.edit(if restore { "Restore Project Link" } else { "Bake Project Link" }, |pr, _| {
        if let Some(link) = pr.project_links.get_mut(&id) {
            link.baked = !restore;
        }
        if let Some(path) = &destination
            && let Some(m) = pr.item_mut(id).and_then(|i| i.as_media_mut())
        {
            m.media = MediaRef::File { path: path.clone() };
        }
        Ok(())
    })?;
    s.project_links.last_scan = None;
    s.media.remove(id);
    if let Some(path) = destination {
        s.media.allow(&path);
    }
    Ok(json!({"item":id.0,"baked":!restore}))
}

fn relink(s: &mut Session, p: &Value) -> Result<Value> {
    let id = selected(s, p)?;
    let old = s.project.project_links.get(&id).cloned().ok_or_else(|| bad("linkedProject.relink", "missing link"))?;
    let path = str_p(p, "path").ok_or_else(|| bad("linkedProject.relink", "need path"))?;
    let object = str_p(p, "object").unwrap_or(&old.object);
    let adapter = s.project_links.adapters.get(&old.provider).ok_or_else(|| bad("linkedProject.relink", "configure renderer first"))?;
    let found = adapter
        .objects(&*s.services, path)
        .map_err(EngineError::Other)?
        .into_iter()
        .find(|o| o.id == object)
        .ok_or_else(|| bad("linkedProject.relink", "object not found"))?;
    let next = ProjectLink { project_path: path.into(), object: found.id, object_name: found.name, revision: String::new(), ..old };
    s.edit("Relink Project", |pr, _| {
        pr.project_links.insert(id, next);
        Ok(())
    })?;
    s.project_links.last_scan = None;
    Ok(json!({"item":id.0}))
}

/// Host calls this with other background job polling. At most one render runs per session.
pub fn poll(s: &mut Session) {
    if let Some((slot, _)) = &s.project_links.worker {
        let outputs = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(outputs) = outputs {
            s.project_links.worker = None;
            for (id, before, result) in outputs {
                if s.project.project_links.get(&id) != Some(&before) || s.project.item(id).and_then(|i| i.as_media()).is_none_or(|m| m.offline) {
                    continue;
                } // undo/relink/project switch
                match result {
                    Ok((revision, path)) => {
                        let same_file = s.project.item(id).and_then(|i| i.as_media()).is_some_and(|m| matches!(&m.media,MediaRef::File{path:p} if p==&path));
                        let stale_offline = s.media.offline_status(id).is_some() || s.offline.missing.contains(&id);
                        if revision == before.revision && same_file && !stale_offline {
                            s.project_links.status.insert(id, Status { state: "ready".into(), message: "Up to date".into() });
                            continue;
                        }
                        s.media.allow(&path);
                        match s.media.open_file(&path, &*s.services) {
                            Ok(src) => {
                                let pr = Arc::make_mut(&mut s.project);
                                if let Some(m) = pr.item_mut(id).and_then(|i| i.as_media_mut()) {
                                    m.info = src.info().clone();
                                    m.media = MediaRef::File { path: path.clone() };
                                    m.offline = false;
                                }
                                if let Some(link) = pr.project_links.get_mut(&id) {
                                    link.revision = revision;
                                }
                                s.media.insert_file(id, &path, src);
                                s.offline.missing.retain(|item| item != &id);
                                s.bump_view();
                                s.project_links.status.insert(id, Status { state: "ready".into(), message: "Updated from saved project".into() });
                            }
                            Err(e) => {
                                s.project_links.status.insert(id, Status { state: "error".into(), message: e.to_string() });
                            }
                        }
                    }
                    Err(e) => {
                        let changed = s.project_links.status.get(&id).is_none_or(|st| st.message != e);
                        s.project_links.status.insert(id, Status { state: "error".into(), message: e.clone() });
                        if changed {
                            s.error_toast("project link", e);
                        }
                    }
                }
            }
        }
    }
    if s.project_links.worker.is_some()
        || s.project_links.paused
        || cfg!(target_arch = "wasm32")
        || s.project_links.last_scan.is_some_and(|t| t.elapsed() < Duration::from_secs(5))
    {
        return;
    }
    s.project_links.last_scan = Some(web_time::Instant::now());
    let mut tasks = Vec::new();
    for (id, link) in s.project.project_links.iter().take(MAX_LINKS) {
        if link.baked || s.project.item(*id).and_then(|i| i.as_media()).is_none_or(|m| m.offline) {
            continue;
        }
        if let Some(adapter) = s.project_links.adapters.get(&link.provider) {
            tasks.push((*id, link.clone(), adapter.clone()));
        } else {
            s.project_links
                .status
                .insert(*id, Status { state: "disabled".into(), message: "Enable the installed renderer in Linked Projects (native desktop only)".into() });
        }
    }
    if tasks.is_empty() {
        return;
    }
    let slot: Slot = Arc::default();
    let out = slot.clone();
    let progress = Arc::new(Progress::default());
    let prog = progress.clone();
    let services = s.services.clone();
    let spawned = std::thread::Builder::new().name("project-links".into()).spawn(move || {
        let recovery = tasks.iter().map(|(id, l, _)| (*id, l.clone())).collect::<Vec<_>>();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tasks
                .into_iter()
                .map(|(id, link, adapter)| {
                    prog.status.lock().unwrap_or_else(PoisonError::into_inner).clear();
                    let result = render_cached(&*adapter, &*services, &link, &prog);
                    (id, link, result)
                })
                .collect()
        }))
        .unwrap_or_else(|_| recovery.into_iter().map(|(id, l)| (id, l, Err("linked renderer worker failed; last valid cache retained".into()))).collect());
        prog.finished.store(true, Ordering::Relaxed);
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
    });
    if spawned.is_ok() {
        s.project_links.worker = Some((slot, progress));
    } else {
        s.error_toast("project link", "Could not start linked-project worker");
    }
}

fn render_cached(
    adapter: &dyn ProjectAdapter,
    services: &dyn Services,
    link: &ProjectLink,
    progress: &Progress,
) -> std::result::Result<(String, String), String> {
    let revision = adapter.revision(services, link)?;
    let dir = cache_dir();
    // Before trusting a cached file: another local user must not own or redirect the folder.
    private_dir(&dir)?;
    let path = dir.join(format!("{revision}.{}", adapter.extension()));
    if path.is_file() {
        return Ok((revision, path.to_string_lossy().into_owned()));
    }
    if progress.cancel.load(Ordering::Relaxed) {
        return Err("linked render cancelled".into());
    }
    static NEXT_RENDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT_RENDER.fetch_add(1, Ordering::Relaxed);
    let part = dir.join(format!("{revision}-{}-{serial}.part.{}", std::process::id(), adapter.extension()));
    *progress.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Rendering {}", link.object_name);
    let rendered = adapter.render(link, &part, progress).and_then(|()| {
        if adapter.revision(services, link)? != revision {
            return Err("source changed during render; retrying on the next scan".into());
        }
        let size = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
        if size == 0 || size > 1024 * 1024 * 1024 {
            return Err("linked render output is empty or exceeds the 1 GiB prototype limit".into());
        }
        // A successful subprocess exit is insufficient: never publish an unreadable cache.
        let pool = crate::MediaPool::default();
        let source = pool.open_file(&part.to_string_lossy(), services).map_err(|e| format!("invalid linked render: {e}"))?;
        if source.info().video.is_none() {
            return Err("linked renderer did not produce visual media".into());
        }
        drop(source);
        // Never replace the last valid file in place: existing readers retain a complete source.
        if !path.exists() {
            std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        }
        Ok((revision, path.to_string_lossy().into_owned()))
    });
    let _ = std::fs::remove_file(&part);
    rendered
}

/// The per-user linked-render cache: `~/Library/Caches/FilmCraft` on macOS, `$XDG_CACHE_HOME/filmcraft`
/// or `~/.cache/filmcraft` elsewhere on Unix. Windows' temp folder is already per-user.
fn cache_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    let user = if cfg!(windows) || cfg!(target_arch = "wasm32") {
        None
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches/FilmCraft"))
    } else {
        env("XDG_CACHE_HOME").map(|d| d.join("filmcraft")).or_else(|| env("HOME").map(|h| h.join(".cache/filmcraft")))
    };
    user.unwrap_or_else(crate::temp_dir).join("FilmCraftLinkedProjects-v1")
}

/// Create `dir` readable only by this user, or check that an existing one is a real folder this user
/// owns: `chmod` fails on another user's folder, so a pre-created shared-temp folder is refused.
fn private_dir(dir: &Path) -> std::result::Result<(), String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            return Err(format!("linked render cache {} is not a private folder", dir.display()));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("linked render cache {} is not owned by this user: {e}", dir.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(())
}

macro_rules! cmd {
    ($id:literal,$label:literal,[$($menu:literal),*],$shortcut:expr,$params:literal,$enabled:expr,$run:expr) => {
        CommandSpec { id:$id,label:$label,menu:&[$($menu),*],shortcut:$shortcut,params:$params,enabled:$enabled,run:$run,journal:true }
    };
}
macro_rules! query {
    ($id:literal,$label:literal,$params:literal,$run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: always, run: $run, journal: false }
    };
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        query!("linkedProject.configure", "Enable Linked Renderer", r#"{"executable":str}"#, configure),
        query!("linkedProject.inspect", "Inspect Linked Project", r#"{"path":str,"provider":str=effectcraft}"#, inspect),
        cmd!("linkedProject.import", "Import Linked Project", [], None, r#"{"path":str,"provider":str=effectcraft,"object":str?,"bin":id?}"#, always, import),
        cmd!("linkedProject.relink", "Relink Project", [], None, r#"{"item":id,"path":str,"object":str?}"#, always, relink),
        cmd!("linkedProject.bake", "Bake / Restore Project Link", [], None, r#"{"item":id,"path":str?,"restore":bool=false}"#, always, bake),
        cmd!("linkedProject.refresh", "Refresh Linked Projects", [], None, "{}", always, |s, _| {
            s.edit("Refresh Linked Projects", |pr, _| {
                for l in pr.project_links.values_mut().filter(|l| !l.baked) {
                    l.cache_generation = l.cache_generation.checked_add(1).ok_or_else(|| bad("linkedProject.refresh", "cache generation exhausted"))?;
                }
                Ok(())
            })?;
            s.project_links.last_scan = None;
            s.project_links.paused = false;
            poll(s);
            Ok(Value::Null)
        }),
        cmd!("linkedProject.cancel", "Cancel Linked Render", [], None, "{}", always, |s, _| {
            s.project_links.paused = true;
            if let Some((_, p)) = &s.project_links.worker {
                p.cancel.store(true, Ordering::Relaxed);
            }
            Ok(Value::Null)
        }),
        query!("linkedProject.status", "Linked Project Status", "{}", |s, _| {
            poll(s);
            Ok(
                json!({"items":s.project.project_links.iter().map(|(id,l)| json!({"item":id.0,"link":l,"status":s.project_links.status.get(id)})).collect::<Vec<_>>(),"running":s.project_links.worker.is_some(),"progress":s.project_links.worker.as_ref().map(|(_,p)|p.status.lock().unwrap_or_else(PoisonError::into_inner).clone())}),
            )
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    struct Mock {
        revision: AtomicU64,
        fail: AtomicBool,
        renders: AtomicU64,
        revision_threads: Mutex<Vec<std::thread::ThreadId>>,
    }
    impl ProjectAdapter for Mock {
        fn id(&self) -> &'static str {
            "synthetic"
        }
        fn extension(&self) -> &'static str {
            "png"
        }
        fn objects(&self, _: &dyn Services, _: &str) -> std::result::Result<Vec<Object>, String> {
            Ok(vec![Object {
                id: "artboard-a".into(),
                name: "Synthetic artboard".into(),
                width: 8,
                height: 8,
                rate: filmcraft_time::FrameRate { num: 24, den: 1 },
                duration: filmcraft_time::Tick(filmcraft_time::TICKS_PER_SECOND),
            }])
        }
        fn revision(&self, _: &dyn Services, l: &ProjectLink) -> std::result::Result<String, String> {
            self.revision_threads.lock().unwrap().push(std::thread::current().id());
            if self.fail.load(Ordering::Relaxed) {
                return Err("synthetic dependency missing".into());
            }
            let n = self.revision.load(Ordering::Relaxed);
            let mut hash = Sha256::new();
            hash.update(format!("mock/{}/{}/{n}/{}", l.project_path, l.object, l.cache_generation));
            Ok(format!("{:x}", hash.finalize()))
        }
        fn render(&self, _: &ProjectLink, out: &Path, p: &Progress) -> std::result::Result<(), String> {
            if p.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            self.renders.fetch_add(1, Ordering::Relaxed);
            image::RgbaImage::from_pixel(8, 8, image::Rgba([self.revision.load(Ordering::Relaxed) as u8, 180, 80, 128]))
                .save_with_format(out, image::ImageFormat::Png)
                .map_err(|e| e.to_string())
        }
    }

    fn setup() -> (Session, Arc<Mock>, String) {
        let mut s = Session::default();
        let mock =
            Arc::new(Mock { revision: AtomicU64::new(10), fail: AtomicBool::new(false), renders: AtomicU64::new(0), revision_threads: Mutex::default() });
        s.project_links.adapters.insert("synthetic".into(), mock.clone());
        let source = crate::media_test_util::tmp_dir("project-links")
            .join(format!("{}.synthetic", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
            .to_string_lossy()
            .into_owned();
        (s, mock, source)
    }

    fn finish(s: &mut Session) {
        let start = web_time::Instant::now();
        loop {
            poll(s);
            if s.project_links.worker.is_none() {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn item(s: &mut Session, path: &str) -> ItemId {
        let r = s.execute("linkedProject.import", json!({"provider":"synthetic","path":path})).unwrap();
        ItemId(r["items"][0].as_u64().unwrap())
    }

    fn sequence_using(s: &mut Session, id: ItemId) -> ItemId {
        use filmcraft_project::{SequenceSettings, TrackKind};
        let p = Arc::make_mut(&mut s.project);
        let rate = filmcraft_time::FrameRate { num: 24, den: 1 };
        let seq = p.new_sequence("Linked export", SequenceSettings::default(), 1, 0, None);
        let clip = p
            .make_track_item(
                id,
                TrackKind::Video,
                filmcraft_time::Tick::ZERO,
                filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, rate.tick_of(12)),
                rate,
            )
            .unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
        seq
    }

    #[test]
    fn export_preflight_follows_the_same_document_relative_cache_as_export() {
        let (mut s, _, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let seq = sequence_using(&mut s, id);
        let original = match &s.project.item(id).unwrap().as_media().unwrap().media {
            MediaRef::File { path } => path.clone(),
            _ => panic!("rendered cache"),
        };
        let valid = std::fs::read(original).unwrap();
        // No TMPDIR or working-directory mutation: the two locations are unique to this test.
        let relative = format!("target/linked-relative-{}.png", Path::new(&path).file_stem().unwrap().to_string_lossy());
        let document = Path::new(&path).with_extension("fcproj");
        let resolved = document.parent().unwrap().join(&relative);
        std::fs::create_dir_all(Path::new(&relative).parent().unwrap()).unwrap();
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(&relative, &valid).unwrap();
        std::fs::write(&resolved, b"damaged document-relative cache").unwrap();
        Arc::make_mut(&mut s.project).item_mut(id).unwrap().as_media_mut().unwrap().media = MediaRef::File { path: relative.clone() };
        s.media.add_stored_paths([relative.clone()], Some(&document.to_string_lossy()));
        assert_eq!(Path::new(&s.media.follow(&relative).unwrap()), resolved);
        let error = export_preflight(&s)(&s.project, seq).unwrap_err().to_string();
        assert!(error.contains("invalid render cache"), "must reject the export's damaged file despite the valid cwd decoy: {error}");
        std::fs::write(&relative, b"damaged cwd decoy").unwrap();
        std::fs::write(&resolved, valid).unwrap();
        export_preflight(&s)(&s.project, seq).unwrap();
        std::fs::remove_file(&relative).unwrap();
        std::fs::remove_file(resolved).unwrap();
    }

    #[test]
    fn export_preflight_runs_on_the_export_worker_and_fails_before_writing() {
        let (mut s, mock, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let seq = sequence_using(&mut s, id);
        mock.revision_threads.lock().unwrap().clear();
        mock.fail.store(true, Ordering::Relaxed);
        let output = Path::new(&path).with_extension("export.mov");
        let settings =
            filmcraft_export::ExportSettings { path: output.to_string_lossy().into_owned(), format: filmcraft_export::Format::ProRes, ..Default::default() };
        let project = s.project.clone();
        let caller = std::thread::current().id();
        let job = crate::export_tools::spawn_export(&mut s, project, seq, settings, "Preflight regression".into(), false).unwrap();
        let start = web_time::Instant::now();
        loop {
            let result = s.jobs.iter().find(|j| j.id == job).unwrap().result.lock().unwrap().clone();
            if let Some(result) = result {
                assert!(result.unwrap_err().contains("synthetic dependency missing"));
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        let threads = mock.revision_threads.lock().unwrap();
        assert!(!threads.is_empty());
        assert!(threads.iter().all(|thread| *thread != caller), "hashing must not run on the command/UI caller");
        assert!(!output.exists(), "failed preflight must not create an export");
    }

    #[test]
    fn export_preflight_checks_only_reachable_links_and_reopens_the_cache() {
        let (mut s, mock, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let seq = sequence_using(&mut s, id);
        let unrelated = Arc::make_mut(&mut s.project).new_sequence("Unrelated", Default::default(), 1, 0, None);
        mock.fail.store(true, Ordering::Relaxed);
        validate_export(&s, &s.project, unrelated).unwrap();
        assert!(validate_export(&s, &s.project, seq).is_err());
        s.project_links.adapters.clear();
        validate_export(&s, &s.project, unrelated).unwrap();
        s.project_links.adapters.insert("synthetic".into(), mock.clone());
        mock.fail.store(false, Ordering::Relaxed);
        // A nested sequence must still include the linked source in its closure.
        let nested = sequence_using(&mut s, seq);
        validate_export(&s, &s.project, nested).unwrap();
        let cache = match &s.project.item(id).unwrap().as_media().unwrap().media {
            MediaRef::File { path } => path.clone(),
            _ => panic!("rendered file"),
        };
        std::fs::write(&cache, b"truncated cache").unwrap();
        let error = validate_export(&s, &s.project, nested).unwrap_err().to_string();
        assert!(error.contains("invalid render cache"), "{error}");
        validate_export(&s, &s.project, unrelated).unwrap();
    }

    #[test]
    fn unchanged_link_recovers_a_cached_offline_slate_when_its_file_returns() {
        let (mut s, mock, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let MediaRef::File { path } = &s.project.item(id).unwrap().as_media().unwrap().media else { panic!("expected a cache file") };
        let path = path.clone();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        s.media.remove(id);
        let slate = s.media.source_for(&s.project, id, &*s.services).unwrap();
        assert!(s.media.offline_status(id).is_some());
        s.offline.missing.push(id);
        std::fs::write(&path, bytes).unwrap();
        s.project_links.last_scan = None;
        finish(&mut s);
        assert_eq!(mock.renders.load(Ordering::Relaxed), 1, "the returned cache does not need another render");
        assert!(s.media.offline_status(id).is_none(), "an unchanged cache must replace the stale offline source");
        assert!(!s.offline.missing.contains(&id));
        let recovered = s.media.source_for(&s.project, id, &*s.services).unwrap();
        assert!(!Arc::ptr_eq(&slate, &recovered));
        assert_eq!(s.project_links.status.get(&id).unwrap().state, "ready");
    }

    #[test]
    fn cached_checks_are_quiet_but_new_renders_report_progress() {
        let (mut s, mock, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let link = s.project.project_links.get(&id).unwrap().clone();
        let progress = Progress::default();
        render_cached(&*mock, &*s.services, &link, &progress).unwrap();
        assert!(progress.status.lock().unwrap().is_empty());
        assert_eq!(mock.renders.load(Ordering::Relaxed), 1);
        mock.revision.fetch_add(1, Ordering::Relaxed);
        render_cached(&*mock, &*s.services, &link, &progress).unwrap();
        assert_eq!(*progress.status.lock().unwrap(), "Rendering Synthetic artboard");
        assert_eq!(mock.renders.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn render_cache_folder_is_created_privately() {
        let root = crate::media_test_util::tmp_dir("project-links-private");
        let dir = root.join(format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())).join("cache");
        private_dir(&dir).unwrap();
        assert!(dir.is_dir());
        private_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            // A folder left open to other users is tightened before any cached file is trusted.
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            private_dir(&dir).unwrap();
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            // A planted symlink can redirect the cache elsewhere, so it is refused.
            let link = dir.with_file_name("planted");
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            assert!(private_dir(&link).is_err());
        }
    }

    #[test]
    fn generic_adapter_updates_same_item_preserves_edits_and_survives_roundtrip() {
        let (mut s, mock, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let seq = sequence_using(&mut s, id);
        let before = s.project.item(id).unwrap().as_media().unwrap().clone();
        assert!(matches!(before.media, MediaRef::File { .. }));
        let undo = s.history.undo.len();
        mock.revision.store(50, Ordering::Relaxed);
        s.project_links.last_scan = None;
        finish(&mut s);
        let after = s.project.item(id).unwrap().as_media().unwrap();
        assert_ne!(before.media, after.media);
        assert_eq!(before.mark_in, after.mark_in);
        assert_eq!(s.history.undo.len(), undo);
        assert_eq!(mock.renders.load(Ordering::Relaxed), 2);
        let document = filmcraft_format::encode(&s.project, false);
        let loaded = filmcraft_format::decode(&document).unwrap();
        assert_eq!(loaded.project.project_links, s.project.project_links);
        s.project_links.last_scan = None;
        finish(&mut s);
        assert_eq!(mock.renders.load(Ordering::Relaxed), 2, "unchanged inputs reuse cache");
        validate_export(&s, &s.project, seq).unwrap();
    }

    #[test]
    fn failure_retains_last_good_cache_and_blocks_stale_export() {
        let (mut s, mock, path) = setup();
        let id = item(&mut s, &path);
        finish(&mut s);
        let seq = sequence_using(&mut s, id);
        let media = s.project.item(id).unwrap().as_media().unwrap().media.clone();
        mock.fail.store(true, Ordering::Relaxed);
        s.project_links.last_scan = None;
        finish(&mut s);
        assert_eq!(s.project.item(id).unwrap().as_media().unwrap().media, media);
        assert_eq!(s.project_links.status[&id].state, "error");
        assert!(validate_export(&s, &s.project, seq).is_err());
        mock.fail.store(false, Ordering::Relaxed);
        mock.revision.store(99, Ordering::Relaxed);
        assert!(validate_export(&s, &s.project, seq).is_err(), "changed saved project cannot export the old picture");
        s.project_links.last_scan = None;
        finish(&mut s);
        validate_export(&s, &s.project, seq).unwrap();
    }

    #[test]
    fn undo_drops_pending_result_and_explicit_refresh_changes_cache_generation() {
        let (mut s, _, path) = setup();
        let id = item(&mut s, &path);
        s.undo();
        finish(&mut s);
        assert!(s.project.item(id).is_none());
        assert!(!s.project.project_links.contains_key(&id));
        s.redo();
        s.project_links.last_scan = None;
        finish(&mut s);
        s.execute("linkedProject.refresh", json!({})).unwrap();
        finish(&mut s);
        assert_eq!(s.project.project_links[&id].cache_generation, 1);
    }

    #[test]
    fn deleting_an_item_discards_the_link_and_pending_result() {
        let (mut s, _, path) = setup();
        let id = item(&mut s, &path);
        s.execute("project.select", json!({"items":[id.0]})).unwrap();
        s.execute("project.delete", json!({"items":[id.0]})).unwrap();
        finish(&mut s);
        assert!(s.project.item(id).is_none());
        assert!(!s.project.project_links.contains_key(&id));
        s.undo();
        s.project_links.last_scan = None;
        finish(&mut s);
        assert!(s.project.project_links.contains_key(&id));
    }

    #[test]
    fn cancel_pauses_retries_until_explicit_refresh() {
        let (mut s, _, path) = setup();
        item(&mut s, &path);
        s.execute("linkedProject.cancel", json!({})).unwrap();
        finish(&mut s);
        assert!(s.project_links.paused);
        s.project_links.last_scan = None;
        poll(&mut s);
        assert!(s.project_links.worker.is_none());
        s.execute("linkedProject.refresh", json!({})).unwrap();
        finish(&mut s);
        assert!(!s.project_links.paused);
    }

    #[test]
    fn unconfigured_providers_and_hostile_parameters_fail_without_panicking() {
        let mut s = Session::default();
        for (cmd, p) in [
            ("linkedProject.import", json!({"path":"untrusted.ecproj"})),
            ("linkedProject.inspect", json!({"path":123})),
            ("linkedProject.configure", json!({"executable":"missing-cli"})),
            ("linkedProject.relink", json!({"item":u64::MAX})),
            ("linkedProject.bake", json!({"item":-1})),
        ] {
            assert!(s.execute(cmd, p).is_err());
        }
        assert!(s.project_links.adapters.is_empty());
    }

    #[test]
    fn effectcraft_metadata_rejects_invalid_schema_rate_dimensions_and_missing_object() {
        let root = crate::media_test_util::tmp_dir("ec-link-metadata");
        let path = root.join("test.ecproj");
        let path = path.to_string_lossy();
        let adapter = EffectCraft { executable: std::env::current_exe().unwrap() };
        let mut doc = json!({"schema":2,"items":{"1":{"name":"A","kind":{"type":"Comp","width":160,"height":90,"frame_rate":{"num":24,"den":1},"duration":254016000000_i64}}}});
        std::fs::write(&*path, doc.to_string()).unwrap();
        assert_eq!(adapter.objects(&crate::FsServices, &path).unwrap()[0].id, "1");
        for (pointer, value) in [
            ("/schema", json!(999)),
            ("/items/1/kind/frame_rate/den", json!(0)),
            ("/items/1/kind/frame_rate/num", json!(-1)),
            ("/items/1/kind/width", json!(u64::MAX)),
            ("/items/1/kind/duration", json!(-1)),
        ] {
            let old = doc.pointer(pointer).unwrap().clone();
            *doc.pointer_mut(pointer).unwrap() = value;
            std::fs::write(&*path, doc.to_string()).unwrap();
            assert!(adapter.objects(&crate::FsServices, &path).is_err());
            *doc.pointer_mut(pointer).unwrap() = old;
        }
    }

    /// Real renderer integration; fixture is generated from code, never private film material.
    #[test]
    #[ignore = "requires FILMCRAFT_EFFECTCRAFT_CLI pointing at an installed EffectCraft CLI"]
    fn native_effectcraft_saved_edit_updates_timeline() {
        use filmcraft_project::{SequenceSettings, TrackKind};
        use filmcraft_time::TimeRange;
        let cli = std::env::var("FILMCRAFT_EFFECTCRAFT_CLI").expect("set FILMCRAFT_EFFECTCRAFT_CLI");
        let root = crate::media_test_util::tmp_dir("native-effectcraft-link");
        let script = root.join("demo.jsx");
        let project = root.join("demo.ecproj");
        std::fs::write(&script, "var c=app.project.items.addComp('Linked demo',160,90,1,1,24); c.layers.addSolid([1,0,0],'Colour',80,90,1,1);").unwrap();
        let made = std::process::Command::new(&cli).arg("script").arg(&script).args(["--empty", "--save-as"]).arg(&project).arg("--json").output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let mut s = Session::default();
        s.execute("linkedProject.configure", json!({"executable":cli})).unwrap();
        let r = s.execute("linkedProject.import", json!({"path":project})).unwrap();
        let id = ItemId(r["items"][0].as_u64().unwrap());
        finish(&mut s);
        assert_eq!(s.project_links.status[&id].state, "ready");
        assert!(s.project.item(id).unwrap().as_media().unwrap().info.video.as_ref().unwrap().has_alpha);
        let mut pr = (*s.project).clone();
        let rate = filmcraft_time::FrameRate { num: 24, den: 1 };
        let seq = pr.new_sequence("Linked test", SequenceSettings { width: 160, height: 90, frame_rate: rate, ..Default::default() }, 1, 0, None);
        let mut clip = pr.make_track_item(id, TrackKind::Video, rate.tick_of(12), TimeRange::new(rate.tick_of(3), rate.tick_of(12)), rate).unwrap();
        for effect in &mut clip.effects {
            filmcraft_project::resolve_auto_points(effect, (160, 90), (160, 90));
        }
        pr.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip.clone());
        s.project = Arc::new(pr);
        s.state.active_sequence = Some(seq);
        let before = crate::media_test_util::frame_rgba(&mut s, 14, 1.0).2;
        let mut doc: Value = serde_json::from_slice(&std::fs::read(&project).unwrap()).unwrap();
        let items = doc["items"].as_object_mut().unwrap();
        let solid = items.values_mut().find(|v| v["kind"]["type"] == "Solid").unwrap();
        solid["kind"]["color"] = json!([0, 0, 1]);
        std::fs::write(&project, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(validate_export(&s, &s.project, seq).is_err());
        s.project_links.last_scan = None;
        finish(&mut s);
        assert_eq!(s.project.sequence(seq).unwrap().video_tracks[0].items[0], clip, "placement, trim and effects must survive refresh");
        let after = crate::media_test_util::frame_rgba(&mut s, 14, 1.0).2;
        assert_ne!(before, after, "saved colour change must update timeline");
        s.execute(
            "file.exportMedia",
            json!({"path":root.join("filmcraft-export.mov"),"format":"prores","proresProfile":"4444","alpha":true,"audio":false,"wait":true}),
        )
        .unwrap();
        let direct = root.join("direct.mov");
        let rendered = std::process::Command::new(&cli)
            .args(["render", "--project"])
            .arg(&project)
            .args(["--comp", "1", "--out"])
            .arg(&direct)
            .args(["--format", "prores", "--prores", "4444", "--channels", "rgba", "--audio", "off"])
            .output()
            .unwrap();
        assert!(rendered.status.success(), "{}", String::from_utf8_lossy(&rendered.stderr));
        let out = s.execute("file.import", json!({"paths":[direct]})).unwrap();
        let direct_id = ItemId(out["items"][0].as_u64().unwrap());
        let mut pr = (*s.project).clone();
        pr.sequence_mut(seq).unwrap().video_tracks[0].items[0].item = direct_id;
        s.project = Arc::new(pr);
        assert_eq!(after, crate::media_test_util::frame_rgba(&mut s, 14, 1.0).2, "linked and direct ProRes frames must match");
        s.execute("linkedProject.bake", json!({"item":id,"path":root.join("baked.mov")})).unwrap();
        assert!(s.project.project_links[&id].baked);
        assert!(s.execute("linkedProject.bake", json!({"item":id,"path":root.join("baked.mov")})).is_err(), "bake must never overwrite an existing file");
        s.execute("linkedProject.bake", json!({"item":id,"restore":true})).unwrap();
        finish(&mut s);
        assert!(!s.project.project_links[&id].baked);
        let reopened = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        assert_eq!(reopened.project.project_links, s.project.project_links);
    }
}

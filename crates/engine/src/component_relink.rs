//! Relink managed OP-Atom components by their retained full file-package identity.
//! Search is read-only, bounded and cancellable. Applying its results reopens each
//! candidate against the current binding, then changes paths in one undo step.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use crate::{EngineError, Result, Services, Session, component_audio, component_media};
use filmcraft_media::reader::SharedReader;
use filmcraft_project::{ItemId, MediaClip, MediaRef, Project};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_TARGETS: usize = 4096;
const MAX_ENTRIES: usize = 20000;
const MAX_DIRS: usize = 1024;
const MAX_COPIES: usize = 16;
const MAX_CANDIDATES: usize = 8192;

fn bad(message: impl Into<String>) -> EngineError {
    EngineError::Other(format!("Component relink: {}", message.into()))
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Audio keys refer to logical channel slots, never the order of files found on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ComponentKey {
    Audio { channel: usize },
    Picture,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub item: ItemId,
    pub component: ComponentKey,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    item: ItemId,
    component: ComponentKey,
    path: String,
}

fn media(project: &Project, target: &Target) -> Result<MediaClip> {
    project.item(target.item).and_then(|i| i.as_media()).cloned().ok_or_else(|| bad("item is not media"))
}
fn identity(m: &MediaClip, key: ComponentKey) -> Result<[u8; 32]> {
    let MediaRef::Components { channels, picture } = &m.media else { return Err(bad("item is not component media")) };
    let id = match key {
        ComponentKey::Audio { channel } => channels.get(channel).map(|c| c.file_package),
        ComponentKey::Picture => picture.as_ref().map(|c| c.file_package),
    }
    .ok_or_else(|| bad("component is missing"))?;
    if id == [0; 32] {
        return Err(bad("component lacks a full package identity"));
    }
    Ok(id)
}
/// Validate using the same decoder and range checks as playback, retaining all edit metadata.
fn replace(m: &mut MediaClip, key: ComponentKey, path: &str, services: &dyn Services) -> Result<()> {
    if path.len() > 4096 || path.contains('\0') || !Path::new(path).is_absolute() {
        return Err(bad("need a bounded absolute file path"));
    }
    identity(m, key)?;
    let MediaRef::Components { channels, picture } = &mut m.media else { return Err(bad("item is not component media")) };
    match key {
        ComponentKey::Audio { channel } => {
            let c = channels.get_mut(channel).ok_or_else(|| bad("channel is missing"))?;
            c.path = Some(path.into());
            let source = component_audio::open_component(c, services)?;
            if source.info().audio_streams.get(c.stream).map(|a| a.sample_rate) != m.info.audio().map(|a| a.sample_rate) {
                return Err(bad("replacement audio sample rate differs from saved binding"));
            }
        }
        ComponentKey::Picture => {
            let c = picture.as_mut().ok_or_else(|| bad("picture is missing"))?;
            c.path = Some(path.into());
            let source = component_media::open_picture(c, services)?;
            if c.format_pending {
                let expected = m.info.video.as_ref().ok_or_else(|| bad("pending picture has no geometry"))?;
                let actual = source.info().video.as_ref().ok_or_else(|| bad("replacement has no picture"))?;
                if expected.width != actual.width
                    || expected.height != actual.height
                    || expected.frame_rate != actual.frame_rate
                    || expected.par != actual.par
                    || expected.has_alpha != actual.has_alpha
                    || expected.pixel_format != actual.pixel_format
                {
                    return Err(bad("replacement picture disagrees with offline geometry or clock"));
                }
                m.info.video = Some(actual.clone());
                c.format_pending = false;
            } else if source.info().video != m.info.video {
                return Err(bad("replacement picture format differs from saved binding"));
            }
        }
    }
    m.offline = false;
    Ok(())
}
fn apply(s: &mut Session, bindings: Vec<Binding>) -> Result<Value> {
    if bindings.is_empty() || bindings.len() > MAX_TARGETS {
        return Err(bad("provide 1–4096 component bindings"));
    }
    let mut seen = BTreeSet::new();
    let mut updates = BTreeMap::new();
    for b in &bindings {
        let target = Target { item: b.item, component: b.component };
        if !seen.insert(target.clone()) {
            return Err(bad("duplicate component target"));
        }
        if let std::collections::btree_map::Entry::Vacant(e) = updates.entry(b.item) {
            e.insert(media(&s.project, &target)?);
        }
        let m = updates.get_mut(&b.item).ok_or_else(|| bad("missing prepared update"))?;
        replace(m, b.component, &b.path, &*s.services)?;
    }
    let ids: Vec<_> = updates.keys().copied().collect();
    s.edit("Relink Media Components", move |p, _| {
        for (id, m) in updates {
            *p.item_mut(id).and_then(|i| i.as_media_mut()).ok_or_else(|| bad("item disappeared"))? = m;
        }
        Ok(())
    })?;
    // These exact files were explicitly chosen (or accepted from verified search results).
    // A saved path with the same spelling may previously have been refused by doc_path.
    for binding in &bindings {
        s.media.allow(&binding.path);
    }
    for id in ids {
        s.media.remove(id);
    }
    crate::relink::refresh(s);
    Ok(json!({"relinked": bindings}))
}
pub(crate) fn relink(s: &mut Session, p: &Value) -> Result<Value> {
    let array = p["bindings"].as_array().filter(|a| !a.is_empty() && a.len() <= MAX_TARGETS).ok_or_else(|| bad("provide 1–4096 component bindings"))?;
    apply(s, serde_json::from_value(Value::Array(array.clone())).map_err(|e| bad(e.to_string()))?)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    path: String,
    ok: bool,
    identity_match: bool,
    problems: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
struct Row {
    #[serde(flatten)]
    target: Target,
    status: &'static str,
    candidates: Vec<Candidate>,
}
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    results: Vec<Row>,
    complete: bool,
    entries: usize,
    diagnostics: Vec<String>,
    omitted_diagnostics: usize,
}
impl SearchResult {
    fn problem(&mut self, message: String) {
        self.complete = false;
        // Keep blocking problems visible even after ordinary notices fill the report.
        if self.diagnostics.len() >= 128 {
            self.diagnostics.pop();
            self.omitted_diagnostics += 1;
        }
        self.diagnostics.insert(0, message);
    }
    fn notice(&mut self, message: String) {
        if self.diagnostics.len() < 128 {
            self.diagnostics.push(message);
        } else {
            self.omitted_diagnostics += 1;
        }
    }
}
// These two native-demux diagnostics are expected in the validated Avid OP-Atom
// samples. Neither loses the local essence package identity. Keep their text;
// sync loss, truncation, missing local essence and all other warnings still block
// automatic matching. Parse the exact generated forms, not arbitrary prefixes.
fn known_atom_notice(s: &str) -> bool {
    if let Some(track) = s.strip_prefix("material track ").and_then(|s| s.strip_suffix(" refers to a package not in this file")) {
        return track.parse::<u32>().is_ok();
    }
    if let Some(packet) = s.strip_prefix("skipped opaque non-SMPTE header packet at byte ").and_then(|s| s.strip_suffix(" bytes)"))
        && let Some((offset, size)) = packet.split_once(" (")
    {
        return offset.parse::<u64>().is_ok() && size.parse::<u64>().is_ok();
    }
    false
}

/// Only the latest search is retained. Its project snapshot prevents stale UI results being applied.
pub struct SearchJob {
    pub job: u64,
    project: Arc<Project>,
    result: Arc<Mutex<Option<SearchResult>>>,
}

/// Cap actual metadata reads and check cancellation between parser reads, not just between files.
struct Source {
    reader: SharedReader,
    progress: Arc<filmcraft_export::Progress>,
    bytes: AtomicU64,
    calls: AtomicU64,
}
impl filmcraft_mxf::ByteSource for Source {
    fn len(&self) -> u64 {
        self.reader.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if self.progress.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("search cancelled"));
        }
        if self.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed).saturating_add(buf.len() as u64) > 64 * 1024 * 1024
            || self.calls.fetch_add(1, Ordering::Relaxed) >= 100000
        {
            return Err(std::io::Error::other("MXF search metadata budget exceeded"));
        }
        self.reader.read_at(offset, buf)
    }
}
/// Search evidence shared by native import preparation and component relinking.
/// Paths are usable automatically only after a complete scan with one copy per identity.
pub(crate) struct IdentityIndex {
    paths: BTreeMap<[u8; 32], Vec<String>>,
    report: SearchResult,
}
impl IdentityIndex {
    pub(crate) fn unique_path(&self, id: [u8; 32]) -> Option<&str> {
        if !self.report.complete {
            return None;
        }
        let copies = self.paths.get(&id)?;
        if copies.len() == 1 { copies.first().map(String::as_str) } else { None }
    }
}
pub(crate) fn index_identities(
    identities: BTreeSet<[u8; 32]>,
    folders: Vec<String>,
    services: &dyn Services,
    progress: Arc<filmcraft_export::Progress>,
) -> Result<IdentityIndex> {
    if identities.len() > MAX_TARGETS || folders.is_empty() || folders.len() > 32 {
        return Err(bad("provide at most 4096 identities and 1–32 media folders"));
    }
    let mut out = SearchResult { complete: true, ..Default::default() };
    let mut index: BTreeMap<[u8; 32], Vec<String>> = identities.into_iter().map(|id| (id, Vec::new())).collect();
    // One identity index and one set of limits across all roots. Canonical directory keys
    // prevent parent/child, repeated and aliased selections from producing duplicate copies.
    let mut dirs = VecDeque::new();
    let mut seen_dirs = BTreeSet::new();
    for folder in folders {
        match std::fs::canonicalize(&folder) {
            Ok(key) => {
                if seen_dirs.insert(key) {
                    dirs.push_back((std::path::PathBuf::from(folder), 0usize));
                }
            }
            Err(e) => out.problem(format!("{folder}: {e}")),
        }
    }
    let mut directory_count = dirs.len();
    'walk: while let Some((dir, depth)) = dirs.pop_front() {
        if progress.cancel.load(Ordering::Relaxed) {
            out.problem("Search cancelled; no automatic matches will be applied".into());
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(v) => v,
            Err(e) => {
                out.problem(format!("{}: {e}", dir.display()));
                continue;
            }
        };
        for entry in entries {
            if progress.cancel.load(Ordering::Relaxed) {
                out.problem("Search cancelled; no automatic matches will be applied".into());
                break 'walk;
            }
            if out.entries >= MAX_ENTRIES {
                out.problem("Search entry limit reached; choose a smaller folder".into());
                break 'walk;
            }
            out.entries += 1;
            progress.done.store(out.entries as u64, Ordering::Relaxed);
            let e = match entry {
                Ok(v) => v,
                Err(e) => {
                    out.problem(e.to_string());
                    continue;
                }
            };
            let ty = match e.file_type() {
                Ok(v) => v,
                Err(e) => {
                    out.problem(e.to_string());
                    continue;
                }
            };
            if ty.is_symlink() {
                out.problem(format!("Skipped symbolic link: {}", e.path().display()));
                continue;
            }
            if ty.is_dir() {
                let key = match std::fs::canonicalize(e.path()) {
                    Ok(key) => key,
                    Err(error) => {
                        out.problem(format!("{}: {error}", e.path().display()));
                        continue;
                    }
                };
                if seen_dirs.contains(&key) {
                    continue;
                }
                if depth >= 32 || directory_count >= MAX_DIRS {
                    out.problem("Search directory limit reached; choose a smaller folder".into());
                    continue;
                }
                seen_dirs.insert(key);
                directory_count += 1;
                dirs.push_back((e.path(), depth + 1));
                continue;
            }
            if !ty.is_file() || !e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("mxf")) {
                continue;
            }
            let path = match e.path().to_str().filter(|s| s.len() <= 4096) {
                Some(v) => v.to_owned(),
                None => {
                    out.problem("Skipped unrepresentable media path".into());
                    continue;
                }
            };
            let reader = match services.reader(&path) {
                Some(Ok(r)) => r,
                Some(Err(e)) => {
                    out.problem(format!("{path}: {e}"));
                    continue;
                }
                None => {
                    out.problem(format!("{path}: random-access services unavailable"));
                    continue;
                }
            };
            let source = Source { reader, progress: progress.clone(), bytes: AtomicU64::new(0), calls: AtomicU64::new(0) };
            let mxf = match filmcraft_mxf::open(&source) {
                Ok(v) => v,
                Err(e) => {
                    out.problem(format!("{path}: {e}"));
                    continue;
                }
            };
            for warning in &mxf.warnings {
                if mxf.operational_pattern == filmcraft_mxf::OperationalPattern::Atom && mxf.tracks.len() == 1 && known_atom_notice(warning) {
                    out.notice(format!("{path}: {warning}"));
                } else {
                    out.problem(format!("{path}: {warning}"));
                }
            }
            let ids: BTreeSet<_> = mxf.tracks.iter().map(|t| t.file_package_uid).collect();
            for id in ids {
                if let Some(copies) = index.get_mut(&id) {
                    if copies.len() >= MAX_COPIES {
                        out.problem(format!("Too many copies of one media identity: {path}"));
                    } else {
                        copies.push(path.clone());
                    }
                }
            }
        }
    }
    if progress.cancel.load(Ordering::Relaxed) {
        return Err(bad("search cancelled"));
    }
    for paths in index.values_mut() {
        paths.sort();
    }
    Ok(IdentityIndex { paths: index, report: out })
}
fn validate_index(
    project: &Project,
    targets: Vec<Target>,
    index: &IdentityIndex,
    services: &dyn Services,
    progress: Arc<filmcraft_export::Progress>,
) -> SearchResult {
    let mut out = index.report.clone();
    let mut candidate_count = 0usize;
    for target in targets {
        let mut row = Row { target: target.clone(), status: "absent", candidates: Vec::new() };
        if let Ok(original) = media(project, &target)
            && let Ok(id) = identity(&original, target.component)
            && let Some(copies) = index.paths.get(&id)
        {
            for path in copies.iter() {
                if candidate_count >= MAX_CANDIDATES {
                    out.problem("Search candidate limit reached; search fewer components".into());
                    break;
                }
                candidate_count += 1;
                if progress.cancel.load(Ordering::Relaxed) {
                    out.problem("Search cancelled during validation".into());
                    break;
                }
                let result = replace(&mut original.clone(), target.component, path, services);
                row.candidates.push(Candidate {
                    path: path.clone(),
                    ok: result.is_ok(),
                    identity_match: true,
                    problems: result.err().map(|e| vec![e.to_string()]).unwrap_or_default(),
                });
            }
            row.status = match row.candidates.as_slice() {
                [] => "absent",
                [c] if c.ok => "unique",
                [_] => "unreadable",
                _ => "ambiguous",
            };
        }
        out.results.push(row);
    }
    if !out.complete {
        for row in &mut out.results {
            if row.status == "absent" {
                row.status = "unresolved";
            }
        }
    }
    out
}
fn scan(project: &Project, targets: Vec<Target>, folders: Vec<String>, services: &dyn Services, progress: Arc<filmcraft_export::Progress>) -> SearchResult {
    let identities = targets.iter().filter_map(|t| media(project, t).and_then(|m| identity(&m, t.component)).ok()).collect();
    match index_identities(identities, folders, services, progress.clone()) {
        Ok(index) => validate_index(project, targets, &index, services, progress),
        Err(e) => {
            let mut report = SearchResult::default();
            report.problem(e.to_string());
            report
        }
    }
}
/// Resolve only newly imported components on a disposable project. A complete search and
/// exactly one validated copy are required; ambiguous or incomplete results stay offline.
#[cfg(test)]
pub(crate) fn link_prepared(
    project: &mut Project,
    items: &[ItemId],
    folders: Vec<String>,
    services: &dyn Services,
    progress: Arc<filmcraft_export::Progress>,
) -> Result<Value> {
    link_prepared_inner(project, items, Some(folders), None, services, progress)
}
pub(crate) fn link_prepared_index(
    project: &mut Project,
    items: &[ItemId],
    index: &IdentityIndex,
    services: &dyn Services,
    progress: Arc<filmcraft_export::Progress>,
) -> Result<Value> {
    // Early binding may have used these paths to corroborate timing. Only publish paths
    // that also pass this final search validation; incomplete results remain offline.
    for id in items {
        let media = project.item_mut(*id).and_then(|i| i.as_media_mut()).ok_or_else(|| bad("new import item is not media"))?;
        if let MediaRef::Components { channels, picture } = &mut media.media {
            for channel in channels {
                channel.path = None;
            }
            if let Some(picture) = picture {
                picture.path = None;
            }
        }
    }
    link_prepared_inner(project, items, None, Some(index), services, progress)
}
fn link_prepared_inner(
    project: &mut Project,
    items: &[ItemId],
    folders: Option<Vec<String>>,
    index: Option<&IdentityIndex>,
    services: &dyn Services,
    progress: Arc<filmcraft_export::Progress>,
) -> Result<Value> {
    let mut targets = Vec::new();
    for id in items {
        let item = project.item(*id).and_then(|i| i.as_media()).ok_or_else(|| bad("new import item is not media"))?;
        if let MediaRef::Components { channels, picture } = &item.media {
            for channel in 0..channels.len() {
                targets.push(Target { item: *id, component: ComponentKey::Audio { channel } });
            }
            if picture.is_some() {
                targets.push(Target { item: *id, component: ComponentKey::Picture });
            }
        }
        if targets.len() > MAX_TARGETS {
            return Err(bad("import exceeds 4096 media components; import without folder search and relink in smaller groups"));
        }
    }
    let report = if let Some(index) = index {
        validate_index(project, targets, index, services, progress.clone())
    } else {
        scan(project, targets, folders.ok_or_else(|| bad("missing media folders"))?, services, progress.clone())
    };
    if progress.cancel.load(Ordering::Relaxed) {
        return Err(bad("import cancelled"));
    }
    let mut linked = 0usize;
    if report.complete {
        for row in &report.results {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err(bad("import cancelled"));
            }
            if row.status == "unique"
                && let Some(candidate) = row.candidates.first()
            {
                let item = project.item_mut(row.target.item).and_then(|i| i.as_media_mut()).ok_or_else(|| bad("imported media disappeared"))?;
                replace(item, row.target.component, &candidate.path, services)?;
                linked += 1;
            }
        }
    }
    Ok(json!({"linkedComponents":linked,"offlineComponents":report.results.len().saturating_sub(linked),"search":report}))
}
pub(crate) fn search(s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(bad("folder identity search requires the desktop file system"));
    }
    let folder = p["folder"]
        .as_str()
        .filter(|p| p.len() <= 4096 && !p.contains('\0') && Path::new(p).is_absolute() && Path::new(p).is_dir())
        .ok_or_else(|| bad("choose an absolute existing folder"))?
        .to_owned();
    let targets: Vec<Target> = match p.get("targets") {
        Some(v) => {
            let a = v.as_array().filter(|a| !a.is_empty() && a.len() <= MAX_TARGETS).ok_or_else(|| bad("provide 1–4096 component targets"))?;
            serde_json::from_value(Value::Array(a.clone())).map_err(|e| bad(e.to_string()))?
        }
        None => crate::relink::scan(&s.project, &s.media, &*s.services)
            .into_iter()
            .filter_map(|m| m.component.map(|component| Target { item: ItemId(m.item), component }))
            .collect(),
    };
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err(bad("provide 1–4096 component targets"));
    }
    let mut seen = BTreeSet::new();
    for t in &targets {
        if !seen.insert(t.clone()) {
            return Err(bad("duplicate component target"));
        }
        identity(&media(&s.project, t)?, t.component)?;
    }
    if s.component_search.as_ref().and_then(|old| s.jobs.iter().find(|j| j.id == old.job)).is_some_and(|j| !j.progress.finished.load(Ordering::Relaxed)) {
        return Err(bad("a component search is still running; cancel or wait for it"));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| bad("job ID overflow"))?;
    let job = crate::Job { id, label: "Find MXF components".into(), progress: Default::default(), result: Default::default() };
    let results = Arc::new(Mutex::new(None));
    let (out, progress, result) = (results.clone(), job.progress.clone(), job.result.clone());
    let project = s.project.clone();
    let services = s.services.clone();
    let run = move || {
        let t0 = web_time::Instant::now();
        let found = scan(&project, targets, vec![folder], &*services, progress.clone());
        *lock(&progress.status) = format!("Searched {} entries; {}", found.entries, if found.complete { "complete" } else { "incomplete: review diagnostics" });
        *lock(&result) = Some(Ok(filmcraft_export::Report {
            path: String::new(),
            frames: found.entries as u64,
            seconds: t0.elapsed().as_secs_f64(),
            bytes: 0,
            render_fps: 0.0,
            extra_files: Vec::new(),
        }));
        *lock(&out) = Some(found);
        progress.finished.store(true, Ordering::Release);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    if p["wait"] == true {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-component-search".into()).spawn(run).map_err(|e| bad(e.to_string()))?;
    }
    s.component_search = Some(SearchJob { job: id, project: s.project.clone(), result: results });
    s.jobs.push(job);
    Ok(json!({"job":id}))
}
pub(crate) fn search_result(s: &mut Session, p: &Value) -> Result<Value> {
    let pending = search_job(s, p)?;
    let j = s.jobs.iter().find(|j| j.id == pending.job).ok_or_else(|| bad("search job disappeared"))?;
    if let Some(Err(e)) = lock(&j.result).as_ref() {
        return Err(bad(e.clone()));
    }
    Ok(
        json!({"job": pending.job, "finished":j.progress.finished.load(Ordering::Acquire), "entries":j.progress.done.load(Ordering::Relaxed), "report":*lock(&pending.result)}),
    )
}
fn search_job<'a>(s: &'a Session, p: &Value) -> Result<&'a SearchJob> {
    let pending = s.component_search.as_ref().filter(|j| Some(j.job) == p["job"].as_u64()).ok_or_else(|| bad("search result expired; search again"))?;
    if !Arc::ptr_eq(&pending.project, &s.project) {
        return Err(bad("project changed since search; search again"));
    }
    Ok(pending)
}
/// An explicit choice resolves duplicate copies; all other targets require a unique complete search.
pub(crate) fn apply_search(s: &mut Session, p: &Value) -> Result<Value> {
    let pending = search_job(s, p)?;
    let report = lock(&pending.result).clone().ok_or_else(|| bad("search is still running"))?;
    let choice: Option<Binding> = p.get("choice").map(|v| serde_json::from_value(v.clone()).map_err(|e| bad(e.to_string()))).transpose()?;
    let mut bindings = Vec::new();
    if let Some(b) = &choice {
        if !report.results.iter().any(|r| r.target.item == b.item && r.target.component == b.component && r.candidates.iter().any(|c| c.path == b.path && c.ok))
        {
            return Err(bad("choice is not a verified search candidate"));
        }
        bindings.push(b.clone());
    }
    if p["relinkOthers"] == true || choice.is_none() {
        if !report.complete {
            return Err(bad("search was incomplete; locate or choose each verified file explicitly"));
        }
        for r in &report.results {
            if r.status == "unique"
                && !bindings.iter().any(|b| b.item == r.target.item && b.component == r.target.component)
                && let Some(c) = r.candidates.first()
            {
                bindings.push(Binding { item: r.target.item, component: r.target.component, path: c.path.clone() });
            }
        }
    }
    apply(s, bindings)
}

#[cfg(test)]
mod tests;

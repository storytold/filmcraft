//! ComfyUI clips (`comfyui.*`): clips whose media is made by a ComfyUI workflow (docs/comfyui.md).
//!
//! A ComfyUI clip is a media item with a [`Recipe`] in `Project::generated`: the workflow (API
//! format, any workflow), the input overrides and the server. It starts as a placeholder (a Color
//! Matte) on the timeline; generating runs the workflow and links the item to what it made, so
//! every clip of the item shows the result. Generating again makes a new version of the file and
//! relinks the item to it.
//!
//! | command | does |
//! |---|---|
//! | `comfyui.settings` | get / set the server, output folder and timeout (preferences); `check` tests the connection |
//! | `comfyui.inspect` | a workflow's nodes and editable inputs (`workflow` / `path`), or a ComfyUI clip's recipe and last run |
//! | `comfyui.newClip` | a new ComfyUI clip from a workflow, placed at the playhead (optionally generated at once) |
//! | `comfyui.setInputs` | change a clip's input overrides, workflow, outputs or server (undoable) |
//! | `comfyui.generate` | run the clips' workflows in a background job and link the results (one undo step per clip) |
//!
//! What a run brings back:
//!
//! - **video / image / audio**: the first video output (else image, else audio) becomes the
//!   item's media. Its clips are fitted to it: shortened to the media's length, moved to an audio
//!   track when the result is sound only, and given a linked audio clip when a video has sound.
//!   A separate audio output of a picture workflow (a video model plus a sound model) is imported
//!   and laid under the picture, linked. Further outputs are imported into the project.
//! - **text**: kept with the clip (`comfyui.inspect` ▸ `lastRun.texts`); when the run made no
//!   media, the clip becomes transparent and the text is added as a title above each of its clips.
//!
//! Clips are generated in timeline order, one after the other, in one job; `jobs.list` shows the
//! progress and `jobs.cancel` stops it (the running prompt is taken off the server's queue).
//!
//! The network goes through [`ComfyState::transport`] when a host or test installed one (the
//! [`filmcraft_comfyui::fake::FakeComfy`] stand-in), else over HTTP (engine feature `comfyui`).

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_comfyui::{self as comfy, Binding, Client, ComfyError, OutputKind, Recipe, RunOptions, Transport, Workflow};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaInfo, MediaKind};
use filmcraft_project::{ClipId, Generation, ItemId, Label, MediaRef, Project, Track, TrackId, TrackItem, TrackKind};
use filmcraft_time::{Tick, TimeRange};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

/// `Generation::provider` of ComfyUI clips.
pub const PROVIDER: &str = "comfyui";

/// The colour of a clip that has not been generated yet.
pub const PLACEHOLDER: [f32; 4] = [0.18, 0.13, 0.28, 1.0];

/// Why ComfyUI can't run in this build.
pub(crate) const NO_COMFY: &str = "ComfyUI is not available in this build (built without the `comfyui` feature)";

/// ComfyUI settings (persisted in the preferences as `comfyui`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ComfyPrefs {
    /// Server for clips that don't name one.
    pub server: String,
    /// Where generated files are written ("" = a `ComfyUI` folder next to the project, else in
    /// the data directory).
    pub output_dir: String,
    /// Give up on a run after this many minutes.
    pub timeout_minutes: u32,
}

impl Default for ComfyPrefs {
    fn default() -> Self {
        Self { server: comfy::DEFAULT_SERVER.into(), output_dir: String::new(), timeout_minutes: 60 }
    }
}

/// ComfyUI state of a session.
#[derive(Default)]
pub struct ComfyState {
    /// Every server is reached through this when set (tests, hosts with their own networking).
    pub transport: Option<Arc<dyn Transport>>,
    pending: Vec<Pending>,
}

impl ComfyState {
    /// Whether `item` is being generated.
    pub fn generating(&self, item: ItemId) -> bool {
        self.pending.iter().any(|p| p.items.contains(&item))
    }
}

/// A file a run wrote: (output, path, bytes).
type Written = (comfy::Output, String, Arc<[u8]>);

/// What one clip's run produced.
struct Done {
    item: ItemId,
    recipe: Recipe,
    result: std::result::Result<(String, Vec<comfy::Output>, Vec<Written>), String>,
}

/// A running generate job; applied by [`poll`] when it finishes.
struct Pending {
    job: u64,
    items: Vec<ItemId>,
    results: Arc<Mutex<Vec<Done>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn comfy_err(cmd: &str, e: ComfyError) -> EngineError {
    match e {
        ComfyError::Workflow(m) => bad(cmd, m),
        e => EngineError::Other(e.to_string()),
    }
}

// ------------------------------------------------------------------------------------- transport

#[cfg(feature = "comfyui")]
fn http_transport(server: &str) -> std::result::Result<Arc<dyn Transport>, ComfyError> {
    Ok(Arc::new(comfy::http::HttpTransport::new(server)?))
}

#[cfg(not(feature = "comfyui"))]
fn http_transport(_server: &str) -> std::result::Result<Arc<dyn Transport>, ComfyError> {
    Err(ComfyError::Unavailable("built without the `comfyui` feature".into()))
}

/// Whether this session can reach ComfyUI (an installed transport, or the `comfyui` feature).
pub fn available(s: &Session) -> bool {
    s.comfyui.transport.is_some() || cfg!(feature = "comfyui")
}

fn transport(s: &Session, server: &str) -> std::result::Result<Arc<dyn Transport>, ComfyError> {
    match &s.comfyui.transport {
        Some(t) => Ok(t.clone()),
        None => http_transport(server),
    }
}

// ------------------------------------------------------------------------------------- params

/// The ComfyUI recipe of `item`.
pub fn recipe_of(s: &Session, item: ItemId) -> Option<Recipe> {
    let g = s.project.generated.get(&item).filter(|g| g.provider == PROVIDER)?;
    serde_json::from_value(g.recipe.clone()).ok()
}

/// The workflow passed as `workflow` (JSON object) or read from `path`.
fn workflow_p(s: &Session, p: &Value, cmd: &str) -> Result<Option<Value>> {
    if let Some(w) = p.get("workflow").filter(|w| !w.is_null()) {
        return Ok(Some(w.clone()));
    }
    let Some(path) = str_p(p, "path") else { return Ok(None) };
    let bytes = s.services.read_file(path).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
    if bytes.len() > 64 << 20 {
        return Err(bad(cmd, format!("{path}: too large for a workflow")));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|e| bad(cmd, format!("{path}: not JSON: {e}")))
}

/// `inputs: [{node, input, value? | file?}]` (a binding with neither removes the override).
fn bindings_p(p: &Value, cmd: &str) -> Result<Vec<Binding>> {
    let Some(list) = p.get("inputs").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let list = list.as_array().ok_or_else(|| bad(cmd, "`inputs` must be a list of {node, input, value | file}"))?;
    list.iter()
        .take(10_000)
        .map(|b| {
            let node = match b.get("node") {
                Some(Value::String(n)) => n.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => return Err(bad(cmd, "every input needs a `node`")),
            };
            let input = str_p(b, "input").filter(|i| !i.is_empty()).ok_or_else(|| bad(cmd, "every input needs an `input` name"))?.to_string();
            let file = str_p(b, "file").filter(|f| !f.is_empty()).map(str::to_string);
            let value = b.get("value").filter(|v| !v.is_null() && file.is_none()).cloned();
            Ok(Binding { node, input, value, file })
        })
        .collect()
}

/// `outputs: [node id]`.
fn outputs_p(p: &Value) -> Option<Vec<String>> {
    let a = p.get("outputs")?.as_array()?;
    Some(a.iter().filter_map(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string()))).collect())
}

/// The ComfyUI clips a command acts on: `items` / `item`, else `clips` / `clip`, else the
/// timeline selection, else the Project panel selection. In timeline order (earliest clip of the
/// active sequence first), then by id: the order a chain of shots generates in.
pub fn targets(s: &Session, p: &Value) -> Vec<ItemId> {
    let ids = |k: &str| p.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect::<Vec<_>>());
    let one = |k: &str| u64_p(p, k).map(|v| vec![v]);
    let seq = s.active_sequence();
    let of_clips = |c: Vec<u64>| -> Vec<ItemId> { c.into_iter().filter_map(|c| seq.and_then(|q| q.find_item(ClipId(c))).map(|(_, it)| it.item)).collect() };
    let mut raw: Vec<ItemId> = if let Some(i) = ids("items").or_else(|| one("item")) {
        i.into_iter().map(ItemId).collect()
    } else if let Some(c) = ids("clips").or_else(|| one("clip")) {
        of_clips(c)
    } else {
        let sel = of_clips(s.state.selection.iter().map(|c| c.0).collect());
        if sel.iter().any(|i| recipe_of(s, *i).is_some()) { sel } else { s.state.project_selection.clone() }
    };
    let mut seen = std::collections::BTreeSet::new();
    raw.retain(|i| recipe_of(s, *i).is_some() && seen.insert(*i));
    let start = |i: &ItemId| -> (Tick, u64) {
        let t = seq.and_then(|q| q.all_tracks().flat_map(|t| t.items.iter()).filter(|it| it.item == *i).map(|it| it.start).min());
        (t.unwrap_or(Tick::MAX), i.0)
    };
    raw.sort_by_key(start);
    raw
}

fn has_target(s: &Session) -> std::result::Result<(), String> {
    if targets(s, &Value::Null).is_empty() { Err("select a ComfyUI clip".into()) } else { Ok(()) }
}

fn can_generate(s: &Session) -> std::result::Result<(), String> {
    has_target(s)?;
    if available(s) { Ok(()) } else { Err(NO_COMFY.into()) }
}

// ------------------------------------------------------------------------------------- commands

fn settings_json(s: &Session) -> Value {
    json!({"settings": s.prefs.comfyui, "available": available(s)})
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comfyui.settings";
    let mut c = s.prefs.comfyui.clone();
    if let Some(v) = str_p(p, "server") {
        let v = v.trim().trim_end_matches('/');
        if !(v.starts_with("http://") || v.starts_with("https://")) || v.len() < "http://x".len() {
            return Err(bad(CMD, "`server` must be an http:// or https:// address"));
        }
        c.server = v.to_string();
    }
    if let Some(v) = str_p(p, "outputDir") {
        c.output_dir = v.trim().to_string();
    }
    if let Some(v) = u64_p(p, "timeoutMinutes") {
        c.timeout_minutes = v.clamp(1, 24 * 60) as u32;
    }
    if c != s.prefs.comfyui {
        let mut prefs = s.prefs.clone();
        prefs.comfyui = c;
        s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    let mut out = settings_json(s);
    if bool_p(p, "check") == Some(true) {
        let server = s.prefs.comfyui.server.clone();
        match transport(s, &server).and_then(|t| Client::new(t).system_stats()) {
            Ok(v) => {
                out["reachable"] = json!(true);
                out["system"] = v.get("system").cloned().unwrap_or(Value::Null);
            }
            Err(e) => {
                out["reachable"] = json!(false);
                out["error"] = json!(e.to_string());
            }
        }
    }
    Ok(out)
}

fn nodes_json(wf: &Workflow, recipe: Option<&Recipe>) -> Value {
    let nodes: Vec<Value> = wf
        .nodes()
        .into_iter()
        .map(|n| {
            let inputs: Vec<Value> = n
                .inputs
                .iter()
                .map(|i| {
                    let mut v = json!(i);
                    if let Some(b) = recipe.and_then(|r| r.inputs.iter().find(|b| b.node == n.node && b.input == i.input)) {
                        v["override"] = json!(b);
                    }
                    v
                })
                .collect();
            json!({"node": n.node, "classType": n.class_type, "title": n.title, "inputs": inputs})
        })
        .collect();
    json!(nodes)
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comfyui.inspect";
    if let Some(w) = workflow_p(s, p, CMD)? {
        let wf = Workflow::parse(&w).map_err(|e| comfy_err(CMD, e))?;
        return Ok(json!({"nodes": nodes_json(&wf, None), "seeds": wf.seeds()}));
    }
    let item = *targets(s, p).first().ok_or_else(|| bad(CMD, "pass `workflow` or `path`, or select a ComfyUI clip"))?;
    let recipe = recipe_of(s, item).ok_or_else(|| bad(CMD, "not a ComfyUI clip"))?;
    let wf = Workflow::parse(&recipe.workflow).map_err(|e| comfy_err(CMD, e))?;
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, "no such item"))?;
    let generated = matches!(pi.as_media().map(|m| &m.media), Some(MediaRef::File { .. }));
    let last_run = s.project.generated.get(&item).map(|g| g.last_run.clone()).unwrap_or_default();
    Ok(json!({
        "item": item.0,
        "name": pi.name,
        "server": recipe.server_or(&s.prefs.comfyui.server),
        "inputs": recipe.inputs,
        "outputs": recipe.outputs,
        "nodes": nodes_json(&wf, Some(&recipe)),
        "seeds": wf.seeds(),
        "generated": generated,
        "generating": s.comfyui.generating(item),
        "lastRun": last_run,
    }))
}

fn check_bindings(wf: &Workflow, b: &[Binding], cmd: &str) -> Result<()> {
    for b in b.iter().filter(|b| b.value.is_some() || b.file.is_some()) {
        wf.check_binding(b).map_err(|e| comfy_err(cmd, e))?;
    }
    Ok(())
}

fn new_clip(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comfyui.newClip";
    let wf_json = workflow_p(s, p, CMD)?.ok_or_else(|| bad(CMD, "need `workflow` (API-format JSON) or `path`"))?;
    let mut recipe = Recipe::new(wf_json).map_err(|e| comfy_err(CMD, e))?;
    let wf = Workflow::parse(&recipe.workflow).map_err(|e| comfy_err(CMD, e))?;
    let bindings = bindings_p(p, CMD)?;
    check_bindings(&wf, &bindings, CMD)?;
    recipe.bind(bindings);
    recipe.outputs = outputs_p(p).unwrap_or_default();
    recipe.server = str_p(p, "server").unwrap_or_default().trim().to_string();
    let stem = str_p(p, "path").and_then(|x| std::path::Path::new(x).file_stem()).map(|x| x.to_string_lossy().into_owned());
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).or(stem).unwrap_or_else(|| "ComfyUI Clip".into());
    let secs = f64_p(p, "duration").unwrap_or(5.0);
    if !secs.is_finite() {
        return Err(bad(CMD, "`duration` must be a number of seconds"));
    }
    let secs = secs.clamp(0.04, 3600.0);
    let recipe_json = serde_json::to_value(&recipe).map_err(|e| EngineError::Other(e.to_string()))?;
    let generation = Arc::new(Generation { provider: PROVIDER.into(), recipe: recipe_json, last_run: Value::Null });
    let pool = s.media.clone();
    let item_name = name.clone();
    let make = move |pr: &mut Project, (w, h, rate): (u32, u32, filmcraft_time::FrameRate)| -> (ItemId, Tick) {
        let dur = rate.snap_nearest(Tick::from_seconds_f64(secs)).max(rate.frame_duration());
        let src = GeneratorSource::new(Generator::ColorMatte { color: PLACEHOLDER }, w.max(16), h.max(16), rate, dur);
        let id = crate::demo::add_generator(pr, &pool, src, &item_name, Label::Violet, None);
        pr.generated.insert(id, generation);
        (id, dur)
    };
    let (item, clip) = if s.active_sequence().is_some() && bool_p(p, "place").unwrap_or(true) {
        let clip = crate::graphics::place_video_clip(s, &name, p, "New ComfyUI Clip", Vec::new(), make)?;
        let item = s.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, it)| it.item).ok_or(EngineError::NoSequence)?;
        (item, Some(clip))
    } else {
        let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
        let id = s.edit("New ComfyUI Clip", |pr, ed| {
            let (id, _) = make(pr, (st.width, st.height, st.frame_rate));
            ed.project_selection = vec![id];
            Ok(id)
        })?;
        (id, None)
    };
    let mut out = json!({"item": item.0, "clip": clip.map(|c| c.0), "name": name});
    if bool_p(p, "generate") == Some(true) {
        out["generate"] = generate(s, &json!({"items": [item.0], "wait": bool_p(p, "wait").unwrap_or(false), "dir": str_p(p, "dir")}))?;
    }
    Ok(out)
}

fn set_inputs(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comfyui.setInputs";
    let item = match targets(s, p).as_slice() {
        [i] => *i,
        [] => return Err(bad(CMD, "select a ComfyUI clip (or pass `item`)")),
        _ => return Err(bad(CMD, "select one ComfyUI clip (or pass `item`)")),
    };
    let mut recipe = recipe_of(s, item).ok_or_else(|| bad(CMD, "not a ComfyUI clip"))?;
    if let Some(w) = workflow_p(s, p, CMD)? {
        let wf = Workflow::parse(&w).map_err(|e| comfy_err(CMD, e))?;
        // overrides of inputs the new workflow still has carry over
        recipe.inputs.retain(|b| wf.check_binding(b).is_ok() && wf.input(&b.node, &b.input).is_some());
        recipe.workflow = w;
    }
    if bool_p(p, "clearInputs") == Some(true) {
        recipe.inputs.clear();
    }
    let wf = Workflow::parse(&recipe.workflow).map_err(|e| comfy_err(CMD, e))?;
    let bindings = bindings_p(p, CMD)?;
    check_bindings(&wf, &bindings, CMD)?;
    recipe.bind(bindings);
    if let Some(o) = outputs_p(p) {
        recipe.outputs = o;
    }
    if let Some(v) = str_p(p, "server") {
        recipe.server = v.trim().to_string();
    }
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    let recipe_json = serde_json::to_value(&recipe).map_err(|e| EngineError::Other(e.to_string()))?;
    s.edit("ComfyUI Clip Settings", |pr, _| {
        let last_run = pr.generated.get(&item).map(|g| g.last_run.clone()).unwrap_or_default();
        pr.generated.insert(item, Arc::new(Generation { provider: PROVIDER.into(), recipe: recipe_json, last_run }));
        if let Some(n) = name
            && let Some(it) = pr.item_mut(item)
        {
            it.name = n;
        }
        Ok(())
    })?;
    inspect(s, &json!({"item": item.0}))
}

// ------------------------------------------------------------------------------------- generate

/// Where generated files go: `dir`, else the `outputDir` setting, else `ComfyUI` next
/// to the project, else `ComfyUI Outputs` in the data (or temporary) directory.
fn output_dir(s: &Session, p: &Value) -> String {
    if let Some(d) = str_p(p, "dir").filter(|d| !d.is_empty()) {
        return d.to_string();
    }
    if !s.prefs.comfyui.output_dir.is_empty() {
        return s.prefs.comfyui.output_dir.clone();
    }
    if let Some(d) = s.path.as_deref().and_then(|x| std::path::Path::new(x).parent()).filter(|d| !d.as_os_str().is_empty()) {
        return d.join("ComfyUI").to_string_lossy().into_owned();
    }
    let base = s.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.to_path_buf()).unwrap_or_else(|| crate::temp_dir().join("FilmCraft"));
    base.join("ComfyUI Outputs").to_string_lossy().into_owned()
}

/// A file name part from a clip name.
fn file_stem(name: &str) -> String {
    let s: String = name.chars().take(60).map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "ComfyUI".into() } else { s }
}

/// `<dir>/<name> <nnn>.<ext>`, the first number not taken.
fn version_path(services: &dyn crate::Services, dir: &str, name: &str, ext: &str) -> String {
    let stem = file_stem(name);
    let ext: String = ext.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    let mut n = 1u32;
    loop {
        let path = std::path::Path::new(dir).join(format!("{stem} {n:03}.{ext}")).to_string_lossy().into_owned();
        if services.file_size(&path).is_err() || n >= 99_999 {
            return path;
        }
        n += 1;
    }
}

/// A new seed for the `k`-th seed input (below 2^50: exact in JSON readers).
fn new_seed(salt: u64, k: usize) -> u64 {
    comfy::fnv1a(&[salt.to_le_bytes(), (k as u64).to_le_bytes()].concat()) % (1 << 50)
}

struct Work {
    item: ItemId,
    name: String,
    recipe: Recipe,
    transport: Arc<dyn Transport>,
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comfyui.generate";
    let items = targets(s, p);
    if items.is_empty() {
        return Err(bad(CMD, "nothing to generate (pass `items` or select ComfyUI clips)"));
    }
    if let Some(i) = items.iter().find(|i| s.comfyui.generating(**i)) {
        let name = s.project.item(*i).map(|x| x.name.clone()).unwrap_or_default();
        return Err(EngineError::Other(format!("{name} is already being generated")));
    }
    let randomize = bool_p(p, "randomizeSeeds").unwrap_or(false);
    let salt = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0) ^ s.revision;
    let default_server = s.prefs.comfyui.server.clone();
    let mut work = Vec::new();
    for item in &items {
        let Some(mut recipe) = recipe_of(s, *item) else { continue };
        if randomize {
            let wf = Workflow::parse(&recipe.workflow).map_err(|e| comfy_err(CMD, e))?;
            let seeds = wf.seeds();
            recipe.bind(seeds.into_iter().enumerate().map(|(k, (node, input))| Binding::value(node, input, json!(new_seed(salt ^ item.0, k)))));
        }
        let server = recipe.server_or(&default_server).to_string();
        let transport = transport(s, &server).map_err(|e| comfy_err(CMD, e))?;
        let name = s.project.item(*item).map(|i| i.name.clone()).unwrap_or_default();
        work.push(Work { item: *item, name, recipe, transport });
    }
    let dir = output_dir(s, p);
    let opts = RunOptions { timeout: std::time::Duration::from_secs(u64::from(s.prefs.comfyui.timeout_minutes.max(1)) * 60), ..Default::default() };
    let services = s.services.clone();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let label = match work.as_slice() {
        [w] => format!("ComfyUI: {}", w.name),
        w => format!("ComfyUI ({} clips)", w.len()),
    };
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(work.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let results: Arc<Mutex<Vec<Done>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let run = move || {
        use std::sync::atomic::Ordering;
        let t0 = web_time::Instant::now();
        let (mut ok, mut failed, mut bytes) = (0u64, Vec::new(), 0u64);
        let mut stopped = false;
        for w in &work {
            if prog.cancel.load(Ordering::Relaxed) {
                stopped = true;
                break;
            }
            let client = Client::new(w.transport.clone());
            let mut read = |path: &str| services.read_file(path).map_err(|e| e.to_string());
            let r = client.run(&w.recipe, &mut read, &opts, &mut |pr| {
                *lock(&prog.status) = format!("{}: {pr}", w.name);
                !prog.cancel.load(Ordering::Relaxed)
            });
            let result = match r {
                Ok(rr) => {
                    if !cfg!(target_arch = "wasm32") && !rr.files.is_empty() {
                        let _ = std::fs::create_dir_all(&dir);
                    }
                    let mut written = Vec::new();
                    let mut err = None;
                    for f in rr.files {
                        let ext = f.output.file.as_ref().and_then(|x| x.filename.rsplit_once('.')).map(|x| x.1.to_string()).unwrap_or_else(|| "bin".into());
                        let path = version_path(&*services, &dir, &w.name, &ext);
                        if let Err(e) = services.write_file(&path, &f.bytes) {
                            err = Some(format!("{path}: {e}"));
                            break;
                        }
                        bytes += f.bytes.len() as u64;
                        written.push((f.output, path, Arc::<[u8]>::from(f.bytes)));
                    }
                    match err {
                        Some(e) => Err(e),
                        None => Ok((rr.prompt_id, rr.outputs, written)),
                    }
                }
                Err(ComfyError::Cancelled) => {
                    stopped = true;
                    break;
                }
                Err(e) => Err(e.to_string()),
            };
            match &result {
                Ok(_) => ok += 1,
                Err(e) => failed.push(format!("{}: {e}", w.name)),
            }
            lock(&out).push(Done { item: w.item, recipe: w.recipe.clone(), result });
            prog.done.fetch_add(1, Ordering::Relaxed);
        }
        let secs = t0.elapsed().as_secs_f64();
        *lock(&prog.status) = if stopped {
            format!("Stopped: {ok} clip(s) generated")
        } else if failed.is_empty() {
            format!("Generated {ok} clip(s) ({secs:.1}s)")
        } else {
            failed.join("; ")
        };
        let r = if ok == 0 && (stopped || !failed.is_empty()) {
            Err(if stopped { "stopped".to_string() } else { failed.join("; ") })
        } else {
            Ok(filmcraft_export::Report { path: dir.clone(), frames: ok, seconds: secs, bytes, render_fps: 0.0, extra_files: Vec::new() })
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.comfyui.pending.push(Pending { job: id, items: items.clone(), results });
    let wait = bool_p(p, "wait").unwrap_or(false);
    if wait || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
        let clips: Vec<Value> = items.iter().map(|i| json!({"item": i.0, "lastRun": s.project.generated.get(i).map(|g| g.last_run.clone())})).collect();
        if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
            return Err(EngineError::Other(e));
        }
        return Ok(json!({"job": id, "items": clips}));
    }
    std::thread::Builder::new().name("filmcraft-comfyui".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({"job": id, "items": items.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// Apply finished generate jobs (one undo step per clip). Called once per UI frame from
/// [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while i < s.comfyui.pending.len() {
        let job = s.jobs.iter().find(|j| j.id == s.comfyui.pending[i].job);
        if !job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed)) {
            i += 1;
            continue;
        }
        let pj = s.comfyui.pending.remove(i);
        // clips that finished before a stop are kept: they are complete
        let done = std::mem::take(&mut *lock(&pj.results));
        for d in done {
            let name = s.project.item(d.item).map(|x| x.name.clone()).unwrap_or_default();
            if let Err(e) = apply(s, d) {
                s.error_toast("comfyui.generate", format!("ComfyUI: {name}: {e}"));
            }
        }
    }
}

/// The output a clip shows: the first video, else image, else audio.
fn primary(files: &[Written]) -> Option<usize> {
    [OutputKind::Video, OutputKind::Image, OutputKind::Audio].iter().find_map(|k| files.iter().position(|f| f.0.kind == *k))
}

fn apply(s: &mut Session, d: Done) -> Result<()> {
    let Done { item, recipe, result } = d;
    let (prompt_id, outputs, files) = result.map_err(EngineError::Other)?;
    if s.project.item(item).and_then(|i| i.as_media()).is_none() {
        return Err(EngineError::Other("the clip was deleted while it was generating".into()));
    }
    let texts: Vec<String> = outputs.iter().filter_map(|o| o.text.clone()).collect();
    let main = primary(&files);
    let last_run = json!({
        "promptId": prompt_id,
        "outputs": outputs,
        "files": files.iter().map(|(o, path, _)| json!({"node": o.node, "kind": o.kind, "path": path})).collect::<Vec<_>>(),
        "texts": texts,
        "media": main.and_then(|k| files.get(k)).map(|f| f.1.clone()),
    });
    let recipe_json = serde_json::to_value(&recipe).map_err(|e| EngineError::Other(e.to_string()))?;
    let generation = Arc::new(Generation { provider: PROVIDER.into(), recipe: recipe_json, last_run });
    let n0 = s.history.undo.len();
    let label = "Generate ComfyUI Clip";
    let mut media_info = None;
    match main.and_then(|k| files.get(k)) {
        Some((_, path, bytes)) => {
            let fname = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let src = s.media.open_bytes(&fname, bytes.clone()).map_err(|e| EngineError::Other(format!("{fname}: {e}")))?;
            let mut info = src.info().clone();
            if info.kind == MediaKind::Still
                && let Some(v) = info.video.as_mut()
            {
                v.frame_rate = crate::settings::timebase_rate(&s.prefs.media.indeterminate_timebase);
            }
            let identity = crate::relink::identity_of_bytes(bytes);
            let (path, info2) = (path.clone(), info.clone());
            s.edit(label, move |pr, _| {
                let m = pr.item_mut(item).and_then(|i| i.as_media_mut()).ok_or_else(|| EngineError::Other("the clip is no longer media".into()))?;
                m.media = MediaRef::File { path };
                m.info = info2.clone();
                m.identity = Some(identity);
                m.offline = false;
                m.mark_in = None;
                m.mark_out = None;
                pr.generated.insert(item, generation);
                fit_clips(pr, item, &info2)
            })?;
            s.media.insert_file(item, path_of(&files, main), src);
            media_info = Some(info);
        }
        None => {
            let no_media = texts.iter().any(|t| !t.trim().is_empty());
            s.edit(label, move |pr, _| {
                // text only: the placeholder steps aside for the titles
                if no_media
                    && let Some(m) = pr.item_mut(item).and_then(|i| i.as_media_mut())
                    && matches!(m.media, MediaRef::Generator(Generator::ColorMatte { color }) if color == PLACEHOLDER)
                {
                    m.media = MediaRef::Generator(Generator::TransparentVideo);
                }
                pr.generated.insert(item, generation);
                Ok(())
            })?;
            s.media.remove(item);
        }
    }
    // further media outputs are imported; a separate sound joins a picture that has none
    let mut sound = None;
    for (k, (o, path, bytes)) in files.iter().enumerate() {
        if Some(k) == main {
            continue;
        }
        match crate::commands::import_bytes(s, path, bytes.clone(), None) {
            Ok(id) if o.kind == OutputKind::Audio && sound.is_none() => sound = Some(id),
            Ok(_) => {}
            Err(e) => s.error_toast("comfyui.generate", format!("ComfyUI: {path}: {e}")),
        }
    }
    if let (Some(info), Some(audio)) = (&media_info, sound)
        && info.video.is_some()
        && info.audio.is_none()
    {
        let dur = s.project.item(audio).map(|i| i.duration()).unwrap_or_default();
        s.edit(label, |pr, _| add_audio_partners(pr, item, audio, dur))?;
    }
    if main.is_none() && !texts.is_empty() {
        let text = texts.join("\n");
        let places: Vec<(Tick, Tick)> = s
            .active_sequence()
            .map(|q| q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| it.item == item).map(|it| (it.start, it.duration)).collect())
            .unwrap_or_default();
        for (start, dur) in places {
            s.execute("graphics.newText", json!({"text": text, "time": start.0, "seconds": dur.seconds(), "size": 60}))?;
        }
    }
    crate::clip_ops::collapse_history(s, n0, label);
    let name = s.project.item(item).map(|i| i.name.clone()).unwrap_or_default();
    s.toast(format!("ComfyUI: {name} generated"));
    Ok(())
}

fn path_of(files: &[Written], k: Option<usize>) -> &str {
    k.and_then(|k| files.get(k)).map(|f| f.1.as_str()).unwrap_or_default()
}

// ------------------------------------------------------------------------------------- clip fitting

/// Put `a` on the first unlocked audio track free over its range (a new track if none is).
fn insert_audio(pr: &mut Project, seq: ItemId, a: TrackItem) -> Result<()> {
    let new_track = TrackId(pr.alloc_id());
    let q = pr.sequence_mut(seq).ok_or(EngineError::NoSequence)?;
    let range = a.range();
    let free = |t: &Track| !t.locked && !t.items.iter().any(|i| i.range().overlaps(&range));
    let idx = match q.audio_tracks.iter().position(free) {
        Some(i) => i,
        None => {
            let n = q.audio_tracks.len() + 1;
            q.audio_tracks.push(Track::new(new_track, TrackKind::Audio, format!("Audio {n}")));
            q.audio_tracks.len() - 1
        }
    };
    let t = q.audio_tracks.get_mut(idx).ok_or(EngineError::NoSequence)?;
    t.items.push(a);
    t.sort();
    Ok(())
}

/// Fit every clip of `item` to its new media: no longer than the media (stills and speed-changed
/// clips keep their length), sound-only media moves to audio tracks, and a video with sound gets
/// a linked audio clip under each video clip that has none.
fn fit_clips(pr: &mut Project, item: ItemId, info: &MediaInfo) -> Result<()> {
    let (has_v, has_a) = (info.video.is_some(), info.audio.is_some());
    let still = info.kind == MediaKind::Still;
    let seqs: Vec<ItemId> = pr.sequences().map(|i| i.id).collect();
    for sid in seqs {
        let Some(q) = pr.sequence_mut(sid) else { continue };
        let rate = q.settings.frame_rate;
        let min = rate.frame_duration();
        let mut to_audio: Vec<TrackItem> = Vec::new();
        for t in q.video_tracks.iter_mut() {
            for it in t.items.iter_mut().filter(|it| it.item == item) {
                if has_v && !still && it.speed == 1.0 && !it.reverse && info.duration > Tick::ZERO {
                    let max = rate.snap(info.duration - it.source_in).max(min);
                    if it.duration > max {
                        it.duration = max;
                    }
                }
                if !has_v && has_a {
                    to_audio.push(it.clone());
                }
            }
            if !has_v && has_a {
                t.items.retain(|it| it.item != item);
            }
        }
        for v in to_audio {
            let dur = if info.duration > Tick::ZERO { v.duration.min(info.duration - v.source_in) } else { v.duration };
            if dur <= Tick::ZERO {
                continue;
            }
            let mut a = pr.make_track_item(item, TrackKind::Audio, v.start, TimeRange::new(v.source_in, dur), rate).ok_or(EngineError::NoSequence)?;
            a.name = v.name.clone();
            insert_audio(pr, sid, a)?;
        }
        if let Some(q) = pr.sequence(sid) {
            q.check().map_err(EngineError::Other)?;
        }
    }
    if has_v && has_a {
        add_audio_partners(pr, item, item, info.duration)?;
    }
    Ok(())
}

/// Give every video clip of `item` the sound of `audio` (its own media, or a separate sound
/// output) as a linked audio clip: a linked audio partner it already has is pointed at the new
/// sound (it never grows), otherwise one is added on a free audio track.
fn add_audio_partners(pr: &mut Project, item: ItemId, audio: ItemId, audio_dur: Tick) -> Result<()> {
    let seqs: Vec<ItemId> = pr.sequences().map(|i| i.id).collect();
    for sid in seqs {
        let Some(q) = pr.sequence(sid) else { continue };
        let rate = q.settings.frame_rate;
        let clips: Vec<(ClipId, Tick, Tick, Tick, Option<u64>)> = q
            .video_tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter(|it| it.item == item)
            .map(|it| (it.id, it.start, it.duration, it.source_in, it.link))
            .collect();
        for (clip, start, dur, src_in, link) in clips {
            let src_in = if audio == item { src_in } else { Tick::ZERO };
            let dur = if audio_dur > Tick::ZERO { dur.min(audio_dur - src_in) } else { dur };
            if dur <= Tick::ZERO {
                continue;
            }
            let q = pr.sequence_mut(sid).ok_or(EngineError::NoSequence)?;
            if let Some(l) = link
                && let Some(partner) = q.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()).find(|a| a.link == Some(l))
            {
                partner.item = audio;
                partner.source_in = src_in;
                partner.duration = partner.duration.min(dur);
                continue;
            }
            let link = pr.alloc_id();
            let mut a = pr.make_track_item(audio, TrackKind::Audio, start, TimeRange::new(src_in, dur), rate).ok_or(EngineError::NoSequence)?;
            a.link = Some(link);
            insert_audio(pr, sid, a)?;
            if let Some((_, v)) = pr.sequence_mut(sid).and_then(|q| q.find_item_mut(clip)) {
                v.link = Some(link);
            }
        }
        if let Some(q) = pr.sequence(sid) {
            q.check().map_err(EngineError::Other)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------- registry

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    let menu: &'static [&'static str] = if id == "comfyui.generate" { &["Clip"] } else { &[] };
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "comfyui.settings",
            "ComfyUI Settings",
            r#"{"server":"http://host:port"?,"outputDir":str?,"timeoutMinutes":n?,"check":bool?}"#,
            always,
            settings,
            true,
        ),
        spec("comfyui.inspect", "Inspect ComfyUI Workflow", r#"{"workflow":{api workflow}?|"path":str?,"item":id?,"clip":id?}"#, always, inspect, false),
        spec(
            "comfyui.newClip",
            "New ComfyUI Clip",
            r#"{"workflow":{api workflow}|"path":str,"inputs":[{"node":id,"input":str,"value":any|"file":path}]?,"outputs":[node id]?,"server":str?,"name":str?,"duration":seconds=5,"time":ticks?,"track":index?,"place":bool=true,"generate":bool=false,"wait":bool=false}"#,
            always,
            new_clip,
            true,
        ),
        spec(
            "comfyui.setInputs",
            "ComfyUI Clip Settings",
            r#"{"item":id?|"clip":id?,"inputs":[{"node":id,"input":str,"value":any|"file":path|neither: remove}]?,"clearInputs":bool?,"workflow":{api workflow}?|"path":str?,"outputs":[node id]?,"server":str?,"name":str?}"#,
            has_target,
            set_inputs,
            true,
        ),
        spec(
            "comfyui.generate",
            "Generate ComfyUI Clip",
            r#"{"items":[id]?|"item":id?|"clips":[id]?|"clip":id?,"randomizeSeeds":bool=false,"dir":str?,"wait":bool=false}"#,
            can_generate,
            generate,
            true,
        ),
    ]
}

#[cfg(test)]
#[path = "comfyui_tests.rs"]
mod tests;

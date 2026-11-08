//! Lettering made by the external animation app (EffectCraft): the "Lettering" window's engine side.
//!
//! The text animation is not drawn here. FilmCraft hands over a complete order and the animation
//! app renders a ProRes 4444 `.mov` with alpha:
//!
//! 1. the selected captions (or the caption under the playhead) give the words, timed to the voice;
//! 2. [`prepare`] (`lettering.fx.prepare`) describes the span: start, duration, frame size, rate, words;
//! 3. [`send`] (`lettering.fx.send`) writes `words.json` (seconds relative to the span start) and
//!    `pedido.json` (`{template, palavras, saida}`) in a work folder and starts the *attendant*
//!    (`atender.py` of the template bank) on a background thread through a [`ProcessRunner`];
//! 4. [`templates`] (`lettering.fx.templates`) is the menu: every `templates/*/template.json` of the bank.
//!
//! Starting a process is a host service: the engine only owns the [`ProcessRunner`] trait. The
//! desktop app installs a runner (`apps/filmcraft`); hosts without one (web) answer "unsupported".
//! Bringing the `.mov` back onto the timeline is not part of this module.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq};
use crate::{EngineError, Result, Session};
use filmcraft_time::Tick;

/// Most words one order carries (a hostile or huge selection must not build a huge file).
pub const MAX_WORDS: usize = 4000;
/// Phrase rules (the prototype's): a pause longer than this starts a new phrase, as do sentence
/// punctuation, more than [`MAX_PHRASE_WORDS`] words or more than [`MAX_PHRASE_LETTERS`] letters.
pub const PAUSE_BREAK: f64 = 0.45;
pub const MAX_PHRASE_WORDS: usize = 4;
pub const MAX_PHRASE_LETTERS: usize = 22;
/// Largest `template.json` read.
const MAX_TEMPLATE_JSON: u64 = 1 << 20;
/// Default time an order may take.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

// ---------------------------------------------------------------------------------------------
// Process runner (implemented by the host)
// ---------------------------------------------------------------------------------------------

/// A program to run. The arguments reach the program as they are: never through a shell.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    /// The run is killed after this long.
    pub timeout: Duration,
    /// At most this many bytes of each of stdout and stderr are kept.
    pub max_output: usize,
}

/// What a finished run left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessOutput {
    /// The exit code; `None` when a signal ended the program.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// An output stream was cut at `max_output`.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ProcessError {
    #[error("program not found: {0}")]
    NotFound(String),
    #[error("timed out after {0} s")]
    Timeout(u64),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(String),
    #[error("this host cannot start programs")]
    Unsupported,
}

/// Starts programs for the engine. `run` blocks until the program ends, `cancel` turns true or the
/// timeout passes (the program is then killed), and it must never panic.
pub trait ProcessRunner: Send + Sync {
    fn run(&self, spec: &ProcessSpec, cancel: &AtomicBool) -> std::result::Result<ProcessOutput, ProcessError>;
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// How an order stands.
#[derive(Clone, Debug, PartialEq)]
pub enum FxStatus {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

impl FxStatus {
    fn name(&self) -> &'static str {
        match self {
            FxStatus::Running => "running",
            FxStatus::Done => "done",
            FxStatus::Failed(_) => "failed",
            FxStatus::Cancelled => "cancelled",
        }
    }
}

/// An order sent to the animation app.
#[derive(Clone)]
pub struct FxJob {
    pub id: u64,
    pub template: String,
    pub out: String,
    pub status: Arc<Mutex<FxStatus>>,
    pub cancel: Arc<AtomicBool>,
}

impl FxJob {
    pub fn to_json(&self) -> Value {
        let st = self.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let error = if let FxStatus::Failed(e) = &st { json!(e) } else { Value::Null };
        json!({"id": self.id, "template": self.template, "out": self.out, "status": st.name(), "error": error})
    }
}

/// Session state of the Lettering window's engine side.
pub struct FxState {
    /// The host's process runner (None: the host cannot start programs).
    pub runner: Option<Arc<dyn ProcessRunner>>,
    /// The template bank folder: `atender.py` and `templates/<id>/template.json`.
    pub bank: Option<String>,
    /// The interpreter that runs `atender.py`.
    pub interpreter: String,
    pub timeout: Duration,
    pub jobs: Vec<FxJob>,
    next_job: u64,
}

impl Default for FxState {
    fn default() -> Self {
        Self { runner: None, bank: None, interpreter: "python3".into(), timeout: DEFAULT_TIMEOUT, jobs: Vec::new(), next_job: 1 }
    }
}

// ---------------------------------------------------------------------------------------------
// Template menu
// ---------------------------------------------------------------------------------------------

/// A template of the bank, as the window's menu shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// An id that is safe as a folder name inside the bank (no separators, no leading dash or dot).
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && !id.starts_with(['-', '.', '_']) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn join(dir: &str, name: &str) -> String {
    std::path::Path::new(dir).join(name).to_string_lossy().into_owned()
}

fn bank_of(s: &Session, p: &Value, cmd: &str) -> Result<String> {
    let b = p.get("bank").and_then(Value::as_str).filter(|b| !b.is_empty()).map(str::to_string).or_else(|| s.lettering_fx.bank.clone());
    let b = b.ok_or_else(|| bad(cmd, "no template bank folder (set `bank`, or FILMCRAFT_LETTERING_BANK)"))?;
    if b.contains('\0') {
        return Err(bad(cmd, "bank path contains a NUL"));
    }
    Ok(b)
}

/// Every `templates/*/template.json` of the bank, by id. Folders that start with `_` (shared code)
/// and templates that cannot be read are left out.
pub fn templates(s: &Session, bank: &str) -> Result<Vec<Template>> {
    let dir = join(bank, "templates");
    let names = s
        .services
        .list_dir(&dir)
        .ok_or_else(|| EngineError::Other("this host cannot list folders".into()))?
        .map_err(|e| EngineError::Other(format!("cannot read the template bank {dir}: {e}")))?;
    let mut out = Vec::new();
    for name in names {
        if !safe_id(&name) {
            continue;
        }
        let file = join(&join(&dir, &name), "template.json");
        let Ok(size) = s.services.file_size(&file) else { continue };
        if size > MAX_TEMPLATE_JSON {
            continue;
        }
        let Ok(bytes) = s.services.read_file(&file) else { continue };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
        let id = v.get("id").and_then(Value::as_str).unwrap_or(&name).to_string();
        // the folder is what the attendant opens: the id must be the folder
        if id != name {
            continue;
        }
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let nome = text("nome");
        out.push(Template { name: if nome.is_empty() { id.clone() } else { nome }, description: text("descricao"), id });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The span and its words
// ---------------------------------------------------------------------------------------------

/// A word with the time it is spoken at, in seconds on the sequence timeline.
struct Said {
    text: String,
    start: f64,
    end: f64,
}

/// The words of the given captions with their times and the span `(start, end)` they cover.
/// Real speech from the transcript is used when there is any; otherwise the words share each
/// caption's time by their length.
fn words_of_captions(s: &Session, ids: &[filmcraft_project::ClipId]) -> Result<(Vec<Said>, f64, f64)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut caps: Vec<(f64, f64, String)> = ids
        .iter()
        .filter_map(|id| q.find_caption(*id))
        .map(|(_, c)| (c.start.seconds(), (c.start + c.duration).seconds(), c.text.replace('\n', " ")))
        .collect();
    if caps.is_empty() {
        return Err(bad("lettering.fx.prepare", "no caption selected"));
    }
    caps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let a = caps.first().map(|c| c.0).unwrap_or(0.0);
    let b = caps.iter().map(|c| c.1).fold(0.0, f64::max);
    let said: Vec<Said> = crate::transcript::sequence_words(s)
        .into_iter()
        .filter(|w| w.start.seconds() >= a - 0.05 && w.end.seconds() <= b + 0.05)
        .map(|w| Said { text: w.text.clone(), start: w.start.seconds(), end: w.end.seconds() })
        .collect();
    if !said.is_empty() {
        return Ok((said, a, b));
    }
    let mut words = Vec::new();
    for (start, end, text) in &caps {
        let toks: Vec<&str> = text.split_whitespace().collect();
        let total: f64 = toks.iter().map(|t| t.chars().count() as f64 + 1.0).sum();
        let mut t = *start;
        for tok in toks {
            let d = (end - start) * (tok.chars().count() as f64 + 1.0) / total;
            words.push(Said { text: tok.into(), start: t, end: t + d });
            t += d;
        }
    }
    Ok((words, a, b))
}

/// A word of the span, in seconds relative to the span start.
#[derive(Clone, Debug, PartialEq)]
pub struct FxWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub hero: bool,
}

/// The captions' span: where it starts on the sequence, how long it is, the frame and its words.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// The captions the span was made from (the window sends them back so the order matches what it showed).
    pub captions: Vec<u64>,
    pub start: Tick,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub words: Vec<FxWord>,
}

fn ms(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The span of the selected captions (the one under the playhead when none is selected).
pub fn span(s: &Session, cmd: &str, pinned: Option<Vec<filmcraft_project::ClipId>>) -> Result<Span> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let ids: Vec<filmcraft_project::ClipId> = if let Some(p) = pinned {
        p
    } else if !s.state.caption_selection.is_empty() {
        s.state.caption_selection.clone()
    } else {
        let t = s.playhead();
        q.caption_tracks.iter().find_map(|tr| tr.caption_at(t).map(|c| c.id)).into_iter().collect()
    };
    if ids.is_empty() {
        return Err(bad(cmd, "select a caption (or put the playhead on one)"));
    }
    let (words, a, b) = words_of_captions(s, &ids)?;
    if words.is_empty() {
        return Err(bad(cmd, "the selected captions have no words"));
    }
    if words.len() > MAX_WORDS {
        return Err(bad(cmd, format!("too many words ({}; at most {MAX_WORDS})", words.len())));
    }
    if ![a, b].iter().all(|x| x.is_finite()) {
        return Err(bad(cmd, "the captions have no valid time"));
    }
    let mut last = 0.0f64;
    let mut out = Vec::with_capacity(words.len());
    for w in &words {
        let start = ms((w.start - a).max(0.0));
        let end = ms((w.end - a).max(start));
        if !start.is_finite() || !end.is_finite() {
            return Err(bad(cmd, "a word has no valid time"));
        }
        last = last.max(end);
        out.push(FxWord { text: w.text.clone(), start, end, hero: false });
    }
    let fr = q.settings.frame_rate.sane();
    Ok(Span {
        captions: ids.iter().map(|c| c.0).collect(),
        start: Tick::from_seconds_f64(a),
        duration: ms((b - a).max(last).max(0.001)),
        width: q.settings.width,
        height: q.settings.height,
        fps: ms(fr.as_f64()),
        words: out,
    })
}

/// Marks the words at `heroes` (indices into the span's words); an index out of range is an error.
pub fn mark_heroes(words: &mut [FxWord], heroes: &[usize], cmd: &str) -> Result<()> {
    let n = words.len();
    for &i in heroes {
        words.get_mut(i).ok_or_else(|| bad(cmd, format!("hero index {i} is out of range (the span has {n} words)")))?.hero = true;
    }
    Ok(())
}

/// `captions`: the ids the span is pinned to; none means the selection (or the caption under the playhead).
fn captions_p(p: &Value, cmd: &str) -> Result<Option<Vec<filmcraft_project::ClipId>>> {
    match p.get("captions") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if !a.is_empty() && a.len() <= MAX_WORDS => a
            .iter()
            .map(|v| v.as_u64().map(filmcraft_project::ClipId).ok_or_else(|| bad(cmd, "`captions` must be caption ids")))
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(cmd, "`captions` must be a non-empty array of caption ids")),
    }
}

fn heroes_p(p: &Value, cmd: &str) -> Result<Vec<usize>> {
    match p.get("heroes") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a.iter().map(|v| v.as_u64().map(|n| n as usize).ok_or_else(|| bad(cmd, "`heroes` must be word indices"))).collect(),
        Some(_) => Err(bad(cmd, "`heroes` must be an array of word indices")),
    }
}

/// Splits the words into phrases (lists of word indices): sentence punctuation, a pause longer than
/// [`PAUSE_BREAK`], more than [`MAX_PHRASE_WORDS`] words or [`MAX_PHRASE_LETTERS`] letters.
pub fn phrases(words: &[FxWord]) -> Vec<Vec<usize>> {
    let letters = |t: &str| t.chars().filter(|c| c.is_alphanumeric()).count();
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_letters = 0usize;
    for (i, w) in words.iter().enumerate() {
        let n = letters(&w.text);
        if let Some(&prev) = cur.last() {
            let p = &words[prev];
            let pause = w.start - p.end;
            let ends = p.text.trim_end().ends_with(['.', '!', '?', '…', ';', ':']);
            if ends || pause > PAUSE_BREAK || cur.len() >= MAX_PHRASE_WORDS || cur_letters + n > MAX_PHRASE_LETTERS {
                out.push(std::mem::take(&mut cur));
                cur_letters = 0;
            }
        }
        cur.push(i);
        cur_letters += n;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The `words.json` of the order.
pub fn words_json(sp: &Span) -> Value {
    let mut prev_end: Option<f64> = None;
    let mut phrases_json = Vec::new();
    for group in phrases(&sp.words) {
        let mut ws = Vec::new();
        for &i in &group {
            let Some(w) = sp.words.get(i) else { continue };
            let pause = prev_end.map_or(0.0, |e| ms((w.start - e).max(0.0)));
            ws.push(json!({"text": w.text, "start": w.start, "end": w.end, "hero": w.hero, "pause_before": pause}));
            prev_end = Some(w.end);
        }
        let (Some(f), Some(l)) = (group.first().and_then(|&i| sp.words.get(i)), group.last().and_then(|&i| sp.words.get(i))) else { continue };
        phrases_json.push(json!({"start": f.start, "end": l.end, "words": ws}));
    }
    json!({"fps": sp.fps, "width": sp.width, "height": sp.height, "duration": sp.duration, "phrases": phrases_json})
}

fn span_json(sp: &Span) -> Value {
    json!({
        "captions": sp.captions,
        "start": sp.start.0,
        "startSeconds": sp.start.seconds(),
        "duration": sp.duration,
        "width": sp.width,
        "height": sp.height,
        "fps": sp.fps,
        "words": sp.words.iter().map(|w| json!({"text": w.text, "start": w.start, "end": w.end, "hero": w.hero})).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

fn prepare(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "lettering.fx.prepare";
    let mut sp = span(s, ID, captions_p(p, ID)?)?;
    mark_heroes(&mut sp.words, &heroes_p(p, ID)?, ID)?;
    let mut v = span_json(&sp);
    v["phrases"] = words_json(&sp)["phrases"].clone();
    Ok(v)
}

fn list_templates(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "lettering.fx.templates";
    let bank = bank_of(s, p, ID)?;
    let t = templates(s, &bank)?;
    Ok(json!({"bank": bank, "templates": t.iter().map(|t| json!({"id": t.id, "nome": t.name, "descricao": t.description})).collect::<Vec<_>>()}))
}

/// Where an order's files go: next to the saved project, else the temporary folder.
fn work_dir(s: &Session, sp: &Span) -> String {
    let base = match &s.path {
        Some(p) => format!("{p}-lettering"),
        None => join(&std::env::temp_dir().to_string_lossy(), "filmcraft-lettering"),
    };
    join(&base, &format!("trecho-{}ms", ms(sp.start.seconds() * 1000.0) as i64))
}

fn send(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "lettering.fx.send";
    let runner = s.lettering_fx.runner.clone().ok_or_else(|| bad(ID, "this host cannot start the animation app"))?;
    let bank = bank_of(s, p, ID)?;
    let template = p.get("template").and_then(Value::as_str).ok_or_else(|| bad(ID, "`template` is required"))?;
    if !safe_id(template) || !templates(s, &bank)?.iter().any(|t| t.id == template) {
        return Err(bad(ID, format!("unknown template `{template}`")));
    }
    let mut sp = span(s, ID, captions_p(p, ID)?)?;
    mark_heroes(&mut sp.words, &heroes_p(p, ID)?, ID)?;
    let dir = match p.get("out").and_then(Value::as_str) {
        Some(o) if o.contains('\0') || o.is_empty() => return Err(bad(ID, "invalid `out`")),
        Some(o) => o.to_string(),
        None => work_dir(s, &sp),
    };
    s.services.create_dir_all(&dir).map_err(|e| EngineError::Other(format!("cannot create {dir}: {e}")))?;
    let out_busy = join(&dir, "lettering.mov");
    if s.lettering_fx.jobs.iter().any(|j| j.out == out_busy && matches!(*j.status.lock().unwrap_or_else(PoisonError::into_inner), FxStatus::Running)) {
        return Err(bad(ID, "an order for this folder is still running"));
    }
    let words_path = join(&dir, "words.json");
    let order_path = join(&dir, "pedido.json");
    let out = join(&dir, "lettering.mov");
    let words = serde_json::to_vec_pretty(&words_json(&sp)).map_err(|e| EngineError::Other(e.to_string()))?;
    let order =
        serde_json::to_vec_pretty(&json!({"template": template, "palavras": words_path, "saida": out})).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(&words_path, &words).map_err(|e| EngineError::Other(format!("cannot write {words_path}: {e}")))?;
    s.services.write_file(&order_path, &order).map_err(|e| EngineError::Other(format!("cannot write {order_path}: {e}")))?;
    // an old render must not pass for the new one
    if s.services.file_size(&out).is_ok() {
        s.services.write_file(&out, &[]).map_err(|e| EngineError::Other(format!("cannot clear {out}: {e}")))?;
    }
    // a relative bank that starts with `-` must not be read by the interpreter as an option
    let script = join(&bank, "atender.py");
    let script = if script.starts_with('-') { join(".", &script) } else { script };
    let spec = ProcessSpec {
        program: s.lettering_fx.interpreter.clone(),
        args: vec![script, order_path.clone()],
        timeout: s.lettering_fx.timeout,
        max_output: 64 * 1024,
    };
    let id = s.lettering_fx.next_job;
    s.lettering_fx.next_job = id.saturating_add(1);
    let job =
        FxJob { id, template: template.into(), out: out.clone(), status: Arc::new(Mutex::new(FxStatus::Running)), cancel: Arc::new(AtomicBool::new(false)) };
    let (status, cancel, services, out_path) = (job.status.clone(), job.cancel.clone(), s.services.clone(), out.clone());
    std::thread::Builder::new()
        .name("filmcraft-lettering".into())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_order(runner.as_ref(), &spec, &cancel, services.as_ref(), &out_path)));
            let r = r.unwrap_or_else(|_| FxStatus::Failed("the lettering job crashed".into()));
            *status.lock().unwrap_or_else(PoisonError::into_inner) = r;
        })
        .map_err(|e| EngineError::Other(format!("cannot start the lettering job: {e}")))?;
    s.lettering_fx.jobs.push(job);
    Ok(json!({"job": id, "words": words_path, "order": order_path, "out": out}))
}

/// Runs the attendant and checks what it left: a clean exit and a `.mov` that is not empty.
pub fn run_order(runner: &dyn ProcessRunner, spec: &ProcessSpec, cancel: &AtomicBool, services: &dyn crate::Services, out: &str) -> FxStatus {
    match runner.run(spec, cancel) {
        Err(ProcessError::Cancelled) => FxStatus::Cancelled,
        Err(e) => FxStatus::Failed(e.to_string()),
        Ok(o) if o.code != Some(0) => {
            let tail: String = o.stderr.trim().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
            FxStatus::Failed(match o.code {
                Some(c) => format!("the attendant exited with code {c}: {tail}"),
                None => format!("the attendant was stopped by a signal: {tail}"),
            })
        }
        Ok(_) => match services.file_size(out) {
            Ok(n) if n > 0 => FxStatus::Done,
            Ok(_) => FxStatus::Failed("the attendant finished but the .mov is empty".into()),
            Err(e) => FxStatus::Failed(format!("the attendant finished but left no .mov ({e})")),
        },
    }
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let want = p.get("job").and_then(Value::as_u64);
    let jobs: Vec<Value> = s.lettering_fx.jobs.iter().filter(|j| want.is_none_or(|w| w == j.id)).map(FxJob::to_json).collect();
    if want.is_some() && jobs.is_empty() {
        return Err(bad("lettering.fx.status", "unknown job"));
    }
    Ok(json!({"jobs": jobs}))
}

fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get("job").and_then(Value::as_u64).ok_or_else(|| bad("lettering.fx.cancel", "`job` is required"))?;
    let j = s.lettering_fx.jobs.iter().find(|j| j.id == id).ok_or_else(|| bad("lettering.fx.cancel", "unknown job"))?;
    j.cancel.store(true, Ordering::Relaxed);
    Ok(json!({"job": id}))
}

fn enabled_captions(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no active sequence")?;
    if q.caption_tracks.iter().any(|t| !t.captions.is_empty()) { Ok(()) } else { Err("the sequence has no captions".into()) }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "lettering.fx.open",
            label: "Lettering with Animation Templates…",
            menu: &["Graphics and Titles"],
            shortcut: None,
            params: r#"{"heroes":[index]?} (opens the Lettering window for the selected captions; as a command it answers like lettering.fx.prepare)"#,
            enabled: enabled_captions,
            run: prepare,
            journal: false,
        },
        CommandSpec {
            id: "lettering.fx.prepare",
            label: "Prepare Lettering Order",
            menu: &[],
            shortcut: None,
            params: r#"{"heroes":[index]?} -> {start:ticks,startSeconds,duration:sec,width,height,fps,words:[{text,start,end,hero}],phrases} of the selected captions (the caption under the playhead when none is selected)"#,
            enabled: enabled_captions,
            run: prepare,
            journal: false,
        },
        CommandSpec {
            id: "lettering.fx.templates",
            label: "List Lettering Templates",
            menu: &[],
            shortcut: None,
            params: r#"{"bank":path? (default the configured bank)} -> {bank,templates:[{id,nome,descricao}]}"#,
            enabled: crate::commands::always,
            run: list_templates,
            journal: false,
        },
        CommandSpec {
            id: "lettering.fx.send",
            label: "Send Lettering Order",
            menu: &[],
            shortcut: None,
            params: r#"{"template":id,"heroes":[index]?,"bank":path?,"out":folder? (default next to the project)} writes words.json and pedido.json and starts the animation app in the background -> {job,words,order,out}"#,
            enabled: enabled_captions,
            run: send,
            journal: false,
        },
        CommandSpec {
            id: "lettering.fx.status",
            label: "Lettering Order Status",
            menu: &[],
            shortcut: None,
            params: r#"{"job":id?} -> {jobs:[{id,template,out,status:running|done|failed|cancelled,error}]}"#,
            enabled: crate::commands::always,
            run: status,
            journal: false,
        },
        CommandSpec {
            id: "lettering.fx.cancel",
            label: "Cancel Lettering Order",
            menu: &[],
            shortcut: None,
            params: r#"{"job":id}"#,
            enabled: crate::commands::always,
            run: cancel,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests;

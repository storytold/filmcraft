//! `link.editIn`: nest the selected video, hand it to EffectCraft, swap in the linked media item.
//!
//! The project changes in one place, after EffectCraft has accepted the hand-off. Any failure before
//! or after that point restores the project, the editor state, and the undo history to the snapshot
//! taken on entry, and closes the link on the EffectCraft side. Success folds nest, media item,
//! swap, and metadata into one undo step.

use std::sync::Arc;

use craft_ipc::activate::{self, Activated};
use filmcraft_engine::clip_ops::collapse_history;
use filmcraft_engine::project::{ClipId, ItemId, MediaRef, Project, TrackKind};
use filmcraft_engine::time::Tick;
use filmcraft_engine::{EditorState, EngineError, Session};
use serde_json::{Map, Value, json};

use crate::place::{Place, PlaceView};
use crate::prompt::{EFFECTCRAFT_BUNDLE_ID, InstallAction, InstanceChoice, Prompt, Unsupported};
use crate::record::{Format, LinkRecord};

/// What `link.editIn` needs from EffectCraft. [`crate::peer::SocketPeer`] is the real one.
pub trait LinkPeer {
    /// The instance id (uuid) this peer talks to. A retry names it so the same window answers.
    fn instance(&self) -> &str;
    /// `link.open`. Returns the reply `result`.
    fn open(&mut self, params: Value) -> Result<Value, EditInError>;
    /// `link.close`. Unbinds the comp on the EffectCraft side. EffectCraft keeps the comp and any
    /// edits made in it.
    fn close(&mut self, link_id: &str) -> Result<Closed, EditInError>;
    /// `app.activate` with the sender token, if one could be minted.
    fn activate(&mut self, token: Option<&str>, link_id: &str) -> Result<Activated, EditInError>;
    /// `link.attach`. Asks EffectCraft to show the comp of a link that was opened before. It does
    /// not create a comp. [`Attached::Missing`] is an answer, not a failure: the caller can open
    /// the link again.
    fn attach(&mut self, link_id: &str, project: &str, comp: &str) -> Result<Attached, EditInError>;
}

/// What EffectCraft answers to `link.attach`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attached {
    /// The comp of the link is open in EffectCraft and is the active one.
    Reopened,
    /// EffectCraft holds no such link and has no sidecar project that does. Carries its reason.
    Missing(String),
}

/// What EffectCraft answers to `link.close`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Closed {
    /// The comp was changed or rendered after the hand-off. EffectCraft still holds that work.
    pub edited: bool,
    /// Name of the comp EffectCraft kept. Empty when EffectCraft did not say.
    pub comp: String,
}

/// Parameters of `link.editIn`. The first comes from a prompt answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditInOptions {
    /// Talk to this EffectCraft instance (uuid) when several run.
    pub instance: Option<String>,
    /// Block until EffectCraft is ready when it has to be started. Without it a window session
    /// answers [`EditInError::Starting`] at once and shows a progress dialog. For callers that
    /// draw nothing (automation, the CLI).
    pub wait: bool,
    /// Where rendered sources go ([`crate::prepare`]). `None` is the user's link cache.
    pub cache_dir: Option<std::path::PathBuf>,
}

impl EditInOptions {
    pub fn from_params(params: &Value) -> Result<Self, EditInError> {
        let instance = match params.get("instance") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
            Some(_) => return Err(EditInError::Refused("`instance` must be a non-empty string".to_string())),
        };
        let wait = match params.get("wait") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return Err(EditInError::Refused("`wait` must be a bool".to_string())),
        };
        let cache_dir = match params.get("cache_dir") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if !text.is_empty() => Some(std::path::PathBuf::from(text)),
            Some(_) => return Err(EditInError::Refused("`cache_dir` must be a non-empty string".to_string())),
        };
        Ok(Self { instance, wait, cache_dir })
    }
}

/// What `link.editIn` says while EffectCraft starts. Automation matches on it and calls again with
/// `wait: true`, or waits for the dialog to finish.
pub const STARTING_MESSAGE: &str = "EffectCraft is starting";

/// What `link.editIn` says while it renders a source that has no file. Same use as [`STARTING_MESSAGE`].
pub const PREPARING_MESSAGE: &str = "Preparing clip for EffectCraft";

/// Why `link.editIn` did not finish. The project is unchanged in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditInError {
    /// EffectCraft is not installed. The action opens the launcher, or the download page.
    NotInstalled(InstallAction),
    /// The two apps speak no common protocol version. `app` is the one to update.
    UpdateApp { app: &'static str, message: String },
    /// Several EffectCraft instances run and none holds this link.
    ChooseInstance(Vec<InstanceChoice>),
    /// EffectCraft is installed and FilmCraft started it, but it exited or never became ready.
    /// The message names the binary and what happened.
    LaunchFailed(String),
    /// FilmCraft started EffectCraft and it is not ready yet. The start goes on in the background.
    /// Carries the parameters of the call, so the dialog can run it again when EffectCraft is up.
    Starting(Value),
    /// A source of the selection has no file, and FilmCraft renders it for EffectCraft in the
    /// background. Carries the parameters of the call, so the dialog can run it again when the
    /// render ends.
    Preparing(Value),
    /// The render of a source failed. The text says why.
    PrepareFailed(String),
    /// The command is not available. Nothing was sent.
    Refused(String),
    /// Nest, export, or the open call failed.
    Failed(String),
}

impl EditInError {
    /// The question for the user, when this error is one.
    pub fn prompt(&self) -> Option<Prompt> {
        match self {
            EditInError::NotInstalled(action) => Some(Prompt::NotInstalled { action: action.clone() }),
            EditInError::UpdateApp { app, message } => Some(Prompt::UpdateApp { app: (*app).to_string(), message: message.clone() }),
            EditInError::ChooseInstance(choices) => Some(Prompt::ChooseInstance { choices: choices.clone() }),
            EditInError::LaunchFailed(message) => Some(Prompt::LaunchFailed { message: message.clone() }),
            EditInError::Starting(retry) => Some(Prompt::Starting { retry: retry.clone() }),
            EditInError::Preparing(retry) => Some(Prompt::Preparing { retry: retry.clone() }),
            EditInError::PrepareFailed(message) => Some(Prompt::PrepareFailed { message: message.clone() }),
            EditInError::Refused(_) | EditInError::Failed(_) => None,
        }
    }
}

impl std::fmt::Display for EditInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditInError::NotInstalled(_) => f.write_str("EffectCraft is not installed"),
            EditInError::UpdateApp { message, .. } => f.write_str(message),
            EditInError::ChooseInstance(choices) => write!(f, "{} EffectCraft windows are open. Choose one.", choices.len()),
            EditInError::Starting(_) => f.write_str(STARTING_MESSAGE),
            EditInError::Preparing(_) => f.write_str(PREPARING_MESSAGE),
            EditInError::PrepareFailed(text) => write!(f, "could not prepare the clip for EffectCraft: {text}"),
            EditInError::LaunchFailed(text) | EditInError::Refused(text) | EditInError::Failed(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for EditInError {}

/// An open sequence with a selected video clip. The menu command uses the same two strings.
///
/// A project without a path passes: the menu asks where to save first. [`require_saved`] is the
/// check that [`edit_in`] makes before it touches the project.
pub fn edit_in_enabled(session: &Session) -> Result<(), String> {
    let Some(sequence) = session.active_sequence() else {
        return Err("no sequence is open".to_string());
    };
    let video = session
        .state
        .selection
        .iter()
        .any(|clip| sequence.find_item(*clip).is_some_and(|(track_id, _)| sequence.track(track_id).is_some_and(|track| track.kind == TrackKind::Video)));
    if !video {
        return Err("select a video clip in the timeline".to_string());
    }
    crate::reopen::linked_selection(session).map(|_| ())
}

/// The result movie lives next to the project file and the link record names it, so `link.editIn`
/// needs a path. The UI saves first (Save As) when there is none; a call without a UI stops here.
pub fn require_saved(session: &Session) -> Result<(), String> {
    if session.path.is_some() { Ok(()) } else { Err("the project has not been saved yet".to_string()) }
}

/// Everything a failed attempt must put back.
struct Snapshot {
    project: Arc<Project>,
    undo_len: usize,
    redo: Vec<(String, Arc<Project>, Option<String>)>,
    state: EditorState,
    revision: u64,
    saved_revision: u64,
    journal_len: usize,
}

impl Snapshot {
    fn take(session: &Session) -> Self {
        Self {
            project: Arc::clone(&session.project),
            undo_len: session.history.undo.len(),
            redo: session.history.redo.clone(),
            state: session.state.clone(),
            revision: session.revision,
            saved_revision: session.saved_revision,
            journal_len: session.journal.len(),
        }
    }

    fn restore(self, session: &mut Session) {
        session.set_project(self.project);
        session.history.undo.truncate(self.undo_len);
        session.history.redo = self.redo;
        session.state = self.state;
        // A failed attempt must not leave a clean project marked as edited.
        session.revision = self.revision;
        session.saved_revision = self.saved_revision;
        session.journal.truncate(self.journal_len);
    }
}

/// Nest the selected video, export OTIO, hand it to `peer`, and swap in the linked item.
///
/// When the selected video clips play a linked item already, nothing is nested: see
/// [`crate::reopen::reopen`].
///
/// On failure the project, editor state, undo history, and revision return to the snapshot, and
/// the link is closed on the peer. Success writes no `.mov`: that path is the future result.
pub fn edit_in(session: &mut Session, peer: &mut dyn LinkPeer, backend: &dyn activate::ActivationBackend, opts: &EditInOptions) -> Result<Value, EditInError> {
    require_saved(session).map_err(EditInError::Refused)?;
    edit_in_enabled(session).map_err(EditInError::Refused)?;
    // A clip that plays a linked result is edited where it was made: nesting it again would
    // link a second result that nothing ever rendered.
    if let Some(linked) = crate::reopen::linked_selection(session).map_err(EditInError::Refused)? {
        return crate::reopen::reopen(session, peer, backend, opts, &linked);
    }
    let snapshot = Snapshot::take(session);
    let undo_len = snapshot.undo_len;
    let links_dir = session.path.as_deref().and_then(links_dir_of);
    let links_dir_existed = links_dir.as_ref().is_some_and(|dir| dir.exists());
    let mut opened = None;
    match edit_in_body(session, peer, opts, undo_len, &mut opened) {
        Ok(mut reply) => {
            // The nest replaced the selected clips with a new one, so the undo would leave nothing selected.
            if let Some((_, before, _)) = session.history.undo.get(undo_len).cloned() {
                session.history.remember_selection(&before, snapshot.state.selection.clone());
            }
            activate_peer(peer, backend, &mut reply);
            Ok(reply)
        }
        Err(mut err) => {
            snapshot.restore(session);
            // `result_file` made the folder for the result movie. Take it away again unless it was
            // there before or EffectCraft already wrote into it (then `remove_dir` refuses).
            if let Some(dir) = links_dir.filter(|_| !links_dir_existed) {
                let _ = std::fs::remove_dir(dir);
            }
            if let Some(link_id) = opened
                && let Err(close) = peer.close(&link_id)
            {
                err = EditInError::Failed(format!("{err}; closing the EffectCraft link also failed: {close}"));
            }
            Err(err)
        }
    }
}

/// `app.activate` after the swap. The link exists whatever this answers, so a failure here is
/// reported in the reply and does not undo the link.
pub(crate) fn activate_peer(peer: &mut dyn LinkPeer, backend: &dyn activate::ActivationBackend, reply: &mut Value) {
    let link_id = reply.get("link_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let token = activate::mint_token(backend);
    let yielded = activate::yield_to(backend, EFFECTCRAFT_BUNDLE_ID);
    let outcome = peer.activate(token.as_deref(), &link_id);
    let Some(map) = reply.as_object_mut() else { return };
    match outcome {
        Ok(activated) => {
            map.insert("activated".to_string(), activated_json(activated));
            if !yielded {
                map.insert("activation_error".to_string(), Value::String("could not yield activation to EffectCraft".to_string()));
            }
        }
        Err(err) => {
            map.insert("activated".to_string(), Value::Bool(false));
            map.insert("activation_error".to_string(), Value::String(err.to_string()));
        }
    }
}

/// The wire form of `app.activate`: `true`, `false`, or `"requested"`.
pub fn activated_json(activated: Activated) -> Value {
    match activated {
        Activated::Yes => Value::Bool(true),
        Activated::No => Value::Bool(false),
        Activated::Requested => Value::String("requested".to_string()),
    }
}

fn edit_in_body(
    session: &mut Session,
    peer: &mut dyn LinkPeer,
    opts: &EditInOptions,
    undo_len: usize,
    opened_link: &mut Option<String>,
) -> Result<Value, EditInError> {
    let place = Place::capture(session, place_view(session)).map_err(|err| EditInError::Failed(err.to_string()))?;
    let clip_name = nest_name(session);
    let nest_label = format!("{clip_name}{NEST_SUFFIX}");
    let nested = session.execute("clip.nest", json!({ "name": nest_label })).map_err(failed)?;
    let (nest_sequence, clip_ids) = nest_result(&nested)?;
    let video_clips = video_clip_ids(session, &clip_ids)?;
    // Effects EffectCraft cannot import stay on the outer clip, so the payload never names them.
    let active = session.state.active_sequence.ok_or_else(|| EditInError::Failed("no sequence is open".to_string()))?;
    let lifted = crate::lift::lift_unimported(session, active, nest_sequence, &video_clips).map_err(failed)?;
    // Sources with no file are rendered to movies first. The export reads the project that names them.
    let exported = crate::prepare::resolve(session, nest_sequence, &crate::prepare::PrepareOptions { cache_dir: opts.cache_dir.clone(), wait: opts.wait })?;
    let data = export_otio(&exported, nest_sequence)?;
    let link_id = new_uuid()?;
    // FilmCraft Project has no document id, so the source id is a new uuid.
    let doc_id = new_uuid()?;
    let project_path = session.path.clone().ok_or_else(|| EditInError::Failed("the project has not been saved yet".to_string()))?;
    let result_path = result_file(&project_path, &link_id)?;
    let params = open_params(&link_id, &data, &place, &result_path, &doc_id, nest_sequence);
    let reply = peer.open(params)?;
    *opened_link = Some(link_id.clone());
    let unsupported = parse_unsupported(&reply)?;
    let record = link_record(&link_id, &result_path, nest_sequence, &reply)?;
    let media_name = linked_item_name(&session.project, &clip_name);
    let media_id = offline_media(session, &media_name, &result_path, &record)?;
    write_link(session, media_id, &video_clips, &record)?;
    collapse_history(session, undo_len, "link.editIn");
    Ok(json!({
        "activated": false,
        "link_id": link_id,
        "sequence": nest_sequence.0,
        "item": media_id.0,
        "result_path": result_path,
        "unsupported": unsupported,
        "lifted_effects": lifted,
        "instance": peer.instance(),
    }))
}

/// `result.unsupported` of `link.open`. Absent means nothing was lost.
fn parse_unsupported(reply: &Value) -> Result<Vec<Unsupported>, EditInError> {
    match reply.get("unsupported") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(list) => serde_json::from_value(list.clone()).map_err(|err| EditInError::Failed(format!("the open result `unsupported` is invalid: {err}"))),
    }
}

pub(crate) fn failed(err: EngineError) -> EditInError {
    EditInError::Failed(err.to_string())
}

pub(crate) fn place_view(session: &Session) -> PlaceView {
    match session.state.active_sequence.and_then(|id| session.state.timeline_views.get(&id)) {
        Some(view) => PlaceView { zoom: view.pps, scroll_s: view.scroll },
        None => PlaceView { zoom: 0.0, scroll_s: 0.0 },
    }
}

fn nest_name(session: &Session) -> String {
    let name = session.active_sequence().and_then(|sequence| {
        let clip_id = session.state.selection.first()?;
        sequence.find_item(*clip_id).map(|(_, clip)| clip.name.clone())
    });
    match name {
        Some(name) if !name.is_empty() => name,
        _ => "Linked".to_string(),
    }
}

/// Ends the name of the nested sequence. EffectCraft names its comp after the sequence, so the
/// comp ends with it too. Neither carries the name of the source clip alone.
const NEST_SUFFIX: &str = " (nest)";

/// Ends the name of the item that stands for the EffectCraft result.
const LINKED_SUFFIX: &str = " (EffectCraft)";

/// Name for the linked media item: `<name> (EffectCraft)`, then `... 2`, `... 3` when a project
/// item has that name already. `name` is the first clip's name. The nest and the EffectCraft comp
/// carry [`NEST_SUFFIX`], so the clip, the nest and the result never share a name. A clip that is
/// linked already carries the suffix, which is not stacked a second time.
fn linked_item_name(project: &Project, name: &str) -> String {
    let base = format!("{}{LINKED_SUFFIX}", name.strip_suffix(LINKED_SUFFIX).unwrap_or(name));
    let taken = |candidate: &str| project.items.values().any(|item| item.name == candidate);
    if !taken(&base) {
        return base;
    }
    (2u32..).map(|n| format!("{base} {n}")).find(|candidate| !taken(candidate)).unwrap_or(base)
}

fn nest_result(value: &Value) -> Result<(ItemId, Vec<u64>), EditInError> {
    let sequence = value.get("sequence").and_then(Value::as_u64).ok_or_else(|| EditInError::Failed("clip.nest returned no sequence".to_string()))?;
    let clips = value.get("clips").and_then(Value::as_array).ok_or_else(|| EditInError::Failed("clip.nest returned no clips".to_string()))?;
    let mut ids = Vec::with_capacity(clips.len());
    for clip in clips {
        let id = clip.as_u64().ok_or_else(|| EditInError::Failed("clip.nest returned a clip id that is not a number".to_string()))?;
        ids.push(id);
    }
    Ok((ItemId(sequence), ids))
}

fn video_clip_ids(session: &Session, ids: &[u64]) -> Result<Vec<ClipId>, EditInError> {
    let sequence = session.active_sequence().ok_or_else(|| EditInError::Failed("no sequence is open".to_string()))?;
    let mut clips = Vec::new();
    for id in ids {
        let clip_id = ClipId(*id);
        let Some((track_id, _)) = sequence.find_item(clip_id) else { continue };
        if sequence.track(track_id).is_some_and(|track| track.kind == TrackKind::Video) {
            clips.push(clip_id);
        }
    }
    if clips.is_empty() {
        return Err(EditInError::Failed("the nest has no video clip to link".to_string()));
    }
    Ok(clips)
}

pub(crate) fn export_otio(project: &Project, sequence: ItemId) -> Result<String, EditInError> {
    let (bytes, _report) =
        filmcraft_interchange::export(project, sequence, filmcraft_interchange::Format::Otio, &filmcraft_interchange::ExportOptions::default())
            .map_err(|err| EditInError::Failed(err.to_string()))?;
    if bytes.is_empty() {
        return Err(EditInError::Failed("the OTIO export is empty".to_string()));
    }
    let data = String::from_utf8(bytes).map_err(|err| EditInError::Failed(err.to_string()))?;
    if data.is_empty() {
        return Err(EditInError::Failed("the OTIO export is empty".to_string()));
    }
    Ok(data)
}

pub(crate) fn new_uuid() -> Result<String, EditInError> {
    craft_ipc::random_uuid().map_err(|err| EditInError::Failed(err.to_string()))
}

/// `<project dir>/Project Links`.
fn links_dir_of(project_path: &str) -> Option<std::path::PathBuf> {
    let parent = std::path::Path::new(project_path).parent().filter(|dir| !dir.as_os_str().is_empty())?;
    Some(parent.join("Project Links"))
}

fn result_file(project_path: &str, link_id: &str) -> Result<String, EditInError> {
    let dir = links_dir_of(project_path).ok_or_else(|| EditInError::Failed("the project path has no directory".to_string()))?;
    std::fs::create_dir_all(&dir).map_err(|err| EditInError::Failed(format!("cannot create Project Links: {err}")))?;
    let file = dir.join(format!("{link_id}.mov"));
    file.to_str().map(str::to_string).ok_or_else(|| EditInError::Failed("the result path is not UTF-8".to_string()))
}

pub(crate) fn open_params(link_id: &str, data: &str, place: &Place, result_path: &str, doc_id: &str, nest_sequence: ItemId) -> Value {
    json!({
        "link_id": link_id,
        "payload": {"format": "otio-json", "data": data},
        "place": place,
        "result_path": result_path,
        "source": {"app": "filmcraft", "doc_id": doc_id, "item": nest_sequence.0.to_string()},
    })
}

fn link_record(link_id: &str, result_path: &str, nest_sequence: ItemId, opened: &Value) -> Result<LinkRecord, EditInError> {
    let target_comp = required_text(opened, "comp")?;
    let target_project = required_text(opened, "project")?;
    let format_value = opened.get("format").cloned().ok_or_else(|| EditInError::Failed("the open result has no `format`".to_string()))?;
    let format: Format = serde_json::from_value(format_value).map_err(|err| EditInError::Failed(format!("the open result format is invalid: {err}")))?;
    let record = LinkRecord {
        v: 1,
        link_id: link_id.to_string(),
        target_app: "effectcraft".to_string(),
        target_project,
        target_comp,
        nested_sequence: nest_sequence.0.to_string(),
        result_path: result_path.to_string(),
        result_epoch: 0,
        format,
        created: rfc3339_utc(std::time::SystemTime::now())?,
        extra: Map::new(),
    };
    record.validate().map_err(EditInError::Failed)?;
    Ok(record)
}

pub(crate) fn required_text(value: &Value, key: &str) -> Result<String, EditInError> {
    match value.get(key).and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        Some(_) => Err(EditInError::Failed(format!("the open result `{key}` is empty"))),
        None => Err(EditInError::Failed(format!("the open result has no `{key}`"))),
    }
}

fn offline_media(session: &mut Session, name: &str, path: &str, record: &LinkRecord) -> Result<ItemId, EditInError> {
    let width = u32::try_from(record.format.w).map_err(|_| EditInError::Failed("format.w does not fit in u32".to_string()))?;
    let height = u32::try_from(record.format.h).map_err(|_| EditInError::Failed("format.h does not fit in u32".to_string()))?;
    let fps = record.format.rate_num as f64 / record.format.rate_den as f64;
    let ticks = i64::try_from(record.format.duration_ticks).map_err(|_| EditInError::Failed("format.duration_ticks does not fit in a tick".to_string()))?;
    let made = session
        .execute(
            "file.newOfflineFile",
            json!({
                "name": name,
                "fileName": path,
                "video": true,
                "audio": false,
                "width": width,
                "height": height,
                "fps": fps,
                "seconds": Tick(ticks).seconds(),
            }),
        )
        .map_err(failed)?;
    let id = made.get("item").and_then(Value::as_u64).ok_or_else(|| EditInError::Failed("file.newOfflineFile returned no item".to_string()))?;
    Ok(ItemId(id))
}

fn write_link(session: &mut Session, media_id: ItemId, clips: &[ClipId], record: &LinkRecord) -> Result<(), EditInError> {
    let active = session.state.active_sequence.ok_or_else(|| EditInError::Failed("no sequence is open".to_string()))?;
    let record_json = serde_json::to_string(record).map_err(|err| EditInError::Failed(err.to_string()))?;
    let result_path = record.result_path.clone();
    let alpha = record.format.alpha;
    let clips = clips.to_vec();
    session
        .edit("link.editIn", move |project, _state| {
            swap_nest_clips(project, active, media_id, &clips)?;
            let item = project.item_mut(media_id).ok_or_else(|| EngineError::Other("the offline item is missing".into()))?;
            if alpha && let Some(video) = item.as_media_mut().and_then(|media| media.info.video.as_mut()) {
                video.has_alpha = true;
            }
            // `file.newOfflineFile` keeps only a file name. The result lives at a full path.
            if let Some(media) = item.as_media_mut() {
                media.media = MediaRef::File { path: result_path };
            }
            item.metadata.insert("craft.link".to_string(), record_json);
            Ok(())
        })
        .map_err(failed)
}

fn swap_nest_clips(project: &mut Project, active: ItemId, media_id: ItemId, clips: &[ClipId]) -> Result<(), EngineError> {
    let (name, label) = {
        let item = project.item(media_id).ok_or_else(|| EngineError::Other("the offline item is missing".into()))?;
        (item.name.clone(), item.label)
    };
    let sequence = project.sequence_mut(active).ok_or(EngineError::NoSequence)?;
    let mut swapped = false;
    for clip_id in clips {
        let Some((track_id, _)) = sequence.find_item(*clip_id) else { continue };
        if sequence.track(track_id).is_none_or(|track| track.kind != TrackKind::Video) {
            continue;
        }
        let Some((_, clip)) = sequence.find_item_mut(*clip_id) else { continue };
        clip.item = media_id;
        clip.name = name.clone();
        clip.label = label;
        clip.source_in = Tick::ZERO;
        clip.frame_hold = None;
        clip.multicam = None;
        swapped = true;
    }
    if !swapped {
        return Err(EngineError::Other("the nest has no video clip to link".into()));
    }
    sequence.check().map_err(EngineError::Other)?;
    Ok(())
}

fn rfc3339_utc(time: std::time::SystemTime) -> Result<String, EditInError> {
    let duration = time.duration_since(std::time::UNIX_EPOCH).map_err(|err| EditInError::Failed(err.to_string()))?;
    let days = i64::try_from(duration.as_secs() / 86_400).map_err(|_| EditInError::Failed("the clock is out of range".to_string()))?;
    let tod = duration.as_secs() % 86_400;
    let (year, month, day) = civil_from_days(days).ok_or_else(|| EditInError::Failed("the clock is out of range".to_string()))?;
    if !(0..=9999).contains(&year) {
        return Err(EditInError::Failed("the clock is out of range".to_string()));
    }
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    Ok(format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"))
}

/// Howard Hinnant `civil_from_days`. Day 0 is 1970-01-01.
fn civil_from_days(days: i64) -> Option<(i32, u32, u32)> {
    let z = days.checked_add(719_468)?;
    let era = if z >= 0 { z } else { z.checked_sub(146_096)? } / 146_097;
    let doe = u32::try_from(z.checked_sub(era.checked_mul(146_097)?)?).ok()?;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut year = i64::from(yoe) + era.checked_mul(400)?;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year = year.checked_add(1)?;
    }
    Some((i32::try_from(year).ok()?, month, day))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::cell::RefCell;

    use craft_ipc::activate::{ActivationBackend, FocusResult};
    use filmcraft_engine::Session;
    use filmcraft_engine::project::{ClipId, ItemId, MediaRef, SequenceSettings};
    use filmcraft_engine::time::FrameRate;
    use serde_json::{Value, json};

    use super::*;
    use crate::fixtures::{TempProject, clip, project_json, session_with, video_session};
    use crate::host::Headless;
    use crate::notice::{self, LinkNotice};

    #[test]
    fn linked_item_names_say_effectcraft_and_stay_unique() {
        let mut project = Project::new("names");
        assert_eq!(linked_item_name(&project, "clipA.mp4"), "clipA.mp4 (EffectCraft)");
        // A clip that is linked already is not suffixed twice.
        assert_eq!(linked_item_name(&project, "clipA.mp4 (EffectCraft)"), "clipA.mp4 (EffectCraft)");
        let first = project.new_sequence("clipA.mp4 (EffectCraft)", Default::default(), 1, 0, None);
        assert_eq!(project.item(first).unwrap().name, "clipA.mp4 (EffectCraft)");
        assert_eq!(linked_item_name(&project, "clipA.mp4"), "clipA.mp4 (EffectCraft) 2");
        project.new_sequence("clipA.mp4 (EffectCraft) 2", Default::default(), 1, 0, None);
        assert_eq!(linked_item_name(&project, "clipA.mp4"), "clipA.mp4 (EffectCraft) 3");
    }

    /// A peer that records what it was asked and answers from fixed values.
    struct Recording {
        calls: Vec<Value>,
        closed: Vec<String>,
        closed_reply: Closed,
        activations: Vec<(Option<String>, String)>,
        attaches: Vec<(String, String, String)>,
        attached: Result<Attached, EditInError>,
        result: Result<Value, EditInError>,
        activation: Result<Activated, EditInError>,
    }

    impl Recording {
        fn ok() -> Self {
            Self {
                calls: Vec::new(),
                closed: Vec::new(),
                closed_reply: Closed::default(),
                activations: Vec::new(),
                attaches: Vec::new(),
                attached: Ok(Attached::Reopened),
                result: Ok(effectcraft_result()),
                activation: Ok(Activated::Requested),
            }
        }
        fn failing(err: EditInError) -> Self {
            Self { result: Err(err), ..Self::ok() }
        }
    }

    impl LinkPeer for Recording {
        fn instance(&self) -> &str {
            "instance-1"
        }
        fn open(&mut self, params: Value) -> Result<Value, EditInError> {
            self.calls.push(params);
            self.result.clone()
        }
        fn close(&mut self, link_id: &str) -> Result<Closed, EditInError> {
            self.closed.push(link_id.to_string());
            Ok(self.closed_reply.clone())
        }
        fn activate(&mut self, token: Option<&str>, link_id: &str) -> Result<Activated, EditInError> {
            self.activations.push((token.map(str::to_string), link_id.to_string()));
            self.activation.clone()
        }
        fn attach(&mut self, link_id: &str, project: &str, comp: &str) -> Result<Attached, EditInError> {
            self.attaches.push((link_id.to_string(), project.to_string(), comp.to_string()));
            self.attached.clone()
        }
    }

    fn effectcraft_result() -> Value {
        json!({
            "comp": "2",
            "project": "/tmp/ec.ecproj",
            "format": {"w": 320, "h": 180, "rate_num": 24, "rate_den": 1, "duration_ticks": 24, "alpha": false}
        })
    }

    fn run(session: &mut Session, peer: &mut Recording) -> Result<Value, EditInError> {
        edit_in(session, peer, &Headless, &EditInOptions::default())
    }

    /// Everything a failed attempt must leave as it found it.
    struct Before {
        json: String,
        undo: usize,
        redo: usize,
        revision: u64,
        saved: u64,
        journal: usize,
        selection: Vec<ClipId>,
        dirty: bool,
    }

    impl Before {
        fn of(session: &Session) -> Self {
            Self {
                json: project_json(session),
                undo: session.history.undo.len(),
                redo: session.history.redo.len(),
                revision: session.revision,
                saved: session.saved_revision,
                journal: session.journal.len(),
                selection: session.state.selection.clone(),
                dirty: session.is_dirty(),
            }
        }

        /// After an undo: the project, selection and undo length are as before. The step waits in redo.
        fn assert_restored(&self, session: &Session) {
            assert_eq!(project_json(session), self.json, "project JSON changed");
            assert_eq!(session.history.undo.len(), self.undo, "undo length");
            assert_eq!(session.state.selection, self.selection, "selection");
        }

        fn assert_same(&self, session: &Session) {
            assert_eq!(project_json(session), self.json, "project JSON changed");
            assert_eq!(session.history.undo.len(), self.undo, "undo length");
            assert_eq!(session.history.redo.len(), self.redo, "redo length");
            assert_eq!(session.revision, self.revision, "revision");
            assert_eq!(session.saved_revision, self.saved, "saved revision");
            assert_eq!(session.journal.len(), self.journal, "journal length");
            assert_eq!(session.state.selection, self.selection, "selection");
            assert_eq!(session.is_dirty(), self.dirty, "dirty flag");
        }
    }

    #[test]
    fn edit_in_refuses_an_unsaved_project() {
        let (mut session, _clip_id) = video_session();
        assert!(session.path.is_none());
        let before = Before::of(&session);
        let mut peer = Recording::ok();
        let err = run(&mut session, &mut peer).expect_err("unsaved");
        assert_eq!(err, EditInError::Refused("the project has not been saved yet".to_string()));
        assert!(peer.calls.is_empty(), "the peer was called");
        before.assert_same(&session);
    }

    /// The menu saves first. A call that skips the menu (control channel, agent) gets the refusal
    /// before the command looks for EffectCraft, so no app is started for a project with no folder.
    #[test]
    fn the_command_refuses_an_unsaved_project_before_it_looks_for_effectcraft() {
        let (mut session, _clip_id) = video_session();
        assert!(session.path.is_none());
        let before = Before::of(&session);
        let err = crate::commands::run_edit_in(&mut session, &json!({}), &crate::host::host()).expect_err("unsaved");
        assert_eq!(err, EditInError::Refused("the project has not been saved yet".to_string()));
        before.assert_same(&session);
    }

    #[test]
    fn edit_in_nests_exports_and_writes_the_link_record() {
        let saved = TempProject::new("");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let before = Arc::clone(&session.project);
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("edit in");

        assert_eq!(reply["activated"], "requested", "the peer said requested");
        assert_eq!(peer.calls.len(), 1);
        let params = &peer.calls[0];
        let mut keys: Vec<&str> = params.as_object().expect("object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["link_id", "payload", "place", "result_path", "source"]);
        assert_eq!(params["payload"]["format"], "otio-json");
        let data = params["payload"]["data"].as_str().expect("otio text");
        assert!(data.contains("OTIO_SCHEMA"), "{data}");
        assert!(params["place"]["seconds"].is_number());
        assert!(params["place"].get("selection").is_some());
        let result_path = params["result_path"].as_str().expect("result path");
        assert!(result_path.contains("Project Links"), "{result_path}");
        assert!(result_path.ends_with(".mov"), "{result_path}");
        assert!(!std::path::Path::new(result_path).exists(), "the .mov was written");
        assert_eq!(params["source"]["app"], "filmcraft");
        assert_eq!(params["link_id"], reply["link_id"]);

        assert_eq!(session.history.undo.len(), 1, "nest, item, swap and metadata are one undo step");
        assert_eq!(session.history.undo[0].0, "link.editIn");
        assert!(Arc::ptr_eq(&before, &session.history.undo[0].1));

        let mut records = Vec::new();
        for item in session.project.items.values() {
            if let Some(text) = item.metadata.get("craft.link") {
                let record: LinkRecord = serde_json::from_str(text).expect("json");
                record.validate().expect("valid link record");
                records.push(record);
            }
        }
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.link_id, params["link_id"].as_str().unwrap());
        assert_eq!(record.target_comp, "2");
        assert_eq!(record.target_project, "/tmp/ec.ecproj");
        assert_eq!(record.result_path, result_path);
        assert_eq!((record.format.w, record.format.h, record.format.rate_num, record.format.rate_den, record.format.duration_ticks), (320, 180, 24, 1, 24));
        let nest_id = ItemId(record.nested_sequence.parse().unwrap());
        assert!(session.project.item(nest_id).expect("nest item").as_sequence().is_some());
        let media_id = ItemId(reply["item"].as_u64().unwrap());
        let media_item = session.project.item(media_id).expect("media");
        let media = media_item.as_media().expect("media clip");
        assert!(media.offline);
        match &media.media {
            MediaRef::File { path } => assert_eq!(path, result_path),
            other => panic!("expected a file path, got {other:?}"),
        }
        let points = session.active_sequence().expect("sequence").video_tracks.iter().flat_map(|track| track.items.iter()).any(|clip| clip.item == media_id);
        assert!(points, "the nest clip does not point at the offline media");
    }

    #[test]
    fn undo_after_edit_in_restores_the_project_json_byte_for_byte() {
        let saved = TempProject::new("undo");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let before = project_json(&session);
        let mut peer = Recording::ok();
        run(&mut session, &mut peer).expect("edit in");
        assert_ne!(project_json(&session), before, "edit in changed nothing");
        assert_eq!(session.undo().as_deref(), Some("link.editIn"));
        assert_eq!(project_json(&session), before, "one undo must put the original clips back");
        assert!(session.undo().is_none(), "a second undo found another step");
    }

    /// The nest replaces the selected clip with a new one, and the undo used to leave nothing selected.
    #[test]
    fn undo_after_edit_in_selects_the_clips_that_were_selected() {
        let saved = TempProject::new("undo-selection");
        let (mut session, clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        run(&mut session, &mut peer).expect("edit in");
        let linked_selection = session.state.selection.clone();
        assert_ne!(linked_selection, vec![clip_id], "the swap kept the old clip selected, so the test proves nothing");
        assert_eq!(session.undo().as_deref(), Some("link.editIn"));
        assert_eq!(session.state.selection, vec![clip_id], "undo must select the clip the user had selected");
        assert_eq!(session.redo().as_deref(), Some("link.editIn"));
        assert_eq!(session.state.selection, linked_selection, "redo must select what the link left selected");
        assert_eq!(session.undo().as_deref(), Some("link.editIn"));
        assert_eq!(session.state.selection, vec![clip_id], "a second undo must restore it again");
    }

    /// The nest and the EffectCraft comp (named after the OTIO timeline) differ from the clip.
    #[test]
    fn the_nest_and_the_comp_are_named_apart_from_the_clip() {
        let saved = TempProject::new("names");
        let (mut session, clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let clip_name = session.active_sequence().unwrap().find_item(clip_id).unwrap().1.name.clone();
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("edit in");
        let timeline: Value = serde_json::from_str(peer.calls[0]["payload"]["data"].as_str().unwrap()).expect("otio json");
        assert_eq!(timeline["name"], json!(format!("{clip_name} (nest)")), "EffectCraft names the comp after this");
        let (_, record) = session
            .project
            .items
            .iter()
            .find_map(|(id, item)| item.metadata.get("craft.link").map(|text| (*id, serde_json::from_str::<LinkRecord>(text).unwrap())))
            .expect("record");
        let nest = session.project.item(ItemId(record.nested_sequence.parse().unwrap())).unwrap();
        assert_eq!(nest.name, format!("{clip_name} (nest)"));
        let media = session.project.item(ItemId(reply["item"].as_u64().unwrap())).unwrap();
        assert_eq!(media.name, format!("{clip_name} (EffectCraft)"));
        let names = [clip_name.as_str(), nest.name.as_str(), media.name.as_str()];
        assert_eq!(names.iter().collect::<std::collections::BTreeSet<_>>().len(), 3, "{names:?}");
    }

    #[test]
    fn missing_effectcraft_leaves_the_project_untouched() {
        let saved = TempProject::new("missing");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let before = Before::of(&session);
        let mut peer = Recording::failing(EditInError::NotInstalled(InstallAction::OpenUrl("https://example.invalid/".to_string())));
        let err = run(&mut session, &mut peer).expect_err("missing");
        assert_eq!(err.to_string(), "EffectCraft is not installed");
        assert!(matches!(err.prompt(), Some(Prompt::NotInstalled { .. })));
        assert_eq!(peer.calls.len(), 1, "the peer was not asked");
        before.assert_same(&session);
        let links = std::path::Path::new(&saved.path).parent().unwrap().join("Project Links");
        assert!(!links.exists(), "the Project Links folder was left behind");
    }

    #[test]
    fn a_failed_open_restores_a_dirty_project_and_its_history() {
        let saved = TempProject::new("dirty");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        // Real earlier edits: a dirty project with one undo entry and one redo entry.
        session
            .edit("rename a", |p, _s| {
                p.name = "A".to_string();
                Ok(())
            })
            .expect("edit a");
        session
            .edit("rename b", |p, _s| {
                p.name = "B".to_string();
                Ok(())
            })
            .expect("edit b");
        assert_eq!(session.undo().as_deref(), Some("rename b"));
        assert_eq!(session.history.redo.len(), 1);
        assert!(session.is_dirty());
        let before = Before::of(&session);
        let mut peer = Recording::failing(EditInError::Failed("EffectCraft refused: payload_invalid".to_string()));
        let err = run(&mut session, &mut peer).expect_err("open failed");
        assert_eq!(err, EditInError::Failed("EffectCraft refused: payload_invalid".to_string()));
        before.assert_same(&session);
    }

    #[test]
    fn a_bad_reply_after_the_open_restores_the_project_and_closes_the_link() {
        let saved = TempProject::new("badreply");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let before = Before::of(&session);
        let mut peer = Recording::ok();
        peer.result = Ok(json!({"comp": "2", "project": "/tmp/ec.ecproj"})); // no `format`
        let err = run(&mut session, &mut peer).expect_err("bad reply");
        assert!(err.to_string().contains("format"), "{err}");
        before.assert_same(&session);
        assert_eq!(peer.closed.len(), 1, "the comp EffectCraft built was not closed");
        assert_eq!(peer.closed[0], peer.calls[0]["link_id"].as_str().unwrap());
    }

    #[test]
    fn unsupported_parts_link_at_once_and_undo_takes_them_back() {
        let saved = TempProject::new("unsupported");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let before = Before::of(&session);
        let mut peer = losing("effect", "Lumetri Color");
        let reply = run(&mut session, &mut peer).expect("nest first: no question before the hand-off");
        assert!(peer.closed.is_empty(), "the comp was closed although the link goes on");
        assert_eq!(reply["unsupported"][0]["name"], "Lumetri Color");
        assert_eq!(reply["instance"], "instance-1");
        assert_eq!(session.history.undo.len(), before.undo + 1);
        let pending = LinkNotice::from_reply(&reply).expect("the user is told afterwards");
        assert_eq!(pending.link_id, reply["link_id"].as_str().unwrap());

        // Undo: the original clip, effects and selection are back in one step, and EffectCraft is told.
        let said = notice::undo(&mut session, Some(&mut peer), &pending).expect("undo");
        assert_eq!(peer.closed, std::slice::from_ref(&pending.link_id));
        assert!(said.contains("Undone"), "{said}");
        before.assert_restored(&session);
    }

    #[test]
    fn undo_after_edits_in_effectcraft_does_not_discard_them() {
        let saved = TempProject::new("undo-edited");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = losing("effect", "Lumetri Color");
        peer.closed_reply = Closed { edited: true, comp: "Shot (nest)".to_string() };
        let reply = run(&mut session, &mut peer).expect("edit in");
        let pending = LinkNotice::from_reply(&reply).unwrap();
        let said = notice::undo(&mut session, Some(&mut peer), &pending).expect("undo");
        assert!(said.contains("Shot (nest)") && said.contains("keeps"), "EffectCraft work must be named, not dropped silently: {said}");
    }

    #[test]
    fn undo_is_refused_once_other_edits_follow() {
        let saved = TempProject::new("undo-stale");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = losing("effect", "Lumetri Color");
        let reply = run(&mut session, &mut peer).expect("edit in");
        let pending = LinkNotice::from_reply(&reply).unwrap();
        session
            .edit("later edit", |p, _| {
                p.name = "later".into();
                Ok(())
            })
            .unwrap();
        assert!(!notice::undoable(&session));
        let err = notice::undo(&mut session, Some(&mut peer), &pending).expect_err("stale");
        assert!(err.contains("other edits"), "{err}");
        assert!(peer.closed.is_empty(), "EffectCraft was told to unlink although nothing was undone");
        assert_eq!(session.project.name, "later");
    }

    #[test]
    fn undo_without_effectcraft_still_restores_and_says_so() {
        let saved = TempProject::new("undo-offline");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let before = Before::of(&session);
        let mut peer = losing("effect", "Lumetri Color");
        let reply = run(&mut session, &mut peer).expect("edit in");
        let pending = LinkNotice::from_reply(&reply).unwrap();
        let said = notice::undo(&mut session, None, &pending).expect("undo");
        assert!(said.contains("could not be reached"), "{said}");
        before.assert_restored(&session);
    }

    fn effect(id: &str) -> filmcraft_engine::project::EffectInstance {
        filmcraft_engine::project::find_effect(id).unwrap_or_else(|| panic!("no effect {id}")).instance()
    }

    /// The effect ids on `clip` in the sequence `id`.
    fn effect_ids(session: &Session, sequence: ItemId, clip: ClipId) -> Vec<String> {
        let seq = session.project.sequence(sequence).expect("sequence");
        seq.find_item(clip).expect("clip").1.effects.iter().map(|e| e.effect.clone()).collect()
    }

    /// The outer clip (the one that points at the linked media item) and the one clip in the nest.
    fn outer_and_inner(session: &Session, reply: &Value) -> (ClipId, ClipId, ItemId, ItemId) {
        let media = ItemId(reply["item"].as_u64().unwrap());
        let active = session.state.active_sequence.unwrap();
        let outer = session.project.sequence(active).unwrap().video_tracks[0].items.iter().find(|c| c.item == media).expect("outer clip").id;
        let nest = ItemId(reply["sequence"].as_u64().unwrap());
        let inner = session.project.sequence(nest).unwrap().video_tracks[0].items[0].id;
        (outer, inner, active, nest)
    }

    fn with_effects(session: &mut Session, clip: ClipId, ids: &[&str]) {
        let active = session.state.active_sequence.unwrap();
        session
            .edit("test effects", |p, _| {
                let (_, c) = p.sequence_mut(active).unwrap().find_item_mut(clip).unwrap();
                c.effects = ids.iter().map(|id| effect(id)).collect();
                Ok(())
            })
            .unwrap();
        // The setup edit is not part of the link's undo history.
        session.history.undo.clear();
    }

    /// EffectCraft answers that it dropped Lumetri (what a payload that still names it would get).
    fn losing(what: &str, name: &str) -> Recording {
        let mut peer = Recording::ok();
        peer.result = Ok(json!({
            "comp": "2", "project": "/tmp/ec.ecproj",
            "format": {"w": 320, "h": 180, "rate_num": 24, "rate_den": 1, "duration_ticks": 24, "alpha": false},
            "unsupported": [{"what": what, "name": name, "kept_as": "none"}]
        }));
        peer
    }

    /// A saved project whose only clip is linked, and that clip selected, the way a user clicks it
    /// after the first hand-off. Returns the linked item and the peer that took the first open.
    fn linked_and_selected(tag: &str) -> (TempProject, Session, ItemId, Recording) {
        let saved = TempProject::new(tag);
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("first edit in");
        let media = ItemId(reply["item"].as_u64().unwrap());
        select_outer(&mut session, media);
        (saved, session, media, peer)
    }

    fn select_outer(session: &mut Session, media: ItemId) {
        let clip = session.active_sequence().unwrap().video_tracks[0].items.iter().find(|clip| clip.item == media).expect("outer clip").id;
        session.state.selection = vec![clip];
    }

    fn items_and_sequences(session: &Session) -> (usize, usize) {
        (session.project.items.len(), session.project.items.values().filter(|item| item.as_sequence().is_some()).count())
    }

    #[test]
    fn a_linked_clip_reopens_its_link_and_changes_nothing() {
        let (_saved, mut session, media, mut peer) = linked_and_selected("reopen");
        let link_id = peer.calls[0]["link_id"].as_str().unwrap().to_string();
        let before = Before::of(&session);
        let counts = items_and_sequences(&session);
        let reply = run(&mut session, &mut peer).expect("reopen");

        before.assert_same(&session);
        assert_eq!(items_and_sequences(&session), counts, "a reopen nested or added an item");
        assert_eq!(peer.calls.len(), 1, "a reopen sent a second link.open");
        assert_eq!(peer.attaches, [(link_id.clone(), "/tmp/ec.ecproj".to_string(), "2".to_string())]);
        assert_eq!(peer.activations.len(), 2, "the first hand-off and the reopen each activate");
        assert_eq!(peer.activations[1].1, link_id);
        assert_eq!((reply["reopened"].clone(), reply["rebuilt"].clone()), (json!(true), json!(false)));
        assert_eq!(reply["item"].as_u64(), Some(media.0));
        assert!(LinkNotice::from_reply(&reply).is_none(), "a reopen must not raise the banner");
        assert!(reply.get("lifted_effects").is_none() && reply.get("unsupported").is_none());
    }

    #[test]
    fn a_reopen_of_a_comp_that_is_gone_opens_it_again_from_the_nest_with_the_same_link() {
        let (_saved, mut session, media, mut peer) = linked_and_selected("rebuild");
        let first = peer.calls[0].clone();
        let record = record_of_item(&session, media);
        peer.attached = Ok(Attached::Missing("no link is open and the sidecar project does not exist".to_string()));
        let counts = items_and_sequences(&session);
        let before = project_json(&session);
        let reply = run(&mut session, &mut peer).expect("rebuild");

        assert_eq!(reply["rebuilt"], true);
        assert_eq!(peer.calls.len(), 2);
        assert_eq!(peer.calls[1]["link_id"], first["link_id"], "the rebuild must keep the link id");
        assert_eq!(peer.calls[1]["result_path"], first["result_path"]);
        assert_eq!(peer.calls[1]["source"]["item"], record.nested_sequence, "the payload comes from the nest of the link");
        assert!(peer.calls[1]["payload"]["data"].as_str().unwrap().contains("OTIO_SCHEMA"));
        assert_eq!(items_and_sequences(&session), counts, "the rebuild made a second item or nest");
        assert_eq!(project_json(&session), before, "EffectCraft named the same comp, so the record is the same");
        assert!(peer.closed.is_empty());
    }

    #[test]
    fn a_rebuild_follows_the_comp_and_sidecar_that_effectcraft_names_now() {
        let (_saved, mut session, media, mut peer) = linked_and_selected("rebuild-moved");
        peer.attached = Ok(Attached::Missing("comp_missing".to_string()));
        peer.result = Ok(json!({
            "comp": "7", "project": "/tmp/other/ec.ecproj",
            "format": {"w": 320, "h": 180, "rate_num": 24, "rate_den": 1, "duration_ticks": 24, "alpha": false}
        }));
        let counts = items_and_sequences(&session);
        run(&mut session, &mut peer).expect("rebuild");
        let record = record_of_item(&session, media);
        assert_eq!((record.target_comp.as_str(), record.target_project.as_str()), ("7", "/tmp/other/ec.ecproj"));
        assert_eq!(items_and_sequences(&session), counts);
        assert_eq!(crate::manager::linked_items(&session.project).len(), 1);
    }

    #[test]
    fn a_rebuild_without_the_nest_says_so_and_changes_nothing() {
        let (_saved, mut session, media, mut peer) = linked_and_selected("rebuild-no-nest");
        let nest = ItemId(record_of_item(&session, media).nested_sequence.parse().unwrap());
        session.project_mut().items.remove(&nest);
        peer.attached = Ok(Attached::Missing("comp_missing".to_string()));
        let before = Before::of(&session);
        let err = run(&mut session, &mut peer).expect_err("no nest");
        assert!(err.to_string().contains("nest of this link is no longer in the project"), "{err}");
        assert_eq!(peer.calls.len(), 1, "link.open went out without a payload to send");
        before.assert_same(&session);
    }

    #[test]
    fn a_failed_rebuild_open_changes_nothing() {
        let (_saved, mut session, _media, mut peer) = linked_and_selected("rebuild-refused");
        peer.attached = Ok(Attached::Missing("comp_missing".to_string()));
        peer.result = Err(EditInError::Failed("EffectCraft refused: payload_invalid".to_string()));
        let before = Before::of(&session);
        let err = run(&mut session, &mut peer).expect_err("refused");
        assert_eq!(err, EditInError::Failed("EffectCraft refused: payload_invalid".to_string()));
        before.assert_same(&session);
    }

    #[test]
    fn a_failed_attach_changes_nothing_and_nests_nothing() {
        let (_saved, mut session, _media, mut peer) = linked_and_selected("attach-busy");
        peer.attached = Err(EditInError::Failed("link is in a project, but the open project has unsaved changes".to_string()));
        let before = Before::of(&session);
        let counts = items_and_sequences(&session);
        let err = run(&mut session, &mut peer).expect_err("busy");
        assert!(err.to_string().contains("unsaved changes"), "{err}");
        assert_eq!(peer.calls.len(), 1);
        before.assert_same(&session);
        assert_eq!(items_and_sequences(&session), counts);
    }

    fn record_of_item(session: &Session, media: ItemId) -> LinkRecord {
        crate::link_ops::record_of(&session.project, media).unwrap().expect("record")
    }

    /// Add an unlinked clip next to the selected one and select both.
    fn select_with_plain_clip(session: &mut Session) {
        let active = session.state.active_sequence.unwrap();
        let rate = FrameRate::FPS_24;
        let plain = clip(9, rate.tick_of(24), rate.tick_of(24));
        let plain_id = plain.id;
        session.project_mut().sequence_mut(active).unwrap().video_tracks[0].items.push(plain);
        session.state.selection.push(plain_id);
    }

    #[test]
    fn a_mixed_selection_is_refused_with_a_reason_and_nests_nothing() {
        let (_saved, mut session, _media, mut peer) = linked_and_selected("mixed");
        select_with_plain_clip(&mut session);
        let before = Before::of(&session);
        let counts = items_and_sequences(&session);
        let reason = "the selection mixes linked and unlinked clips";
        assert_eq!(session.enabled_state("link.editIn"), Err(reason.to_string()), "the menu entry must say why it is grey");
        assert_eq!(edit_in_enabled(&session), Err(reason.to_string()));
        let err = run(&mut session, &mut peer).expect_err("mixed");
        assert_eq!(err, EditInError::Refused(reason.to_string()));
        assert_eq!((peer.calls.len(), peer.attaches.len()), (1, 0), "the peer was asked");
        before.assert_same(&session);
        assert_eq!(items_and_sequences(&session), counts);
    }

    #[test]
    fn clips_of_two_links_are_refused() {
        let (_saved, mut session, media, mut peer) = linked_and_selected("two-links");
        let other = crate::fixtures::linked_offline(&mut session, "99999999-9999-4999-8999-999999999999", "/tmp/fc-other.mov");
        let active = session.state.active_sequence.unwrap();
        let rate = FrameRate::FPS_24;
        let mut second = clip(9, rate.tick_of(24), rate.tick_of(24));
        second.item = other;
        let second_id = second.id;
        session.project_mut().sequence_mut(active).unwrap().video_tracks[0].items.push(second);
        session.state.selection.push(second_id);
        assert_ne!(media, other);
        let before = Before::of(&session);
        let err = run(&mut session, &mut peer).expect_err("two links");
        assert_eq!(err, EditInError::Refused("the selection holds clips of more than one link".to_string()));
        before.assert_same(&session);
        assert!(peer.attaches.is_empty());
    }

    #[test]
    fn an_unlinked_selection_still_nests() {
        let saved = TempProject::new("still-nests");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("edit in");
        assert!(reply.get("reopened").is_none());
        assert!(peer.attaches.is_empty());
        assert_eq!(peer.calls.len(), 1);
    }

    #[test]
    fn effects_effectcraft_cannot_import_move_to_the_outer_clip() {
        let saved = TempProject::new("lift");
        let (mut session, clip_id) = video_session();
        session.path = Some(saved.path.clone());
        with_effects(&mut session, clip_id, &["lumetri", "motion"]);
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("edit in");

        let (outer, inner, active, nest) = outer_and_inner(&session, &reply);
        assert_eq!(effect_ids(&session, active, outer).last().map(String::as_str), Some("lumetri"), "the outer clip must carry the effect");
        assert_eq!(effect_ids(&session, nest, inner), ["motion"], "the inner clip keeps what EffectCraft imports");
        assert_eq!(reply["lifted_effects"], json!(["Color Grading"]));
        let details = LinkNotice::from_reply(&reply).expect("a notice").details().join(" ");
        assert_eq!(details, "Unimportable FilmCraft effects: Color Grading");
        let payload = peer.calls[0]["payload"]["data"].as_str().unwrap();
        assert!(!payload.contains("Lumetri") && !payload.contains("lumetri"), "the payload still names the effect");
        assert!(payload.contains("Motion") || payload.contains("motion"), "the effect EffectCraft imports left the payload");
    }

    #[test]
    fn lifted_effects_survive_the_back_round_trip() {
        let saved = TempProject::new("lift-back");
        let (mut session, clip_id) = video_session();
        session.path = Some(saved.path.clone());
        with_effects(&mut session, clip_id, &["lumetri"]);
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("edit in");
        let (outer, _, active, _) = outer_and_inner(&session, &reply);
        // EffectCraft's Back: `link.focus` with the link id, after it wrote the result over the same path.
        crate::link_ops::focus(&mut session, &json!({"link_id": reply["link_id"], "place": {"seconds": 0.5}})).expect("link.focus");
        assert!(effect_ids(&session, active, outer).contains(&"lumetri".to_string()), "Back dropped the lifted effect");
    }

    #[test]
    fn undo_puts_the_lifted_effects_back_in_one_step() {
        let saved = TempProject::new("lift-undo");
        let (mut session, clip_id) = video_session();
        session.path = Some(saved.path.clone());
        with_effects(&mut session, clip_id, &["lumetri", "motion"]);
        let before = project_json(&session);
        let mut peer = Recording::ok();
        run(&mut session, &mut peer).expect("edit in");
        assert_eq!(session.history.undo.len(), 1, "nest, lift, swap and metadata are one undo step");
        assert_eq!(session.undo().as_deref(), Some("link.editIn"));
        assert_eq!(project_json(&session), before, "one undo must restore the effects on the original clip");
        assert!(session.undo().is_none());
    }

    #[test]
    fn an_ambiguous_nest_keeps_its_effects_and_only_informs() {
        let saved = TempProject::new("lift-ambiguous");
        let rate = FrameRate::FPS_24;
        let a = clip(1, rate.tick_of(0), rate.tick_of(12));
        let mut b = clip(2, rate.tick_of(12), rate.tick_of(12));
        b.name = "clip-2".to_string();
        let mut session = session_with(rate, 1, &[(0, a), (0, b)]);
        session.path = Some(saved.path.clone());
        with_effects(&mut session, ClipId(1), &["lumetri"]);
        with_effects(&mut session, ClipId(2), &["brightness_contrast"]);
        session.state.selection = vec![ClipId(1), ClipId(2)];
        let mut peer = losing("import", "effect \"Lumetri Color\" is not imported");
        // No popup: only effects are lost.
        let reply = run(&mut session, &mut peer).expect("effects only: no question");
        assert_eq!(reply["lifted_effects"], json!([]));
        let (_, _, _, nest) = outer_and_inner(&session, &reply);
        let nested: Vec<Vec<String>> =
            session.project.sequence(nest).unwrap().video_tracks[0].items.iter().map(|c| c.effects.iter().map(|e| e.effect.clone()).collect()).collect();
        assert_eq!(nested, [vec!["lumetri".to_string()], vec!["brightness_contrast".to_string()]], "an ambiguous nest must keep its effects inside");
        let details = LinkNotice::from_reply(&reply).expect("an informational notice").details().join(" ");
        assert!(details.starts_with("Unimportable FilmCraft effects: Lumetri Color"), "{details}");
        assert!(!details.contains("do not show on the timeline"), "the banner has no third line: {details}");
        assert!(peer.closed.is_empty(), "the comp was closed although the link goes on");
    }

    /// Two clips on one track with the given effects each, both selected, as a nest of two.
    fn two_clip_session(saved: &TempProject, first: &[&str], second: &[&str]) -> Session {
        let rate = FrameRate::FPS_24;
        let a = clip(1, rate.tick_of(0), rate.tick_of(12));
        let b = clip(2, rate.tick_of(12), rate.tick_of(12));
        let mut session = session_with(rate, 1, &[(0, a), (0, b)]);
        session.path = Some(saved.path.clone());
        with_effects(&mut session, ClipId(1), first);
        with_effects(&mut session, ClipId(2), second);
        session.state.selection = vec![ClipId(1), ClipId(2)];
        session
    }

    #[test]
    fn the_same_grade_on_every_clip_of_the_nest_moves_to_the_outer_clip() {
        let saved = TempProject::new("lift-same");
        let mut session = two_clip_session(&saved, &["lumetri"], &["lumetri"]);
        let before = project_json(&session);
        let mut peer = Recording::ok();
        let reply = run(&mut session, &mut peer).expect("edit in");
        assert_eq!(reply["lifted_effects"], json!(["Color Grading"]));
        let media = ItemId(reply["item"].as_u64().unwrap());
        let active = session.state.active_sequence.unwrap();
        let outer = session.project.sequence(active).unwrap().video_tracks[0].items.iter().find(|c| c.item == media).unwrap().id;
        assert!(effect_ids(&session, active, outer).contains(&"lumetri".to_string()));
        let nest = ItemId(reply["sequence"].as_u64().unwrap());
        for inner in &session.project.sequence(nest).unwrap().video_tracks[0].items {
            assert!(!inner.effects.iter().any(|e| e.effect == "lumetri"), "an inner clip kept the grade");
        }
        let payload = peer.calls[0]["payload"]["data"].as_str().unwrap();
        assert!(!payload.contains("umetri"), "the payload still names the grade");
        assert_eq!(session.undo().as_deref(), Some("link.editIn"));
        assert_eq!(project_json(&session), before, "one undo restores both clips with their grade");
    }

    #[test]
    fn a_grade_on_overlapping_clips_stays_inside_the_nest() {
        // A grade that runs on each clip before the composite is not the same grade on the composite.
        let saved = TempProject::new("lift-overlap");
        let rate = FrameRate::FPS_24;
        let a = clip(1, rate.tick_of(0), rate.tick_of(24));
        let b = clip(2, rate.tick_of(12), rate.tick_of(24));
        let mut session = session_with(rate, 2, &[(0, a), (1, b)]);
        session.path = Some(saved.path.clone());
        with_effects(&mut session, ClipId(1), &["lumetri"]);
        with_effects(&mut session, ClipId(2), &["lumetri"]);
        session.state.selection = vec![ClipId(1), ClipId(2)];
        let mut peer = losing("import", "effect \"Lumetri Color\" is not imported");
        let reply = run(&mut session, &mut peer).expect("edit in");
        assert_eq!(reply["lifted_effects"], json!([]));
        let nest = ItemId(reply["sequence"].as_u64().unwrap());
        let kept = session
            .project
            .sequence(nest)
            .unwrap()
            .video_tracks
            .iter()
            .flat_map(|t| &t.items)
            .filter(|c| c.effects.iter().any(|e| e.effect == "lumetri"))
            .count();
        assert_eq!(kept, 2, "both clips keep the grade");
        let details = LinkNotice::from_reply(&reply).unwrap().details().join(" ");
        assert!(details.starts_with("Unimportable FilmCraft effects: Lumetri Color"), "{details}");
    }

    #[test]
    fn a_grade_missing_from_one_clip_is_not_lifted_and_the_banner_says_it_is_lost() {
        let saved = TempProject::new("lift-partial");
        let mut session = two_clip_session(&saved, &["lumetri"], &[]);
        let mut peer = losing("import", "effect \"Lumetri Color\" is not imported");
        let reply = run(&mut session, &mut peer).expect("edit in");
        assert_eq!(reply["lifted_effects"], json!([]));
        let nest = ItemId(reply["sequence"].as_u64().unwrap());
        assert!(
            session.project.sequence(nest).unwrap().video_tracks[0].items.iter().any(|c| c.effects.iter().any(|e| e.effect == "lumetri")),
            "the grade must stay in the nest"
        );
        let details = LinkNotice::from_reply(&reply).unwrap().details().join(" ");
        assert!(details.starts_with("Unimportable FilmCraft effects: Lumetri Color"), "{details}");
        assert!(!details.contains("do not show on the timeline"), "{details}");
    }

    #[test]
    fn a_real_fidelity_loss_is_reported_in_the_notice() {
        let saved = TempProject::new("lift-asks");
        let (mut session, clip_id) = video_session();
        session.path = Some(saved.path.clone());
        with_effects(&mut session, clip_id, &["lumetri"]);
        let mut peer = losing("media", "shot.mov");
        let reply = run(&mut session, &mut peer).expect("nest first");
        let pending = LinkNotice::from_reply(&reply).expect("a notice");
        assert!(pending.details().iter().any(|line| line.contains("shot.mov")), "{:?}", pending.details());
        assert!(pending.details().iter().any(|line| line == "Unimportable FilmCraft effects: Color Grading"), "{:?}", pending.details());
    }

    #[test]
    fn the_effect_drop_reads_the_old_and_the_new_wire_form() {
        use crate::lift::is_effect_drop;
        let old = Unsupported { what: "import".into(), name: "effect \"Lumetri Color\" is not imported".into(), kept_as: "none".into() };
        let new = Unsupported { what: "effect".into(), name: "Lumetri Color".into(), kept_as: "none".into() };
        let other = Unsupported { what: "import".into(), name: "caption tracks are not imported".into(), kept_as: "none".into() };
        assert!(is_effect_drop(&old) && is_effect_drop(&new) && !is_effect_drop(&other));
    }

    #[test]
    fn activation_failure_keeps_the_link_and_says_so() {
        let saved = TempProject::new("activation");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        peer.activation = Err(EditInError::Failed("connection closed".to_string()));
        let reply = run(&mut session, &mut peer).expect("the link exists");
        assert_eq!(reply["activated"], false);
        assert_eq!(reply["activation_error"], "connection closed");
        assert_eq!(peer.activations.len(), 1);
        assert_eq!(peer.activations[0].1, reply["link_id"].as_str().unwrap());
        assert_eq!(session.history.undo.len(), 1, "the swap was undone because activation failed");
    }

    /// A window that mints a token and records the order of calls.
    struct Tokened {
        log: RefCell<Vec<String>>,
    }

    impl ActivationBackend for Tokened {
        fn mint_token(&self) -> Option<String> {
            self.log.borrow_mut().push("mint".to_string());
            Some("tok-123".to_string())
        }
        fn set_pending_token(&self, _token: &str) {}
        fn focus(&self) -> FocusResult {
            FocusResult::Pending
        }
        fn request_user_attention(&self) {}
        fn yield_activation_to(&self, _bundle_id: &str) -> bool {
            true
        }
        fn activate_application(&self) -> bool {
            true
        }
    }

    #[test]
    fn the_minted_token_reaches_app_activate() {
        let saved = TempProject::new("token");
        let (mut session, _clip_id) = video_session();
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        let backend = Tokened { log: RefCell::new(Vec::new()) };
        edit_in(&mut session, &mut peer, &backend, &EditInOptions::default()).expect("edit in");
        // `mint_token` returns the backend token on Linux and `None` elsewhere. Either way the
        // peer must receive exactly what the platform rule gave.
        let want = craft_ipc::activate::mint_token(&backend);
        assert_eq!(peer.activations[0].0, want);
        if cfg!(target_os = "linux") {
            assert_eq!(want.as_deref(), Some("tok-123"));
        }
    }

    #[test]
    fn options_reject_wrong_types() {
        assert!(EditInOptions::from_params(&json!({"instance": 4})).is_err());
        let ok = EditInOptions::from_params(&json!({"instance": "abc"})).unwrap();
        assert_eq!(ok, EditInOptions { instance: Some("abc".to_string()), wait: false, cache_dir: None });
        assert!(EditInOptions::from_params(&json!({"wait": true})).unwrap().wait);
        assert!(EditInOptions::from_params(&json!({"wait": "yes"})).is_err());
        assert_eq!(EditInOptions::from_params(&json!({})).unwrap(), EditInOptions::default());
    }

    #[test]
    fn the_static_command_and_the_link_crate_agree_on_enablement() {
        use filmcraft_engine::project::TrackKind;
        let rate = FrameRate::FPS_24;
        let video = clip(1, rate.tick_of(0), rate.tick_of(24));
        let mut session = session_with(rate, 1, &[(0, video)]);
        // no selection, unsaved
        let cases = |session: &Session| (session.is_enabled("link.editIn"), edit_in_enabled(session).is_ok());
        assert_eq!(cases(&session), (false, false));
        session.path = Some("/tmp/x/Film.fcproj".to_string());
        assert_eq!(cases(&session), (false, false), "saved but nothing selected");
        session.state.selection = vec![ClipId(1)];
        assert_eq!(cases(&session), (true, true), "saved with a video clip selected");
        session.path = None;
        assert_eq!(cases(&session), (true, true), "unsaved: the menu saves first");
        assert!(require_saved(&session).is_err(), "but the command itself needs the path");
        // an audio-only selection is not enough
        let mut audio = Session::default();
        let seq = audio.project_mut().new_sequence("Seq", SequenceSettings { frame_rate: rate, ..Default::default() }, 1, 1, None);
        let audio_id = {
            let sequence = audio.project_mut().sequence_mut(seq).unwrap();
            let track = sequence.audio_tracks.first_mut().expect("an audio track");
            assert_eq!(track.kind, TrackKind::Audio);
            track.items.push(clip(99, rate.tick_of(0), rate.tick_of(24)));
            ClipId(99)
        };
        audio.state.active_sequence = Some(seq);
        audio.state.open_sequences = vec![seq];
        audio.path = Some("/tmp/x/Film.fcproj".to_string());
        audio.state.selection = vec![audio_id];
        let session = audio;
        assert_eq!(cases(&session), (false, false), "audio only");
    }

    // ---- sources with no file -----------------------------------------------------------------

    use crate::fixtures::{generator_nest, nested_generator, prepare_serial};

    /// A private link cache next to the project, so tests never read or fill the user's.
    fn options_in(saved: &TempProject, wait: bool) -> EditInOptions {
        let cache = std::path::Path::new(&saved.path).with_file_name("cache").to_string_lossy().into_owned();
        EditInOptions { wait, cache_dir: Some(cache.into()), ..Default::default() }
    }

    /// The first clip of the OTIO that `link.open` carried.
    fn sent_clip(peer: &Recording) -> Value {
        let data = peer.calls[0]["payload"]["data"].as_str().expect("payload data");
        let doc: Value = serde_json::from_str(data).expect("otio json");
        let tracks = doc["tracks"]["children"].as_array().expect("tracks");
        tracks
            .iter()
            .flat_map(|track| track["children"].as_array().cloned().unwrap_or_default())
            .find(|child| child["OTIO_SCHEMA"].as_str().is_some_and(|schema| schema.starts_with("Clip")))
            .expect("a clip in the payload")
    }

    fn file_of(clip: &Value) -> std::path::PathBuf {
        let reference = &clip["media_references"]["DEFAULT_MEDIA"];
        assert_eq!(reference["OTIO_SCHEMA"], "ExternalReference.1", "the clip must name a file: {reference}");
        let url = reference["target_url"].as_str().expect("target_url");
        std::path::PathBuf::from(url.strip_prefix("file://").unwrap_or(url))
    }

    fn frames_of(time: &Value) -> i64 {
        time["value"].as_f64().expect("value") as i64
    }

    /// The bug: a generator has no file, so EffectCraft got a clip it could not open.
    #[test]
    fn a_generator_clip_reaches_effectcraft_as_a_movie_that_exists() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-gen");
        let (mut session, _seq, _clip) = generator_nest(64, 36, 48);
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, true)).expect("edit in");
        let clip = sent_clip(&peer);
        let movie = file_of(&clip);
        assert!(movie.is_file() && std::fs::metadata(&movie).unwrap().len() > 0, "no movie at {}", movie.display());
        // The movie holds frames 0..28 of the source (frames 6..16 are used, 12 of handle each side,
        // cut at the source start and at its 48 frames). So the clip starts 6 frames in.
        let reference = &clip["media_references"]["DEFAULT_MEDIA"];
        assert_eq!(frames_of(&reference["available_range"]["start_time"]), 0);
        assert_eq!(frames_of(&reference["available_range"]["duration"]), 28);
        assert_eq!(frames_of(&clip["source_range"]["start_time"]), 6);
        assert_eq!(frames_of(&clip["source_range"]["duration"]), 10);
        assert_eq!(clip["source_range"]["start_time"]["rate"], 24.0);
    }

    /// The movie keeps alpha and the frame rate of the nest.
    #[test]
    fn the_prepared_movie_is_prores_4444_with_alpha() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-format");
        let (mut session, _seq, _clip) = generator_nest(64, 36, 48);
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, true)).expect("edit in");
        let bytes = std::fs::read(file_of(&sent_clip(&peer))).expect("movie");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("ap4h"), "ProRes 4444 sample entry (ap4h) missing");
        let width_height = (64u16.to_be_bytes(), 36u16.to_be_bytes());
        assert!(bytes.windows(4).any(|w| w == [width_height.0[0], width_height.0[1], width_height.1[0], width_height.1[1]]), "64x36 not in the sample entry");
    }

    #[test]
    fn a_second_call_for_the_same_source_reuses_the_movie() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-hit");
        let (mut first, _, _) = generator_nest(64, 36, 48);
        first.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        edit_in(&mut first, &mut peer, &Headless, &options_in(&saved, true)).expect("first");
        assert_eq!(first.jobs.len(), 1, "the first call renders once");
        let movie = file_of(&sent_clip(&peer));
        let stamp = std::fs::metadata(&movie).unwrap().modified().unwrap();
        // The same source in a new session: no render, same file.
        let (mut second, _, _) = generator_nest(64, 36, 48);
        second.path = Some(saved.path.clone());
        let mut again = Recording::ok();
        edit_in(&mut second, &mut again, &Headless, &options_in(&saved, false)).expect("second, no wait: it must not need one");
        assert!(second.jobs.is_empty(), "a cache hit starts no render");
        assert_eq!(file_of(&sent_clip(&again)), movie);
        assert!(std::fs::metadata(&movie).unwrap().modified().unwrap() >= stamp);
    }

    /// Without `wait` the call returns at once, leaves the project alone, and the same call finishes later.
    #[test]
    fn the_render_never_blocks_the_caller_and_a_rerun_finishes() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-async");
        let (mut session, _seq, _clip) = generator_nest(64, 36, 48);
        session.path = Some(saved.path.clone());
        let before = Before::of(&session);
        let mut peer = Recording::ok();
        let err = edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, false)).expect_err("a render must run first");
        assert!(matches!(err, EditInError::Preparing(_)), "{err:?}");
        assert_eq!(err.to_string(), PREPARING_MESSAGE);
        assert!(matches!(err.prompt(), Some(Prompt::Preparing { .. })));
        assert!(peer.calls.is_empty(), "EffectCraft must not be called before the movie exists");
        before.assert_same(&session);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while crate::prepare::state() != crate::prepare::State::Idle {
            assert!(std::time::Instant::now() < until, "the render did not end");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, false)).expect("the rerun finds the movie");
        assert!(file_of(&sent_clip(&peer)).is_file());
    }

    #[test]
    fn cancel_leaves_no_movie_and_the_next_call_renders_again() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-cancel");
        let (mut session, _seq, _clip) = generator_nest(64, 36, 48);
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        let err = edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, false)).expect_err("renders");
        assert!(matches!(err, EditInError::Preparing(_)));
        crate::prepare::cancel();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while crate::prepare::state() != crate::prepare::State::Idle {
            assert!(std::time::Instant::now() < until, "the cancelled render did not end");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // A cancelled render may still be winding down. Wait for its thread to drop the part file.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let cache = std::path::Path::new(&saved.path).with_file_name("cache");
        let left: Vec<_> = std::fs::read_dir(&cache).unwrap().filter_map(Result::ok).map(|e| e.file_name()).collect();
        assert!(left.iter().all(|name| !name.to_string_lossy().ends_with(".mov")), "a cancelled render left a movie: {left:?}");
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, true)).expect("renders again");
    }

    #[test]
    fn a_render_that_cannot_start_is_a_visible_failure_and_the_project_is_unchanged() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-fail");
        let (mut session, _seq, _clip) = generator_nest(64, 36, 48);
        session.path = Some(saved.path.clone());
        std::fs::create_dir_all(std::path::Path::new(&saved.path).parent().unwrap()).unwrap();
        // The cache path is a file, so no movie can be stored there.
        let blocker = std::path::Path::new(&saved.path).with_file_name("cache");
        std::fs::write(&blocker, b"not a folder").unwrap();
        let before = Before::of(&session);
        let mut peer = Recording::ok();
        let err = edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, true)).expect_err("no cache");
        assert!(matches!(err, EditInError::PrepareFailed(_)), "{err:?}");
        assert!(matches!(err.prompt(), Some(Prompt::PrepareFailed { .. })), "the user must see it");
        assert!(err.to_string().contains("link cache"), "{err}");
        assert!(peer.calls.is_empty());
        before.assert_same(&session);
    }

    /// Nested sequences are sources with no file as well.
    #[test]
    fn a_nested_sequence_clip_is_rendered_too() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-nested");
        let (mut session, _outer, _clip) = nested_generator(64, 36, 48);
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, true)).expect("edit in");
        let clip = sent_clip(&peer);
        let movie = file_of(&clip);
        assert!(movie.is_file());
        // The inner sequence is 10 frames long, so there is no handle to add.
        assert_eq!(frames_of(&clip["media_references"]["DEFAULT_MEDIA"]["available_range"]["duration"]), 10);
        assert_eq!(frames_of(&clip["source_range"]["start_time"]), 0);
    }

    /// Every media reference in the OTIO that `link.open` carried, as `(schema, track kind)`.
    fn sent_references(peer: &Recording) -> Vec<(String, String)> {
        let data = peer.calls[0]["payload"]["data"].as_str().expect("payload data");
        let doc: Value = serde_json::from_str(data).expect("otio json");
        let mut found = Vec::new();
        for track in doc["tracks"]["children"].as_array().expect("tracks") {
            let kind = track["kind"].as_str().unwrap_or_default().to_string();
            for child in track["children"].as_array().cloned().unwrap_or_default() {
                if let Some(refs) = child["media_references"].as_object() {
                    for reference in refs.values() {
                        found.push((reference["OTIO_SCHEMA"].as_str().unwrap_or_default().to_string(), kind.clone()));
                    }
                }
            }
        }
        found
    }

    const SCENES: [filmcraft_media::DemoScene; 3] =
        [filmcraft_media::DemoScene::CityNight, filmcraft_media::DemoScene::Dunes, filmcraft_media::DemoScene::Forest];

    /// The bug from the demo project: a selection of several generator clips. Their audio clips and
    /// an audio-only generator also name no file, and EffectCraft turned each into a grey solid.
    #[test]
    fn a_selection_of_generator_clips_sends_no_generator_reference() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-many");
        let (mut session, _seq) = crate::fixtures::demo_edit(&SCENES, 64, 36, 48);
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, true)).expect("edit in");
        let refs = sent_references(&peer);
        assert!(!refs.iter().any(|(schema, _)| schema.starts_with("Generator")), "a generator reached EffectCraft: {refs:?}");
        let videos: Vec<_> = refs.iter().filter(|(_, kind)| kind == "Video").collect();
        assert_eq!(videos.len(), SCENES.len());
        assert!(videos.iter().all(|(schema, _)| schema == "ExternalReference.1"), "{refs:?}");
        let data = peer.calls[0]["payload"]["data"].as_str().expect("payload data");
        assert_eq!(craft_ipc::payload::missing_media(data), Ok(Vec::<String>::new()), "every file in the payload opens");
        assert_eq!(session.jobs.len(), SCENES.len(), "one render per scene");
    }

    /// Without `wait` the dialog flow runs the call again after the renders end. Several renders
    /// run at once, and the second run must find all of them.
    #[test]
    fn the_rerun_after_several_renders_finds_every_movie() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-many-async");
        let (mut session, _seq) = crate::fixtures::demo_edit(&SCENES, 64, 36, 48);
        session.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        let err = edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, false)).expect_err("renders");
        assert!(matches!(err, EditInError::Preparing(_)), "{err:?}");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while crate::prepare::state() != crate::prepare::State::Idle {
            assert!(std::time::Instant::now() < until, "the renders did not end");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        edit_in(&mut session, &mut peer, &Headless, &options_in(&saved, false)).expect("the rerun finds every movie");
        let refs = sent_references(&peer);
        assert!(!refs.iter().any(|(schema, _)| schema.starts_with("Generator")), "{refs:?}");
        let cache = std::path::Path::new(&saved.path).with_file_name("cache");
        let movies = std::fs::read_dir(cache).unwrap().filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "mov")).count();
        assert_eq!(movies, SCENES.len());
    }

    /// Clips prepared by an earlier call (another job) are reused, the rest render now.
    #[test]
    fn clips_prepared_in_separate_calls_come_together() {
        let _serial = prepare_serial();
        let saved = TempProject::new("prep-split");
        let (mut first, _) = crate::fixtures::demo_edit(&SCENES[..1], 64, 36, 48);
        first.path = Some(saved.path.clone());
        edit_in(&mut first, &mut Recording::ok(), &Headless, &options_in(&saved, true)).expect("first");
        let (mut second, _) = crate::fixtures::demo_edit(&SCENES, 64, 36, 48);
        second.path = Some(saved.path.clone());
        let mut peer = Recording::ok();
        edit_in(&mut second, &mut peer, &Headless, &options_in(&saved, true)).expect("second");
        assert_eq!(second.jobs.len(), SCENES.len() - 1, "the first scene is a cache hit");
        let refs = sent_references(&peer);
        assert!(!refs.iter().any(|(schema, _)| schema.starts_with("Generator")), "{refs:?}");
    }
}

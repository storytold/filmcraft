//! Project-scoped MCP edit review for the active Android editor.

use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};
use filmcraft_ui_egui::{AudioOut, playback_mix};
use serde_json::{Value, json};

use crate::audio::AndroidAudio;
use crate::mcp::{EditorCommand, EditorRequest, Server};

const MAX_STEPS: usize = 12;

struct Proposal {
    revision: u64,
    document: u64,
    sequence: Option<u64>,
    linked_selection: bool,
    ripple_sequence_markers: bool,
    steps: Vec<ReviewStep>,
}

#[derive(Clone)]
struct ReviewStep {
    id: String,
    args: Value,
    description: String,
    impact: StepImpact,
}

#[derive(Clone, Copy)]
struct StepImpact {
    range: TimeRange,
    sample: Tick,
}

const PREVIEW_FPS: usize = 10;
const PREVIEW_FRAMES: usize = 15;
type PreviewImage = (usize, usize, Vec<u8>);

struct Preview {
    time: Tick,
    anchor: usize,
    frames: [Vec<PreviewImage>; 2],
    samples: [Vec<f32>; 2],
}

struct Playback {
    data: Preview,
    textures: [Vec<egui::TextureHandle>; 2],
    samples: [Arc<[f32]>; 2],
    playing: Option<Instant>,
    audio: AndroidAudio,
    note: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompareTarget {
    Step(usize),
    Playhead(Tick),
}

struct Comparison {
    target: CompareTarget,
    result: ComparisonResult,
}

enum ComparisonResult {
    Waiting { result: Receiver<Result<Preview, String>>, audio: AndroidAudio, _cancel: CancelPreview },
    Ready(Box<Playback>),
    Failed(String),
}

struct CancelPreview(Arc<AtomicBool>);

impl Drop for CancelPreview {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub struct Connection {
    pub server: Server,
    document: Option<u64>,
    pub saved_pairing: bool,
    requests: Receiver<EditorRequest>,
    proposal: Option<Proposal>,
    selected_step: usize,
    comparison: Option<Comparison>,
    status: String,
}

impl Connection {
    pub fn start(ctx: egui::Context, document: u64, ip: Ipv4Addr, saved_token: Option<String>) -> io::Result<Self> {
        let (sender, requests) = mpsc::sync_channel(8);
        let saved_pairing = saved_token.is_some();
        let server = Server::start(ip, saved_token, sender, move || ctx.request_repaint())?;
        Ok(Self {
            server,
            document: Some(document),
            saved_pairing,
            requests,
            proposal: None,
            selected_step: 0,
            comparison: None,
            status: "Ready to review edits.".into(),
        })
    }

    pub fn drain(&mut self, session: &mut Session) {
        for _ in 0..8 {
            let Ok(request) = self.requests.try_recv() else { break };
            if !request.reply.is_closed() {
                let _ = request.reply.send(self.handle(session, request.command));
            }
        }
    }

    fn review_status(&self, session: &Session) -> Value {
        json!({"revision": session.revision, "document": session.document_epoch,
            "sequence": session.state.active_sequence.map(|id| id.0), "pending": self.proposal.is_some(), "status": self.status})
    }

    fn handle(&mut self, session: &mut Session, request: EditorCommand) -> Result<Value, String> {
        if !self.is_for(session.document_epoch) {
            return Err("In FilmCraft, open MCP Config and allow access to the current project.".into());
        }
        match request {
            EditorCommand::ReviewStatus => Ok(self.review_status(session)),
            EditorCommand::Inspect => {
                let review = self.review_status(session);
                let project = session.execute("project.inspect", json!({})).map_err(|err| err.to_string())?;
                let timeline = if session.state.active_sequence.is_some() {
                    session.execute("sequence.inspect", json!({})).map_err(|err| err.to_string())?
                } else {
                    Value::Null
                };
                let mut result = json!({
                    "revision": {"document": review["document"], "sequence": review["sequence"], "revision": review["revision"]},
                    "project": project, "timeline": timeline, "review": review,
                });
                redact_paths(&mut result);
                Ok(result)
            }
            EditorCommand::Propose(params) => self.queue_proposal(session, &params),
        }
    }

    fn queue_proposal(&mut self, session: &Session, params: &Value) -> Result<Value, String> {
        if self.proposal.is_some() {
            return Err("Review or dismiss the pending edit first.".into());
        }
        let Some(steps) = params["steps"].as_array().filter(|steps| !steps.is_empty() && steps.len() <= MAX_STEPS) else {
            return Err(format!("A proposal needs 1 to {MAX_STEPS} edit steps."));
        };
        let sequence = session.state.active_sequence.map(|id| id.0);
        if params["revision"].as_u64() != Some(session.revision)
            || params["document"].as_u64() != Some(session.document_epoch)
            || sequence.is_none()
            || params["sequence"].as_u64() != sequence
        {
            return Err("Inspect with filmcraft_inspect and use the current revision to propose edits.".into());
        }
        let mut commands = Vec::with_capacity(steps.len());
        // Resolve later ranges against the earlier edits; this snapshot never touches live history.
        let mut planned = Session::default();
        planned.project = session.project.clone();
        planned.state = session.state.clone();
        planned.services = session.services.clone();
        planned.prefs = session.prefs.clone();
        for (index, step) in steps.iter().enumerate() {
            let (id, args, description) = edit_step(&planned, step)?;
            let impact = estimate_impact(&planned, &id, &args)?;
            planned.execute(&id, args.clone()).map_err(|error| format!("Step {} cannot be applied. {error}", index + 1))?;
            commands.push(ReviewStep { id, args, description, impact });
        }
        self.proposal = Some(Proposal {
            revision: session.revision,
            document: session.document_epoch,
            sequence,
            linked_selection: session.state.linked_selection,
            ripple_sequence_markers: session.state.ripple_sequence_markers,
            steps: commands,
        });
        self.selected_step = 0;
        self.comparison = None;
        self.status = "An edit is waiting for review.".into();
        Ok(json!({"status": "pending_review", "revision": session.revision}))
    }

    pub fn show_review(&mut self, ui: &mut egui::Ui, session: &mut Session, transport_running: bool) {
        ui.label(&self.status);
        let Some(proposal) = &self.proposal else { return };
        ui.small("Estimated ranges in proposal order");
        egui::ScrollArea::vertical().id_salt("mcp-review-steps").max_height(160.0).show(ui, |ui| {
            for (index, step) in proposal.steps.iter().enumerate() {
                let start = step.impact.range.start.seconds();
                let end = step.impact.range.end().seconds();
                ui.selectable_value(&mut self.selected_step, index, format!("{}. {}\n{start:.2}–{end:.2} s", index + 1, step.description));
            }
        });
        let current = proposal.matches(session);
        if !current {
            self.stop_preview();
            ui.label("The timeline or edit settings changed. Dismiss this edit and request an updated proposal.");
        } else {
            let rendering = self.comparison.as_ref().is_some_and(|c| matches!(c.result, ComparisonResult::Waiting { .. }));
            ui.horizontal_wrapped(|ui| {
                if let Some(step) = proposal.steps.get(self.selected_step)
                    && ui
                        .add_enabled(!transport_running, egui::Button::new("Jump to step"))
                        .on_disabled_hover_text("Pause playback or finish recording to jump.")
                        .clicked()
                {
                    session.set_playhead(step.impact.sample);
                    ui.ctx().request_repaint();
                }
                if ui.add_enabled(!rendering, egui::Button::new("Compare selected step")).clicked() {
                    let target = CompareTarget::Step(self.selected_step);
                    self.comparison = Some(Comparison { target, result: compare(ui.ctx(), session, proposal, target) });
                }
                if ui.add_enabled(!rendering, egui::Button::new("Compare all at playhead")).clicked() {
                    let target = CompareTarget::Playhead(session.playhead());
                    self.comparison = Some(Comparison { target, result: compare(ui.ctx(), session, proposal, target) });
                }
            });
            self.show_comparison(ui, transport_running);
        }
        ui.horizontal(|ui| {
            if ui.button("Dismiss").clicked() {
                self.proposal = None;
                self.comparison = None;
                self.status = "Edit dismissed.".into();
            }
            if ui.add_enabled(current, egui::Button::new("Apply edit")).clicked()
                && let Some(proposal) = self.proposal.take()
                && proposal.matches(session)
            {
                self.comparison = None;
                self.status = match apply_steps(session, &proposal.steps) {
                    Ok(()) => "Edit applied. Use Undo to revert it.".into(),
                    Err(err) => format!("Edit reverted: {err}"),
                };
            }
        });
    }

    pub fn has_proposal(&self) -> bool {
        self.proposal.is_some()
    }

    pub fn stop_preview(&mut self) {
        self.comparison = None;
    }

    pub fn is_for(&self, document: u64) -> bool {
        self.document == Some(document)
    }

    pub fn require_project_approval(&mut self) -> bool {
        if self.document.take().is_none() {
            return false;
        }
        self.proposal = None;
        self.comparison = None;
        self.status = "Allow access to the current project to resume MCP editing.".into();
        true
    }

    pub fn approve_project(&mut self, document: u64) {
        self.document = Some(document);
        self.status = "Ready to review edits.".into();
    }

    fn show_comparison(&mut self, ui: &mut egui::Ui, transport_running: bool) {
        let Some(comparison) = &mut self.comparison else { return };
        if let ComparisonResult::Waiting { result, .. } = &comparison.result {
            let status = match result.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("Preview stopped.".into())),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(status) = status {
                let pending = std::mem::replace(&mut comparison.result, ComparisonResult::Failed("Preview stopped.".into()));
                if let ComparisonResult::Waiting { audio, .. } = pending {
                    comparison.result = match status {
                        Ok(mut data) => {
                            let textures = std::mem::take(&mut data.frames).map(|frames| {
                                frames
                                    .into_iter()
                                    .map(|(width, height, pixels)| {
                                        ui.ctx().load_texture(
                                            "ai-review",
                                            egui::ColorImage::from_rgba_unmultiplied([width, height], &pixels),
                                            egui::TextureOptions::LINEAR,
                                        )
                                    })
                                    .collect()
                            });
                            let samples = std::mem::take(&mut data.samples).map(Arc::from);
                            ComparisonResult::Ready(Box::new(Playback { data, textures, samples, audio, playing: None, note: None }))
                        }
                        Err(error) => ComparisonResult::Failed(error),
                    };
                }
            }
        }
        let (scope, labels) = match comparison.target {
            CompareTarget::Step(index) => (format!("Step {} preview", index + 1), ["Before step", "After step"]),
            CompareTarget::Playhead(_) => ("Whole proposal preview".into(), ["Before edits", "After edits"]),
        };
        match &mut comparison.result {
            ComparisonResult::Ready(playback) => {
                ui.label(format!("{scope} · {:.2} s", playback.data.time.seconds()));
                playback.show(ui, labels, transport_running);
            }
            ComparisonResult::Waiting { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(scope);
                });
            }
            ComparisonResult::Failed(error) => {
                ui.label(format!("{scope} failed. {error}"));
            }
        }
    }
}

impl Playback {
    fn show(&mut self, ui: &mut egui::Ui, labels: [&str; 2], transport_running: bool) {
        let frame = self.playing.map(|started| (started.elapsed().as_secs_f64() * PREVIEW_FPS as f64) as usize);
        if transport_running || frame.is_some_and(|index| index >= PREVIEW_FRAMES) {
            self.stop();
        }
        let shown = frame.filter(|_| self.playing.is_some()).unwrap_or(self.data.anchor);
        ui.horizontal_wrapped(|ui| {
            for (index, label) in labels.into_iter().enumerate() {
                ui.vertical(|ui| {
                    ui.label(label);
                    if let Some(texture) = self.textures[index].get(shown) {
                        let size = texture.size_vec2();
                        ui.image((texture.id(), size * (176.0 / size.x.max(size.y)).min(1.0)));
                    }
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (index, label) in ["Play before", "Play after"].into_iter().enumerate() {
                if ui.add_enabled(!transport_running, egui::Button::new(label)).clicked() {
                    self.play(index);
                }
            }
            if self.playing.is_some() && ui.button("Stop").clicked() {
                self.stop();
            }
        });
        if let Some(note) = &self.note {
            ui.label(note);
        }
        if self.playing.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }

    fn play(&mut self, index: usize) {
        self.stop();
        let samples = self.samples[index].clone();
        let mut offset = 0usize;
        self.note = self
            .audio
            .start(Box::new(move |output, _| {
                output.fill(0.0);
                let available = output.len().min(samples.len().saturating_sub(offset));
                output[..available].copy_from_slice(&samples[offset..offset + available]);
                offset += available;
            }))
            .err()
            .map(|error| format!("Playing without audio. {error}"));
        self.playing = Some(Instant::now());
    }

    fn stop(&mut self) {
        if self.playing.take().is_some() {
            self.audio.stop();
        }
    }
}

fn redact_paths(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|key, _| !key.to_ascii_lowercase().ends_with("path"));
            for field in fields.values_mut() {
                redact_paths(field);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_paths),
        _ => {}
    }
}

fn apply_steps(session: &mut Session, steps: &[ReviewStep]) -> Result<(), String> {
    session
        .edit_group("GoogleBook MCP edit", |group| {
            for step in steps {
                group.execute(&step.id, step.args.clone())?;
            }
            Ok(())
        })
        .map_err(|error| error.to_string())
}

fn compare(ctx: &egui::Context, session: &Session, proposal: &Proposal, target: CompareTarget) -> ComparisonResult {
    let project = session.project.clone();
    let state = session.state.clone();
    let services = session.services.clone();
    let prefs = session.prefs.clone();
    let media = Arc::new(session.media.for_review());
    let steps = proposal.steps.clone();
    let ctx = ctx.clone();
    let audio = AndroidAudio::new();
    let sample_rate = audio.sample_rate();
    let channels = audio.channels();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = CancelPreview(cancelled.clone());
    let (reply, result) = mpsc::sync_channel(1);
    let worker = thread::Builder::new().name("filmcraft-ai-preview".into()).spawn(move || {
        let render = |sandbox: &Session, time: Tick| -> Result<_, String> {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Preview cancelled.".into());
            }
            let sequence = sandbox.active_sequence().ok_or("Open a sequence to preview.")?;
            let longest = sequence.settings.width.max(sequence.settings.height).max(1);
            let image = sandbox.try_render_program_at((224.0 / longest as f32).min(1.0), time).map_err(|e| e.to_string())?;
            Ok((image.w, image.h, image.over_black_rgba8()))
        };
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Preview changes use their own project history and media cache.
            let mut sandbox = Session::default();
            sandbox.project = project;
            sandbox.state = state;
            sandbox.services = services;
            sandbox.prefs = prefs;
            sandbox.media = media;
            let (time, compared_steps) = match target {
                CompareTarget::Step(index) => {
                    let (earlier, remaining) = steps.split_at_checked(index).ok_or("Select a proposal step to preview.")?;
                    let step = remaining.first().ok_or("Select a proposal step to preview.")?;
                    if !earlier.is_empty() {
                        apply_steps(&mut sandbox, earlier)?;
                    }
                    let time = step.impact.sample;
                    (time, std::slice::from_ref(step))
                }
                CompareTarget::Playhead(time) => (time, steps.as_slice()),
            };
            let step = Tick(TICKS_PER_SECOND / PREVIEW_FPS as i64);
            let anchor = (time.0 / step.0).clamp(0, 5) as usize;
            let start = time - Tick(step.0 * anchor as i64);
            let sequence = sandbox.state.active_sequence.ok_or("Open a sequence before previewing.")?;
            let sample = |sandbox: &Session| -> Result<_, String> {
                let frames = (0..PREVIEW_FRAMES).map(|index| render(sandbox, start + Tick(step.0 * index as i64))).collect::<Result<Vec<_>, _>>()?;
                let mut audio = Vec::new();
                if (8000..=384_000).contains(&sample_rate) && (1..=8).contains(&channels) && !cancelled.load(Ordering::Relaxed) {
                    audio.resize(sample_rate as usize * PREVIEW_FRAMES / PREVIEW_FPS * channels, 0.0);
                    let offset = (start.seconds() * f64::from(sample_rate)).round() as i64;
                    playback_mix(sandbox, sequence, sample_rate)(offset, &mut audio, channels);
                }
                Ok((frames, audio))
            };
            let (before, audio_before) = sample(&sandbox)?;
            apply_steps(&mut sandbox, compared_steps)?;
            let (after, audio_after) = sample(&sandbox)?;
            Ok::<_, String>(Preview { time, anchor, frames: [before, after], samples: [audio_before, audio_after] })
        }))
        .unwrap_or_else(|_| Err("The preview could not be rendered.".into()));
        let _ = reply.send(rendered);
        ctx.request_repaint();
    });
    match worker {
        Ok(_) => ComparisonResult::Waiting { result, audio, _cancel: cancel },
        Err(error) => ComparisonResult::Failed(error.to_string()),
    }
}

/// An estimate on the supplied timeline after all earlier proposal steps.
fn estimate_impact(session: &Session, id: &str, args: &Value) -> Result<StepImpact, String> {
    let sequence = session.active_sequence().ok_or("Open a sequence before previewing a step.")?;
    let rate = sequence.frame_rate();
    let frame = rate.frame_duration();
    let time = |value: &Value| value.as_i64().map(|ticks| rate.snap(Tick(ticks))).ok_or("The step needs a timeline time.");
    let clip_id = |value: &Value| value.as_u64().map(ClipId).ok_or("The step needs a clip ID.");
    let clip = |clip: ClipId| sequence.find_item(clip).map(|(_, item)| item).ok_or("The target clip is unavailable at this step.");
    let edge = |item: &filmcraft_project::TrackItem| if args["edge"] == "in" { item.start } else { item.end() };
    let ripple_end = || {
        let duration = sequence.duration();
        if session.state.ripple_sequence_markers {
            sequence.markers.iter().fold(duration, |end, marker| end.max(marker.start + marker.duration))
        } else {
            duration
        }
    };

    let (start, end, sample) = match id {
        "timeline.razor" => {
            let at = time(&args["time"])?;
            (at, at + frame, at)
        }
        "timeline.trim" => {
            let at = edge(clip(clip_id(&args["clip"])?)?);
            let offset = rate.tick_of(args["deltaFrames"].as_i64().ok_or("The trim needs a frame offset.")?);
            let other = (at + offset).max(Tick::ZERO);
            let start = at.min(other);
            let mut end = at.max(other).max(start + frame);
            if args["mode"] == "ripple" {
                end = end.max(ripple_end() + offset.abs());
            }
            (start, end, start)
        }
        "timeline.move" => {
            let movement = args["moves"].get(0).ok_or("The move needs a destination.")?;
            let item = clip(clip_id(&movement["clip"])?)?;
            let at = time(&movement["time"])?;
            (item.start.min(at), item.end().max(at + item.duration), at)
        }
        "timeline.delete" => {
            let id = clip_id(&args["clip"])?;
            let item = clip(id)?;
            let mut start = item.start;
            let mut end = item.end();
            for linked in filmcraft_engine::commands::with_links(session, &[id]) {
                let linked = clip(linked)?;
                start = start.min(linked.start);
                end = end.max(linked.end());
            }
            if args["ripple"] == true {
                end = end.max(ripple_end());
            }
            (start, end, item.start)
        }
        "graphics.newText" | "captions.add" => {
            let at = time(&args["time"])?;
            let duration = args.get(if id == "graphics.newText" { "seconds" } else { "durationSeconds" }).and_then(Value::as_f64).unwrap_or(3.0);
            (at, at + Tick::from_seconds_f64(duration), at)
        }
        "effects.apply" | "clip.audioGain" => {
            let item = clip(clip_id(&args["clip"])?)?;
            (item.start, item.end(), item.start + Tick(item.duration.0 / 2))
        }
        "sequence.applyVideoTransition" => {
            let at = edge(clip(clip_id(&args["clip"])?)?);
            let duration = rate.tick_of(args["frames"].as_i64().ok_or("The transition needs a frame count.")?);
            (at - duration, at + duration, at - Tick(duration.0 / 2))
        }
        _ => return Err("Unsupported preview step.".into()),
    };
    let start = start.max(Tick::ZERO);
    Ok(StepImpact { range: TimeRange::from_bounds(start, end.max(start)), sample: rate.snap(sample.max(Tick::ZERO)) })
}

fn edit_step(session: &Session, step: &Value) -> Result<(String, Value, String), String> {
    let id = step["id"].as_str().ok_or("Unsupported edit action.")?;
    let args = &step["params"];
    let clip = || -> Result<(u64, String), String> {
        let id = args["clip"].as_u64().ok_or("A clip edit needs an explicit clip ID.")?;
        let (_, item) = session.active_sequence().and_then(|seq| seq.find_item(ClipId(id))).ok_or("The clip is outside the open sequence.")?;
        let name = item.name.chars().take(70).collect::<String>();
        Ok((id, if name.is_empty() { format!("clip {id}") } else { name }))
    };
    let time = || args["time"].as_i64().filter(|t| (0..=TICKS_PER_SECOND * 24 * 3600).contains(t)).ok_or("Supply a valid timeline time in ticks.");
    let seconds = |key, default| {
        args.get(key).map_or(Some(default), Value::as_f64).filter(|d| (0.05..=300.0).contains(d)).ok_or("The duration must be between 0.05 and 300 seconds.")
    };
    let (clean, description) = match id {
        "timeline.razor" => {
            let (clip, name) = clip()?;
            let time = time()?;
            (json!({"clip": clip, "time": time}), format!("Split {name} at {:.2} s", Tick(time).seconds()))
        }
        "timeline.trim" => {
            let (clip, name) = clip()?;
            let frames = args["deltaFrames"].as_i64().filter(|n| *n != 0 && n.unsigned_abs() <= 36_000).ok_or("Supply a nonzero frame offset.")?;
            let edge = args["edge"].as_str().filter(|s| matches!(*s, "in" | "out")).ok_or("Choose the start or end of the clip.")?;
            let mode = args
                .get("mode")
                .map_or(Some("regular"), Value::as_str)
                .filter(|s| matches!(*s, "regular" | "ripple"))
                .ok_or("Choose regular or ripple trim.")?;
            let end = if edge == "in" { "start" } else { "end" };
            let label = if mode == "ripple" { "Ripple trim" } else { "Trim" };
            (json!({"clip": clip, "edge": edge, "mode": mode, "deltaFrames": frames}), format!("{label} the {end} of {name} by {frames:+} frames"))
        }
        "timeline.move" => {
            let (clip, name) = clip()?;
            let time = time()?;
            let track = args["track"]
                .as_str()
                .filter(|s| {
                    matches!(s.as_bytes().first(), Some(b'V' | b'A')) && s.get(1..).and_then(|i| i.parse::<u16>().ok()).is_some_and(|i| (1..=64).contains(&i))
                })
                .ok_or("Choose a timeline track such as V1 or A1.")?;
            (
                json!({"moves": [{"clip": clip, "track": track, "time": time}], "insert": false}),
                format!("Move {name} to {track} at {:.2} s", Tick(time).seconds()),
            )
        }
        "timeline.delete" => {
            let (clip, name) = clip()?;
            let ripple = args["ripple"].as_bool().ok_or("Choose delete or ripple delete.")?;
            let sequence = session.active_sequence().ok_or("Open a sequence before deleting a clip.")?;
            let linked = filmcraft_engine::commands::with_links(session, &[ClipId(clip)]);
            if linked.iter().any(|id| sequence.find_item(*id).and_then(|(track, _)| sequence.track(track)).is_none_or(|track| track.locked)) {
                return Err(format!("Unlock the clip and any linked clip tracks before deleting {name}."));
            }
            let action = if ripple { "Ripple delete" } else { "Delete" };
            let mut description = format!("{action} {name} (clip {clip})");
            let partners = linked.len().saturating_sub(1);
            match partners {
                0 => {}
                1 => description.push_str(" and 1 linked clip"),
                _ => description.push_str(&format!(" and {partners} linked clips")),
            }
            if ripple {
                description.push_str(". Close the gap and shift later clips on affected and sync-locked tracks. Captions on sync-locked tracks may change.");
                if session.state.ripple_sequence_markers {
                    description.push_str(" Later sequence markers may shift too.");
                }
            } else {
                description.push_str(". Leave the timeline positions unchanged.");
            }
            (json!({"clip": clip, "ripple": ripple}), description)
        }
        "graphics.newText" | "captions.add" => {
            let text = args["text"].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 400).ok_or("Enter up to 400 bytes of title or caption text.")?;
            let time = time()?;
            let duration = seconds("durationSeconds", 3.0)?;
            let (clean, label) = if id == "graphics.newText" {
                (json!({"text": text, "time": time, "seconds": duration, "newClip": true}), "title")
            } else {
                (json!({"text": text, "time": time, "durationSeconds": duration}), "caption")
            };
            (clean, format!("Add {label} “{text}” for {duration:.1} s at {:.2} s", Tick(time).seconds()))
        }
        "effects.apply" | "sequence.applyVideoTransition" => {
            let (clip, name) = clip()?;
            let effect = args["effect"]
                .as_str()
                .filter(|e| !e.is_empty() && e.len() <= 64 && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                .ok_or("Choose an effect ID such as gaussian_blur or cross_dissolve.")?;
            let label = effect.replace('_', " ");
            if id == "effects.apply" {
                let allowed = ["gaussian_blur", "light_leaks", "sharpen", "tint"];
                if !allowed.contains(&effect) {
                    return Err("Choose gaussian_blur, light_leaks, sharpen, or tint.".into());
                }
                (json!({"clip": clip, "effect": effect}), format!("Add {label} to {name}"))
            } else {
                let allowed = ["cross_dissolve", "dip_to_black", "dip_to_white"];
                if !allowed.contains(&effect) {
                    return Err("Choose cross_dissolve, dip_to_black, or dip_to_white.".into());
                }
                let frames =
                    args.get("frames").map_or(Some(12), Value::as_u64).filter(|f| (1..=120).contains(f)).ok_or("A transition needs 1 to 120 frames.")?;
                let edge =
                    args.get("edge").map_or(Some("in"), Value::as_str).filter(|s| matches!(*s, "in" | "out")).ok_or("Choose the start or end of the clip.")?;
                let end = if edge == "in" { "start" } else { "end" };
                (json!({"clip": clip, "effect": effect, "frames": frames, "edge": edge}), format!("Add a {frames}-frame {label} at the {end} of {name}"))
            }
        }
        "clip.audioGain" => {
            let (clip, name) = clip()?;
            let gain = args["db"].as_f64().filter(|d| (-60.0..=24.0).contains(d)).ok_or("Gain must be between −60 and +24 dB.")?;
            (json!({"clip": clip, "db": gain, "mode": "adjust"}), format!("Adjust {name} by {gain:+.1} dB"))
        }
        _ => return Err("Unsupported edit action.".into()),
    };
    Ok((id.into(), clean, description))
}

impl Proposal {
    fn matches(&self, session: &Session) -> bool {
        self.revision == session.revision
            && self.document == session.document_epoch
            && self.sequence == session.state.active_sequence.map(|id| id.0)
            && self.linked_selection == session.state.linked_selection
            && self.ripple_sequence_markers == session.state.ripple_sequence_markers
    }
}

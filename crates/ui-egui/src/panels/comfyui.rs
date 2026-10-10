//! Window ▸ ComfyUI…: make clips with ComfyUI workflows (docs/comfyui.md).
//!
//! The window shows the server (the preference every clip runs on, with a connection test), and
//! either a workflow loaded from a file (Load Workflow…: any workflow saved with ComfyUI's
//! Workflow ▸ Export (API)) to make a new clip from, or the ComfyUI clip selected in the timeline
//! or the Project panel. Every literal input of the workflow is editable: text, numbers,
//! switches, and files for loader nodes (Load Image / Audio / Video), which are uploaded when the
//! clip is generated. Files named by a project file (not chosen in this session) are listed with
//! an **Allow Uploads** button: nothing is sent until the user confirms them. Everything runs
//! through the `comfyui.*` engine commands, so agents can do the same over the control channel.
//!
//! Inputs can be **exposed** (the eye toggle on each row, `comfyui.expose`): exposed inputs are
//! shown first, above All Inputs, for every clip of that workflow and whenever it is loaded again
//! (kept per workflow in the preferences).
//!
//! Automation ids: `comfyui.server`, `comfyui.test`, `comfyui.loadWorkflow`, `comfyui.name`,
//! `comfyui.duration`, `comfyui.allInputs`, `comfyui.node.<node>`, `comfyui.input.<node>.<input>`
//! (with `.expose`, `.browse`, `.reset`), the same under `comfyui.exposed.<node>.<input>` for the
//! Exposed group, `comfyui.unconfirmed`, `comfyui.allowUploads`, `comfyui.create`,
//! `comfyui.createGenerate`, `comfyui.apply`, `comfyui.generate`, `comfyui.generateNewSeeds`,
//! `comfyui.status`.

use serde_json::{Value, json};

use crate::FilmcraftApp;

/// One editable input.
#[derive(Clone, Debug, PartialEq)]
struct Field {
    node: String,
    input: String,
    /// `text`, `int`, `float`, `bool`, `file`, `other` (the engine's input kinds).
    kind: String,
    /// The workflow's own value.
    default: Value,
    value: Value,
    /// A local file to upload (file inputs).
    file: String,
    /// The file was chosen (or allowed) in this window: send it even when the clip already
    /// names it, which confirms it for upload.
    picked: bool,
}

impl Field {
    fn changed(&self) -> bool {
        !self.file.is_empty() || self.value != self.default
    }
    fn binding(&self) -> Option<Value> {
        if !self.file.is_empty() {
            Some(json!({"node": self.node, "input": self.input, "file": self.file}))
        } else if self.value != self.default {
            Some(json!({"node": self.node, "input": self.input, "value": self.value}))
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Draft {
    server: String,
    /// The ComfyUI clip shown (None: a new clip from `workflow`).
    item: Option<u64>,
    workflow: Option<Value>,
    name: String,
    duration: f64,
    /// (node id, title, inputs)
    nodes: Vec<(String, String, Vec<Field>)>,
    /// Input files the clip names that may not be uploaded yet: (node, input, path).
    unconfirmed: Vec<(String, String, String)>,
    /// The workflow's key and its exposed inputs (node, input), shown first.
    key: String,
    exposed: Vec<(String, String)>,
    texts: Vec<String>,
    status: String,
    generating: bool,
}

fn draft_id() -> egui::Id {
    egui::Id::new("comfyui-window")
}

/// Menu route: `window.comfyui` opens the window.
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str) -> Option<Result<Value, String>> {
    (id == "window.comfyui").then(|| open(app, ctx))
}

pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let mut d = Draft { server: app.session.prefs.comfyui.server.clone(), duration: 5.0, ..Default::default() };
    if let Some(item) = selected_clip(app) {
        load_clip(app, &mut d, item);
    }
    ctx.data_mut(|m| m.insert_temp(draft_id(), Some(d)));
    Ok(json!({"dialog": "comfyui"}))
}

pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())).flatten().is_some()
}

fn selected_clip(app: &FilmcraftApp) -> Option<u64> {
    filmcraft_engine::comfyui::targets(&app.session, &Value::Null).first().map(|i| i.0)
}

/// Fields of a `comfyui.inspect` answer.
fn nodes_of(v: &Value) -> Vec<(String, String, Vec<Field>)> {
    let mut out = Vec::new();
    for n in v["nodes"].as_array().into_iter().flatten() {
        let node = n["node"].as_str().unwrap_or_default().to_string();
        let title = n["title"].as_str().unwrap_or_default().to_string();
        let fields: Vec<Field> = n["inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                let default = i["value"].clone();
                let ov = &i["override"];
                Field {
                    node: node.clone(),
                    input: i["input"].as_str().unwrap_or_default().to_string(),
                    kind: i["kind"].as_str().unwrap_or("other").to_string(),
                    value: if ov["value"].is_null() { default.clone() } else { ov["value"].clone() },
                    file: ov["file"].as_str().unwrap_or_default().to_string(),
                    default,
                    picked: false,
                }
            })
            .collect();
        if !fields.is_empty() {
            out.push((node, title, fields));
        }
    }
    out
}

fn load_clip(app: &mut FilmcraftApp, d: &mut Draft, item: u64) {
    match app.session.execute("comfyui.inspect", json!({"item": item})) {
        Ok(v) => {
            d.item = Some(item);
            d.workflow = None;
            d.name = v["name"].as_str().unwrap_or_default().to_string();
            d.nodes = nodes_of(&v);
            d.key = v["key"].as_str().unwrap_or_default().to_string();
            d.exposed = exposed_of(&v);
            d.unconfirmed = v["unconfirmedFiles"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|u| {
                    (
                        u["node"].as_str().unwrap_or_default().to_string(),
                        u["input"].as_str().unwrap_or_default().to_string(),
                        u["file"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            d.texts = v["lastRun"]["texts"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            d.generating = v["generating"].as_bool().unwrap_or(false);
        }
        Err(e) => d.status = e.to_string(),
    }
}

fn bindings(d: &Draft) -> Vec<Value> {
    d.nodes.iter().flat_map(|n| n.2.iter()).filter_map(Field::binding).collect()
}

enum Action {
    Test,
    Load,
    Browse(usize, usize),
    Create {
        generate: bool,
    },
    Apply,
    Allow,
    /// Expose (true) or hide an input of the workflow: (node, input, exposed).
    Expose(String, String, bool),
    Generate {
        new_seeds: bool,
    },
}

/// What a click in an input row asks for.
enum RowClick {
    Browse,
    Expose(bool),
}

/// The workflow key and exposed inputs of a `comfyui.inspect` / `comfyui.expose` answer.
fn exposed_of(v: &Value) -> Vec<(String, String)> {
    v["exposed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| (e["node"].as_str().unwrap_or_default().to_string(), e["input"].as_str().unwrap_or_default().to_string()))
        .collect()
}

/// One input row: the expose toggle, the input's name, its editor, and the choose-file and reset
/// buttons. Automation ids: `<id>`, `<id>.expose`, `<id>.browse`, `<id>.reset`.
#[allow(clippy::too_many_arguments)]
fn field_row(
    ui: &mut egui::Ui,
    f: &mut Field,
    label: &str,
    id: &str,
    held: bool,
    exposed: bool,
    t: &crate::theme::Tokens,
    elems: &mut Vec<(String, egui::Rect, String)>,
) -> Option<RowClick> {
    let mut click = None;
    ui.horizontal(|ui| {
        let (icon, tip) = if exposed {
            (crate::icons::Icon::Eye, tl!("Exposed: shown first for this workflow (click to stop)"))
        } else {
            (crate::icons::Icon::EyeOff, tl!("Expose: show this input first for this workflow"))
        };
        let e = crate::icons::button(ui, icon, 16.0, exposed, t, tip);
        elems.push((format!("{id}.expose"), e.rect, if exposed { "Exposed".into() } else { "Expose".into() }));
        if e.clicked() {
            click = Some(RowClick::Expose(!exposed));
        }
        if held {
            ui.colored_label(t.danger, label).on_hover_text(tl!("Named by the project file: uploaded only after Allow Uploads"));
        } else {
            ui.label(label);
        }
        let r = field_widget(ui, f);
        elems.push((id.to_string(), r.rect, field_label(f)));
        if f.kind == "file" {
            let b = ui.small_button("…").on_hover_text(tl!("Choose a file to upload"));
            elems.push((format!("{id}.browse"), b.rect, "Choose file".into()));
            if b.clicked() {
                click = Some(RowClick::Browse);
            }
        }
        if f.changed() {
            let b = ui.small_button("↺").on_hover_text(tl!("Back to the workflow's value"));
            elems.push((format!("{id}.reset"), b.rect, "Reset".into()));
            if b.clicked() {
                f.value = f.default.clone();
                f.file.clear();
                f.picked = false;
            }
        }
    });
    click
}

/// The status line of the running ComfyUI job, if any.
fn job_status(app: &FilmcraftApp) -> Option<String> {
    app.session.jobs.iter().rev().map(|j| j.to_json()).find(|j| j["label"].as_str().is_some_and(|l| l.starts_with("ComfyUI")) && j["finished"] != true).map(
        |j| {
            let status = j["status"].as_str().unwrap_or_default();
            if status.is_empty() { tl!("Generating…").to_string() } else { status.to_string() }
        },
    )
}

/// The status line of the last finished ComfyUI job (what it generated, or why it failed).
fn finished_status(app: &FilmcraftApp) -> Option<String> {
    app.session
        .jobs
        .iter()
        .rev()
        .map(|j| j.to_json())
        .find(|j| j["label"].as_str().is_some_and(|l| l.starts_with("ComfyUI")) && j["finished"] == true)
        .and_then(|j| j["status"].as_str().map(str::to_string))
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(Some(mut d)) = ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())) else { return };
    // follow the selection while showing a clip; refresh when its generation finished
    let sel = selected_clip(app);
    let busy = d.item.is_some_and(|i| app.session.comfyui.generating(filmcraft_project::ItemId(i)));
    let finished = d.generating && !busy;
    if finished {
        // "Generating…" gives way to how the run ended, here and in the status bar
        d.status = finished_status(app).unwrap_or_default();
        if !d.status.is_empty() {
            app.ui.status.clone_from(&d.status);
        }
    }
    if let Some(s) = sel
        && d.workflow.is_none()
        && (d.item != Some(s) || finished)
    {
        load_clip(app, &mut d, s);
    }
    d.generating = busy;
    let mut open = true;
    let mut action = None;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let tokens = app.tokens;
    let (accent, danger) = (tokens.accent, tokens.danger);
    let running = job_status(app);
    egui::Window::new("ComfyUI").id(egui::Id::new("comfyui-window-ui")).open(&mut open).collapsible(true).resizable(true).default_width(440.0).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                ui.label(tl!("Server"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.server).desired_width(240.0).hint_text(filmcraft_comfyui::DEFAULT_SERVER));
                elems.push(("comfyui.server".into(), r.rect, d.server.clone()));
                let t = ui.button(tl!("Test"));
                elems.push(("comfyui.test".into(), t.rect, "Test".into()));
                if t.clicked() {
                    action = Some(Action::Test);
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let l = ui.button(tl!("Load Workflow…")).on_hover_text(tl!("A workflow saved with ComfyUI's Workflow ▸ Export (API)"));
                elems.push(("comfyui.loadWorkflow".into(), l.rect, "Load Workflow…".into()));
                if l.clicked() {
                    action = Some(Action::Load);
                }
                match (d.item, &d.workflow) {
                    (_, Some(_)) => ui.weak(tl!("New clip")),
                    (Some(_), None) => ui.weak(tl!("Selected ComfyUI clip")),
                    (None, None) => ui.weak(tl!("Load a workflow, or select a ComfyUI clip")),
                };
            });
            if d.nodes.is_empty() {
                return;
            }
            egui::Grid::new("comfyui-clip").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(tl!("Name"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(220.0));
                elems.push(("comfyui.name".into(), r.rect, d.name.clone()));
                ui.end_row();
                if d.workflow.is_some() {
                    ui.label(tl!("Duration"));
                    let r = ui.add(egui::DragValue::new(&mut d.duration).speed(0.1).range(0.04..=3600.0).suffix(" s").max_decimals(2));
                    elems.push(("comfyui.duration".into(), r.rect, format!("{:.2} s", d.duration)));
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                let Draft { nodes, exposed, unconfirmed, key, .. } = &mut d;
                let held = |f: &Field| !f.picked && unconfirmed.iter().any(|u| u.0 == f.node && u.1 == f.input && u.2 == f.file);
                // the inputs exposed for this workflow, first
                if !exposed.is_empty() {
                    ui.weak(tl!("Exposed"));
                    for (node, input) in exposed.iter() {
                        let Some(ni) = nodes.iter().position(|n| n.0 == *node) else { continue };
                        let Some((title, fields)) = nodes.get_mut(ni).map(|n| (n.1.clone(), &mut n.2)) else { continue };
                        let Some(fi) = fields.iter().position(|f| f.input == *input) else { continue };
                        let Some(f) = fields.get_mut(fi) else { continue };
                        let id = format!("comfyui.exposed.{node}.{input}");
                        let h = held(f);
                        match field_row(ui, f, &format!("{title} · {input}"), &id, h, true, &tokens, &mut elems) {
                            Some(RowClick::Browse) => action = Some(Action::Browse(ni, fi)),
                            Some(RowClick::Expose(on)) => action = Some(Action::Expose(node.clone(), input.clone(), on)),
                            None => {}
                        }
                    }
                    ui.add_space(4.0);
                }
                let count: usize = nodes.iter().map(|n| n.2.len()).sum();
                let all_head = tlf!("All Inputs ({count})", count);
                let all = egui::CollapsingHeader::new(&all_head).id_salt(("comfyui-all", key.as_str())).default_open(exposed.is_empty()).show(ui, |ui| {
                    for (ni, (node, title, fields)) in nodes.iter_mut().enumerate() {
                        let edited = fields.iter().any(Field::changed);
                        let wanted = fields.iter().any(|f| f.kind == "file" || (f.kind == "text" && f.default.as_str().is_some_and(|s| s.len() > 12)));
                        let head = if edited { format!("{title} #{node} •") } else { format!("{title} #{node}") };
                        let h = egui::CollapsingHeader::new(&head).id_salt(("comfyui-node", node.as_str())).default_open(wanted || edited).show(ui, |ui| {
                            for (fi, f) in fields.iter_mut().enumerate() {
                                let id = format!("comfyui.input.{}.{}", f.node, f.input);
                                let on = exposed.iter().any(|e| e.0 == f.node && e.1 == f.input);
                                let (h, input) = (held(f), f.input.clone());
                                match field_row(ui, f, &input, &id, h, on, &tokens, &mut elems) {
                                    Some(RowClick::Browse) => action = Some(Action::Browse(ni, fi)),
                                    Some(RowClick::Expose(on)) => action = Some(Action::Expose(node.clone(), input, on)),
                                    None => {}
                                }
                            }
                        });
                        elems.push((format!("comfyui.node.{node}"), h.header_response.rect, head));
                    }
                });
                elems.push(("comfyui.allInputs".into(), all.header_response.rect, all_head));
            });
            if d.workflow.is_none() && !d.unconfirmed.is_empty() {
                ui.add_space(6.0);
                let files: Vec<&str> = d.unconfirmed.iter().map(|u| u.2.as_str()).collect();
                let line = tlf!("This clip uploads files named by the project file: {files}", files = files.join(", "));
                let r = ui
                    .colored_label(danger, &line)
                    .on_hover_text(tl!("A project can come from anyone: its files are sent to the server only after you allow them"));
                elems.push(("comfyui.unconfirmed".into(), r.rect, line));
                let b = ui.button(tl!("Allow Uploads")).on_hover_text(tl!("Send these files to the ComfyUI server when the clip is generated"));
                elems.push(("comfyui.allowUploads".into(), b.rect, "Allow Uploads".into()));
                if b.clicked() {
                    action = Some(Action::Allow);
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let primary = |t: &str| egui::Button::new(egui::RichText::new(t).color(egui::Color32::WHITE)).fill(accent);
                if d.workflow.is_some() {
                    let c = ui.button(tl!("Create Clip"));
                    elems.push(("comfyui.create".into(), c.rect, "Create Clip".into()));
                    if c.clicked() {
                        action = Some(Action::Create { generate: false });
                    }
                    let g = ui.add(primary(tl!("Create & Generate")));
                    elems.push(("comfyui.createGenerate".into(), g.rect, "Create & Generate".into()));
                    if g.clicked() {
                        action = Some(Action::Create { generate: true });
                    }
                } else {
                    let a = ui.button(tl!("Apply"));
                    elems.push(("comfyui.apply".into(), a.rect, "Apply".into()));
                    if a.clicked() {
                        action = Some(Action::Apply);
                    }
                    let g = ui.add_enabled(!d.generating, primary(tl!("Generate")));
                    elems.push(("comfyui.generate".into(), g.rect, "Generate".into()));
                    if g.clicked() {
                        action = Some(Action::Generate { new_seeds: false });
                    }
                    let n = ui.add_enabled(!d.generating, egui::Button::new(tl!("New Seeds"))).on_hover_text(tl!("Generate again with new random seeds"));
                    elems.push(("comfyui.generateNewSeeds".into(), n.rect, "New Seeds".into()));
                    if n.clicked() {
                        action = Some(Action::Generate { new_seeds: true });
                    }
                }
            });
            if !d.texts.is_empty() {
                ui.add_space(4.0);
                ui.weak(tl!("Text output"));
                for t in &d.texts {
                    ui.label(t);
                }
            }
            let line = running.clone().unwrap_or_else(|| d.status.clone());
            if !line.is_empty() {
                ui.add_space(4.0);
                let r = ui.weak(&line);
                elems.push(("comfyui.status".into(), r.rect, line));
            }
        },
    );
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if let Some(a) = action {
        run(app, &mut d, a);
    }
    if d.generating || running.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    ctx.data_mut(|m| m.insert_temp(draft_id(), open.then_some(d)));
}

fn field_label(f: &Field) -> String {
    if !f.file.is_empty() {
        return f.file.clone();
    }
    match &f.value {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

fn field_widget(ui: &mut egui::Ui, f: &mut Field) -> egui::Response {
    match f.kind.as_str() {
        "text" => {
            let mut s = f.value.as_str().unwrap_or_default().to_string();
            let long = s.len() > 40 || ["text", "prompt"].iter().any(|k| f.input.contains(k));
            let r = if long {
                ui.add(egui::TextEdit::multiline(&mut s).desired_rows(3).desired_width(300.0))
            } else {
                ui.add(egui::TextEdit::singleline(&mut s).desired_width(220.0))
            };
            if r.changed() {
                f.value = Value::String(s);
            }
            r
        }
        "int" => {
            let mut n = f.value.as_i64().unwrap_or_default();
            let r = ui.add(egui::DragValue::new(&mut n).speed(1.0));
            if r.changed() {
                f.value = json!(n);
            }
            r
        }
        "float" => {
            let mut x = f.value.as_f64().unwrap_or_default();
            let r = ui.add(egui::DragValue::new(&mut x).speed(0.01).max_decimals(4));
            if r.changed() {
                f.value = json!(x);
            }
            r
        }
        "bool" => {
            let mut b = f.value.as_bool().unwrap_or_default();
            let r = ui.checkbox(&mut b, "");
            if r.changed() {
                f.value = json!(b);
            }
            r
        }
        "file" => {
            let hint = f.default.as_str().unwrap_or_default().to_string();
            ui.add(egui::TextEdit::singleline(&mut f.file).desired_width(220.0).hint_text(hint))
        }
        _ => {
            let mut s = f.value.to_string();
            if s.len() > 60 {
                s = format!("{}…", s.chars().take(60).collect::<String>());
            }
            ui.weak(s)
        }
    }
}

fn run(app: &mut FilmcraftApp, d: &mut Draft, a: Action) {
    let r: Result<String, String> = match a {
        Action::Test => app.session.execute("comfyui.settings", json!({"server": d.server, "check": true})).map_err(|e| e.to_string()).and_then(|v| {
            if v["reachable"] == true {
                let ver = v["system"]["comfyui_version"].as_str().unwrap_or("?");
                Ok(tlf!("Connected (ComfyUI {ver})", ver))
            } else {
                Err(v["error"].as_str().unwrap_or(tl!("not reachable")).to_string())
            }
        }),
        Action::Load => {
            let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f(tl!("ComfyUI workflow (API)"), &["json"])) else { return };
            let wf = app
                .session
                .services
                .read_file(&path)
                .map_err(|e| format!("{path}: {e}"))
                .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| format!("{path}: {e}")));
            wf.and_then(|wf| {
                let v = app.session.execute("comfyui.inspect", json!({"workflow": wf})).map_err(|e| e.to_string())?;
                d.nodes = nodes_of(&v);
                d.key = v["key"].as_str().unwrap_or_default().to_string();
                d.exposed = exposed_of(&v);
                d.unconfirmed.clear();
                d.workflow = Some(wf);
                d.item = None;
                d.texts.clear();
                d.name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                Ok(tlf!("{n} inputs", n = d.nodes.iter().map(|n| n.2.len()).sum::<usize>()))
            })
        }
        Action::Browse(ni, fi) => {
            let exts: Vec<&str> =
                filmcraft_media::STILL_EXTENSIONS.iter().chain(filmcraft_media::VIDEO_EXTENSIONS).chain(filmcraft_media::AUDIO_EXTENSIONS).copied().collect();
            let picked = app.hooks.pick_open_file.as_mut().and_then(|f| f(tl!("Media"), &exts));
            if let (Some(p), Some(f)) = (picked, d.nodes.get_mut(ni).and_then(|n| n.2.get_mut(fi))) {
                f.file = p;
                f.picked = true;
            }
            return;
        }
        Action::Create { generate } => {
            let p = json!({"workflow": d.workflow, "name": d.name, "duration": d.duration, "inputs": bindings(d), "generate": generate});
            save_server(app, d).and_then(|()| app.session.execute("comfyui.newClip", p).map_err(|e| e.to_string())).map(|v| {
                if let Some(item) = v["item"].as_u64() {
                    load_clip(app, d, item);
                }
                if generate { tl!("Generating…").to_string() } else { tlf!("Created {name}", name = d.name) }
            })
        }
        Action::Apply => apply(app, d).map(|_| tl!("Applied").to_string()),
        Action::Allow => {
            let held = std::mem::take(&mut d.unconfirmed);
            for f in d.nodes.iter_mut().flat_map(|n| n.2.iter_mut()) {
                f.picked |= held.iter().any(|u| u.0 == f.node && u.1 == f.input && u.2 == f.file);
            }
            apply(app, d).map(|_| tlf!("{n} file(s) allowed", n = held.len()))
        }
        Action::Expose(node, input, on) => {
            let target = match (&d.workflow, d.item) {
                (Some(w), _) => json!({"workflow": w}),
                (None, Some(item)) => json!({"item": item}),
                (None, None) => return,
            };
            let mut p = target;
            p["inputs"] = json!([{"node": node, "input": input}]);
            p["exposed"] = json!(on);
            app.session.execute("comfyui.expose", p).map_err(|e| e.to_string()).map(|v| {
                d.exposed = exposed_of(&v);
                if on { tlf!("Exposed {input}", input) } else { tlf!("{input} no longer exposed", input) }
            })
        }
        Action::Generate { new_seeds } => apply(app, d).and_then(|item| {
            app.session.execute("comfyui.generate", json!({"item": item, "randomizeSeeds": new_seeds})).map_err(|e| e.to_string())?;
            d.generating = true;
            Ok(tl!("Generating…").to_string())
        }),
    };
    d.status = match r {
        Ok(s) => s,
        Err(e) => e,
    };
    app.ui.status = d.status.clone();
}

/// The window's server is the preference every clip runs on: save it when it was edited.
fn save_server(app: &mut FilmcraftApp, d: &Draft) -> Result<(), String> {
    let s = d.server.trim().trim_end_matches('/');
    if s.is_empty() || s == app.session.prefs.comfyui.server {
        return Ok(());
    }
    app.session.execute("comfyui.settings", json!({"server": s})).map(|_| ()).map_err(|e| e.to_string())
}

/// Write the window's changes into the shown clip; its item id. Only inputs that differ from the
/// clip's (or files chosen here) are sent: passing a file to `comfyui.setInputs` confirms it for
/// upload, so a file the project named must never be sent back unseen.
fn apply(app: &mut FilmcraftApp, d: &mut Draft) -> Result<u64, String> {
    let item = d.item.ok_or(tl!("select a ComfyUI clip"))?;
    save_server(app, d)?;
    let current = app.session.execute("comfyui.inspect", json!({"item": item})).map_err(|e| e.to_string())?;
    let now = current["inputs"].as_array().cloned().unwrap_or_default();
    let mut send = Vec::new();
    for f in d.nodes.iter().flat_map(|n| n.2.iter()) {
        let cur = now.iter().find(|b| b["node"] == json!(f.node) && b["input"] == json!(f.input));
        let want = f.binding();
        let differs = match (&want, cur) {
            (Some(w), Some(c)) => w != c,
            (None, None) => false,
            _ => true,
        };
        if differs || (f.picked && want.is_some()) {
            // no value and no file: back to the workflow's value
            send.push(want.unwrap_or_else(|| json!({"node": f.node, "input": f.input})));
        }
    }
    if !send.is_empty() || current["name"] != json!(d.name) {
        app.session.execute("comfyui.setInputs", json!({"item": item, "inputs": send, "name": d.name})).map_err(|e| e.to_string())?;
    }
    load_clip(app, d, item);
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf() -> Value {
        json!({
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat"}, "_meta": {"title": "Prompt"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "x", "images": ["6", 0]}}
        })
    }

    #[test]
    fn window_makes_and_edits_clips_through_the_engine() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.execute("file.newSequence", json!({"name": "Seq", "width": 64, "height": 36, "fps": 24})).unwrap();
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "window.comfyui", json!({})).unwrap();
        assert!(is_open(&ctx));
        // what Load Workflow… does with the chosen file
        let v = app.session.execute("comfyui.inspect", json!({"workflow": wf()})).unwrap();
        let mut d = Draft {
            server: app.session.prefs.comfyui.server.clone(),
            duration: 2.0,
            workflow: Some(wf()),
            name: "Cat".into(),
            nodes: nodes_of(&v),
            ..Default::default()
        };
        assert_eq!(d.nodes.len(), 2);
        d.nodes[0].2[0].value = json!("a dog");
        // the window's server is the preference (a clip has none)
        d.server = "http://gpu.local:8188/".into();
        run(&mut app, &mut d, Action::Create { generate: false });
        let item = d.item.expect(&d.status);
        assert_eq!(app.session.prefs.comfyui.server, "http://gpu.local:8188");
        let r = filmcraft_engine::comfyui::recipe_of(&app.session, filmcraft_project::ItemId(item)).unwrap();
        assert_eq!(r.inputs, vec![filmcraft_comfyui::Binding::value("6", "text", json!("a dog"))]);
        // back to the workflow's value: Apply removes the override
        d.nodes[0].2[0].value = json!("a cat");
        run(&mut app, &mut d, Action::Apply);
        assert!(filmcraft_engine::comfyui::recipe_of(&app.session, filmcraft_project::ItemId(item)).unwrap().inputs.is_empty(), "{}", d.status);
    }

    #[test]
    fn files_named_by_a_project_wait_for_allow_uploads() {
        use filmcraft_project::ItemId;
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.execute("file.newSequence", json!({"name": "Seq", "width": 64, "height": 36, "fps": 24})).unwrap();
        let wf = json!({
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat"}},
            "10": {"class_type": "LoadImage", "inputs": {"image": "example.png"}}
        });
        let item = app.session.execute("comfyui.newClip", json!({"workflow": wf, "name": "Shot"})).unwrap()["item"].as_u64().unwrap();
        // what a shared project file carries: an input file the user never chose
        let secret = "/home/someone/.ssh/id_rsa";
        let mut r = filmcraft_engine::comfyui::recipe_of(&app.session, ItemId(item)).unwrap();
        r.inputs = vec![filmcraft_comfyui::Binding::file("10", "image", secret)];
        let mut p = (*app.session.project).clone();
        let g = filmcraft_project::Generation { provider: "comfyui".into(), recipe: serde_json::to_value(&r).unwrap(), last_run: Value::Null };
        p.generated.insert(ItemId(item), std::sync::Arc::new(g));
        app.session.project = std::sync::Arc::new(p);
        let mut d = Draft { server: app.session.prefs.comfyui.server.clone(), ..Default::default() };
        load_clip(&mut app, &mut d, item);
        assert_eq!(d.unconfirmed, [("10".to_string(), "image".to_string(), secret.to_string())]);
        // editing another input and applying sends only that input: the file stays unconfirmed
        let text = d.nodes.iter_mut().flat_map(|n| n.2.iter_mut()).find(|f| f.input == "text").unwrap();
        text.value = json!("a dog");
        run(&mut app, &mut d, Action::Apply);
        assert!(!app.session.comfyui.confirmed(secret), "{}", d.status);
        assert_eq!(d.unconfirmed.len(), 1);
        let r = filmcraft_engine::comfyui::recipe_of(&app.session, ItemId(item)).unwrap();
        assert_eq!(r.inputs.len(), 2, "{:?}", r.inputs);
        // Allow Uploads confirms exactly the listed files
        run(&mut app, &mut d, Action::Allow);
        assert!(app.session.comfyui.confirmed(secret), "{}", d.status);
        assert!(d.unconfirmed.is_empty());
    }

    #[test]
    fn exposed_inputs_come_back_with_the_workflow() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.execute("file.newSequence", json!({"name": "Seq", "width": 64, "height": 36, "fps": 24})).unwrap();
        let ctx = egui::Context::default();
        let v = app.session.execute("comfyui.inspect", json!({"workflow": wf()})).unwrap();
        let mut d = Draft { workflow: Some(wf()), name: "Cat".into(), duration: 1.0, nodes: nodes_of(&v), ..Default::default() };
        run(&mut app, &mut d, Action::Expose("6".into(), "text".into(), true));
        assert_eq!(d.exposed, [("6".to_string(), "text".to_string())], "{}", d.status);
        // a clip made from the workflow shows it exposed too
        run(&mut app, &mut d, Action::Create { generate: false });
        assert_eq!(d.exposed, [("6".to_string(), "text".to_string())]);
        // the window draws the Exposed group, with its own automation ids
        ctx.data_mut(|m| m.insert_temp(draft_id(), Some(d.clone())));
        ctx.run_ui(egui::RawInput::default(), |ui| show(&mut app, ui.ctx())).textures_delta.clear();
        let ids: Vec<String> = app.auto.elements.iter().map(|e| e.id.clone()).collect();
        for id in ["comfyui.exposed.6.text", "comfyui.exposed.6.text.expose", "comfyui.allInputs"] {
            assert!(ids.iter().any(|i| i == id), "{id} in {ids:?}");
        }
        // with inputs exposed, All Inputs starts folded
        assert!(!ids.iter().any(|i| i == "comfyui.input.6.text"), "{ids:?}");
        // hiding it again
        run(&mut app, &mut d, Action::Expose("6".into(), "text".into(), false));
        assert!(d.exposed.is_empty());
        assert!(app.session.prefs.comfyui.exposed.is_empty());
    }
}

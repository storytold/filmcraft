//! Window ▸ ComfyUI…: make clips with ComfyUI workflows (docs/comfyui.md).
//!
//! The window shows the server (with a connection test), and either a workflow loaded from a
//! file (Load Workflow…: any workflow saved with ComfyUI's Workflow ▸ Export (API)) to make a new
//! clip from, or the ComfyUI clip selected in the timeline or the Project panel. Every literal
//! input of the workflow is editable: text, numbers, switches, and files for loader nodes (Load
//! Image / Audio / Video), which are uploaded when the clip is generated. Everything runs through
//! the `comfyui.*` engine commands, so agents can do the same over the control channel.
//!
//! Automation ids: `comfyui.server`, `comfyui.test`, `comfyui.loadWorkflow`, `comfyui.name`,
//! `comfyui.duration`, `comfyui.input.<node>.<input>`, `comfyui.input.<node>.<input>.browse`,
//! `comfyui.create`, `comfyui.createGenerate`, `comfyui.apply`, `comfyui.generate`,
//! `comfyui.generateNewSeeds`, `comfyui.status`.

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
    Create { generate: bool },
    Apply,
    Generate { new_seeds: bool },
}

/// The status line of the running ComfyUI job, if any.
fn job_status(app: &FilmcraftApp) -> Option<String> {
    app.session.jobs.iter().rev().map(|j| j.to_json()).find(|j| j["label"].as_str().is_some_and(|l| l.starts_with("ComfyUI")) && j["finished"] != true).map(
        |j| {
            let status = j["status"].as_str().unwrap_or_default();
            if status.is_empty() { "Generating…".to_string() } else { status.to_string() }
        },
    )
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(Some(mut d)) = ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())) else { return };
    // follow the selection while showing a clip; refresh when its generation finished
    let sel = selected_clip(app);
    let busy = d.item.is_some_and(|i| app.session.comfyui.generating(filmcraft_project::ItemId(i)));
    if let Some(s) = sel
        && d.workflow.is_none()
        && (d.item != Some(s) || (d.generating && !busy))
    {
        load_clip(app, &mut d, s);
    }
    d.generating = busy;
    let mut open = true;
    let mut action = None;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let accent = app.tokens.accent;
    let running = job_status(app);
    egui::Window::new("ComfyUI").open(&mut open).collapsible(true).resizable(true).default_width(440.0).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label("Server");
            let r = ui.add(egui::TextEdit::singleline(&mut d.server).desired_width(240.0).hint_text(filmcraft_comfyui::DEFAULT_SERVER));
            elems.push(("comfyui.server".into(), r.rect, d.server.clone()));
            let t = ui.button("Test");
            elems.push(("comfyui.test".into(), t.rect, "Test".into()));
            if t.clicked() {
                action = Some(Action::Test);
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let l = ui.button("Load Workflow…").on_hover_text("A workflow saved with ComfyUI's Workflow ▸ Export (API)");
            elems.push(("comfyui.loadWorkflow".into(), l.rect, "Load Workflow…".into()));
            if l.clicked() {
                action = Some(Action::Load);
            }
            match (d.item, &d.workflow) {
                (_, Some(_)) => ui.weak("New clip"),
                (Some(_), None) => ui.weak("Selected ComfyUI clip"),
                (None, None) => ui.weak("Load a workflow, or select a ComfyUI clip"),
            };
        });
        if d.nodes.is_empty() {
            return;
        }
        egui::Grid::new("comfyui-clip").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(220.0));
            elems.push(("comfyui.name".into(), r.rect, d.name.clone()));
            ui.end_row();
            if d.workflow.is_some() {
                ui.label("Duration");
                let r = ui.add(egui::DragValue::new(&mut d.duration).speed(0.1).range(0.04..=3600.0).suffix(" s").max_decimals(2));
                elems.push(("comfyui.duration".into(), r.rect, format!("{:.2} s", d.duration)));
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
            for (ni, (node, title, fields)) in d.nodes.iter_mut().enumerate() {
                let edited = fields.iter().any(Field::changed);
                let wanted = fields.iter().any(|f| f.kind == "file" || (f.kind == "text" && f.default.as_str().is_some_and(|s| s.len() > 12)));
                let head = if edited { format!("{title} #{node} •") } else { format!("{title} #{node}") };
                egui::CollapsingHeader::new(head).id_salt(("comfyui-node", node.as_str())).default_open(wanted || edited).show(ui, |ui| {
                    for (fi, f) in fields.iter_mut().enumerate() {
                        let id = format!("comfyui.input.{}.{}", f.node, f.input);
                        ui.horizontal(|ui| {
                            ui.label(&f.input);
                            let r = field_widget(ui, f);
                            elems.push((id.clone(), r.rect, field_label(f)));
                            if f.kind == "file" {
                                let b = ui.small_button("…").on_hover_text("Choose a file to upload");
                                elems.push((format!("{id}.browse"), b.rect, "Choose file".into()));
                                if b.clicked() {
                                    action = Some(Action::Browse(ni, fi));
                                }
                            }
                            if f.changed() && ui.small_button("↺").on_hover_text("Back to the workflow's value").clicked() {
                                f.value = f.default.clone();
                                f.file.clear();
                            }
                        });
                    }
                });
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let primary = |t: &str| egui::Button::new(egui::RichText::new(t).color(egui::Color32::WHITE)).fill(accent);
            if d.workflow.is_some() {
                let c = ui.button("Create Clip");
                elems.push(("comfyui.create".into(), c.rect, "Create Clip".into()));
                if c.clicked() {
                    action = Some(Action::Create { generate: false });
                }
                let g = ui.add(primary("Create & Generate"));
                elems.push(("comfyui.createGenerate".into(), g.rect, "Create & Generate".into()));
                if g.clicked() {
                    action = Some(Action::Create { generate: true });
                }
            } else {
                let a = ui.button("Apply");
                elems.push(("comfyui.apply".into(), a.rect, "Apply".into()));
                if a.clicked() {
                    action = Some(Action::Apply);
                }
                let g = ui.add_enabled(!d.generating, primary("Generate"));
                elems.push(("comfyui.generate".into(), g.rect, "Generate".into()));
                if g.clicked() {
                    action = Some(Action::Generate { new_seeds: false });
                }
                let n = ui.add_enabled(!d.generating, egui::Button::new("New Seeds")).on_hover_text("Generate again with new random seeds");
                elems.push(("comfyui.generateNewSeeds".into(), n.rect, "New Seeds".into()));
                if n.clicked() {
                    action = Some(Action::Generate { new_seeds: true });
                }
            }
        });
        if !d.texts.is_empty() {
            ui.add_space(4.0);
            ui.weak("Text output");
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
    });
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
                Ok(format!("Connected (ComfyUI {ver})"))
            } else {
                Err(v["error"].as_str().unwrap_or("not reachable").to_string())
            }
        }),
        Action::Load => {
            let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f("ComfyUI workflow (API)", &["json"])) else { return };
            let wf = app
                .session
                .services
                .read_file(&path)
                .map_err(|e| format!("{path}: {e}"))
                .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| format!("{path}: {e}")));
            wf.and_then(|wf| {
                let v = app.session.execute("comfyui.inspect", json!({"workflow": wf})).map_err(|e| e.to_string())?;
                d.nodes = nodes_of(&v);
                d.workflow = Some(wf);
                d.item = None;
                d.texts.clear();
                d.name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                Ok(format!("{} inputs", d.nodes.iter().map(|n| n.2.len()).sum::<usize>()))
            })
        }
        Action::Browse(ni, fi) => {
            let exts: Vec<&str> =
                filmcraft_media::STILL_EXTENSIONS.iter().chain(filmcraft_media::VIDEO_EXTENSIONS).chain(filmcraft_media::AUDIO_EXTENSIONS).copied().collect();
            let picked = app.hooks.pick_open_file.as_mut().and_then(|f| f("Media", &exts));
            if let (Some(p), Some(f)) = (picked, d.nodes.get_mut(ni).and_then(|n| n.2.get_mut(fi))) {
                f.file = p;
            }
            return;
        }
        Action::Create { generate } => {
            let p = json!({"workflow": d.workflow, "name": d.name, "duration": d.duration, "inputs": bindings(d), "server": server_override(app, d), "generate": generate});
            app.session.execute("comfyui.newClip", p).map_err(|e| e.to_string()).map(|v| {
                if let Some(item) = v["item"].as_u64() {
                    load_clip(app, d, item);
                }
                if generate { "Generating…".to_string() } else { format!("Created {}", d.name) }
            })
        }
        Action::Apply => apply(app, d).map(|_| "Applied".to_string()),
        Action::Generate { new_seeds } => apply(app, d).and_then(|item| {
            app.session.execute("comfyui.generate", json!({"item": item, "randomizeSeeds": new_seeds})).map_err(|e| e.to_string())?;
            d.generating = true;
            Ok("Generating…".to_string())
        }),
    };
    d.status = match r {
        Ok(s) => s,
        Err(e) => e,
    };
    app.ui.status = d.status.clone();
}

/// The server a clip names: only when it differs from the preferences' one.
fn server_override(app: &FilmcraftApp, d: &Draft) -> String {
    let s = d.server.trim().trim_end_matches('/');
    if s == app.session.prefs.comfyui.server { String::new() } else { s.to_string() }
}

/// Write the window's inputs into the shown clip (only when something changed); its item id.
fn apply(app: &mut FilmcraftApp, d: &mut Draft) -> Result<u64, String> {
    let item = d.item.ok_or("select a ComfyUI clip")?;
    let current = app.session.execute("comfyui.inspect", json!({"item": item})).map_err(|e| e.to_string())?;
    let wanted = bindings(d);
    let server = server_override(app, d);
    if current["inputs"] != json!(wanted) || current["name"] != json!(d.name) || current["server"].as_str() != Some(d.server.trim().trim_end_matches('/')) {
        app.session
            .execute("comfyui.setInputs", json!({"item": item, "clearInputs": true, "inputs": wanted, "name": d.name, "server": server}))
            .map_err(|e| e.to_string())?;
    }
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
        run(&mut app, &mut d, Action::Create { generate: false });
        let item = d.item.expect(&d.status);
        let r = filmcraft_engine::comfyui::recipe_of(&app.session, filmcraft_project::ItemId(item)).unwrap();
        assert_eq!(r.inputs, vec![filmcraft_comfyui::Binding::value("6", "text", json!("a dog"))]);
        assert!(r.server.is_empty());
        // back to the workflow's value: Apply removes the override
        d.nodes[0].2[0].value = json!("a cat");
        run(&mut app, &mut d, Action::Apply);
        assert!(filmcraft_engine::comfyui::recipe_of(&app.session, filmcraft_project::ItemId(item)).unwrap().inputs.is_empty(), "{}", d.status);
    }
}

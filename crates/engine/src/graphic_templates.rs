//! Graphics templates, responsive design, per-character styles, source graphics and font
//! replacement (M10.7).
//!
//! - **Templates** (`graphics.template.*`): FilmCraft's own `.fcgt` format
//!   ([`filmcraft_project::gtemplate`]). Built-in templates are original designs in code; user
//!   templates live in `<data dir>/Graphics Templates/*.fcgt`. Export As Motion Graphics Template
//!   makes one from a graphic clip with chosen editable properties; Install copies a `.fcgt` into
//!   the user folder (Adobe `.mogrt` files are refused without being opened); Apply places a new
//!   graphic clip; `graphics.template.set` edits a placed template's properties.
//! - **Responsive design:** `graphics.setRoll` (roll / crawl options), `graphics.setResponsiveTime`
//!   (protected intro / outro), `graphics.pin` (pin a layer's edges to another layer or the frame).
//! - **Per-character styles:** `graphics.setCharStyle` styles a range of characters.
//! - **Upgrade Caption to Graphic**, **Upgrade to Source Graphic** (one set of layers shared by
//!   every clip of a project item; editing one instance updates the others) and **Replace Fonts
//!   in Projects**.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::graphic::{self, layer_display_name, layer_indices};
use filmcraft_project::graphic_design::{CharStyle, GraphicMeta, LayerExtra, Pin, PinTarget, RollMode, SourceGraphic, apply_char_style, clear_char_style};
use filmcraft_project::gtemplate::{
    self, ControlKind, FontResource, GraphicsTemplate, TEMPLATE_EXTENSION, TemplateControl, base64_decode, base64_encode, builtin_templates, ensure_uids,
    layer_uid,
};
use filmcraft_project::{ClipId, EffectInstance, ItemId, ItemKind, Label, ParamValue, Project, TrackItem};
use filmcraft_render::graphic_clip::{item_layer_specs, layer_bounds};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, u64_p};
use crate::graphics::{layer_effect_index, place_video_clip, set_props, target_clip};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

const G: &[&str] = &["Graphics and Titles"];

fn has_graphic(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    target_clip(s, &Value::Null).map(|_| ()).ok_or_else(|| "select a graphic clip".into())
}

fn has_captions(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.active_sequence().is_some_and(|q| q.caption_tracks.iter().any(|t| !t.captions.is_empty())) { Ok(()) } else { Err("there are no captions".into()) }
}

// ------------------------------------------------------------------------------------------------
// Template library
// ------------------------------------------------------------------------------------------------

/// Where user graphics templates live: `<data dir>/Graphics Templates` (None without a data
/// directory).
pub fn user_templates_dir(s: &Session) -> Option<std::path::PathBuf> {
    s.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("Graphics Templates"))
}

/// A template from the library: built in, or a user file.
#[derive(Clone, Debug)]
pub struct LibraryEntry {
    pub template: GraphicsTemplate,
    /// None for built-in templates.
    pub path: Option<String>,
}

/// Built-in and user templates (user files that fail to read are skipped).
pub fn library(s: &Session) -> Vec<LibraryEntry> {
    let mut v: Vec<LibraryEntry> = builtin_templates().into_iter().map(|t| LibraryEntry { template: t, path: None }).collect();
    if let Some(dir) = user_templates_dir(s)
        && let Some(Ok(mut names)) = s.services.list_dir(&dir.to_string_lossy())
    {
        names.sort();
        for n in names.into_iter().filter(|n| n.to_ascii_lowercase().ends_with(&format!(".{TEMPLATE_EXTENSION}"))) {
            let path = dir.join(&n).to_string_lossy().to_string();
            if let Ok(b) = s.services.read_file(&path)
                && let Ok(t) = GraphicsTemplate::from_bytes(&b)
            {
                v.push(LibraryEntry { template: t, path: Some(path) });
            }
        }
    }
    v
}

fn is_mogrt(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".mogrt")
}

const MOGRT: &str = "FilmCraft does not open Adobe .mogrt files (or other applications' template packages); it reads only its own .fcgt graphics templates";

/// Find a template by id, name (case-insensitive) or `.fcgt` path.
pub fn find_template(s: &Session, key: &str) -> Result<LibraryEntry> {
    if is_mogrt(key) {
        return Err(EngineError::Other(MOGRT.into()));
    }
    if key.to_ascii_lowercase().ends_with(&format!(".{TEMPLATE_EXTENSION}")) || key.contains('/') || key.contains('\\') {
        let b = s.services.read_file(key).map_err(|e| EngineError::Other(format!("{key}: {e}")))?;
        let t = GraphicsTemplate::from_bytes(&b).map_err(|e| EngineError::Other(format!("{key}: {e}")))?;
        return Ok(LibraryEntry { template: t, path: Some(key.to_string()) });
    }
    let lib = library(s);
    lib.iter()
        .find(|e| e.template.id == key)
        .or_else(|| lib.iter().find(|e| e.template.name.eq_ignore_ascii_case(key)))
        .cloned()
        .ok_or_else(|| bad("graphics.template", format!("no graphics template `{key}`")))
}

/// Register the fonts a template embeds (once per font file).
fn register_fonts(t: &GraphicsTemplate) -> usize {
    static SEEN: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    let mut n = 0;
    for f in &t.resources.fonts {
        let Some(data) = base64_decode(&f.data) else { continue };
        let h = data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
        let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if seen.insert(h) {
            n += filmcraft_text::fonts::add_font_data(data).len();
        }
    }
    n
}

fn control_json(c: &TemplateControl) -> Value {
    json!({"id": c.id, "name": c.name, "kind": c.kind.name(), "layer": c.layer, "param": c.param, "min": c.min, "max": c.max})
}

fn entry_json(e: &LibraryEntry) -> Value {
    let t = &e.template;
    json!({
        "id": t.id, "name": t.name, "category": t.category, "description": t.description, "author": t.author,
        "license": t.license, "tags": t.tags, "source": if e.path.is_some() { "user" } else { "builtin" }, "path": e.path,
        "canvas": t.canvas, "duration": t.duration.0, "seconds": t.duration.seconds(), "layers": t.layers.len(),
        "roll": t.graphic.roll.mode.label(), "controls": t.controls.iter().map(control_json).collect::<Vec<_>>(),
        "fonts": t.resources.fonts.iter().map(|f| json!({"family": f.family, "style": f.style, "license": f.license})).collect::<Vec<_>>(),
    })
}

/// Render a template's thumbnail (static: rolls are shown at rest) as straight sRGB RGBA8,
/// `w` pixels wide with the template's aspect ratio. Returns (width, height, pixels).
pub fn thumbnail(t: &GraphicsTemplate, w: u32) -> (u32, u32, Vec<u8>) {
    let w = w.clamp(16, 1920);
    let h = ((w as f64 * t.canvas[1] as f64 / t.canvas[0].max(1) as f64).round() as u32).max(1);
    register_fonts(t);
    let mut meta = t.graphic.clone();
    meta.roll.mode = RollMode::Off;
    let mut p = Project::new("thumb");
    let rate = filmcraft_time::FrameRate::FPS_30;
    let g = p.add_item("G", Label::Rose, ItemKind::Graphic { width: t.canvas[0], height: t.canvas[1], rate }, None);
    let Some(mut it) = p.make_track_item(
        g,
        filmcraft_project::TrackKind::Video,
        Tick::ZERO,
        filmcraft_time::TimeRange::new(Tick::ZERO, t.duration.max(rate.frame_duration())),
        rate,
    ) else {
        return (w, h, vec![0; w as usize * h as usize * 4]);
    };
    it.effects.extend(t.layers.iter().cloned());
    it.graphic = Some(Box::new(meta));
    // a moment into the clip, so intro animations have settled a little
    let at = Tick((t.duration.0 / 2).min(filmcraft_time::TICKS_PER_SECOND));
    let mut img = filmcraft_render::image::Image::new(w as usize, h as usize);
    let k = w as f64 / t.canvas[0].max(1) as f64;
    filmcraft_render::graphic_clip::render_graphic(&it, at, (t.canvas[0], t.canvas[1]), &Affine::scale(k, k), &mut img);
    (w, h, img.to_rgba8())
}

/// Cached thumbnails by template id + content + width (the UI asks every frame).
pub fn thumbnail_cached(t: &GraphicsTemplate, w: u32) -> Arc<(u32, u32, Vec<u8>)> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<(String, u64, u32), Arc<(u32, u32, Vec<u8>)>>>> = OnceLock::new();
    let json = t.to_json();
    let h = json.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let key = (t.id.clone(), h, w);
    if let Some(v) = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return v.clone();
    }
    let v = Arc::new(thumbnail(t, w));
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 256 {
        c.clear();
    }
    c.insert(key, v.clone());
    v
}

fn list_templates(s: &mut Session, p: &Value) -> Result<Value> {
    let q = str_p(p, "query").unwrap_or("").to_ascii_lowercase();
    let cat = str_p(p, "category").map(str::to_ascii_lowercase);
    let src = str_p(p, "source");
    let v: Vec<Value> = library(s)
        .iter()
        .filter(|e| {
            let t = &e.template;
            (q.is_empty()
                || t.name.to_ascii_lowercase().contains(&q)
                || t.category.to_ascii_lowercase().contains(&q)
                || t.description.to_ascii_lowercase().contains(&q)
                || t.tags.iter().any(|x| x.to_ascii_lowercase().contains(&q)))
                && cat.as_ref().is_none_or(|c| t.category.to_ascii_lowercase() == *c)
                && src.is_none_or(|x| (x == "user") == e.path.is_some())
        })
        .map(entry_json)
        .collect();
    Ok(Value::Array(v))
}

/// Value of a control on a placed graphic, as JSON.
fn control_value(it: &TrackItem, c: &TemplateControl, mt: Tick) -> Value {
    let Some(e) = it.effects.iter().find(|e| graphic::is_layer(e) && layer_uid(e) == c.layer) else { return Value::Null };
    if c.param == "enabled" {
        return json!(e.enabled);
    }
    let Some(prm) = e.params.get(&c.param) else { return Value::Null };
    match prm.value_at(mt) {
        ParamValue::Text(t) if c.kind == ControlKind::Font => {
            let style = match e.params.get("font_style").map(|p| p.value_at(mt)) {
                Some(ParamValue::Text(s)) => s,
                _ => String::new(),
            };
            json!({"family": t, "style": style})
        }
        ParamValue::Text(t) => json!(t),
        ParamValue::Color(c) => json!(format!(
            "#{:02x}{:02x}{:02x}",
            (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[2].clamp(0.0, 1.0) * 255.0).round() as u8
        )),
        ParamValue::Vec2(v) => json!([v.x, v.y]),
        ParamValue::Bool(b) => json!(b),
        ParamValue::Float(f) => json!(f),
        ParamValue::Choice(c) => json!(c),
        _ => Value::Null,
    }
}

fn clip_item(s: &Session, clip: ClipId) -> Result<TrackItem> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    q.find_item(clip).map(|(_, it)| it.clone()).ok_or_else(|| bad("graphics", "no such clip"))
}

fn mt_at_playhead(s: &Session, it: &TrackItem) -> Tick {
    it.source_time_at(s.playhead().clamp(it.start, it.end() - Tick(1)))
}

/// Set template controls `values` ({control id or name: value}) on `clip`, each as its own
/// property change (callers fold the undo steps).
fn set_controls(s: &mut Session, clip: ClipId, values: &serde_json::Map<String, Value>) -> Result<usize> {
    let it = clip_item(s, clip)?;
    let link = it.graphic.as_ref().and_then(|m| m.template.clone()).ok_or_else(|| bad("graphics.template.set", "the graphic was not made from a template"))?;
    let tl = s.playhead().clamp(it.start, it.end() - Tick(1));
    let mut n = 0;
    for (k, v) in values {
        let c = link
            .controls
            .iter()
            .find(|c| c.id == *k)
            .or_else(|| link.controls.iter().find(|c| c.name.eq_ignore_ascii_case(k)))
            .ok_or_else(|| bad("graphics.template.set", format!("the template has no property `{k}`")))?;
        let ei = it
            .effects
            .iter()
            .position(|e| graphic::is_layer(e) && layer_uid(e) == c.layer)
            .ok_or_else(|| bad("graphics.template.set", "the property's layer is gone"))?;
        let mut props = serde_json::Map::new();
        match (c.kind, v) {
            (ControlKind::Font, Value::Object(o)) => {
                if let Some(f) = o.get("family") {
                    props.insert("font".into(), f.clone());
                }
                if let Some(st) = o.get("style") {
                    props.insert("font_style".into(), st.clone());
                }
            }
            (ControlKind::Slider, Value::Number(x)) => {
                let x = x.as_f64().unwrap_or(0.0).clamp(c.min.unwrap_or(f64::MIN), c.max.unwrap_or(f64::MAX));
                props.insert(c.param.clone(), json!(x));
            }
            _ => {
                props.insert(c.param.clone(), v.clone());
            }
        }
        set_props(s, clip, ei, &props, tl, "Change Template Property")?;
        n += 1;
    }
    Ok(n)
}

/// Keep only the first of the undo steps made since `before`, under `label`.
fn fold_undo(s: &mut Session, before: usize, label: &str) {
    if s.history.undo.len() > before + 1 {
        s.history.undo.truncate(before + 1);
    }
    if let Some(e) = s.history.undo.get_mut(before) {
        e.0 = label.to_string();
    }
}

fn apply_template(s: &mut Session, p: &Value) -> Result<Value> {
    let key = str_p(p, "template").or_else(|| str_p(p, "id")).or_else(|| str_p(p, "name")).ok_or_else(|| bad("graphics.template.apply", "need `template`"))?;
    let entry = find_template(s, key)?;
    let t = entry.template;
    register_fonts(&t);
    let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).ok_or(EngineError::NoSequence)?;
    let (layers, meta) = t.instantiate((w, h));
    let before = s.history.undo.len();
    let dur = t.duration;
    let clip = place_video_clip(s, "graphics.template.apply", &t.name, p, "Apply Graphics Template", layers, move |pr, (w, h, rate)| {
        let src = crate::graphics::graphic_source(pr, w, h, rate);
        (src, rate.snap_nearest(dur))
    })?;
    let meta2 = meta.clone();
    s.edit_sequence("Apply Graphics Template", move |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        it.graphic = Some(Box::new(meta2));
        Ok(())
    })?;
    if let Some(vals) = p.get("values").and_then(Value::as_object) {
        let vals = vals.clone();
        if let Err(e) = set_controls(s, clip, &vals) {
            // undo the whole apply
            fold_undo(s, before, "Apply Graphics Template");
            let _ = s.execute("edit.undo", json!({}));
            return Err(e);
        }
    }
    fold_undo(s, before, "Apply Graphics Template");
    s.state.graphic_layers.clear();
    Ok(json!({"clip": clip.0, "template": t.id, "name": t.name, "controls": t.controls.iter().map(control_json).collect::<Vec<_>>()}))
}

fn template_controls(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.template.controls", "no graphic clip"))?;
    let it = clip_item(s, clip)?;
    let mt = mt_at_playhead(s, &it);
    let Some(link) = it.graphic.as_ref().and_then(|m| m.template.as_ref()) else {
        return Ok(json!({"clip": clip.0, "template": Value::Null, "controls": []}));
    };
    let controls: Vec<Value> = link
        .controls
        .iter()
        .map(|c| {
            let mut j = control_json(c);
            j["value"] = control_value(&it, c, mt);
            j
        })
        .collect();
    Ok(json!({"clip": clip.0, "template": link.id, "name": link.name, "controls": controls}))
}

/// Parse export `controls`: `[{layer: n|name, param, name?, kind?, min?, max?, id?}]`.
fn export_controls(it: &TrackItem, layers: &[EffectInstance], p: &Value) -> Result<Vec<TemplateControl>> {
    let idx = layer_indices(&it.effects);
    let mut out: Vec<TemplateControl> = Vec::new();
    let Some(arr) = p.get("controls").and_then(Value::as_array) else {
        // default: every text layer's text
        for (i, &ei) in idx.iter().enumerate() {
            let e = &it.effects[ei];
            if e.effect == graphic::TEXT_LAYER {
                let name = layer_display_name(e, i);
                out.push(TemplateControl {
                    id: format!("text{}", out.len() + 1),
                    name,
                    kind: ControlKind::Text,
                    layer: layer_uid(&layers[i]),
                    param: "text".into(),
                    min: None,
                    max: None,
                });
            }
        }
        return Ok(out);
    };
    for (n, c) in arr.iter().enumerate() {
        let li = match c.get("layer") {
            Some(Value::Number(x)) => x.as_u64().unwrap_or(0) as usize,
            Some(Value::String(name)) => (0..idx.len())
                .find(|&i| layer_display_name(&it.effects[idx[i]], i).eq_ignore_ascii_case(name))
                .ok_or_else(|| bad("graphics.template.export", format!("no layer named `{name}`")))?,
            _ => return Err(bad("graphics.template.export", "each control needs `layer`")),
        };
        let layer = layers.get(li).ok_or_else(|| bad("graphics.template.export", format!("no layer {li}")))?;
        let param = str_p(c, "param").unwrap_or("text").to_string();
        let param = match param.as_str() {
            "color" | "fillColor" => "fill_color".to_string(),
            "fontSize" => "size".to_string(),
            "visible" | "show" => "enabled".to_string(),
            _ => param,
        };
        if param != "enabled" && !layer.params.contains_key(&param) {
            return Err(bad("graphics.template.export", format!("layer {li} has no property `{param}`")));
        }
        let kind = str_p(c, "kind").and_then(ControlKind::from_name).unwrap_or_else(|| ControlKind::for_param(&param));
        let name =
            str_p(c, "name").map(str::to_string).unwrap_or_else(|| format!("{} {}", layer_display_name(&it.effects[idx[li]], li), param.replace('_', " ")));
        let id = str_p(c, "id").map(str::to_string).unwrap_or_else(|| {
            let base = gtemplate::slug(&name).replace('-', "_");
            if out.iter().any(|o| o.id == base) { format!("{base}_{n}") } else { base }
        });
        let (mut min, mut max) = (f64_p(c, "min"), f64_p(c, "max"));
        if kind == ControlKind::Slider && (min.is_none() || max.is_none()) {
            let d = filmcraft_project::find_effect(&layer.effect).and_then(|d| d.param(&param)).map(|d| d.kind.clone());
            if let Some(filmcraft_project::ParamKind::Float { soft_min, soft_max, .. }) = d {
                min = min.or(Some(soft_min));
                max = max.or(Some(soft_max));
            }
        }
        out.push(TemplateControl { id, name, kind, layer: layer_uid(layer), param, min, max });
    }
    Ok(out)
}

fn export_template(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.template.export", "select a graphic clip"))?;
    let it = clip_item(s, clip)?;
    let size = match s.project.item(it.item).map(|x| &x.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => [*width, *height],
        _ => [1920, 1080],
    };
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| it.name.clone());
    if name.trim().is_empty() {
        return Err(bad("graphics.template.export", "need a `name`"));
    }
    let idx = layer_indices(&it.effects);
    if idx.is_empty() {
        return Err(bad("graphics.template.export", "the graphic has no layers"));
    }
    // layers with uids, keyframes made relative to the clip start
    let mut layers: Vec<EffectInstance> = idx.iter().map(|&i| it.effects[i].clone()).collect();
    let base_uids: Vec<u64> = layers.iter().map(layer_uid).collect();
    ensure_uids(&mut layers);
    if it.source_in != Tick::ZERO {
        for l in &mut layers {
            for prm in l.params.values_mut() {
                prm.keyframes.iter_mut().for_each(|k| k.time -= it.source_in);
            }
        }
    }
    let controls = export_controls(&it, &layers, p)?;
    let mut meta = it.graphic.as_deref().cloned().unwrap_or_default();
    meta.template = None;
    if meta.has_responsive_time() {
        meta.design_in -= it.source_in;
    }
    let mut fonts = Vec::new();
    if bool_p(p, "embedFonts").unwrap_or(false) {
        let license = str_p(p, "fontLicense").unwrap_or("UNKNOWN").to_string();
        let mut seen = HashSet::new();
        for l in &layers {
            let fam = match l.params.get("font").map(|x| &x.value) {
                Some(ParamValue::Text(f)) => f.clone(),
                _ => continue,
            };
            let st = match l.params.get("font_style").map(|x| &x.value) {
                Some(ParamValue::Text(f)) => f.clone(),
                _ => "Regular".into(),
            };
            let r = filmcraft_text::resolve(&fam, &st);
            let face = filmcraft_text::fonts::face(r.face);
            if r.missing || face.info.origin == "bundled" || !seen.insert(r.face) {
                continue;
            }
            if let Some(data) = face.data() {
                fonts.push(FontResource {
                    family: face.info.family.clone(),
                    style: face.info.style.clone(),
                    license: license.clone(),
                    file: String::new(),
                    data: base64_encode(&data),
                });
            }
        }
    }
    let t = GraphicsTemplate {
        id: format!("user:{}", gtemplate::slug(&name)),
        name: name.clone(),
        category: str_p(p, "category").unwrap_or("My Templates").to_string(),
        description: str_p(p, "description").unwrap_or("").to_string(),
        author: str_p(p, "author").unwrap_or("").to_string(),
        license: str_p(p, "license").unwrap_or("").to_string(),
        tags: p.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
        canvas: size,
        duration: it.duration,
        layers,
        graphic: meta,
        controls,
        resources: gtemplate::Resources { fonts },
        ..Default::default()
    };
    t.validate().map_err(|e| EngineError::Other(e.to_string()))?;
    let path = match str_p(p, "path") {
        Some(x) => {
            if is_mogrt(x) {
                return Err(bad("graphics.template.export", "graphics templates are saved as .fcgt"));
            }
            x.to_string()
        }
        None => {
            let dir = user_templates_dir(s).ok_or_else(|| bad("graphics.template.export", "need `path` (no data directory for templates)"))?;
            let _ = std::fs::create_dir_all(&dir);
            dir.join(format!("{}.{TEMPLATE_EXTENSION}", gtemplate::slug(&name))).to_string_lossy().to_string()
        }
    };
    s.services.write_file(&path, t.to_json().as_bytes()).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    // the clip's layers keep the uids the template refers to
    if base_uids.contains(&0) {
        let uids: Vec<u64> = t.layers.iter().map(layer_uid).collect();
        let before = s.history.undo.len();
        let _ = s.edit_sequence("Export Graphics Template", move |q, _, _| {
            let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
            for (ei, uid) in layer_indices(&it.effects).into_iter().zip(uids) {
                it.effects[ei].layer.get_or_insert_with(Default::default).uid = uid;
            }
            Ok(())
        });
        // bookkeeping, not an edit the user undoes
        if s.history.undo.len() > before {
            s.history.undo.pop();
        }
    }
    Ok(json!({"path": path, "id": t.id, "name": t.name, "controls": t.controls.iter().map(control_json).collect::<Vec<_>>(), "fonts": t.resources.fonts.len()}))
}

fn install_template(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("graphics.template.install", "need `path`"))?.to_string();
    if is_mogrt(&path) {
        return Err(EngineError::Other(MOGRT.into()));
    }
    let b = s.services.read_file(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let mut t = GraphicsTemplate::from_bytes(&b).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    if !t.id.starts_with("user:") {
        t.id = format!("user:{}", gtemplate::slug(&t.name));
    }
    let dir = user_templates_dir(s).ok_or_else(|| bad("graphics.template.install", "no data directory for templates"))?;
    let _ = std::fs::create_dir_all(&dir);
    let dest = dir.join(format!("{}.{TEMPLATE_EXTENSION}", gtemplate::slug(&t.name))).to_string_lossy().to_string();
    s.services.write_file(&dest, t.to_json().as_bytes()).map_err(|e| EngineError::Other(format!("{dest}: {e}")))?;
    let fonts = register_fonts(&t);
    s.toast(format!("Installed graphics template \"{}\"", t.name));
    Ok(json!({"id": t.id, "name": t.name, "path": dest, "fontsRegistered": fonts}))
}

fn remove_template(s: &mut Session, p: &Value) -> Result<Value> {
    let key = str_p(p, "template").ok_or_else(|| bad("graphics.template.remove", "need `template`"))?;
    let e = find_template(s, key)?;
    let path = e.path.ok_or_else(|| bad("graphics.template.remove", "built-in templates cannot be removed"))?;
    std::fs::remove_file(&path).map_err(|x| EngineError::Other(format!("{path}: {x}")))?;
    Ok(json!({"removed": e.template.id}))
}

fn template_thumbnail(s: &mut Session, p: &Value) -> Result<Value> {
    let key = str_p(p, "template").ok_or_else(|| bad("graphics.template.thumbnail", "need `template`"))?;
    let e = find_template(s, key)?;
    let (w, h, rgba) = thumbnail(&e.template, u64_p(p, "width").unwrap_or(320) as u32);
    let png = filmcraft_export::encode_png(rgba, w, h).map_err(|x| EngineError::Other(x.to_string()))?;
    if let Some(path) = str_p(p, "path") {
        s.services.write_file(path, &png).map_err(|x| EngineError::Other(format!("{path}: {x}")))?;
        return Ok(json!({"width": w, "height": h, "path": path}));
    }
    Ok(json!({"width": w, "height": h, "pngBase64": base64_encode(&png)}))
}

// ------------------------------------------------------------------------------------------------
// Responsive design
// ------------------------------------------------------------------------------------------------

/// Duration parameter: `<key>Frames` (sequence frames), `<key>Seconds`, or `<key>` in ticks.
fn dur_p(s: &Session, p: &Value, key: &str) -> Option<Tick> {
    let rate = s.sequence_rate();
    if let Some(f) = f64_p(p, &format!("{key}Frames")) {
        return Some(rate.tick_of(f.round() as i64));
    }
    if let Some(sec) = f64_p(p, &format!("{key}Seconds")) {
        return Some(Tick::from_seconds_f64(sec.max(0.0)));
    }
    p.get(key).and_then(Value::as_i64).map(Tick)
}

fn edit_meta(s: &mut Session, clip: ClipId, label: &str, f: impl FnOnce(&mut GraphicMeta, &TrackItem) -> Result<()>) -> Result<GraphicMeta> {
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mut m = it.graphic.as_deref().cloned().unwrap_or_default();
        f(&mut m, it)?;
        it.graphic = (!m.is_empty()).then(|| Box::new(m.clone()));
        Ok(m)
    })
}

fn set_roll(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.setRoll", "no graphic clip"))?;
    let mode = match str_p(p, "mode") {
        Some(m) => Some(RollMode::from_name(m).ok_or_else(|| bad("graphics.setRoll", format!("unknown mode `{m}` (off, roll, crawlLeft, crawlRight)")))?),
        None => None,
    };
    let (pre, ei, eo, post) = (dur_p(s, p, "preroll"), dur_p(s, p, "easeIn"), dur_p(s, p, "easeOut"), dur_p(s, p, "postroll"));
    let (so, eoff) = (bool_p(p, "startOffScreen"), bool_p(p, "endOffScreen"));
    let m = edit_meta(s, clip, "Roll/Crawl Options", |m, _| {
        if let Some(x) = mode {
            m.roll.mode = x;
        }
        if let Some(x) = so {
            m.roll.start_off_screen = x;
        }
        if let Some(x) = eoff {
            m.roll.end_off_screen = x;
        }
        for (dst, v) in [(&mut m.roll.preroll, pre), (&mut m.roll.ease_in, ei), (&mut m.roll.ease_out, eo), (&mut m.roll.postroll, post)] {
            if let Some(v) = v {
                *dst = v.max(Tick::ZERO);
            }
        }
        Ok(())
    })?;
    Ok(json!({"clip": clip.0, "roll": serde_json::to_value(&m.roll).unwrap_or_default()}))
}

fn set_responsive_time(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.setResponsiveTime", "no graphic clip"))?;
    let (intro, outro) = (dur_p(s, p, "intro"), dur_p(s, p, "outro"));
    if intro.is_none() && outro.is_none() {
        return Err(bad("graphics.setResponsiveTime", "need `introFrames` / `outroFrames` (or seconds / ticks)"));
    }
    let m = edit_meta(s, clip, "Responsive Design - Time", |m, it| {
        if !m.has_responsive_time() {
            m.design_in = it.source_in;
            m.design_duration = Tick((it.duration.0 as f64 * it.speed.abs()).round() as i64);
        }
        if let Some(x) = intro {
            m.intro = x.max(Tick::ZERO);
        }
        if let Some(x) = outro {
            m.outro = x.max(Tick::ZERO);
        }
        if m.intro + m.outro > m.design_duration {
            return Err(bad("graphics.setResponsiveTime", "the intro and outro are longer than the graphic"));
        }
        if m.intro == Tick::ZERO && m.outro == Tick::ZERO {
            m.design_in = Tick::ZERO;
            m.design_duration = Tick::ZERO;
        }
        Ok(())
    })?;
    Ok(json!({"clip": clip.0, "intro": m.intro.0, "outro": m.outro.0, "designDuration": m.design_duration.0}))
}

fn edges_p(p: &Value) -> [bool; 4] {
    match p.get("edges") {
        Some(Value::Array(a)) => {
            let has = |n: &str| a.iter().any(|v| v.as_str().is_some_and(|x| x.eq_ignore_ascii_case(n)));
            [has("left"), has("top"), has("right"), has("bottom")]
        }
        Some(Value::String(x)) if x != "all" => {
            let has = |n: &str| x.split([',', ' ']).any(|v| v.eq_ignore_ascii_case(n));
            [has("left"), has("top"), has("right"), has("bottom")]
        }
        _ => [true; 4],
    }
}

fn pin_layer(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.pin", "no graphic clip"))?;
    let (l, ei) = layer_effect_index(s, clip, p)?;
    let it = clip_item(s, clip)?;
    let size = match s.project.item(it.item).map(|x| &x.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => (*width, *height),
        _ => (1920, 1080),
    };
    let idx = layer_indices(&it.effects);
    let to = p.get("to").cloned().unwrap_or(json!("frame"));
    let target: Option<Option<usize>> = match &to {
        Value::String(x) if x == "none" || x.is_empty() => None,
        Value::String(x) if x == "frame" || x.eq_ignore_ascii_case("video frame") => Some(None),
        Value::Number(n) => Some(Some(n.as_u64().unwrap_or(0) as usize)),
        Value::String(name) => Some(Some(
            (0..idx.len())
                .find(|&i| layer_display_name(&it.effects[idx[i]], i).eq_ignore_ascii_case(name))
                .ok_or_else(|| bad("graphics.pin", format!("no layer named `{name}`")))?,
        )),
        _ => return Err(bad("graphics.pin", "`to`: \"frame\", \"none\", a layer index or name")),
    };
    if let Some(Some(t)) = target
        && (t >= idx.len() || t == l)
    {
        return Err(bad("graphics.pin", "pin to another layer of the same graphic"));
    }
    let edges = edges_p(p);
    let mt = mt_at_playhead(s, &it);
    // pins are measured at rest (without the roll / crawl offset)
    let mut rest = it.clone();
    if let Some(m) = rest.graphic.as_mut() {
        m.roll.mode = RollMode::Off;
    }
    let shown = item_layer_specs(&rest, mt, size);
    let bounds_of = |e: usize| shown.iter().find(|(x, _)| *x == e).map(|(_, sp)| layer_bounds(sp));
    let own = bounds_of(ei).ok_or_else(|| bad("graphics.pin", "bad layer"))?;
    let pin = match target {
        None => None,
        Some(None) => Some((PinTarget::Frame, [0.0, 0.0, size.0 as f64, size.1 as f64], None)),
        Some(Some(t)) => Some((PinTarget::Layer(0), bounds_of(idx[t]).ok_or_else(|| bad("graphics.pin", "bad target"))?, Some(idx[t]))),
    };
    let r = s.edit_sequence("Responsive Design - Position", move |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let li = layer_indices(&it.effects);
        let mut layers: Vec<EffectInstance> = li.iter().map(|&i| it.effects[i].clone()).collect();
        ensure_uids(&mut layers);
        for (k, &i) in li.iter().enumerate() {
            it.effects[i].layer = layers[k].layer.clone();
        }
        let new_pin = pin.map(|(to, tb, teff)| {
            let to = match (to, teff) {
                (PinTarget::Layer(_), Some(te)) => PinTarget::Layer(layer_uid(&it.effects[te])),
                _ => PinTarget::Frame,
            };
            Pin::new(to, edges, own, tb)
        });
        it.effects[ei].layer.get_or_insert_with(Default::default).pin = new_pin;
        let uid = it.effects[ei].layer.as_ref().map_or(0, |x| x.uid);
        Ok(json!({"clip": clip.0, "layer": l, "uid": uid, "pin": it.effects[ei].layer.as_ref().and_then(|x| x.pin.as_ref()).map(|p| serde_json::to_value(p).unwrap_or_default())}))
    })?;
    Ok(r)
}

/// Character style from command JSON (friendly names accepted).
fn char_style_p(v: &Value) -> Result<CharStyle> {
    let o = v.as_object().ok_or_else(|| bad("graphics.setCharStyle", "`style` must be an object"))?;
    let mut c = CharStyle::default();
    for (k, x) in o {
        match k.as_str() {
            "font" | "family" => c.font = x.as_str().map(str::to_string),
            "fontStyle" | "font_style" | "style" => c.font_style = x.as_str().map(str::to_string),
            "size" | "fontSize" => c.size = x.as_f64().map(|v| v as f32),
            "fill" | "color" | "fillColor" | "fill_color" => {
                c.fill = Some(match crate::commands::json_to_param(&ParamValue::Color([0.0; 4]), x) {
                    Some(ParamValue::Color(col)) => col,
                    _ => return Err(bad("graphics.setCharStyle", "`color`: \"#rrggbb\" or [r,g,b,a]")),
                })
            }
            "bold" | "fauxBold" | "faux_bold" => c.faux_bold = x.as_bool(),
            "italic" | "fauxItalic" | "faux_italic" => c.faux_italic = x.as_bool(),
            "underline" => c.underline = x.as_bool(),
            "tracking" => c.tracking = x.as_f64().map(|v| v as f32),
            "baselineShift" | "baseline_shift" => c.baseline_shift = x.as_f64().map(|v| v as f32),
            "caps" => {
                c.caps = match x {
                    Value::Number(n) => n.as_u64().map(|v| v.min(2) as u32),
                    Value::String(s) => graphic::CAPS_OPTS.iter().position(|o| o.eq_ignore_ascii_case(s.replace('_', " ").as_str())).map(|i| i as u32),
                    _ => None,
                }
            }
            other => return Err(bad("graphics.setCharStyle", format!("unknown style property `{other}`"))),
        }
    }
    Ok(c)
}

fn set_char_style(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.setCharStyle", "no graphic clip"))?;
    let (l, ei) = layer_effect_index(s, clip, p)?;
    let clear = bool_p(p, "clear").unwrap_or(false);
    let style = match p.get("style") {
        Some(v) => char_style_p(v)?,
        None if clear => CharStyle::default(),
        None => return Err(bad("graphics.setCharStyle", "need `style` (or `clear`)")),
    };
    let tl = s.playhead();
    let (start, end) = (u64_p(p, "start").map(|v| v as usize), u64_p(p, "end").map(|v| v as usize));
    let runs = s.edit_sequence("Character Style", move |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
        let e = &mut it.effects[ei];
        if e.effect != graphic::TEXT_LAYER {
            return Err(bad("graphics.setCharStyle", "the layer is not a text layer"));
        }
        let n = graphic::text_chars(e, mt);
        let (a, b) = (start.unwrap_or(0).min(n), end.unwrap_or(n).min(n));
        if a >= b {
            return Err(bad("graphics.setCharStyle", "empty character range"));
        }
        let x = e.layer.get_or_insert_with(Default::default);
        x.runs = if clear { clear_char_style(&x.runs, n, a, b) } else { apply_char_style(&x.runs, n, a, b, &style) };
        let runs = serde_json::to_value(&x.runs).unwrap_or_default();
        if x.is_empty() {
            e.layer = None;
        }
        Ok(runs)
    })?;
    Ok(json!({"clip": clip.0, "layer": l, "styles": runs}))
}

// ------------------------------------------------------------------------------------------------
// Upgrade Caption to Graphic / Upgrade to Source Graphic
// ------------------------------------------------------------------------------------------------

/// Caption text with `<i>`/`<b>`/`<u>` → plain text and character style runs.
fn caption_runs(text: &str) -> (String, Vec<filmcraft_project::StyleRun>) {
    let mut out = String::new();
    let mut runs: Vec<filmcraft_project::StyleRun> = Vec::new();
    let (mut it, mut bo, mut ul) = (false, false, false);
    let mut rest = text;
    let mut n = 0usize;
    while let Some(ch) = rest.chars().next() {
        if ch == '<'
            && let Some(end) = rest.find('>')
        {
            match rest[1..end].trim().to_ascii_lowercase().as_str() {
                "i" => it = true,
                "/i" => it = false,
                "b" => bo = true,
                "/b" => bo = false,
                "u" => ul = true,
                "/u" => ul = false,
                _ => {}
            }
            rest = &rest[end + 1..];
            continue;
        }
        let decoded = match ch {
            '&' if rest.starts_with("&amp;") => ('&', 5),
            '&' if rest.starts_with("&lt;") => ('<', 4),
            '&' if rest.starts_with("&gt;") => ('>', 4),
            c => (c, c.len_utf8()),
        };
        out.push(decoded.0);
        if it || bo || ul {
            let st = CharStyle { faux_italic: it.then_some(true), faux_bold: bo.then_some(true), underline: ul.then_some(true), ..Default::default() };
            runs = apply_char_style(&runs, n + 1, n, n + 1, &st);
        }
        n += 1;
        rest = &rest[decoded.1..];
    }
    (out, runs)
}

fn upgrade_caption(s: &mut Session, p: &Value) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (w, h) = (seq.settings.width, seq.settings.height);
    let ids: Vec<ClipId> = match p.get("captions").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(ClipId).collect(),
        None if !s.state.caption_selection.is_empty() => s.state.caption_selection.clone(),
        None => {
            let t = s.playhead();
            seq.caption_tracks.iter().find_map(|tr| tr.caption_at(t).map(|c| c.id)).into_iter().collect()
        }
    };
    if ids.is_empty() {
        return Err(bad("graphics.upgradeCaption", "select captions to upgrade"));
    }
    let mut jobs = Vec::new();
    for tr in &seq.caption_tracks {
        for c in tr.captions.iter().filter(|c| ids.contains(&c.id)) {
            jobs.push((c.clone(), tr.style.clone()));
        }
    }
    if jobs.is_empty() {
        return Err(bad("graphics.upgradeCaption", "no such captions"));
    }
    let before = s.history.undo.len();
    let mut clips = Vec::new();
    for (c, st) in &jobs {
        let k = h as f64 / 1080.0;
        let size = st.size as f64 * k;
        let (text, runs) = caption_runs(&c.text);
        let lines = text.lines().count().max(1) as f64;
        let line_h = size * st.line_spacing as f64;
        let x = match st.align {
            filmcraft_project::CaptionAlign::Left => w as f64 * 0.1,
            filmcraft_project::CaptionAlign::Center => w as f64 / 2.0,
            filmcraft_project::CaptionAlign::Right => w as f64 * 0.9,
        };
        let margin = st.margin as f64 * h as f64;
        let first_baseline = match st.anchor {
            filmcraft_project::CaptionAnchor::Bottom => h as f64 - margin - (lines - 1.0) * line_h - size * 0.25,
            filmcraft_project::CaptionAnchor::Top => margin + size,
            filmcraft_project::CaptionAnchor::Middle => h as f64 / 2.0 - (lines - 1.0) * line_h / 2.0 + size * 0.35,
        };
        let mut layer = graphic::new_text_layer(&text, Vec2::new(x, first_baseline), size);
        let set = |e: &mut EffectInstance, k: &str, v: ParamValue| {
            e.params.insert(k.into(), filmcraft_project::Param::new(v));
        };
        let col = |c: [u8; 4]| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0];
        set(&mut layer, "font", ParamValue::Text(st.font.clone()));
        set(&mut layer, "font_style", ParamValue::Text("SemiBold".into()));
        set(&mut layer, "align", ParamValue::Choice(st.align as u32));
        set(&mut layer, "fill_color", ParamValue::Color(col(st.color)));
        set(&mut layer, "leading", ParamValue::Float(line_h - size * 1.21));
        if st.background {
            set(&mut layer, "background", ParamValue::Bool(true));
            set(&mut layer, "background_color", ParamValue::Color(col(st.background_color)));
            set(&mut layer, "background_opacity", ParamValue::Float(st.background_color[3] as f64 / 2.55));
            set(&mut layer, "background_size", ParamValue::Float(size * 0.2));
        }
        if st.outline > 0.0 {
            set(&mut layer, "stroke", ParamValue::Bool(true));
            set(&mut layer, "stroke_color", ParamValue::Color(col(st.outline_color)));
            set(&mut layer, "stroke_width", ParamValue::Float(st.outline as f64 * k));
        }
        if !runs.is_empty() {
            layer.layer = Some(Box::new(LayerExtra { runs, ..Default::default() }));
        }
        let name = text.lines().next().unwrap_or("Caption").chars().take(40).collect::<String>();
        let dur = c.duration;
        let q = json!({"time": c.start.0});
        let clip = place_video_clip(s, "graphics.upgradeCaption", &name, &q, "Upgrade Caption to Graphic", vec![layer], move |pr, (w, h, rate)| {
            (crate::graphics::graphic_source(pr, w, h, rate), dur)
        })?;
        clips.push(clip.0);
    }
    let del: Vec<ClipId> = jobs.iter().map(|j| j.0.id).collect();
    s.edit_sequence("Upgrade Caption to Graphic", move |q, _, st| {
        filmcraft_edit::captions::delete_captions(q, &del, false)?;
        st.caption_selection.clear();
        Ok(())
    })?;
    fold_undo(s, before, "Upgrade Caption to Graphic");
    Ok(json!({"clips": clips, "upgraded": jobs.len()}))
}

fn upgrade_to_source(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.upgradeToSourceGraphic", "select a graphic clip"))?;
    let it = clip_item(s, clip)?;
    if s.project.source_graphics.contains_key(&it.item) {
        return Err(bad("graphics.upgradeToSourceGraphic", "the graphic already is a source graphic"));
    }
    let kind = s.project.item(it.item).map(|x| x.kind.clone()).ok_or_else(|| bad("graphics.upgradeToSourceGraphic", "no item"))?;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let name = it.name.clone();
    let item = s.edit("Upgrade to Source Graphic", move |pr, st| {
        let item = pr.add_item(&name, Label::Rose, kind, None);
        let q = pr.sequence_mut(seq).ok_or(EngineError::NoSequence)?;
        let (_, ti) = q.find_item_mut(clip).ok_or_else(|| bad("graphics.upgradeToSourceGraphic", "no such clip"))?;
        ti.item = item;
        let layers: Vec<EffectInstance> = ti.effects.iter().filter(|e| graphic::is_layer(e)).cloned().collect();
        let meta = ti.graphic.as_deref().cloned();
        pr.source_graphics.insert(item, SourceGraphic { layers, meta });
        st.project_selection = vec![item];
        Ok(item)
    })?;
    Ok(json!({"clip": clip.0, "item": item.0}))
}

/// After a `graphics.*` command: copy an edited instance of a source graphic to the shared
/// layers and every other instance (in the same undo step).
pub fn sync_source_graphics(s: &mut Session) {
    if s.project.source_graphics.is_empty() {
        return;
    }
    let p = Arc::make_mut(&mut s.project);
    let items: Vec<ItemId> = p.source_graphics.keys().copied().collect();
    for item in items {
        let master = p.source_graphics[&item].clone();
        let layers_of = |t: &TrackItem| t.effects.iter().filter(|e| graphic::is_layer(e)).cloned().collect::<Vec<_>>();
        // the first instance that differs from the master is the edited one
        let mut changed: Option<SourceGraphic> = None;
        for q in p.sequences() {
            if let ItemKind::Sequence(sq) = &q.kind {
                for tr in &sq.video_tracks {
                    for t in tr.items.iter().filter(|t| t.item == item) {
                        let l = layers_of(t);
                        let m = t.graphic.as_deref().cloned();
                        if l != master.layers || m != master.meta {
                            changed = Some(SourceGraphic { layers: l, meta: m });
                        }
                    }
                }
            }
            if changed.is_some() {
                break;
            }
        }
        let Some(new) = changed else { continue };
        let ids: Vec<ItemId> = p.sequences().map(|q| q.id).collect();
        for id in ids {
            if let Some(sq) = p.sequence_mut(id) {
                for tr in &mut sq.video_tracks {
                    for t in tr.items.iter_mut().filter(|t| t.item == item) {
                        let mut fx: Vec<EffectInstance> = t.effects.iter().filter(|e| !graphic::is_layer(e)).cloned().collect();
                        let at = fx.iter().position(|e| !e.def().is_some_and(|d| d.intrinsic)).unwrap_or(fx.len());
                        for (k, l) in new.layers.iter().enumerate() {
                            fx.insert(at + k, l.clone());
                        }
                        t.effects = fx;
                        t.graphic = new.meta.clone().map(Box::new);
                    }
                }
            }
        }
        p.source_graphics.insert(item, new);
    }
}

// ------------------------------------------------------------------------------------------------
// Fonts
// ------------------------------------------------------------------------------------------------

/// Every graphic layer (all sequences and source graphics), mutably.
fn for_each_layer(p: &mut Project, mut f: impl FnMut(&mut EffectInstance)) {
    let ids: Vec<ItemId> = p.sequences().map(|q| q.id).collect();
    for id in ids {
        if let Some(sq) = p.sequence_mut(id) {
            for tr in &mut sq.video_tracks {
                for t in &mut tr.items {
                    t.effects.iter_mut().filter(|e| e.effect == graphic::TEXT_LAYER).for_each(&mut f);
                }
            }
        }
    }
    for sg in p.source_graphics.values_mut() {
        sg.layers.iter_mut().filter(|e| e.effect == graphic::TEXT_LAYER).for_each(&mut f);
    }
}

fn text_of(e: &EffectInstance, k: &str) -> String {
    match e.params.get(k).map(|x| &x.value) {
        Some(ParamValue::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

/// Fonts used by graphics (layers and character styles) and caption tracks.
pub fn fonts_used(s: &Session) -> Vec<(String, String, usize)> {
    let mut v: Vec<(String, String, usize)> = Vec::new();
    let mut add = |fam: String, st: String| match v.iter_mut().find(|x| x.0.eq_ignore_ascii_case(&fam) && x.1.eq_ignore_ascii_case(&st)) {
        Some(x) => x.2 += 1,
        None => v.push((fam, st, 1)),
    };
    let mut p = (*s.project).clone();
    for_each_layer(&mut p, |e| {
        let (fam, st) = (text_of(e, "font"), text_of(e, "font_style"));
        add(if fam.is_empty() { "Inter".into() } else { fam.clone() }, if st.is_empty() { "Regular".into() } else { st.clone() });
        for r in e.layer.as_ref().map(|x| x.runs.clone()).unwrap_or_default() {
            if r.style.font.is_some() || r.style.font_style.is_some() {
                add(r.style.font.clone().unwrap_or_else(|| fam.clone()), r.style.font_style.clone().unwrap_or_else(|| st.clone()));
            }
        }
    });
    for q in s.project.sequences() {
        if let ItemKind::Sequence(sq) = &q.kind {
            for t in &sq.caption_tracks {
                add(t.style.font.clone(), "SemiBold".into());
            }
        }
    }
    v.sort();
    v
}

fn replace_fonts(s: &mut Session, p: &Value) -> Result<Value> {
    let (from, from_style) = match p.get("from") {
        Some(Value::String(f)) => (f.clone(), None),
        Some(Value::Object(o)) => {
            (o.get("family").and_then(Value::as_str).unwrap_or("").to_string(), o.get("style").and_then(Value::as_str).map(str::to_string))
        }
        _ => return Err(bad("file.replaceFonts", "need `from` (a family, or {family, style})")),
    };
    let (to, to_style) = match p.get("to") {
        Some(Value::String(f)) => (f.clone(), str_p(p, "toStyle").map(str::to_string)),
        Some(Value::Object(o)) => {
            (o.get("family").and_then(Value::as_str).unwrap_or("").to_string(), o.get("style").and_then(Value::as_str).map(str::to_string))
        }
        _ => return Err(bad("file.replaceFonts", "need `to` (a family, or {family, style})")),
    };
    if from.is_empty() || to.is_empty() {
        return Err(bad("file.replaceFonts", "font families cannot be empty"));
    }
    let n = s.edit("Replace Fonts", move |pr, _| {
        let mut n = 0usize;
        let matches = |fam: &str, st: &str| fam.eq_ignore_ascii_case(&from) && from_style.as_ref().is_none_or(|x| x.eq_ignore_ascii_case(st));
        for_each_layer(pr, |e| {
            let (fam, st) = (text_of(e, "font"), text_of(e, "font_style"));
            if matches(&fam, &st) {
                e.params.insert("font".into(), filmcraft_project::Param::new(ParamValue::Text(to.clone())));
                if let Some(ts) = &to_style {
                    e.params.insert("font_style".into(), filmcraft_project::Param::new(ParamValue::Text(ts.clone())));
                }
                n += 1;
            }
            if let Some(x) = e.layer.as_mut() {
                for r in &mut x.runs {
                    let rf = r.style.font.clone().unwrap_or_else(|| fam.clone());
                    let rs = r.style.font_style.clone().unwrap_or_else(|| st.clone());
                    if r.style.font.is_some() && matches(&rf, &rs) {
                        r.style.font = Some(to.clone());
                        if let Some(ts) = &to_style {
                            r.style.font_style = Some(ts.clone());
                        }
                        n += 1;
                    }
                }
            }
        });
        let ids: Vec<ItemId> = pr.sequences().map(|q| q.id).collect();
        for id in ids {
            if let Some(sq) = pr.sequence_mut(id) {
                for t in &mut sq.caption_tracks {
                    if t.style.font.eq_ignore_ascii_case(&from) && from_style.is_none() {
                        t.style.font = to.clone();
                        n += 1;
                    }
                }
            }
        }
        Ok(n)
    })?;
    Ok(json!({"replaced": n}))
}

fn list_fonts_used(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(Value::Array(
        fonts_used(s)
            .into_iter()
            .map(|(f, st, n)| {
                let missing = filmcraft_text::resolve(&f, &st).missing;
                json!({"family": f, "style": st, "uses": n, "missing": missing})
            })
            .collect(),
    ))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        query(
            "graphics.template.list",
            "List Graphics Templates",
            r#"{"query":str? (search names, categories, descriptions, tags),"category":str?,"source":"builtin|user"?}"#,
            list_templates,
        ),
        spec(
            "graphics.template.apply",
            "Apply Graphics Template",
            &[],
            r#"{"template":id|name|path,"values":{control id or name: value}?,"time":ticks?,"track":index?}"#,
            has_seq,
            apply_template,
        ),
        spec(
            "graphics.template.export",
            "Export As Motion Graphics Template…",
            G,
            r#"{"clip":id?,"name":str,"category":str="My Templates","description":str?,"controls":[{"layer":n|name,"param":"text|fill_color|size|font|position|enabled|…","name":str?,"kind":"text|color|slider|checkbox|font|position"?,"min":f64?,"max":f64?,"id":str?}]? (default: each text layer's text),"path":str? (default: the user templates folder),"embedFonts":bool=false,"fontLicense":str?}"#,
            has_graphic,
            export_template,
        ),
        spec("file.exportGraphicsTemplate", "Motion Graphics Template…", &["File", "Export"], r#"as graphics.template.export"#, has_graphic, export_template),
        spec(
            "graphics.template.install",
            "Install Motion Graphics Template…",
            G,
            r#"{"path":str (a .fcgt file; Adobe .mogrt files are refused)}"#,
            always,
            install_template,
        ),
        spec("graphics.template.remove", "Remove Graphics Template", &[], r#"{"template":id|name (a user template)}"#, always, remove_template),
        spec(
            "graphics.template.set",
            "Set Template Property",
            &[],
            r#"{"clip":id?,"control":id|name,"value":any} or {"clip":id?,"values":{control: value}}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.template.set", "no graphic clip"))?;
                let mut vals = p.get("values").and_then(Value::as_object).cloned().unwrap_or_default();
                if let Some(c) = str_p(p, "control") {
                    vals.insert(c.to_string(), p.get("value").cloned().unwrap_or(Value::Null));
                }
                if vals.is_empty() {
                    return Err(bad("graphics.template.set", "need `control` and `value`, or `values`"));
                }
                let before = s.history.undo.len();
                let n = set_controls(s, clip, &vals);
                fold_undo(s, before, "Change Template Property");
                Ok(json!({"clip": clip.0, "set": n?}))
            },
        ),
        query("graphics.template.controls", "List Template Properties", r#"{"clip":id?}"#, template_controls),
        query(
            "graphics.template.thumbnail",
            "Graphics Template Thumbnail",
            r#"{"template":id|name|path,"width":px=320,"path":str? (write a PNG there; else returned as pngBase64)}"#,
            template_thumbnail,
        ),
        spec(
            "graphics.setRoll",
            "Roll/Crawl Options",
            &[],
            r#"{"clip":id?,"mode":"off|roll|crawlLeft|crawlRight"?,"startOffScreen":bool?,"endOffScreen":bool?,"prerollFrames":n?,"easeInFrames":n?,"easeOutFrames":n?,"postrollFrames":n? (or …Seconds, or ticks as preroll/easeIn/easeOut/postroll)}"#,
            has_graphic,
            set_roll,
        ),
        spec(
            "graphics.setResponsiveTime",
            "Responsive Design - Time",
            &[],
            r#"{"clip":id?,"introFrames":n?,"outroFrames":n? (or introSeconds / outroSeconds, or ticks as intro / outro)}"#,
            has_graphic,
            set_responsive_time,
        ),
        spec(
            "graphics.pin",
            "Responsive Design - Position",
            &[],
            r#"{"clip":id?,"layer":n|name?,"to":"frame"|"none"|layer index|layer name,"edges":["left","top","right","bottom"]|"all"="all"}"#,
            has_graphic,
            pin_layer,
        ),
        spec(
            "graphics.setCharStyle",
            "Character Style",
            &[],
            r##"{"clip":id?,"layer":n|name?,"start":char=0,"end":char=len,"style":{"font":str,"fontStyle":str,"size":px,"color":"#rrggbb","bold":bool,"italic":bool,"underline":bool,"tracking":n,"baselineShift":px,"caps":"normal|all caps|small caps"},"clear":bool? (remove styles from the range)}"##,
            has_graphic,
            set_char_style,
        ),
        spec(
            "graphics.upgradeCaption",
            "Upgrade Caption to Graphic",
            G,
            r#"{"captions":[id]? (default: the selected captions, else the one under the playhead)}"#,
            has_captions,
            upgrade_caption,
        ),
        spec("graphics.upgradeToSourceGraphic", "Upgrade to Source Graphic", G, r#"{"clip":id?}"#, has_graphic, upgrade_to_source),
        spec(
            "file.replaceFonts",
            "Replace Fonts in Projects…",
            G,
            r#"{"from":family|{"family","style"?},"to":family|{"family","style"?},"toStyle":str?}"#,
            always,
            replace_fonts,
        ),
        query("graphics.fonts.used", "List Fonts Used", "{}", list_fonts_used),
    ]
}

#[cfg(test)]
#[path = "graphic_templates_tests.rs"]
mod tests;

use super::labels::LabelPreset;
use super::media_cache::{cache_usage, clean};
use super::schema::{CATEGORIES, Kind, fields};
use super::{CommandSpec, EngineError, Label, Path, Result, Session, Value, always, bad, bool_p, json, str_p};

// ------------------------------------------------------------------ commands

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

fn kind_json(k: Kind) -> Value {
    match k {
        Kind::Bool => json!({"type": "bool"}),
        Kind::Int { min, max, unit } => json!({"type": "int", "min": min, "max": max, "unit": unit}),
        Kind::Float { min, max, decimals, unit } => json!({"type": "float", "min": min, "max": max, "decimals": decimals, "unit": unit}),
        Kind::Choice(o) => json!({"type": "choice", "choices": o.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>()}),
        Kind::Duration { unit_key } => json!({"type": "duration", "unitKey": unit_key}),
        Kind::Text => json!({"type": "text"}),
        Kind::Path => json!({"type": "path"}),
        Kind::Color => json!({"type": "color"}),
        Kind::Font => json!({"type": "font"}),
        Kind::Device(d) => json!({"type": "device", "list": format!("{d:?}").to_ascii_lowercase()}),
        Kind::Label => json!({"type": "label", "choices": Label::ALL.iter().map(|l| l.name()).collect::<Vec<_>>()}),
    }
}

fn schema(s: &mut Session, p: &Value) -> Result<Value> {
    let want = str_p(p, "category");
    let cats: Vec<Value> = CATEGORIES
        .iter()
        .filter(|c| want.is_none_or(|w| w == c.id))
        .map(|c| {
            let fields: Vec<Value> = fields()
                .into_iter()
                .filter(|(cat, _)| *cat == c.id)
                .map(|(_, f)| {
                    json!({"key": f.key, "label": f.label, "kind": kind_json(f.kind), "wired": f.wired, "enabledBy": f.enabled_by, "value": s.prefs.get(f.key)})
                })
                .collect();
            json!({"id": c.id, "title": c.title, "command": format!("app.settings.{}", c.id), "fields": fields})
        })
        .collect();
    if let Some(w) = want
        && cats.is_empty()
    {
        return Err(bad("prefs.schema", format!("unknown category `{w}`")));
    }
    Ok(json!({"categories": cats}))
}

fn cache_info(s: &mut Session, _: &Value) -> Result<Value> {
    let dir = s.media_cache_dir();
    let (bytes, files) = dir.as_deref().map(cache_usage).unwrap_or((0, 0));
    Ok(json!({"location": dir.map(|d| d.to_string_lossy().into_owned()), "bytes": bytes, "files": files, "policy": s.prefs.media_cache.management}))
}

fn cache_clean(s: &mut Session, p: &Value) -> Result<Value> {
    let dir = s.media_cache_dir().ok_or_else(|| EngineError::Other("there is no media cache folder in this session".into()))?;
    // the open project's previews stay unless `all` is set
    let keep = if bool_p(p, "all") == Some(true) { None } else { s.previews.dir() };
    let (files, bytes) = clean(&dir, keep.as_deref());
    if keep.is_none() {
        s.previews.set_dir(s.previews.dir());
    }
    Ok(json!({"files": files, "bytes": bytes, "location": dir.to_string_lossy()}))
}

fn presets_json(s: &Session) -> Value {
    let presets: Vec<Value> = s.label_presets().into_iter().map(|(p, file)| json!({"name": p.name, "builtin": file.is_none(), "labels": p.labels})).collect();
    json!({"dir": s.label_presets_dir(), "selected": s.prefs.labels.preset, "presets": presets})
}

fn label_presets(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(presets_json(s))
}

fn import_label_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("labels.importPreset", "need `path`"))?;
    let preset = s.import_label_preset(Path::new(path)).map_err(|e| bad("labels.importPreset", e))?;
    Ok(json!({"name": preset.name, "presets": presets_json(s)}))
}

fn export_label_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("labels.exportPreset", "need `path`"))?;
    let preset = LabelPreset::export(Path::new(path), &s.prefs.labels).map_err(|e| bad("labels.exportPreset", e))?;
    Ok(json!({"name": preset.name, "path": path}))
}

fn delete_label_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").ok_or_else(|| bad("labels.deletePreset", "need `name`"))?;
    s.delete_label_preset(name).map_err(|e| bad("labels.deletePreset", e))?;
    Ok(presets_json(s))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("prefs.schema", "Settings Schema", r#"{"category":str?}"#, schema, false),
        spec("mediaCache.info", "Media Cache Info", "{}", cache_info, false),
        spec("mediaCache.clean", "Delete Media Cache Files", r#"{"all":bool?}"#, cache_clean, true),
        spec("labels.presets", "Label Color Presets", "{}", label_presets, false),
        spec("labels.importPreset", "Import Label Color Preset", r#"{"path":str}"#, import_label_preset, true),
        spec("labels.exportPreset", "Export Label Color Preset", r#"{"path":str}"#, export_label_preset, true),
        spec("labels.deletePreset", "Delete Label Color Preset", r#"{"name":str}"#, delete_label_preset, true),
    ]
}

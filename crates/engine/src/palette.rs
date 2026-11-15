//! Saved swatch operations do not change colors already assigned to media or markers.
use crate::{Result, Session, commands::bad};
use filmcraft_project::HexColor;
use serde_json::{Value, json};
fn color(p: &Value, key: &str, id: &str) -> Result<HexColor> {
    p.get(key).and_then(Value::as_str).and_then(HexColor::parse).ok_or_else(|| bad(id, "need a six-digit hex color"))
}
pub(crate) fn update(s: &mut Session, p: &Value) -> Result<Value> {
    let id = "colors.updateCustom";
    let old = color(p, "color", id)?;
    let new = if p.get("replacement").is_some() { color(p, "replacement", id)? } else { old };
    let name = p
        .get("name")
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= 128 && !s.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or_else(|| bad(id, "name must be at most 128 bytes without control characters"))
        })
        .transpose()?;
    if !s.project.custom_colors.contains(&old) {
        return Err(bad(id, "no such saved color"));
    }
    if old != new && s.project.custom_colors.contains(&new) {
        return Err(bad(id, "that color is already saved"));
    }
    s.edit("Edit Custom Color", |project, _| {
        for c in &mut project.custom_colors {
            if *c == old {
                *c = new;
            }
        }
        let previous = project.custom_color_names.remove(&old).unwrap_or_default();
        let name = name.unwrap_or(previous);
        if !name.trim().is_empty() {
            project.custom_color_names.insert(new, name);
        }
        Ok(())
    })?;
    Ok(json!({"color":new}))
}
pub(crate) fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let id = "colors.removeCustom";
    let old = color(p, "color", id)?;
    if !s.project.custom_colors.contains(&old) {
        return Err(bad(id, "no such saved color"));
    }
    s.edit("Remove Custom Color", |project, _| {
        project.custom_colors.retain(|c| *c != old);
        project.custom_color_names.remove(&old);
        Ok(())
    })?;
    Ok(Value::Null)
}

pub(crate) fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let id = "colors.addCustom";
    let color = color(p, "color", id)?;
    let name = p
        .get("name")
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= 128 && !s.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or_else(|| bad(id, "name must be at most 128 bytes without control characters"))
        })
        .transpose()?;
    if s.project.custom_colors.contains(&color) {
        if let Some(name) = name {
            return update(s, &json!({"color":color,"name":name}));
        }
        return Ok(json!({"color":color}));
    }
    if s.project.custom_colors.len() >= 64 {
        return Err(bad(id, "the project already has 64 custom colors"));
    }
    s.edit("Add Custom Color", |project, _| {
        project.custom_colors.push(color);
        if let Some(name) = name.filter(|s| !s.trim().is_empty()) {
            project.custom_color_names.insert(color, name);
        }
        Ok(())
    })?;
    Ok(json!({"color":color}))
}

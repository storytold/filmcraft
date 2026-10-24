//! Commands for native lined scripts. Text import accepts content, not a filesystem path.

use std::sync::Arc;

use filmcraft_project::{
    BinId, ItemId, ItemKind, Label, MediaIdentity, MediaRef, Project, ScriptColor, ScriptDocument, ScriptLine, ScriptLineColor, ScriptLineId, ScriptMark,
    ScriptMarkId, ScriptSlate, ScriptSlateId, ScriptTake, ScriptTakeId,
};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

fn id(p: &Value, key: &str, cmd: &str) -> Result<u64> {
    p.get(key).and_then(Value::as_u64).filter(|n| *n > 0).ok_or_else(|| bad(cmd, format!("need a positive `{key}` ID")))
}

fn text<'a>(p: &'a Value, key: &str, cmd: &str, max_bytes: usize) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|s| s.len() <= max_bytes).ok_or_else(|| bad(cmd, format!("need `{key}` text of at most {max_bytes} bytes")))
}

fn script_mut<'a>(project: &'a mut Project, item: ItemId, cmd: &str) -> Result<&'a mut ScriptDocument> {
    project.item_mut(item).and_then(filmcraft_project::ProjectItem::as_script_mut).ok_or_else(|| bad(cmd, "item is not a Script"))
}

fn script<'a>(project: &'a Project, item: ItemId, cmd: &str) -> Result<&'a ScriptDocument> {
    project.item(item).and_then(filmcraft_project::ProjectItem::as_script).ok_or_else(|| bad(cmd, "item is not a Script"))
}

fn source_identity(project: &Project, source: ItemId) -> Option<MediaIdentity> {
    project.resolve_media(source).and_then(|(_, clip, _)| clip.identity)
}

fn allowed_source(project: &Project, source: ItemId) -> Result<(String, Option<MediaIdentity>)> {
    let Some(it) = project.item(source) else { return Err(bad("script.addTake", "source item does not exist")) };
    let valid = matches!(it.kind, ItemKind::Media(_) | ItemKind::Subclip { .. }) || matches!(&it.kind, ItemKind::Sequence(seq) if seq.multicam.is_some());
    if !valid {
        return Err(bad("script.addTake", "source must be a media clip, subclip, or multicamera source sequence"));
    }
    Ok((it.name.clone(), source_identity(project, source)))
}

fn item(p: &Value, cmd: &str) -> Result<ItemId> {
    Ok(ItemId(id(p, "item", cmd)?))
}

fn create(s: &mut Session, p: &Value, import: bool) -> Result<Value> {
    let cmd = if import { "script.importText" } else { "script.create" };
    let name = text(p, "name", cmd, 256)?.trim();
    if name.is_empty() {
        return Err(bad(cmd, "name must not be empty"));
    }
    let doc = if import {
        ScriptDocument::from_text(text(p, "text", cmd, ScriptDocument::MAX_TEXT_BYTES)?).map_err(|e| bad(cmd, e))?
    } else {
        let mut doc = ScriptDocument::new();
        let id = ScriptLineId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        doc.lines.push(ScriptLine { id, text: String::new(), scene: None, page: None });
        doc
    };
    let count = doc.lines.len();
    let bin = p.get("bin").and_then(Value::as_u64).map(BinId);
    let added = s.edit(if import { "Import Script Text" } else { "New Script" }, |project, state| {
        if let Some(bin) = bin
            && project.root.find_bin(bin).is_none()
        {
            return Err(bad(cmd, "bin does not exist"));
        }
        let added = project.add_item(name, Label::Blue, ItemKind::Script(Arc::new(doc)), bin);
        state.project_selection = vec![added];
        Ok(added)
    })?;
    Ok(json!({"item":added.0,"lines":count}))
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let item = item(p, "script.inspect")?;
    let it = s.project.item(item).ok_or_else(|| bad("script.inspect", "item does not exist"))?;
    let doc = it.as_script().ok_or_else(|| bad("script.inspect", "item is not a Script"))?;
    Ok(json!({"item":item.0,"name":it.name,"document":doc}))
}

#[derive(Clone, Deserialize, Serialize)]
struct ScriptSection {
    lines: Vec<ScriptLine>,
    slates: Vec<ScriptSlate>,
}

fn copy_section(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.copySection";
    let doc = script(&s.project, item(p, cmd)?, cmd)?;
    let start = ScriptLineId(id(p, "startLine", cmd)?);
    let end = ScriptLineId(id(p, "endLine", cmd)?);
    let (first, last) = doc.line_range(start, end).ok_or_else(|| bad(cmd, "selected lines must exist in script order"))?;
    let selected: std::collections::BTreeSet<_> = p
        .get("slates")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(cmd, "need a list of selected setup IDs"))?
        .iter()
        .map(|value| value.as_u64().filter(|id| *id > 0).map(ScriptSlateId).ok_or_else(|| bad(cmd, "invalid setup ID")))
        .collect::<Result<_>>()?;
    if selected.iter().any(|id| !doc.slates.iter().any(|slate| slate.id == *id)) {
        return Err(bad(cmd, "selected setup does not exist"));
    }
    let lines = doc.lines[first..=last].to_vec();
    let positions: std::collections::BTreeMap<_, _> = doc.lines.iter().enumerate().map(|(i, line)| (line.id, i)).collect();
    let inside = |line: ScriptLineId| positions.get(&line).is_some_and(|i| (first..=last).contains(i));
    let mut slates = Vec::new();
    for slate in doc.slates.iter().filter(|slate| selected.contains(&slate.id)) {
        let mut copied = slate.clone();
        copied.takes = slate
            .takes
            .iter()
            .filter_map(|take| {
                let a = *positions.get(&take.start_line)?;
                let b = *positions.get(&take.end_line)?;
                let overlap_start = a.max(first);
                let overlap_end = b.min(last);
                if overlap_start > overlap_end {
                    return None;
                }
                let mut take = take.clone();
                take.start_line = doc.lines[overlap_start].id;
                take.end_line = doc.lines[overlap_end].id;
                take.marks.retain(|mark| inside(mark.line));
                take.offscreen_lines.retain(|line| inside(*line));
                take.line_colors.retain(|entry| inside(entry.line));
                Some(take)
            })
            .collect();
        if copied.takes.is_empty() {
            continue;
        }
        copied.thumbnail_line = Some(copied.thumbnail_line.filter(|line| inside(*line)).unwrap_or(copied.takes[0].start_line));
        slates.push(copied);
    }
    serde_json::to_value(ScriptSection { lines, slates }).map_err(|error| bad(cmd, error.to_string()))
}

fn paste_section(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.pasteSection";
    let item = item(p, cmd)?;
    let after = p.get("afterLine").filter(|v| !v.is_null()).map(|_| id(p, "afterLine", cmd).map(ScriptLineId)).transpose()?;
    let section: ScriptSection = serde_json::from_value(p.get("section").cloned().ok_or_else(|| bad(cmd, "need a Script section"))?)
        .map_err(|error| bad(cmd, format!("invalid Script section: {error}")))?;
    if section.lines.is_empty() {
        return Err(bad(cmd, "section has no lines"));
    }
    let mut fragment = ScriptDocument { next_id: u64::MAX, lines: section.lines.clone(), slates: section.slates.clone() };
    fragment.validate().map_err(|error| bad(cmd, format!("invalid Script section: {error}")))?;
    let (first, last) = s.edit("Paste Script Section", |project, _| {
        let mut draft = script(project, item, cmd)?.clone();
        let insert_at = match after {
            Some(line) => draft.line_index(line).ok_or_else(|| bad(cmd, "paste row does not exist"))? + 1,
            None => draft.lines.len(),
        };
        let mut line_ids = std::collections::BTreeMap::new();
        for line in &mut fragment.lines {
            let old = line.id;
            line.id = ScriptLineId(draft.alloc_id().map_err(|error| bad(cmd, error))?);
            line_ids.insert(old, line.id);
        }
        let first = fragment.lines.first().map(|line| line.id.0).unwrap_or_default();
        let last = fragment.lines.last().map(|line| line.id.0).unwrap_or_default();
        for slate in &mut fragment.slates {
            slate.id = ScriptSlateId(draft.alloc_id().map_err(|error| bad(cmd, error))?);
            slate.thumbnail_line = slate.thumbnail_line.and_then(|line| line_ids.get(&line).copied());
            for take in &mut slate.takes {
                take.id = ScriptTakeId(draft.alloc_id().map_err(|error| bad(cmd, error))?);
                take.start_line = *line_ids.get(&take.start_line).ok_or_else(|| bad(cmd, "take start is missing"))?;
                take.end_line = *line_ids.get(&take.end_line).ok_or_else(|| bad(cmd, "take end is missing"))?;
                if take.source.is_some_and(|source| allowed_source(project, source).is_err()) {
                    take.source = None;
                }
                for mark in &mut take.marks {
                    mark.id = ScriptMarkId(draft.alloc_id().map_err(|error| bad(cmd, error))?);
                    mark.line = *line_ids.get(&mark.line).ok_or_else(|| bad(cmd, "mark line is missing"))?;
                }
                for line in &mut take.offscreen_lines {
                    *line = *line_ids.get(line).ok_or_else(|| bad(cmd, "off-screen line is missing"))?;
                }
                for entry in &mut take.line_colors {
                    entry.line = *line_ids.get(&entry.line).ok_or_else(|| bad(cmd, "color line is missing"))?;
                }
            }
        }
        draft.lines.splice(insert_at..insert_at, fragment.lines.iter().cloned());
        draft.slates.extend(fragment.slates.iter().cloned());
        draft.validate().map_err(|error| bad(cmd, error))?;
        *script_mut(project, item, cmd)? = draft;
        Ok((first, last))
    })?;
    Ok(json!({"startLine":first,"endLine":last}))
}

fn edit_line(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.editLine";
    let item = item(p, cmd)?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    let value = text(p, "text", cmd, ScriptDocument::MAX_LINE_BYTES)?.to_owned();
    s.edit("Edit Script Line", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let old = doc.lines.iter().find(|row| row.id == line).ok_or_else(|| bad(cmd, "line does not exist"))?.text.len();
        let total = doc.text_bytes().ok_or_else(|| bad(cmd, "script text size overflow"))?;
        let updated = total.checked_sub(old).and_then(|bytes| bytes.checked_add(value.len())).ok_or_else(|| bad(cmd, "script text size overflow"))?;
        if updated > ScriptDocument::MAX_TEXT_BYTES {
            return Err(bad(cmd, "script text would exceed the 4 MiB limit"));
        }
        let line = doc.lines.iter_mut().find(|row| row.id == line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        line.text = value;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn split_line(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.splitLine";
    let item = item(p, cmd)?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    let value = text(p, "text", cmd, ScriptDocument::MAX_LINE_BYTES)?;
    let at = p.get("at").and_then(Value::as_u64).and_then(|value| usize::try_from(value).ok()).ok_or_else(|| bad(cmd, "need a valid character offset `at`"))?;
    let byte = value
        .char_indices()
        .nth(at)
        .map_or_else(|| (at == value.chars().count()).then_some(value.len()), |(index, _)| Some(index))
        .ok_or_else(|| bad(cmd, "split offset is outside the line"))?;
    let (before, after) = value.split_at(byte);
    let created = s.edit("Split Script Line", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let index = doc.line_index(line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        if doc.lines.len() >= ScriptDocument::MAX_LINES {
            return Err(bad(cmd, "script already has 50,000 lines"));
        }
        let total = doc.text_bytes().ok_or_else(|| bad(cmd, "script text size overflow"))?;
        let updated = total
            .checked_sub(doc.lines[index].text.len())
            .and_then(|n| n.checked_add(value.len()))
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| bad(cmd, "script text size overflow"))?;
        if updated > ScriptDocument::MAX_TEXT_BYTES {
            return Err(bad(cmd, "script text would exceed the 4 MiB limit"));
        }
        let created = ScriptLineId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        doc.lines[index].text = before.to_owned();
        doc.lines.insert(index + 1, ScriptLine { id: created, text: after.to_owned(), scene: None, page: None });
        for take in doc.slates.iter_mut().flat_map(|slate| &mut slate.takes) {
            if take.end_line == line {
                take.end_line = created;
            }
        }
        Ok(created)
    })?;
    Ok(json!({"line":created.0}))
}

fn merge_with_previous(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.mergeWithPrevious";
    let item = item(p, cmd)?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    // The editor may contain changes that have not yet been committed as a
    // separate command. Merge those bytes in the same undoable transaction.
    let value = text(p, "text", cmd, ScriptDocument::MAX_LINE_BYTES)?;
    let (previous, merged, caret) = s.edit("Join Script Lines", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let index = doc.line_index(line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        if index == 0 {
            return Err(bad(cmd, "first line has no preceding row"));
        }
        let prior = &doc.lines[index - 1];
        let caret = prior.text.chars().count();
        let merged = format!("{}{value}", prior.text);
        if merged.len() > ScriptDocument::MAX_LINE_BYTES {
            return Err(bad(cmd, "joined line would exceed the line size limit"));
        }
        let total = doc.text_bytes().ok_or_else(|| bad(cmd, "script text size overflow"))?;
        let updated = total
            .checked_sub(doc.lines[index].text.len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .and_then(|bytes| bytes.checked_sub(1))
            .ok_or_else(|| bad(cmd, "script text size overflow"))?;
        if updated > ScriptDocument::MAX_TEXT_BYTES {
            return Err(bad(cmd, "script text would exceed the 4 MiB limit"));
        }
        let previous = prior.id;
        doc.lines[index - 1].text = merged.clone();
        // delete_line removes the old row's marks and keeps marks already on
        // the preceding row, while adjusting candle endpoints atomically.
        doc.delete_line(line);
        Ok((previous, merged, caret))
    })?;
    Ok(json!({"line":previous.0,"text":merged,"at":caret}))
}

fn shift_slate(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.shiftSlate";
    let item = item(p, cmd)?;
    let slate = ScriptSlateId(id(p, "slate", cmd)?);
    let delta =
        p.get("delta").and_then(Value::as_i64).and_then(|value| isize::try_from(value).ok()).ok_or_else(|| bad(cmd, "need signed row offset `delta`"))?;
    let x = p
        .get("x")
        .map(|value| {
            value.as_f64().filter(|x| x.is_finite() && (0.0..=1.0).contains(x)).map(|x| x as f32).ok_or_else(|| bad(cmd, "x must be a number between 0 and 1"))
        })
        .transpose()?;
    s.edit("Move Script Setup", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let ids: Vec<_> = doc.lines.iter().map(|line| line.id).collect();
        let positions: std::collections::HashMap<_, _> = ids.iter().enumerate().map(|(index, id)| (*id, index)).collect();
        let moved = |line: ScriptLineId| -> Result<ScriptLineId> {
            let index = positions.get(&line).copied().ok_or_else(|| bad(cmd, "setup refers to a missing line"))?;
            let target =
                index.checked_add_signed(delta).and_then(|index| ids.get(index)).copied().ok_or_else(|| bad(cmd, "setup would move outside the script"))?;
            Ok(target)
        };
        let slate = doc.slates.iter_mut().find(|candidate| candidate.id == slate).ok_or_else(|| bad(cmd, "setup does not exist"))?;
        let thumbnail = slate.thumbnail_line.map(moved).transpose()?;
        for take in &slate.takes {
            moved(take.start_line)?;
            moved(take.end_line)?;
            for line in &take.offscreen_lines {
                moved(*line)?;
            }
            for entry in &take.line_colors {
                moved(entry.line)?;
            }
            for mark in &take.marks {
                moved(mark.line)?;
            }
        }
        for take in &mut slate.takes {
            take.start_line = moved(take.start_line)?;
            take.end_line = moved(take.end_line)?;
            for line in &mut take.offscreen_lines {
                *line = moved(*line)?;
            }
            for entry in &mut take.line_colors {
                entry.line = moved(entry.line)?;
            }
            for mark in &mut take.marks {
                mark.line = moved(mark.line)?;
            }
        }
        slate.thumbnail_line = thumbnail;
        if let Some(x) = x {
            slate.thumbnail_x = Some(x);
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn move_take(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.moveTake";
    let item = item(p, cmd)?;
    let take = ScriptTakeId(id(p, "take", cmd)?);
    let direction = p.get("direction").and_then(Value::as_i64).filter(|value| matches!(value, -1 | 1)).ok_or_else(|| bad(cmd, "direction must be -1 or 1"))?;
    let index = s.edit("Reorder Script Candle", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let slate =
            doc.slates.iter_mut().find(|slate| slate.takes.iter().any(|candidate| candidate.id == take)).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let index = slate.takes.iter().position(|candidate| candidate.id == take).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let next =
            index.checked_add_signed(direction as isize).filter(|index| *index < slate.takes.len()).ok_or_else(|| bad(cmd, "take is already at that edge"))?;
        slate.takes.swap(index, next);
        Ok(next)
    })?;
    Ok(json!({"index":index}))
}

fn set_thumbnail_time(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.setThumbnailTime";
    let item = item(p, cmd)?;
    let take = ScriptTakeId(id(p, "take", cmd)?);
    let time = p.get("time").and_then(Value::as_i64).filter(|time| *time >= 0).ok_or_else(|| bad(cmd, "time must be nonnegative ticks"))?;
    s.edit("Set Script Thumbnail Frame", |project, _| {
        let take = script_mut(project, item, cmd)?.take_mut(take).ok_or_else(|| bad(cmd, "take does not exist"))?;
        take.thumbnail_time = Some(Tick(time));
        Ok(())
    })?;
    Ok(json!({"time":time}))
}

fn move_thumbnail(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.moveThumbnail";
    let item = item(p, cmd)?;
    let slate = ScriptSlateId(id(p, "slate", cmd)?);
    let line = ScriptLineId(id(p, "line", cmd)?);
    let x = p
        .get("x")
        .map(|value| {
            value.as_f64().filter(|x| x.is_finite() && (0.0..=1.0).contains(x)).map(|x| x as f32).ok_or_else(|| bad(cmd, "x must be a number between 0 and 1"))
        })
        .transpose()?;
    s.edit("Move Script Thumbnail", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        if doc.line_index(line).is_none() {
            return Err(bad(cmd, "line does not exist"));
        }
        let slate = doc.slates.iter_mut().find(|candidate| candidate.id == slate).ok_or_else(|| bad(cmd, "setup does not exist"))?;
        slate.thumbnail_line = Some(line);
        if let Some(x) = x {
            slate.thumbnail_x = Some(x);
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn insert_line(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.insertLine";
    let item = item(p, cmd)?;
    let after = p.get("after").filter(|v| !v.is_null()).map(|_| id(p, "after", cmd).map(ScriptLineId)).transpose()?;
    let value = text(p, "text", cmd, ScriptDocument::MAX_LINE_BYTES)?.to_owned();
    let created = s.edit("Insert Script Line", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        if doc.lines.len() >= ScriptDocument::MAX_LINES {
            return Err(bad(cmd, "script already has 50,000 lines"));
        }
        let total = doc.text_bytes().ok_or_else(|| bad(cmd, "script text size overflow"))?;
        let extra_separator = usize::from(!doc.lines.is_empty());
        let updated =
            total.checked_add(value.len()).and_then(|bytes| bytes.checked_add(extra_separator)).ok_or_else(|| bad(cmd, "script text size overflow"))?;
        if updated > ScriptDocument::MAX_TEXT_BYTES {
            return Err(bad(cmd, "script text would exceed the 4 MiB limit"));
        }
        let index = match after {
            Some(line) => doc.line_index(line).ok_or_else(|| bad(cmd, "after line does not exist"))?.saturating_add(1),
            None => 0,
        };
        let created = ScriptLineId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        doc.lines.insert(index, ScriptLine { id: created, text: value, scene: None, page: None });
        Ok(created)
    })?;
    Ok(json!({"line":created.0}))
}

fn delete_line(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.deleteLine";
    let item = item(p, cmd)?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    s.edit("Delete Script Line", |project, _| {
        if !script_mut(project, item, cmd)?.delete_line(line) {
            return Err(bad(cmd, "line does not exist"));
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_label(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.setLabel";
    let item = item(p, cmd)?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    let field = |key: &str| -> Result<Option<Option<String>>> {
        match p.get(key) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(None)),
            Some(v) => v
                .as_str()
                .filter(|v| v.len() <= 256)
                .map(|v| Some(Some(v.to_owned())))
                .ok_or_else(|| bad(cmd, format!("`{key}` must be text of at most 256 bytes or null"))),
        }
    };
    let scene = field("scene")?;
    let page = field("page")?;
    s.edit("Set Script Scene/Page", |project, _| {
        let row = script_mut(project, item, cmd)?.lines.iter_mut().find(|row| row.id == line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        if let Some(value) = scene {
            row.scene = value
        }
        if let Some(value) = page {
            row.page = value
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn add_slate(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.addSlate";
    let item = item(p, cmd)?;
    let name = text(p, "name", cmd, 256)?.to_owned();
    let slate = s.edit("Add Script Slate", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        if doc.slates.len() >= ScriptDocument::MAX_SLATES {
            return Err(bad(cmd, "script already has the maximum number of slates"));
        }
        let slate = ScriptSlateId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        doc.slates.push(ScriptSlate { id: slate, name, thumbnail_line: None, thumbnail_x: None, takes: Vec::new() });
        Ok(slate)
    })?;
    Ok(json!({"slate":slate.0}))
}

fn add_take(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.addTake";
    let item = item(p, cmd)?;
    let source = ItemId(id(p, "source", cmd)?);
    let start = ScriptLineId(id(p, "startLine", cmd)?);
    let end = ScriptLineId(id(p, "endLine", cmd)?);
    let existing_slate = p.get("slate").filter(|v| !v.is_null()).map(|_| id(p, "slate", cmd).map(ScriptSlateId)).transpose()?;
    let (source_name, identity) = allowed_source(&s.project, source)?;
    let (slate, take) = s.edit("Add Script Take", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let take_count = doc.slates.iter().try_fold(0usize, |sum, slate| sum.checked_add(slate.takes.len())).ok_or_else(|| bad(cmd, "take count overflow"))?;
        if take_count >= ScriptDocument::MAX_TAKES {
            return Err(bad(cmd, "script already has the maximum number of takes"));
        }
        doc.line_range(start, end).ok_or_else(|| bad(cmd, "startLine and endLine must exist and be in script order"))?;
        let slate = if let Some(slate) = existing_slate {
            if !doc.slates.iter().any(|s| s.id == slate) {
                return Err(bad(cmd, "slate does not exist"));
            }
            slate
        } else {
            if doc.slates.len() >= ScriptDocument::MAX_SLATES {
                return Err(bad(cmd, "script already has the maximum number of slates"));
            }
            let slate = ScriptSlateId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
            doc.slates.push(ScriptSlate { id: slate, name: source_name.clone(), thumbnail_line: None, thumbnail_x: None, takes: Vec::new() });
            slate
        };
        let take = ScriptTakeId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        let slate_ref = doc.slates.iter_mut().find(|s| s.id == slate).ok_or_else(|| bad(cmd, "slate does not exist"))?;
        slate_ref.takes.push(ScriptTake {
            id: take,
            source: Some(source),
            source_name,
            start_line: start,
            end_line: end,
            thumbnail_time: None,
            color: ScriptColor::Default,
            line_colors: Vec::new(),
            offscreen_lines: Vec::new(),
            marks: Vec::new(),
            source_identity: identity,
        });
        Ok((slate, take))
    })?;
    Ok(json!({"slate":slate.0,"take":take.0}))
}

fn rebind_take(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.rebindTake";
    let item = item(p, cmd)?;
    let take_id = ScriptTakeId(id(p, "take", cmd)?);
    let source = ItemId(id(p, "source", cmd)?);
    let discard = p.get("discardMarks").and_then(Value::as_bool).unwrap_or(false);
    let (source_name, identity) = allowed_source(&s.project, source)?;
    let stable_without_fingerprint = match s.project.item(source).map(|item| &item.kind) {
        Some(ItemKind::Sequence(sequence)) => sequence.multicam.is_some(),
        Some(ItemKind::Media(media)) => matches!(media.media, MediaRef::Generator(_)),
        Some(ItemKind::Subclip { .. }) => s.project.resolve_media(source).is_some_and(|(_, media, _)| matches!(media.media, MediaRef::Generator(_))),
        _ => false,
    };
    let source_view = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(cmd, "source cannot open in the Source monitor"))?;
    s.edit("Rebind Script Take", |project, _| {
        let take = script_mut(project, item, cmd)?.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let same_footage = take.source_identity.is_some_and(|old| Some(old) == identity)
            || (take.source == Some(source) && take.source_identity.is_none() && identity.is_none() && stable_without_fingerprint);
        if !same_footage && !take.marks.is_empty() && !discard {
            return Err(bad(
                cmd,
                "replacement is not identity-confirmed as the same footage; review it, then retry with `discardMarks:true` to clear old marks",
            ));
        }
        if !discard && take.marks.iter().any(|mark| mark.source_time < source_view.start || mark.source_time >= source_view.end) {
            return Err(bad(cmd, "replacement source span excludes existing marks; review it, then retry with `discardMarks:true` to clear old marks"));
        }
        if discard {
            take.marks.clear();
        }
        if discard || !same_footage {
            take.thumbnail_time = None;
        } else if take.thumbnail_time.is_some_and(|time| time < source_view.start || time >= source_view.end) {
            take.thumbnail_time = Some(source_view.start);
        }
        take.source = Some(source);
        take.source_name = source_name;
        take.source_identity = identity;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_coverage(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.setCoverage";
    let item = item(p, cmd)?;
    let take_id = ScriptTakeId(id(p, "take", cmd)?);
    let start = ScriptLineId(id(p, "startLine", cmd)?);
    let end = ScriptLineId(id(p, "endLine", cmd)?);
    s.edit("Set Script Take Coverage", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        doc.line_range(start, end).ok_or_else(|| bad(cmd, "startLine and endLine must exist and be in script order"))?;
        let take = doc.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        take.start_line = start;
        take.end_line = end;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_style(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.setTakeStyle";
    let item = item(p, cmd)?;
    let take_id = ScriptTakeId(id(p, "take", cmd)?);
    let color =
        p.get("color").map(|v| v.as_str().and_then(ScriptColor::from_name).ok_or_else(|| bad(cmd, "color must be a known script color"))).transpose()?;
    let line_colors = p
        .get("lineColors")
        .map(|value| -> Result<Vec<ScriptLineColor>> {
            let rows = value.as_array().ok_or_else(|| bad(cmd, "lineColors must be an array"))?;
            if rows.len() > ScriptDocument::MAX_LINES {
                return Err(bad(cmd, "too many colored lines"));
            }
            rows.iter()
                .map(|entry| {
                    let line = ScriptLineId(id(entry, "line", cmd)?);
                    let color = entry
                        .get("color")
                        .and_then(Value::as_str)
                        .and_then(ScriptColor::from_name)
                        .ok_or_else(|| bad(cmd, "line color must be a known script color"))?;
                    Ok(ScriptLineColor { line, color })
                })
                .collect()
        })
        .transpose()?;
    let mut offscreen = p
        .get("offscreenLines")
        .map(|v| -> Result<Vec<ScriptLineId>> {
            let a = v.as_array().ok_or_else(|| bad(cmd, "offscreenLines must be an array of line IDs"))?;
            if a.len() > ScriptDocument::MAX_LINES {
                return Err(bad(cmd, "too many off-screen lines"));
            }
            a.iter().map(|x| x.as_u64().filter(|n| *n > 0).map(ScriptLineId).ok_or_else(|| bad(cmd, "offscreenLines must contain positive line IDs"))).collect()
        })
        .transpose()?;
    if let Some(selected) = offscreen.as_mut() {
        selected.sort_unstable();
        selected.dedup();
    }
    s.edit("Set Script Take Style", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let take = doc.take(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        doc.line_range(take.start_line, take.end_line).ok_or_else(|| bad(cmd, "take coverage is damaged"))?;
        if let Some(ref selected) = offscreen {
            let annotations = doc.annotation_count().ok_or_else(|| bad(cmd, "annotation count overflow"))?;
            let updated = annotations
                .checked_sub(take.offscreen_lines.len())
                .and_then(|n| n.checked_add(selected.len()))
                .ok_or_else(|| bad(cmd, "annotation count overflow"))?;
            if updated > ScriptDocument::MAX_ANNOTATIONS {
                return Err(bad(cmd, "script already has the maximum number of annotations"));
            }
            if selected.iter().any(|line| doc.line_index(*line).is_none()) {
                return Err(bad(cmd, "off-screen line does not exist"));
            }
        }
        if let Some(ref selected) = line_colors {
            let annotations = doc.annotation_count().ok_or_else(|| bad(cmd, "annotation count overflow"))?;
            let updated = annotations
                .checked_sub(take.line_colors.len())
                .and_then(|n| n.checked_add(selected.len()))
                .ok_or_else(|| bad(cmd, "annotation count overflow"))?;
            if updated > ScriptDocument::MAX_ANNOTATIONS {
                return Err(bad(cmd, "script already has the maximum number of annotations"));
            }
            let mut seen = std::collections::BTreeSet::new();
            if selected.iter().any(|entry| !seen.insert(entry.line) || doc.line_index(entry.line).is_none()) {
                return Err(bad(cmd, "colored line is duplicated or does not exist"));
            }
        }
        if let (Some(offscreen), Some(line_colors)) = (&offscreen, &line_colors) {
            let updated = doc
                .annotation_count()
                .and_then(|n| n.checked_sub(take.offscreen_lines.len()))
                .and_then(|n| n.checked_sub(take.line_colors.len()))
                .and_then(|n| n.checked_add(offscreen.len()))
                .and_then(|n| n.checked_add(line_colors.len()))
                .ok_or_else(|| bad(cmd, "annotation count overflow"))?;
            if updated > ScriptDocument::MAX_ANNOTATIONS {
                return Err(bad(cmd, "script already has the maximum number of annotations"));
            }
        }
        let take = doc.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        if let Some(color) = color {
            take.color = color;
        }
        if let Some(selected) = offscreen {
            take.offscreen_lines = selected;
        }
        if let Some(selected) = line_colors {
            take.line_colors = selected;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn add_mark(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.addMark";
    let item = item(p, cmd)?;
    let take_id = ScriptTakeId(id(p, "take", cmd)?);
    let line = ScriptLineId(id(p, "line", cmd)?);
    let source_time = p.get("sourceTime").and_then(Value::as_i64).map(Tick).ok_or_else(|| bad(cmd, "need integer `sourceTime` ticks"))?;
    if source_time < Tick::ZERO {
        return Err(bad(cmd, "sourceTime must not be negative"));
    }
    let take = script(&s.project, item, cmd)?.take(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
    let source = take.source.ok_or_else(|| bad(cmd, "take is unlinked"))?;
    let view = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(cmd, "source item is gone"))?;
    if source_time < view.start || source_time >= view.end {
        return Err(bad(cmd, "sourceTime is outside the source monitor span"));
    }
    let mark = s.edit("Add Script Mark", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        if doc.annotation_count().is_none_or(|n| n >= ScriptDocument::MAX_ANNOTATIONS) {
            return Err(bad(cmd, "script already has the maximum number of annotations"));
        }
        let take = doc.take(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let (a, b) = doc.line_range(take.start_line, take.end_line).ok_or_else(|| bad(cmd, "take coverage is damaged"))?;
        let row = doc.line_index(line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        if !(a..=b).contains(&row) {
            return Err(bad(cmd, "line is outside the take"));
        }
        let mark = ScriptMarkId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        let take = doc.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        take.marks.push(ScriptMark { id: mark, line, source_time });
        Ok(mark)
    })?;
    Ok(json!({"mark":mark.0}))
}

fn set_mark_time(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.setMarkTime";
    let item = item(p, cmd)?;
    let take_id = ScriptTakeId(id(p, "take", cmd)?);
    let mark_id = ScriptMarkId(id(p, "mark", cmd)?);
    let source_time = p.get("sourceTime").and_then(Value::as_i64).map(Tick).ok_or_else(|| bad(cmd, "need integer `sourceTime` ticks"))?;
    if source_time < Tick::ZERO {
        return Err(bad(cmd, "sourceTime must not be negative"));
    }
    let take = script(&s.project, item, cmd)?.take(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
    if !take.marks.iter().any(|mark| mark.id == mark_id) {
        return Err(bad(cmd, "mark does not exist"));
    }
    let source = take.source.ok_or_else(|| bad(cmd, "take is unlinked"))?;
    let view = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(cmd, "source item is gone"))?;
    if source_time < view.start || source_time >= view.end {
        return Err(bad(cmd, "sourceTime is outside the source monitor span"));
    }
    s.edit("Set Script Mark Time", |project, _| {
        let take = script_mut(project, item, cmd)?.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let mark = take.marks.iter_mut().find(|mark| mark.id == mark_id).ok_or_else(|| bad(cmd, "mark does not exist"))?;
        mark.source_time = source_time;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn delete_mark(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.deleteMark";
    let item = item(p, cmd)?;
    let take = ScriptTakeId(id(p, "take", cmd)?);
    let mark = ScriptMarkId(id(p, "mark", cmd)?);
    s.edit("Delete Script Mark", |project, _| {
        let take = script_mut(project, item, cmd)?.take_mut(take).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let before = take.marks.len();
        take.marks.retain(|m| m.id != mark);
        if before == take.marks.len() {
            return Err(bad(cmd, "mark does not exist"));
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn move_mark(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.moveMark";
    let item = item(p, cmd)?;
    let take_id = ScriptTakeId(id(p, "take", cmd)?);
    let mark_id = ScriptMarkId(id(p, "mark", cmd)?);
    let line = ScriptLineId(id(p, "line", cmd)?);
    s.edit("Move Script Mark", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let take = doc.take(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let (start, end) = doc.line_range(take.start_line, take.end_line).ok_or_else(|| bad(cmd, "take coverage is damaged"))?;
        let row = doc.line_index(line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        if !(start..=end).contains(&row) {
            return Err(bad(cmd, "line is outside the take"));
        }
        let take = doc.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let mark = take.marks.iter_mut().find(|mark| mark.id == mark_id).ok_or_else(|| bad(cmd, "mark does not exist"))?;
        mark.line = line;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn cue_mark(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.cueMark";
    let doc = script(&s.project, item(p, cmd)?, cmd)?;
    let take = doc.take(ScriptTakeId(id(p, "take", cmd)?)).ok_or_else(|| bad(cmd, "take does not exist"))?;
    let source = take.source.ok_or_else(|| bad(cmd, "take is unlinked"))?;
    let mark_id = ScriptMarkId(id(p, "mark", cmd)?);
    let mark = take.marks.iter().find(|m| m.id == mark_id).ok_or_else(|| bad(cmd, "mark does not exist"))?;
    let (start, end) = doc.line_range(take.start_line, take.end_line).ok_or_else(|| bad(cmd, "take coverage is invalid"))?;
    if doc.line_index(mark.line).is_none_or(|row| !(start..=end).contains(&row)) {
        return Err(bad(cmd, "mark is hidden outside the take's coverage"));
    }
    let at = mark.source_time;
    let view = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(cmd, "source item is gone"))?;
    if at < view.start || at >= view.end {
        return Err(bad(cmd, "mark is outside the linked source's visible span after relink; review the footage or rebind the take"));
    }
    s.execute("source.open", json!({"item":source.0}))?;
    s.execute("source.setPlayhead", json!({"time":at.0}))?;
    Ok(json!({"item":source.0,"time":s.state.source_playhead.0}))
}

/// Cue a take at a script row. Marks anchor the source time; rows between two marks use
/// linear interpolation, while rows outside the marked span use the nearest mark.
fn cue_line(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.cueLine";
    let doc = script(&s.project, item(p, cmd)?, cmd)?;
    let take = doc.take(ScriptTakeId(id(p, "take", cmd)?)).ok_or_else(|| bad(cmd, "take does not exist"))?;
    let source = take.source.ok_or_else(|| bad(cmd, "take is unlinked"))?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    let row = doc.line_index(line).ok_or_else(|| bad(cmd, "line does not exist"))?;
    let (start, end) = doc.line_range(take.start_line, take.end_line).ok_or_else(|| bad(cmd, "take coverage is invalid"))?;
    if !(start..=end).contains(&row) {
        return Err(bad(cmd, "line is outside the take"));
    }
    let view = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(cmd, "source item is gone"))?;
    let mut before: Option<(usize, Tick)> = None;
    let mut after: Option<(usize, Tick)> = None;
    for mark in &take.marks {
        let Some(at) = doc.line_index(mark.line).filter(|at| (start..=end).contains(at)) else { continue };
        if at <= row && before.is_none_or(|(previous, _)| at > previous) {
            before = Some((at, mark.source_time));
        }
        if at >= row && after.is_none_or(|(next, _)| at < next) {
            after = Some((at, mark.source_time));
        }
    }
    let at = match (before, after) {
        (Some((left_row, left_time)), Some((right_row, right_time))) if left_row < right_row => {
            let delta = i128::from(right_time.0) - i128::from(left_time.0);
            let span = i128::try_from(right_row - left_row).map_err(|_| bad(cmd, "row span is too large"))?;
            let offset = i128::try_from(row - left_row).map_err(|_| bad(cmd, "row offset is too large"))?;
            let interpolated = i128::from(left_time.0) + delta * offset / span;
            Tick(i64::try_from(interpolated).map_err(|_| bad(cmd, "interpolated source time is too large"))?)
        }
        (Some((_, time)), _) | (_, Some((_, time))) => time,
        (None, None) => view.start,
    };
    if at < view.start || at >= view.end {
        return Err(bad(cmd, "cue is outside the linked source's visible span after relink; review the footage or rebind the take"));
    }
    s.execute("source.open", json!({"item":source.0}))?;
    s.execute("source.setPlayhead", json!({"time":at.0}))?;
    Ok(json!({"item":source.0,"time":s.state.source_playhead.0}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("script.create", "New Script", r#"{"name":str,"bin":id?}"#, |s, p| create(s, p, false), true),
        spec("script.importText", "Import Script Text", r#"{"name":str,"text":str,"bin":id?}"#, |s, p| create(s, p, true), true),
        spec("script.inspect", "Inspect Script", r#"{"item":id}"#, inspect, false),
        spec("script.copySection", "Copy Script Section", r#"{"item":id,"startLine":id,"endLine":id,"slates":[id]}"#, copy_section, false),
        spec("script.pasteSection", "Paste Script Section", r#"{"item":id,"afterLine":id?,"section":object}"#, paste_section, true),
        spec("script.editLine", "Edit Script Line", r#"{"item":id,"line":id,"text":str}"#, edit_line, true),
        spec("script.splitLine", "Split Script Line", r#"{"item":id,"line":id,"text":str,"at":number}"#, split_line, true),
        spec("script.mergeWithPrevious", "Join Script Lines", r#"{"item":id,"line":id,"text":str}"#, merge_with_previous, true),
        spec("script.shiftSlate", "Move Script Setup", r#"{"item":id,"slate":id,"delta":number,"x":number?}"#, shift_slate, true),
        spec("script.moveThumbnail", "Move Script Thumbnail", r#"{"item":id,"slate":id,"line":id,"x":number?}"#, move_thumbnail, true),
        spec("script.insertLine", "Insert Script Line", r#"{"item":id,"after":id?,"text":str}"#, insert_line, true),
        spec("script.deleteLine", "Delete Script Line", r#"{"item":id,"line":id}"#, delete_line, true),
        spec("script.setLabel", "Set Script Scene/Page", r#"{"item":id,"line":id,"scene":str|null?,"page":str|null?}"#, set_label, true),
        spec("script.addSlate", "Add Script Slate", r#"{"item":id,"name":str}"#, add_slate, true),
        spec("script.addTake", "Add Script Take", r#"{"item":id,"slate":id?,"source":id,"startLine":id,"endLine":id}"#, add_take, true),
        spec("script.moveTake", "Reorder Script Take", r#"{"item":id,"take":id,"direction":-1|1}"#, move_take, true),
        spec("script.setThumbnailTime", "Set Script Thumbnail Frame", r#"{"item":id,"take":id,"time":ticks}"#, set_thumbnail_time, true),
        spec("script.rebindTake", "Rebind Script Take", r#"{"item":id,"take":id,"source":id,"discardMarks":bool?}"#, rebind_take, true),
        spec("script.setCoverage", "Set Script Take Coverage", r#"{"item":id,"take":id,"startLine":id,"endLine":id}"#, set_coverage, true),
        spec(
            "script.setTakeStyle",
            "Set Script Take Style",
            r#"{"item":id,"take":id,"color":str?,"lineColors":[{"line":id,"color":str}]?,"offscreenLines":[id]?}"#,
            set_style,
            true,
        ),
        spec("script.addMark", "Add Script Mark", r#"{"item":id,"take":id,"line":id,"sourceTime":ticks}"#, add_mark, true),
        spec("script.setMarkTime", "Set Script Mark Time", r#"{"item":id,"take":id,"mark":id,"sourceTime":ticks}"#, set_mark_time, true),
        spec("script.moveMark", "Move Script Mark", r#"{"item":id,"take":id,"mark":id,"line":id}"#, move_mark, true),
        spec("script.deleteMark", "Delete Script Mark", r#"{"item":id,"take":id,"mark":id}"#, delete_mark, true),
        spec("script.cueMark", "Cue Script Mark", r#"{"item":id,"take":id,"mark":id}"#, cue_mark, false),
        spec("script.cueLine", "Cue Script Take Row", r#"{"item":id,"take":id,"line":id}"#, cue_line, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{Generator, MediaSource};
    use filmcraft_project::{MediaClip, MediaRef};
    use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

    fn source(s: &mut Session, name: &str, fingerprint: u64) -> ItemId {
        let generator = Generator::CountingLeader;
        let info = GeneratorSource::new(generator.clone(), 64, 36, FrameRate::FPS_24, FrameRate::FPS_24.tick_of(120)).info().clone();
        Arc::make_mut(&mut s.project).add_item(
            name,
            Label::Blue,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(generator),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: Vec::new(),
                offline: false,
                proxy: None,
                identity: Some(MediaIdentity { size: 10, fingerprint }),
            }),
            None,
        )
    }

    #[test]
    fn section_copy_paste_clips_candles_remaps_ids_and_undoes() {
        let mut s = Session::default();
        let media = source(&mut s, "setup", 10);
        let origin = s.execute("script.importText", json!({"name":"Original","text":"A\nB\nC\nD"})).unwrap()["item"].as_u64().unwrap();
        let rows: Vec<_> = s.project.item(ItemId(origin)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
        s.execute("script.setLabel", json!({"item":origin,"line":rows[1],"scene":"2A"})).unwrap();
        let ids = s.execute("script.addTake", json!({"item":origin,"source":media.0,"startLine":rows[0],"endLine":rows[3]})).unwrap();
        let take = ids["take"].as_u64().unwrap();
        let slate = ids["slate"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":origin,"take":take,"line":rows[0],"sourceTime":10})).unwrap();
        s.execute("script.addMark", json!({"item":origin,"take":take,"line":rows[2],"sourceTime":20})).unwrap();
        s.execute(
            "script.setTakeStyle",
            json!({"item":origin,"take":take,
            "lineColors":[{"line":rows[1],"color":"Rose"}],"offscreenLines":[rows[2]]}),
        )
        .unwrap();
        let section = s.execute("script.copySection", json!({"item":origin,"startLine":rows[1],"endLine":rows[2],"slates":[slate]})).unwrap();
        assert_eq!(section["lines"].as_array().unwrap().len(), 2);
        assert_eq!(section["slates"][0]["takes"][0]["marks"].as_array().unwrap().len(), 1);
        let target = s.execute("script.importText", json!({"name":"Target","text":"Before\nAfter"})).unwrap()["item"].as_u64().unwrap();
        let after = s.project.item(ItemId(target)).unwrap().as_script().unwrap().lines[0].id.0;
        let result = s.execute("script.pasteSection", json!({"item":target,"afterLine":after,"section":section})).unwrap();
        let doc = s.project.item(ItemId(target)).unwrap().as_script().unwrap();
        doc.validate().unwrap();
        assert_eq!(doc.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(), ["Before", "B", "C", "After"]);
        assert_eq!(doc.lines[1].scene.as_deref(), Some("2A"));
        assert_eq!(doc.lines[1].id.0, result["startLine"].as_u64().unwrap());
        let pasted = &doc.slates[0].takes[0];
        assert_eq!(pasted.source, Some(media));
        assert_eq!(pasted.start_line, doc.lines[1].id);
        assert_eq!(pasted.end_line, doc.lines[2].id);
        assert_eq!(pasted.marks[0].line, doc.lines[2].id);
        assert_eq!(pasted.offscreen_lines, vec![doc.lines[2].id]);
        assert_eq!(pasted.line_colors[0].line, doc.lines[1].id);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.item(ItemId(target)).unwrap().as_script().unwrap().lines.len(), 2);
    }

    #[test]
    fn script_commands_round_trip_and_discard_policy() {
        let mut s = Session::default();
        let media = source(&mut s, "same", 10);
        let different = source(&mut s, "different", 20);
        let script = s.execute("script.importText", json!({"name":"Dialog", "text":"A\nB\nC"})).unwrap()["item"].as_u64().unwrap();
        let rows = &s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines;
        let (first, middle, last) = (rows[0].id.0, rows[1].id.0, rows[2].id.0);
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":first,"endLine":last})).unwrap()["take"].as_u64().unwrap();
        let mark = s.execute("script.addMark", json!({"item":script,"take":take,"line":middle,"sourceTime":FrameRate::FPS_24.tick_of(48).0})).unwrap()["mark"]
            .as_u64()
            .unwrap();
        s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":first,"endLine":first})).unwrap();
        assert!(s.execute("script.cueMark", json!({"item":script,"take":take,"mark":mark})).is_err(), "a hidden mark cannot cue Source");
        let bytes = filmcraft_format::encode(&s.project, false);
        let loaded = filmcraft_format::decode(&bytes).unwrap();
        let saved_take = loaded.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap();
        assert_eq!(saved_take.marks[0].id.0, mark);
        assert_eq!(saved_take.marks[0].line.0, middle);
        s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":first,"endLine":last})).unwrap();
        assert!(s.execute("script.rebindTake", json!({"item":script,"take":take,"source":different.0})).is_err());
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().source, Some(media));
        s.execute("script.rebindTake", json!({"item":script,"take":take,"source":different.0,"discardMarks":true})).unwrap();
        assert!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks.is_empty());
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().source, Some(media));
    }

    #[test]
    fn coverage_trims_hide_and_restore_annotations_without_changing_marks() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Trim","text":"A\nB\nC\nD\nE"})).unwrap()["item"].as_u64().unwrap();
        let rows: Vec<_> = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
        let added = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":rows[0],"endLine":rows[4]})).unwrap();
        let take = added["take"].as_u64().unwrap();
        let slate = added["slate"].as_u64().unwrap();
        let times = [FrameRate::FPS_24.tick_of(12).0, FrameRate::FPS_24.tick_of(24).0, FrameRate::FPS_24.tick_of(72).0, FrameRate::FPS_24.tick_of(96).0];
        let marked_rows = [rows[0], rows[1], rows[3], rows[4]];
        let marks: Vec<_> = marked_rows
            .iter()
            .zip(times)
            .map(|(&line, time)| {
                s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":time})).unwrap()["mark"].as_u64().unwrap()
            })
            .collect();
        s.execute(
            "script.setTakeStyle",
            json!({"item":script,"take":take,
            "lineColors":[{"line":rows[0],"color":"Rose"},{"line":rows[4],"color":"Blue"}],
            "offscreenLines":[rows[0],rows[4]]}),
        )
        .unwrap();
        let baseline = s.execute("script.cueLine", json!({"item":script,"take":take,"line":rows[2]})).unwrap()["time"].clone();
        let stored = |session: &Session| session.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().clone();
        let original = stored(&s);

        // Shrink the top, then bottom: several marks and both kinds of style are hidden.
        s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":rows[1],"endLine":rows[4]})).unwrap();
        s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":rows[1],"endLine":rows[3]})).unwrap();
        s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":rows[2],"endLine":rows[2]})).unwrap();
        let hidden = stored(&s);
        assert_eq!(hidden.marks, original.marks);
        assert_eq!(hidden.line_colors, original.line_colors);
        assert_eq!(hidden.offscreen_lines, original.offscreen_lines);
        for mark in &marks {
            assert!(s.execute("script.cueMark", json!({"item":script,"take":take,"mark":mark})).is_err());
        }
        // No visible anchors means Source In; hidden marks must not interpolate.
        assert_eq!(s.execute("script.cueLine", json!({"item":script,"take":take,"line":rows[2]})).unwrap()["time"], 0);
        let bytes = filmcraft_format::encode(&s.project, false);
        let loaded = filmcraft_format::decode(&bytes).unwrap();
        assert_eq!(loaded.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap(), &hidden);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(stored(&s).marks, original.marks);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(stored(&s), hidden);
        let section = s.execute("script.copySection", json!({"item":script,"startLine":rows[0],"endLine":rows[4],"slates":[slate]})).unwrap();
        let target = s.execute("script.importText", json!({"name":"Pasted","text":"Before\nAfter"})).unwrap()["item"].as_u64().unwrap();
        let after = s.project.item(ItemId(target)).unwrap().as_script().unwrap().lines[0].id.0;
        s.execute("script.pasteSection", json!({"item":target,"afterLine":after,"section":section})).unwrap();
        let pasted_doc = s.project.item(ItemId(target)).unwrap().as_script().unwrap();
        pasted_doc.validate().unwrap();
        let pasted = &pasted_doc.slates[0].takes[0];
        assert_eq!(pasted_doc.line_index(pasted.start_line), Some(3));
        assert_eq!(pasted_doc.line_index(pasted.end_line), Some(3));
        assert_eq!(pasted.marks.iter().map(|mark| mark.source_time.0).collect::<Vec<_>>(), times);
        assert_ne!(pasted.marks[0].id.0, marks[0], "pasted annotations receive new IDs in the target document");
        assert_eq!(pasted.offscreen_lines.len(), 2);
        assert_eq!(pasted.line_colors.len(), 2);

        // Repeated expansion and re-trimming must restore exactly the same IDs and times.
        for _ in 0..2 {
            s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":rows[0],"endLine":rows[4]})).unwrap();
            assert_eq!(stored(&s), original);
            assert_eq!(s.execute("script.cueLine", json!({"item":script,"take":take,"line":rows[2]})).unwrap()["time"], baseline);
            s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":rows[2],"endLine":rows[2]})).unwrap();
            assert_eq!(stored(&s), hidden);
        }
        // Editing style while trimmed carries hidden annotations forward.
        s.execute(
            "script.setTakeStyle",
            json!({"item":script,"take":take,
            "lineColors":[{"line":rows[0],"color":"Rose"},{"line":rows[4],"color":"Blue"}],
            "offscreenLines":[rows[0],rows[4]]}),
        )
        .unwrap();
        s.execute("script.deleteLine", json!({"item":script,"line":rows[4]})).unwrap();
        let after_delete = stored(&s);
        assert_eq!(after_delete.marks.iter().map(|mark| mark.id.0).collect::<Vec<_>>(), marks[..3]);
        assert_eq!(after_delete.offscreen_lines, vec![filmcraft_project::ScriptLineId(rows[0])]);
        assert_eq!(after_delete.line_colors.len(), 1);
        s.project.item(ItemId(script)).unwrap().as_script().unwrap().validate().unwrap();
    }

    #[test]
    fn candle_row_color_and_mark_retime_round_trip_and_undo() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Color","text":"A\nB\nC"})).unwrap()["item"].as_u64().unwrap();
        let ids: Vec<_> = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":ids[0],"endLine":ids[2]})).unwrap()["take"].as_u64().unwrap();
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().color, ScriptColor::Default);
        s.execute(
            "script.setTakeStyle",
            json!({"item":script,"take":take,"lineColors":[{"line":ids[0],"color":"Rose"},{"line":ids[1],"color":"Rose"}],"offscreenLines":[ids[0],ids[1]]}),
        )
        .unwrap();
        let mark = s.execute("script.addMark", json!({"item":script,"take":take,"line":ids[1],"sourceTime":100})).unwrap()["mark"].as_u64().unwrap();
        s.execute("script.setMarkTime", json!({"item":script,"take":take,"mark":mark,"sourceTime":200})).unwrap();
        let current = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        let take_ref = current.take(ScriptTakeId(take)).unwrap();
        assert_eq!(take_ref.line_colors.len(), 2);
        assert_eq!(take_ref.offscreen_lines.len(), 2);
        assert_eq!(take_ref.marks[0].source_time, Tick(200));
        assert!(
            s.execute("script.setTakeStyle", json!({"item":script,"take":take,"lineColors":[{"line":ids[0],"color":"Blue"},{"line":ids[0],"color":"Gray"}]}))
                .is_err()
        );
        assert!(s.execute("script.setMarkTime", json!({"item":script,"take":take,"mark":mark,"sourceTime":i64::MAX})).is_err());
        let opened = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        let saved = opened.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap();
        assert_eq!(saved.line_colors[0].color, ScriptColor::Rose);
        assert_eq!(saved.offscreen_lines.len(), 2);
        assert_eq!(saved.marks[0].source_time, Tick(200));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0].source_time, Tick(100));
    }

    #[test]
    fn split_line_preserves_marks_and_extends_candle() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Split","text":"One café line\nNext"})).unwrap()["item"].as_u64().unwrap();
        let first = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":first,"endLine":first})).unwrap()["take"].as_u64().unwrap();
        let mark = s.execute("script.addMark", json!({"item":script,"take":take,"line":first,"sourceTime":100})).unwrap()["mark"].as_u64().unwrap();
        let created = s.execute("script.splitLine", json!({"item":script,"line":first,"text":"One café line","at":8})).unwrap()["line"].as_u64().unwrap();
        let doc = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(), ["One café", " line", "Next"]);
        assert_eq!(doc.lines[0].id.0, first);
        assert_eq!(doc.lines[1].id.0, created);
        let saved_take = doc.take(ScriptTakeId(take)).unwrap();
        assert_eq!(saved_take.end_line.0, created);
        assert_eq!(saved_take.marks[0].id.0, mark);
        assert_eq!(saved_take.marks[0].line.0, first);
        assert!(s.execute("script.splitLine", json!({"item":script,"line":first,"text":"bad","at":999})).is_err());
        s.execute("edit.undo", json!({})).unwrap();
        let doc = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.take(ScriptTakeId(take)).unwrap().end_line.0, first);
    }

    #[test]
    fn joining_rows_discards_only_marks_on_the_removed_row() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Join","text":"Café\n arrived\nEnd"})).unwrap()["item"].as_u64().unwrap();
        let ids: Vec<_> = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":ids[0],"endLine":ids[2]})).unwrap()["take"].as_u64().unwrap();
        let first_mark = s.execute("script.addMark", json!({"item":script,"take":take,"line":ids[0],"sourceTime":100})).unwrap()["mark"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":ids[1],"sourceTime":200})).unwrap();
        let joined = s.execute("script.mergeWithPrevious", json!({"item":script,"line":ids[1],"text":" came back"})).unwrap();
        assert_eq!(joined["line"], ids[0]);
        assert_eq!(joined["text"], "Café came back");
        assert_eq!(joined["at"], 4);
        let doc = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(), ["Café came back", "End"]);
        let marks = &doc.take(ScriptTakeId(take)).unwrap().marks;
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].id.0, first_mark);
        assert_eq!(marks[0].line.0, ids[0]);
        assert!(s.execute("script.mergeWithPrevious", json!({"item":script,"line":ids[0],"text":"bad"})).is_err());
        s.execute("edit.undo", json!({})).unwrap();
        let doc = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.take(ScriptTakeId(take)).unwrap().marks.len(), 2);
    }

    #[test]
    fn shifting_setup_moves_candles_marks_and_offscreen_together() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Shift","text":"A\nB\nC\nD\nE"})).unwrap()["item"].as_u64().unwrap();
        let ids: Vec<_> = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
        let created = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":ids[0],"endLine":ids[1]})).unwrap();
        let slate = created["slate"].as_u64().unwrap();
        let take = created["take"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":ids[1],"sourceTime":100})).unwrap();
        s.execute("script.setTakeStyle", json!({"item":script,"take":take,"offscreenLines":[ids[0]]})).unwrap();
        s.execute("script.addTake", json!({"item":script,"slate":slate,"source":media.0,"startLine":ids[1],"endLine":ids[2]})).unwrap();
        s.execute("script.moveThumbnail", json!({"item":script,"slate":slate,"line":ids[1],"x":0.3})).unwrap();
        assert!(s.execute("script.moveThumbnail", json!({"item":script,"slate":slate,"line":999999})).is_err());
        assert!(s.execute("script.moveThumbnail", json!({"item":script,"slate":slate,"line":ids[1],"x":-1})).is_err());
        let before = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(before.slates[0].thumbnail_line, Some(ScriptLineId(ids[1])));
        assert_eq!(before.slates[0].thumbnail_x, Some(0.3));
        assert_eq!(before.take(ScriptTakeId(take)).unwrap().start_line.0, ids[0]);
        s.execute("script.shiftSlate", json!({"item":script,"slate":slate,"delta":2})).unwrap();
        let doc = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        let first = doc.take(ScriptTakeId(take)).unwrap();
        assert_eq!((first.start_line.0, first.end_line.0), (ids[2], ids[3]));
        assert_eq!(first.marks[0].line.0, ids[3]);
        assert_eq!(first.marks[0].source_time.0, 100);
        assert_eq!(first.offscreen_lines[0].0, ids[2]);
        assert_eq!(doc.slates[0].thumbnail_line, Some(ScriptLineId(ids[3])));
        assert_eq!(doc.slates[0].thumbnail_x, Some(0.3));
        let reopened = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        assert_eq!(reopened.project.item(ItemId(script)).unwrap().as_script().unwrap().slates[0].thumbnail_line, Some(ScriptLineId(ids[3])));
        assert_eq!(reopened.project.item(ItemId(script)).unwrap().as_script().unwrap().slates[0].thumbnail_x, Some(0.3));
        assert!(s.execute("script.shiftSlate", json!({"item":script,"slate":slate,"delta":2})).is_err());
        assert!(s.execute("script.shiftSlate", json!({"item":script,"slate":slate,"delta":"many"})).is_err());
        s.execute("edit.undo", json!({})).unwrap();
        let doc = s.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.take(ScriptTakeId(take)).unwrap().start_line.0, ids[0]);
        assert_eq!(doc.slates[0].thumbnail_line, Some(ScriptLineId(ids[1])));
    }

    #[test]
    fn candle_order_and_thumbnail_frame_round_trip() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Order","text":"A\nB"})).unwrap()["item"].as_u64().unwrap();
        let first = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let added = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":first,"endLine":first})).unwrap();
        let slate = added["slate"].as_u64().unwrap();
        let a = added["take"].as_u64().unwrap();
        let b = s.execute("script.addTake", json!({"item":script,"slate":slate,"source":media.0,"startLine":first,"endLine":first})).unwrap()["take"]
            .as_u64()
            .unwrap();
        s.execute("script.setThumbnailTime", json!({"item":script,"take":b,"time":TICKS_PER_SECOND})).unwrap();
        assert!(s.execute("script.moveTake", json!({"item":script,"take":a,"direction":-1})).is_err());
        s.execute("script.moveTake", json!({"item":script,"take":b,"direction":-1})).unwrap();
        let restored = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        let takes = &restored.project.item(ItemId(script)).unwrap().as_script().unwrap().slates[0].takes;
        assert_eq!((takes[0].id.0, takes[1].id.0), (b, a));
        assert_eq!(takes[0].thumbnail_time, Some(Tick(TICKS_PER_SECOND)));
        s.execute("edit.undo", json!({})).unwrap();
        let takes = &s.project.item(ItemId(script)).unwrap().as_script().unwrap().slates[0].takes;
        assert_eq!((takes[0].id.0, takes[1].id.0), (a, b));
    }

    #[test]
    fn moving_mark_changes_script_line_but_keeps_source_frame() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Mark","text":"A\nB\nC\nD"})).unwrap()["item"].as_u64().unwrap();
        let ids: Vec<_> = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":ids[0],"endLine":ids[2]})).unwrap()["take"].as_u64().unwrap();
        let mark = s.execute("script.addMark", json!({"item":script,"take":take,"line":ids[0],"sourceTime":100})).unwrap()["mark"].as_u64().unwrap();
        s.execute("script.moveMark", json!({"item":script,"take":take,"mark":mark,"line":ids[2]})).unwrap();
        let stored = &s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0];
        assert_eq!((stored.line.0, stored.source_time.0), (ids[2], 100));
        assert!(s.execute("script.moveMark", json!({"item":script,"take":take,"mark":mark,"line":ids[3]})).is_err());
        assert!(s.execute("script.moveMark", json!({"item":script,"take":take,"mark":999,"line":ids[1]})).is_err());
        s.execute("edit.undo", json!({})).unwrap();
        let stored = &s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0];
        assert_eq!((stored.line.0, stored.source_time.0), (ids[0], 100));
    }

    #[test]
    fn editing_keeps_line_marks_and_deletion_undo_restores_them() {
        let mut session = Session::default();
        let media = source(&mut session, "clip", 10);
        let script = session.execute("script.importText", json!({"name":"Scene", "text":"MARA\nOriginal line"})).unwrap()["item"].as_u64().unwrap();
        let lines = &session.project.item(ItemId(script)).unwrap().as_script().unwrap().lines;
        let (first, second) = (lines[0].id.0, lines[1].id.0);
        let take =
            session.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":first,"endLine":second})).unwrap()["take"].as_u64().unwrap();
        let mark = session.execute("script.addMark", json!({"item":script,"take":take,"line":second,"sourceTime":100})).unwrap()["mark"].as_u64().unwrap();
        session.execute("script.editLine", json!({"item":script,"line":second,"text":"Revised line"})).unwrap();
        let doc = session.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines[1].text, "Revised line");
        assert_eq!(doc.take(ScriptTakeId(take)).unwrap().marks[0].id.0, mark);

        session.execute("script.deleteLine", json!({"item":script,"line":second})).unwrap();
        let doc = session.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines.len(), 1);
        assert!(doc.take(ScriptTakeId(take)).unwrap().marks.is_empty());
        session.execute("edit.undo", json!({})).unwrap();
        let doc = session.project.item(ItemId(script)).unwrap().as_script().unwrap();
        assert_eq!(doc.lines[1].text, "Revised line");
        assert_eq!(doc.take(ScriptTakeId(take)).unwrap().marks[0].id.0, mark);
    }

    #[test]
    fn delete_unbinds_and_same_identity_rebind_preserves_marks() {
        let mut s = Session::default();
        let original = source(&mut s, "original", 10);
        let same_file = source(&mut s, "moved", 10);
        let script = s.execute("script.importText", json!({"name":"Script","text":"hello"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":original.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":100})).unwrap();
        s.execute("project.delete", json!({"items":[original.0]})).unwrap();
        assert!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().source.is_none());
        s.execute("script.rebindTake", json!({"item":script,"take":take,"source":same_file.0})).unwrap();
        let take = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap();
        assert_eq!(take.source, Some(same_file));
        assert_eq!(take.marks[0].source_time, Tick(100));
    }

    #[test]
    fn same_item_after_forced_relink_does_not_authorize_old_marks() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Script","text":"line"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":100})).unwrap();
        Arc::make_mut(&mut s.project).item_mut(media).unwrap().as_media_mut().unwrap().identity = Some(MediaIdentity { size: 10, fingerprint: 99 });
        assert!(s.execute("script.rebindTake", json!({"item":script,"take":take,"source":media.0})).is_err());
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks.len(), 1);
        s.execute("script.rebindTake", json!({"item":script,"take":take,"source":media.0,"discardMarks":true})).unwrap();
        assert!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks.is_empty());
    }

    #[test]
    fn line_edits_and_inserts_respect_aggregate_text_budget() {
        let mut s = Session::default();
        let row = "x".repeat(ScriptDocument::MAX_LINE_BYTES - 1);
        let text = vec![row; 256].join("\n");
        assert_eq!(text.len(), ScriptDocument::MAX_TEXT_BYTES - 1);
        let script = s.execute("script.importText", json!({"name":"long","text":text})).unwrap()["item"].as_u64().unwrap();
        let lines = &s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines;
        let first = lines[0].id.0;
        let second = lines[1].id.0;
        let full = "y".repeat(ScriptDocument::MAX_LINE_BYTES);
        s.execute("script.editLine", json!({"item":script,"line":first,"text":full})).unwrap();
        let before = s.project.clone();
        assert!(s.execute("script.editLine", json!({"item":script,"line":second,"text":full})).is_err());
        assert!(s.execute("script.insertLine", json!({"item":script,"after":first,"text":"z"})).is_err());
        assert_eq!(s.project.as_ref(), before.as_ref());
    }

    #[test]
    fn shifted_source_marks_keep_same_footage_frame_and_export_selection_keeps_source() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let script = s.execute("script.importText", json!({"name":"Script","text":"line"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":4000})).unwrap();
        let before = Tick(4000);
        let shift = Tick(1000);
        Arc::make_mut(&mut s.project).shift_media_time(media, shift);
        let after = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0].source_time;
        assert_eq!(after, before - shift);
        assert_eq!(after + shift, before);
        let keep = crate::project_tools::closure(&s.project, &[ItemId(script)]);
        assert!(keep.contains(&media));
    }

    #[test]
    fn shifted_out_of_range_mark_stays_signed_and_cue_reports_without_moving_monitor() {
        let mut s = Session::default();
        let media = source(&mut s, "clip", 10);
        let other = source(&mut s, "other", 20);
        let script = s.execute("script.importText", json!({"name":"Script","text":"line"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":media.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        let mark = s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":100})).unwrap()["mark"].as_u64().unwrap();
        Arc::make_mut(&mut s.project).shift_media_time(media, Tick(200));
        let stored = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0].source_time;
        assert_eq!(stored, Tick(-100));
        s.execute("source.open", json!({"item":other.0})).unwrap();
        s.execute("source.setPlayhead", json!({"time":FrameRate::FPS_24.tick_of(10).0})).unwrap();
        let previous = (s.state.source_item, s.state.source_playhead);
        let error = s.execute("script.cueMark", json!({"item":script,"take":take,"mark":mark})).unwrap_err().to_string();
        assert!(error.contains("outside the linked source's visible span"), "{error}");
        assert_eq!((s.state.source_item, s.state.source_playhead), previous);
    }

    #[test]
    fn subclip_mark_uses_parent_media_time_with_nonzero_in() {
        let mut s = Session::default();
        let parent = source(&mut s, "parent", 10);
        let rate = FrameRate::FPS_24;
        let subclip = Arc::make_mut(&mut s.project).add_item(
            "subclip",
            Label::Blue,
            ItemKind::Subclip { parent, range: TimeRange::new(rate.tick_of(24), rate.tick_of(48)), restrict_trims: true },
            None,
        );
        let script = s.execute("script.importText", json!({"name":"Script","text":"line"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":subclip.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        assert!(s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":rate.tick_of(23).0})).is_err());
        let mark =
            s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":rate.tick_of(30).0})).unwrap()["mark"].as_u64().unwrap();
        let cued = s.execute("script.cueMark", json!({"item":script,"take":take,"mark":mark})).unwrap();
        assert_eq!(cued["time"], rate.tick_of(30).0);
        Arc::make_mut(&mut s.project).shift_media_time(parent, rate.tick_of(5));
        let shifted = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0].source_time;
        assert_eq!(shifted, rate.tick_of(25));
        assert_eq!(
            s.project.item(subclip).unwrap().kind,
            ItemKind::Subclip { parent, range: TimeRange::new(rate.tick_of(19), rate.tick_of(48)), restrict_trims: true }
        );
    }

    #[test]
    fn relink_shifts_nested_subclip_take_and_both_ranges() {
        let mut s = Session::default();
        let parent = source(&mut s, "parent", 10);
        let rate = FrameRate::FPS_24;
        let first = Arc::make_mut(&mut s.project).add_item(
            "first",
            Label::Blue,
            ItemKind::Subclip { parent, range: TimeRange::new(rate.tick_of(24), rate.tick_of(48)), restrict_trims: true },
            None,
        );
        let second = Arc::make_mut(&mut s.project).add_item(
            "second",
            Label::Blue,
            ItemKind::Subclip { parent: first, range: TimeRange::new(rate.tick_of(30), rate.tick_of(24)), restrict_trims: true },
            None,
        );
        let script = s.execute("script.importText", json!({"name":"Script","text":"line"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":second.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":rate.tick_of(35).0})).unwrap();
        Arc::make_mut(&mut s.project).shift_media_time(parent, rate.tick_of(5));
        let mark = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks[0].source_time;
        assert_eq!(mark, rate.tick_of(30));
        assert_eq!(
            s.project.item(first).unwrap().kind,
            ItemKind::Subclip { parent, range: TimeRange::new(rate.tick_of(19), rate.tick_of(48)), restrict_trims: true }
        );
        assert_eq!(
            s.project.item(second).unwrap().kind,
            ItemKind::Subclip { parent: first, range: TimeRange::new(rate.tick_of(25), rate.tick_of(24)), restrict_trims: true }
        );
    }

    #[test]
    fn invalid_inputs_leave_project_unchanged() {
        let mut s = Session::default();
        let before = s.project.clone();
        assert!(s.execute("script.importText", json!({"name":"oversize","text":"x".repeat(ScriptDocument::MAX_TEXT_BYTES + 1)})).is_err());
        assert_eq!(s.project.as_ref(), before.as_ref());
        let script = s.execute("script.importText", json!({"name":"short","text":"x"})).unwrap()["item"].as_u64().unwrap();
        let before = s.project.clone();
        assert!(s.execute("script.deleteLine", json!({"item":script,"line":999999})).is_err());
        assert_eq!(s.project.as_ref(), before.as_ref());
    }
}

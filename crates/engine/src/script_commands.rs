//! Commands for native lined scripts. Text import accepts content, not a filesystem path.

use std::sync::Arc;

use filmcraft_project::{
    BinId, ItemId, ItemKind, Label, MediaIdentity, Project, ScriptDocument, ScriptLine, ScriptLineId, ScriptMark, ScriptMarkId, ScriptSlate, ScriptSlateId,
    ScriptTake, ScriptTakeId,
};
use filmcraft_time::Tick;
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
    let doc =
        if import { ScriptDocument::from_text(text(p, "text", cmd, ScriptDocument::MAX_TEXT_BYTES)?).map_err(|e| bad(cmd, e))? } else { ScriptDocument::new() };
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

fn edit_line(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.editLine";
    let item = item(p, cmd)?;
    let line = ScriptLineId(id(p, "line", cmd)?);
    let value = text(p, "text", cmd, ScriptDocument::MAX_LINE_BYTES)?.to_owned();
    s.edit("Edit Script Line", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let line = doc.lines.iter_mut().find(|row| row.id == line).ok_or_else(|| bad(cmd, "line does not exist"))?;
        line.text = value;
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
        let slate = ScriptSlateId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
        doc.slates.push(ScriptSlate { id: slate, name, takes: Vec::new() });
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
        doc.line_range(start, end).ok_or_else(|| bad(cmd, "startLine and endLine must exist and be in script order"))?;
        let slate = if let Some(slate) = existing_slate {
            if !doc.slates.iter().any(|s| s.id == slate) {
                return Err(bad(cmd, "slate does not exist"));
            }
            slate
        } else {
            let slate = ScriptSlateId(doc.alloc_id().map_err(|e| bad(cmd, e))?);
            doc.slates.push(ScriptSlate { id: slate, name: source_name.clone(), takes: Vec::new() });
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
            color: Label::Blue,
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
    let source_view = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(cmd, "source cannot open in the Source monitor"))?;
    s.edit("Rebind Script Take", |project, _| {
        let take = script_mut(project, item, cmd)?.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let same_footage = take.source == Some(source) || take.source_identity.is_some_and(|old| Some(old) == identity);
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
        let (a, b) = doc.line_range(start, end).ok_or_else(|| bad(cmd, "startLine and endLine must exist and be in script order"))?;
        let covered: Vec<_> = doc.lines.get(a..=b).ok_or_else(|| bad(cmd, "invalid line range"))?.iter().map(|l| l.id).collect();
        let take = doc.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        if take.marks.iter().any(|mark| !covered.contains(&mark.line)) {
            return Err(bad(cmd, "new candle range excludes existing marks; move or delete those marks first"));
        }
        if take.offscreen_lines.iter().any(|line| !covered.contains(line)) {
            return Err(bad(cmd, "new candle range excludes an off-screen passage; clear that passage first"));
        }
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
        p.get("color").map(|v| v.as_str().and_then(Label::from_name).ok_or_else(|| bad(cmd, "color must be a known Filmcraft label name"))).transpose()?;
    let offscreen = p
        .get("offscreenLines")
        .map(|v| -> Result<Vec<ScriptLineId>> {
            let a = v.as_array().ok_or_else(|| bad(cmd, "offscreenLines must be an array of line IDs"))?;
            if a.len() > ScriptDocument::MAX_LINES {
                return Err(bad(cmd, "too many off-screen lines"));
            }
            a.iter().map(|x| x.as_u64().filter(|n| *n > 0).map(ScriptLineId).ok_or_else(|| bad(cmd, "offscreenLines must contain positive line IDs"))).collect()
        })
        .transpose()?;
    s.edit("Set Script Take Style", |project, _| {
        let doc = script_mut(project, item, cmd)?;
        let take = doc.take(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        let (a, b) = doc.line_range(take.start_line, take.end_line).ok_or_else(|| bad(cmd, "take coverage is damaged"))?;
        if let Some(ref selected) = offscreen {
            let covered = doc.lines.get(a..=b).ok_or_else(|| bad(cmd, "take coverage is damaged"))?;
            if selected.iter().any(|line| !covered.iter().any(|row| row.id == *line)) {
                return Err(bad(cmd, "off-screen line is outside the take"));
            }
        }
        let take = doc.take_mut(take_id).ok_or_else(|| bad(cmd, "take does not exist"))?;
        if let Some(color) = color {
            take.color = color;
        }
        if let Some(mut selected) = offscreen {
            selected.sort_unstable();
            selected.dedup();
            take.offscreen_lines = selected;
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

fn cue_mark(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "script.cueMark";
    let doc = script(&s.project, item(p, cmd)?, cmd)?;
    let take = doc.take(ScriptTakeId(id(p, "take", cmd)?)).ok_or_else(|| bad(cmd, "take does not exist"))?;
    let source = take.source.ok_or_else(|| bad(cmd, "take is unlinked"))?;
    let mark_id = ScriptMarkId(id(p, "mark", cmd)?);
    let mark = take.marks.iter().find(|m| m.id == mark_id).ok_or_else(|| bad(cmd, "mark does not exist"))?;
    let at = mark.source_time;
    if s.project.item(source).is_none() {
        return Err(bad(cmd, "source item is gone"));
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
        spec("script.editLine", "Edit Script Line", r#"{"item":id,"line":id,"text":str}"#, edit_line, true),
        spec("script.insertLine", "Insert Script Line", r#"{"item":id,"after":id?,"text":str}"#, insert_line, true),
        spec("script.deleteLine", "Delete Script Line", r#"{"item":id,"line":id}"#, delete_line, true),
        spec("script.setLabel", "Set Script Scene/Page", r#"{"item":id,"line":id,"scene":str|null?,"page":str|null?}"#, set_label, true),
        spec("script.addSlate", "Add Script Slate", r#"{"item":id,"name":str}"#, add_slate, true),
        spec("script.addTake", "Add Script Take", r#"{"item":id,"slate":id?,"source":id,"startLine":id,"endLine":id}"#, add_take, true),
        spec("script.rebindTake", "Rebind Script Take", r#"{"item":id,"take":id,"source":id,"discardMarks":bool?}"#, rebind_take, true),
        spec("script.setCoverage", "Set Script Take Coverage", r#"{"item":id,"take":id,"startLine":id,"endLine":id}"#, set_coverage, true),
        spec("script.setTakeStyle", "Set Script Take Style", r#"{"item":id,"take":id,"color":str?,"offscreenLines":[id]?}"#, set_style, true),
        spec("script.addMark", "Add Script Mark", r#"{"item":id,"take":id,"line":id,"sourceTime":ticks}"#, add_mark, true),
        spec("script.deleteMark", "Delete Script Mark", r#"{"item":id,"take":id,"mark":id}"#, delete_mark, true),
        spec("script.cueMark", "Cue Script Mark", r#"{"item":id,"take":id,"mark":id}"#, cue_mark, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{Generator, MediaSource};
    use filmcraft_project::{MediaClip, MediaRef};
    use filmcraft_time::{FrameRate, TimeRange};

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
        assert!(s.execute("script.setCoverage", json!({"item":script,"take":take,"startLine":first,"endLine":first})).is_err());
        let bytes = filmcraft_format::encode(&s.project, false);
        let loaded = filmcraft_format::decode(&bytes).unwrap();
        let saved_take = loaded.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap();
        assert_eq!(saved_take.marks[0].id.0, mark);
        assert!(s.execute("script.rebindTake", json!({"item":script,"take":take,"source":different.0})).is_err());
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().source, Some(media));
        s.execute("script.rebindTake", json!({"item":script,"take":take,"source":different.0,"discardMarks":true})).unwrap();
        assert!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().marks.is_empty());
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(ScriptTakeId(take)).unwrap().source, Some(media));
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

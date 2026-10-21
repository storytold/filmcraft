//! Native lined scripts. A line's identity is independent of wrapping and its current row.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use filmcraft_time::Tick;

use crate::{ItemId, Label, MediaIdentity};

macro_rules! script_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

script_id!(ScriptLineId);
script_id!(ScriptSlateId);
script_id!(ScriptTakeId);
script_id!(ScriptMarkId);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptLine {
    pub id: ScriptLineId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptMark {
    pub id: ScriptMarkId,
    pub line: ScriptLineId,
    /// Time in the linked source's frame space, never display timecode.
    pub source_time: Tick,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptTake {
    pub id: ScriptTakeId,
    /// None after the linked Project item is deleted; the slate and range remain for replacement.
    pub source: Option<ItemId>,
    pub source_name: String,
    pub start_line: ScriptLineId,
    pub end_line: ScriptLineId,
    pub color: Label,
    /// Off-screen is a line-level annotation. Rendering coalesces adjacent lines into passages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offscreen_lines: Vec<ScriptLineId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<ScriptMark>,
    /// Identity of the underlying linked file where available; timecode alone is not identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity: Option<MediaIdentity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptSlate {
    pub id: ScriptSlateId,
    pub name: String,
    pub takes: Vec<ScriptTake>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScriptDocument {
    pub next_id: u64,
    pub lines: Vec<ScriptLine>,
    pub slates: Vec<ScriptSlate>,
}

impl ScriptDocument {
    /// Maximum accepted plain text size and logical line count. Source text is untrusted input.
    pub const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
    pub const MAX_LINES: usize = 50_000;
    pub const MAX_LINE_BYTES: usize = 16_384;
    pub const MAX_SLATES: usize = 10_000;
    pub const MAX_TAKES: usize = 50_000;
    pub const MAX_ANNOTATIONS: usize = 200_000;

    pub fn new() -> Self {
        Self { next_id: 1, ..Self::default() }
    }

    /// Import logical lines without changing indentation, blank rows or repeated dialogue.
    /// A terminal line break is not interpreted as an extra blank line.
    pub fn from_text(text: &str) -> Result<Self, &'static str> {
        if text.len() > Self::MAX_TEXT_BYTES {
            return Err("script text exceeds the 4 MiB limit");
        }
        let mut doc = Self::new();
        if text.is_empty() {
            return Ok(doc);
        }
        let body = text.strip_prefix('\u{feff}').unwrap_or(text);
        for row in body.split_terminator('\n') {
            if doc.lines.len() >= Self::MAX_LINES {
                return Err("script text exceeds the 50,000-line limit");
            }
            if row.len() > Self::MAX_LINE_BYTES.saturating_add(1) {
                return Err("a script line exceeds the 16 KiB limit");
            }
            let id = ScriptLineId(doc.alloc_id()?);
            let content = row.strip_suffix('\r').unwrap_or(row);
            if content.len() > Self::MAX_LINE_BYTES {
                return Err("a script line exceeds the 16 KiB limit");
            }
            doc.lines.push(ScriptLine { id, text: content.to_owned(), scene: None, page: None });
        }
        Ok(doc)
    }

    pub fn alloc_id(&mut self) -> Result<u64, &'static str> {
        let id = self.next_id;
        if id == 0 {
            return Err("script has exhausted its IDs");
        }
        self.next_id = id.checked_add(1).ok_or("script has exhausted its IDs")?;
        Ok(id)
    }

    /// Sum visible text and the line separators that a plain-text export would need.
    pub fn text_bytes(&self) -> Option<usize> {
        let characters = self.lines.iter().try_fold(0usize, |sum, row| sum.checked_add(row.text.len()))?;
        characters.checked_add(self.lines.len().saturating_sub(1))
    }

    pub fn annotation_count(&self) -> Option<usize> {
        self.slates
            .iter()
            .flat_map(|slate| &slate.takes)
            .try_fold(0usize, |sum, take| sum.checked_add(take.marks.len()).and_then(|n| n.checked_add(take.offscreen_lines.len())))
    }

    /// Validate untrusted serialized scripts before an editor can mutate them. Stable IDs are
    /// unique across all node types, and every annotation must point into its take's candle.
    pub fn validate(&self) -> Result<(), String> {
        if self.lines.len() > Self::MAX_LINES || self.slates.len() > Self::MAX_SLATES {
            return Err("too many script lines or slates".into());
        }
        if self.text_bytes().is_none_or(|bytes| bytes > Self::MAX_TEXT_BYTES) {
            return Err("script text exceeds the 4 MiB limit".into());
        }
        let mut ids = BTreeSet::new();
        let mut greatest = 0;
        let mut accept_id = |id: u64| -> Result<(), String> {
            if id == 0 || !ids.insert(id) {
                return Err(format!("duplicate or zero script ID {id}"));
            }
            greatest = greatest.max(id);
            Ok(())
        };
        let mut positions = BTreeMap::new();
        for (index, row) in self.lines.iter().enumerate() {
            accept_id(row.id.0)?;
            if row.text.len() > Self::MAX_LINE_BYTES || row.scene.as_ref().is_some_and(|s| s.len() > 256) || row.page.as_ref().is_some_and(|s| s.len() > 256) {
                return Err("script line or Scene/Page label exceeds its size limit".into());
            }
            positions.insert(row.id, index);
        }
        let mut take_count = 0usize;
        let mut annotation_count = 0usize;
        for slate in &self.slates {
            accept_id(slate.id.0)?;
            if slate.name.len() > 4096 {
                return Err("script slate name exceeds its size limit".into());
            }
            take_count = take_count.checked_add(slate.takes.len()).ok_or("too many script takes")?;
            if take_count > Self::MAX_TAKES {
                return Err("too many script takes".into());
            }
            for take in &slate.takes {
                accept_id(take.id.0)?;
                if take.source_name.len() > 4096 {
                    return Err("script take source name exceeds its size limit".into());
                }
                if take.source.is_some_and(|id| id.0 == 0) {
                    return Err("script take has a zero source ID".into());
                }
                let Some((&a, &b)) = positions.get(&take.start_line).zip(positions.get(&take.end_line)) else {
                    return Err("script take refers to a missing line".into());
                };
                if a > b {
                    return Err("script take has reversed coverage".into());
                }
                annotation_count = annotation_count
                    .checked_add(take.offscreen_lines.len())
                    .and_then(|n| n.checked_add(take.marks.len()))
                    .ok_or("too many script annotations")?;
                if annotation_count > Self::MAX_ANNOTATIONS {
                    return Err("too many script annotations".into());
                }
                for line in &take.offscreen_lines {
                    if positions.get(line).is_none_or(|i| !(a..=b).contains(i)) {
                        return Err("off-screen annotation is outside its take".into());
                    }
                }
                for mark in &take.marks {
                    accept_id(mark.id.0)?;
                    if positions.get(&mark.line).is_none_or(|i| !(a..=b).contains(i)) {
                        return Err("script mark is outside its take".into());
                    }
                }
            }
        }
        if self.next_id == 0 || self.next_id <= greatest {
            return Err("script next_id does not follow its object IDs".into());
        }
        Ok(())
    }

    pub fn line_index(&self, id: ScriptLineId) -> Option<usize> {
        self.lines.iter().position(|line| line.id == id)
    }

    pub fn take(&self, id: ScriptTakeId) -> Option<&ScriptTake> {
        self.slates.iter().flat_map(|slate| &slate.takes).find(|take| take.id == id)
    }

    pub fn take_mut(&mut self, id: ScriptTakeId) -> Option<&mut ScriptTake> {
        self.slates.iter_mut().flat_map(|slate| &mut slate.takes).find(|take| take.id == id)
    }

    pub fn line_range(&self, start: ScriptLineId, end: ScriptLineId) -> Option<(usize, usize)> {
        let a = self.line_index(start)?;
        let b = self.line_index(end)?;
        (a <= b).then_some((a, b))
    }

    /// A deleted line loses its marks and annotations. Endpoints move to the nearest surviving
    /// line inside the old coverage, or the take is removed if no covered line remains.
    pub fn delete_line(&mut self, line: ScriptLineId) -> bool {
        let Some(index) = self.line_index(line) else { return false };
        let indexes: BTreeMap<ScriptLineId, usize> = self.lines.iter().enumerate().map(|(i, row)| (row.id, i)).collect();
        let ranges: BTreeMap<ScriptTakeId, Option<(ScriptLineId, ScriptLineId)>> = self
            .slates
            .iter()
            .flat_map(|slate| slate.takes.iter())
            .filter_map(|take| {
                let (&a, &b) = (indexes.get(&take.start_line)?, indexes.get(&take.end_line)?);
                if a > b {
                    return None;
                }
                let left = if a == index { a.saturating_add(1) } else { a };
                let right = if b == index { b.saturating_sub(1) } else { b };
                let kept = (left <= right).then(|| self.lines.get(left).zip(self.lines.get(right)).map(|(x, y)| (x.id, y.id))).flatten();
                Some((take.id, kept))
            })
            .collect();
        self.lines.remove(index);
        for slate in &mut self.slates {
            slate.takes.retain(|take| ranges.get(&take.id).is_none_or(Option::is_some));
            for take in &mut slate.takes {
                if let Some(Some((a, b))) = ranges.get(&take.id) {
                    take.start_line = *a;
                    take.end_line = *b;
                }
                take.marks.retain(|mark| mark.line != line);
                take.offscreen_lines.retain(|id| *id != line);
            }
        }
        true
    }

    pub fn unbind_source(&mut self, source: ItemId) {
        for take in self.slates.iter_mut().flat_map(|slate| &mut slate.takes) {
            if take.source == Some(source) {
                take.source = None;
            }
        }
    }

    pub fn shift_source_time(&mut self, sources: &[ItemId], delta: Tick) {
        for take in self.slates.iter_mut().flat_map(|slate| &mut slate.takes) {
            if take.source.is_some_and(|id| sources.contains(&id)) {
                for mark in &mut take.marks {
                    // Preserve signed time: a relink can place a mark before the new source's
                    // start. Cueing then reports it as unavailable instead of changing its frame.
                    mark.source_time = Tick(mark.source_time.0.saturating_sub(delta.0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_import_preserves_logical_rows() {
        let doc = ScriptDocument::from_text("\u{feff} A\r\n\r\nA\n").unwrap();
        assert_eq!(doc.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), [" A", "", "A"]);
        assert_eq!(doc.lines.iter().map(|l| l.id.0).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(ScriptDocument::from_text(&"x".repeat(ScriptDocument::MAX_LINE_BYTES + 1)).is_err());
    }

    #[test]
    fn deleting_a_line_discards_its_marks_but_keeps_remaining_coverage() {
        let mut doc = ScriptDocument::from_text("one\ntwo\nthree").unwrap();
        doc.slates.push(ScriptSlate {
            id: ScriptSlateId(4),
            name: "setup".into(),
            takes: vec![ScriptTake {
                id: ScriptTakeId(5),
                source: Some(ItemId(9)),
                source_name: "clip".into(),
                start_line: ScriptLineId(1),
                end_line: ScriptLineId(3),
                color: Label::Blue,
                offscreen_lines: vec![ScriptLineId(2)],
                marks: vec![ScriptMark { id: ScriptMarkId(6), line: ScriptLineId(2), source_time: Tick(42) }],
                source_identity: None,
            }],
        });
        assert!(doc.delete_line(ScriptLineId(2)));
        let take = doc.take(ScriptTakeId(5)).unwrap();
        assert_eq!((take.start_line.0, take.end_line.0), (1, 3));
        assert!(take.marks.is_empty());
        assert!(take.offscreen_lines.is_empty());
        assert_eq!(doc.lines.len(), 2);
    }

    #[test]
    fn validator_rejects_duplicate_and_stale_ids_and_outside_marks() {
        let mut doc = ScriptDocument::from_text("first\nsecond").unwrap();
        assert!(doc.validate().is_ok());
        doc.lines[1].id = doc.lines[0].id;
        assert!(doc.validate().is_err());
        doc.lines[1].id = ScriptLineId(2);
        doc.next_id = 2;
        assert!(doc.validate().is_err());
        doc.next_id = 4;
        doc.slates.push(ScriptSlate {
            id: ScriptSlateId(3),
            name: "setup".into(),
            takes: vec![ScriptTake {
                id: ScriptTakeId(4),
                source: None,
                source_name: "gone".into(),
                start_line: ScriptLineId(1),
                end_line: ScriptLineId(1),
                color: Label::Blue,
                offscreen_lines: vec![],
                marks: vec![ScriptMark { id: ScriptMarkId(5), line: ScriptLineId(2), source_time: Tick(-100) }],
                source_identity: None,
            }],
        });
        doc.next_id = 6;
        assert!(doc.validate().is_err());
        doc.slates[0].takes[0].marks[0].line = ScriptLineId(1);
        assert!(doc.validate().is_ok(), "signed post-relink source time is valid stored data");
    }
}

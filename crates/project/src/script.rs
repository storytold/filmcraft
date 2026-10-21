//! Native lined scripts. A line's identity is independent of wrapping and its current row.

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
            let id = ScriptLineId(doc.alloc_id()?);
            doc.lines.push(ScriptLine { id, text: row.strip_suffix('\r').unwrap_or(row).to_owned(), scene: None, page: None });
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
        let ranges: Vec<(ScriptTakeId, Option<(ScriptLineId, ScriptLineId)>)> = self
            .slates
            .iter()
            .flat_map(|slate| slate.takes.iter())
            .filter_map(|take| {
                let (a, b) = self.line_range(take.start_line, take.end_line)?;
                let kept: Vec<_> = (a..=b).filter(|i| *i != index).filter_map(|i| self.lines.get(i).map(|l| l.id)).collect();
                Some((take.id, kept.first().copied().zip(kept.last().copied())))
            })
            .collect();
        self.lines.remove(index);
        for slate in &mut self.slates {
            slate.takes.retain(|take| ranges.iter().find(|(id, _)| *id == take.id).is_none_or(|(_, r)| r.is_some()));
            for take in &mut slate.takes {
                if let Some((_, Some((a, b)))) = ranges.iter().find(|(id, _)| *id == take.id) {
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
                    // Relink metadata is untrusted. Saturation avoids wrapping a corrupt mark
                    // into an unrelated frame; healthy timecode shifts remain exact.
                    mark.source_time = Tick(mark.source_time.0.saturating_sub(delta.0).max(0));
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
}

//! Native lined scripts. A line's identity is independent of wrapping and its current row.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use filmcraft_time::Tick;

use crate::{ItemId, Label, MediaIdentity};

/// Candle colors are independent of Project-item labels. Default follows the
/// Script canvas background, while White and Gray remain explicit colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptColor {
    #[default]
    Default,
    White,
    Gray,
    Violet,
    Iris,
    Caribbean,
    Lavender,
    Cerulean,
    Forest,
    Rose,
    Mango,
    Purple,
    Blue,
    Teal,
    Magenta,
    Tan,
    Green,
    Brown,
    Yellow,
}

impl ScriptColor {
    pub const ALL: [Self; 19] = [
        Self::Default,
        Self::White,
        Self::Gray,
        Self::Violet,
        Self::Iris,
        Self::Caribbean,
        Self::Lavender,
        Self::Cerulean,
        Self::Forest,
        Self::Rose,
        Self::Mango,
        Self::Purple,
        Self::Blue,
        Self::Teal,
        Self::Magenta,
        Self::Tan,
        Self::Green,
        Self::Brown,
        Self::Yellow,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::White => "White",
            Self::Gray => "Gray",
            Self::Violet => "Violet",
            Self::Iris => "Iris",
            Self::Caribbean => "Caribbean",
            Self::Lavender => "Lavender",
            Self::Cerulean => "Cerulean",
            Self::Forest => "Forest",
            Self::Rose => "Rose",
            Self::Mango => "Mango",
            Self::Purple => "Purple",
            Self::Blue => "Blue",
            Self::Teal => "Teal",
            Self::Magenta => "Magenta",
            Self::Tan => "Tan",
            Self::Green => "Green",
            Self::Brown => "Brown",
            Self::Yellow => "Yellow",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.name() == name)
    }
    pub fn label(self) -> Option<Label> {
        match self {
            Self::Default | Self::White | Self::Gray => None,
            Self::Violet => Some(Label::Violet),
            Self::Iris => Some(Label::Iris),
            Self::Caribbean => Some(Label::Caribbean),
            Self::Lavender => Some(Label::Lavender),
            Self::Cerulean => Some(Label::Cerulean),
            Self::Forest => Some(Label::Forest),
            Self::Rose => Some(Label::Rose),
            Self::Mango => Some(Label::Mango),
            Self::Purple => Some(Label::Purple),
            Self::Blue => Some(Label::Blue),
            Self::Teal => Some(Label::Teal),
            Self::Magenta => Some(Label::Magenta),
            Self::Tan => Some(Label::Tan),
            Self::Green => Some(Label::Green),
            Self::Brown => Some(Label::Brown),
            Self::Yellow => Some(Label::Yellow),
        }
    }
}

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
pub struct ScriptLineColor {
    pub line: ScriptLineId,
    pub color: ScriptColor,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptTake {
    pub id: ScriptTakeId,
    /// None after the linked Project item is deleted; the slate and range remain for replacement.
    pub source: Option<ItemId>,
    pub source_name: String,
    pub start_line: ScriptLineId,
    pub end_line: ScriptLineId,
    /// Chosen thumbnail frame for this candle. None uses its first visible mark or source In.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_time: Option<Tick>,
    #[serde(default)]
    pub color: ScriptColor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_colors: Vec<ScriptLineColor>,
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
    /// Optional independent row for the setup thumbnail; defaults to its first candle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_line: Option<ScriptLineId>,
    /// Thumbnail and candle-group center as a fraction of the Script canvas width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_x: Option<f32>,
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
        self.slates.iter().flat_map(|slate| &slate.takes).try_fold(0usize, |sum, take| {
            sum.checked_add(take.marks.len()).and_then(|n| n.checked_add(take.offscreen_lines.len())).and_then(|n| n.checked_add(take.line_colors.len()))
        })
    }

    /// Validate untrusted serialized scripts before an editor can mutate them. Stable IDs are
    /// unique across all node types. Annotations may live on hidden lines outside candle
    /// coverage so shortening and re-expanding a take never discards editorial work.
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
            if slate.thumbnail_line.is_some_and(|line| !positions.contains_key(&line)) {
                return Err("script setup thumbnail refers to a missing line".into());
            }
            if slate.thumbnail_x.is_some_and(|x| !x.is_finite() || !(0.0..=1.0).contains(&x)) {
                return Err("script setup thumbnail position must be between 0 and 1".into());
            }
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
                    .and_then(|n| n.checked_add(take.line_colors.len()))
                    .ok_or("too many script annotations")?;
                if annotation_count > Self::MAX_ANNOTATIONS {
                    return Err("too many script annotations".into());
                }
                for line in &take.offscreen_lines {
                    if !positions.contains_key(line) {
                        return Err("off-screen annotation refers to a missing line".into());
                    }
                }
                let mut colored = BTreeSet::new();
                for entry in &take.line_colors {
                    if !colored.insert(entry.line) || !positions.contains_key(&entry.line) {
                        return Err("candle color is duplicated or refers to a missing line".into());
                    }
                }
                for mark in &take.marks {
                    accept_id(mark.id.0)?;
                    if !positions.contains_key(&mark.line) {
                        return Err("script mark refers to a missing line".into());
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

    /// First stored mark on a currently covered line, for the default thumbnail frame.
    /// Hidden marks remain in storage but cannot choose a visible thumbnail.
    pub fn first_visible_mark_time(&self, take: &ScriptTake) -> Option<Tick> {
        let (start, end) = self.line_range(take.start_line, take.end_line)?;
        take.marks.iter().find(|mark| self.line_index(mark.line).is_some_and(|row| (start..=end).contains(&row))).map(|mark| mark.source_time)
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
            if slate.thumbnail_line == Some(line) {
                slate.thumbnail_line = None;
            }
            slate.takes.retain(|take| ranges.get(&take.id).is_none_or(Option::is_some));
            for take in &mut slate.takes {
                if let Some(Some((a, b))) = ranges.get(&take.id) {
                    take.start_line = *a;
                    take.end_line = *b;
                }
                take.marks.retain(|mark| mark.line != line);
                take.offscreen_lines.retain(|id| *id != line);
                take.line_colors.retain(|entry| entry.line != line);
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

    pub fn unverified_marked_takes(&self, sources: &[ItemId], identity: Option<MediaIdentity>) -> usize {
        self.slates
            .iter()
            .flat_map(|slate| &slate.takes)
            .filter(|take| {
                take.source.is_some_and(|id| sources.contains(&id))
                    && !take.marks.is_empty()
                    && (take.source_identity.is_none() || take.source_identity != identity)
            })
            .count()
    }

    /// A media relink keeps Project item IDs and Script marks stable. Refresh linked takes'
    /// file identities after a verified or explicitly confirmed same-footage relink.
    pub fn relink_sources(&mut self, sources: &[ItemId], identity: Option<MediaIdentity>) {
        for take in self.slates.iter_mut().flat_map(|slate| &mut slate.takes) {
            if take.source.is_some_and(|id| sources.contains(&id)) {
                take.source_identity = identity;
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
            thumbnail_line: None,
            thumbnail_x: None,
            takes: vec![ScriptTake {
                id: ScriptTakeId(5),
                source: Some(ItemId(9)),
                source_name: "clip".into(),
                start_line: ScriptLineId(1),
                end_line: ScriptLineId(3),
                thumbnail_time: None,
                color: ScriptColor::Blue,
                line_colors: vec![ScriptLineColor { line: ScriptLineId(2), color: ScriptColor::Rose }],
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
        assert!(take.line_colors.is_empty());
        assert_eq!(doc.lines.len(), 2);
    }

    #[test]
    fn validator_rejects_duplicate_and_stale_ids_but_keeps_hidden_marks() {
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
            thumbnail_line: None,
            thumbnail_x: None,
            takes: vec![ScriptTake {
                id: ScriptTakeId(4),
                source: None,
                source_name: "gone".into(),
                start_line: ScriptLineId(1),
                end_line: ScriptLineId(1),
                thumbnail_time: None,
                color: ScriptColor::Blue,
                line_colors: vec![],
                offscreen_lines: vec![],
                marks: vec![ScriptMark { id: ScriptMarkId(5), line: ScriptLineId(2), source_time: Tick(-100) }],
                source_identity: None,
            }],
        });
        doc.next_id = 6;
        assert!(doc.validate().is_ok(), "hidden marks and signed source times remain valid stored data");
        assert_eq!(doc.first_visible_mark_time(doc.take(ScriptTakeId(4)).unwrap()), None);
        doc.slates[0].takes[0].end_line = ScriptLineId(2);
        assert_eq!(doc.first_visible_mark_time(doc.take(ScriptTakeId(4)).unwrap()), Some(Tick(-100)));
        doc.slates[0].takes[0].marks[0].line = ScriptLineId(99);
        assert!(doc.validate().is_err(), "marks on deleted or nonexistent lines remain invalid");
    }
}

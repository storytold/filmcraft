//! Project panel queries: Edit ▸ Find… (column / operator / text rows, match all or any) and
//! search bins (File ▸ New ▸ Search Bin), which are saved queries whose contents update live.
//!
//! A query is evaluated against the text of a column of a project item ([`column_text`]): the
//! name, label, media type, frame rate, duration, video / audio info, file path, or any metadata
//! field (Description, Scene, Shot, Log Note, Tape Name…). `All` searches every column.

use serde::{Deserialize, Serialize};

use crate::{BinId, ItemId, ItemKind, MediaRef, Project, ProjectItem};

/// How a row compares the column text with the search text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FindOp {
    #[default]
    Contains,
    /// The whole column text equals the search text.
    Matches,
    BeginsWith,
    EndsWith,
    DoesNotContain,
}

impl FindOp {
    pub const ALL: [FindOp; 5] = [FindOp::Contains, FindOp::Matches, FindOp::BeginsWith, FindOp::EndsWith, FindOp::DoesNotContain];

    pub fn label(self) -> &'static str {
        match self {
            FindOp::Contains => "Contains",
            FindOp::Matches => "Matches",
            FindOp::BeginsWith => "Begins With",
            FindOp::EndsWith => "Ends With",
            FindOp::DoesNotContain => "Does Not Contain",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            FindOp::Contains => "contains",
            FindOp::Matches => "matches",
            FindOp::BeginsWith => "beginsWith",
            FindOp::EndsWith => "endsWith",
            FindOp::DoesNotContain => "doesNotContain",
        }
    }
    pub fn from_name(s: &str) -> Option<FindOp> {
        let k = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        FindOp::ALL.into_iter().find(|o| o.name().to_ascii_lowercase() == k || o.label().to_ascii_lowercase().replace(' ', "") == k)
    }
    fn test(self, hay: &str, needle: &str) -> bool {
        match self {
            FindOp::Contains => hay.contains(needle),
            FindOp::Matches => hay == needle,
            FindOp::BeginsWith => hay.starts_with(needle),
            FindOp::EndsWith => hay.ends_with(needle),
            FindOp::DoesNotContain => !hay.contains(needle),
        }
    }
}

/// One row of the Find dialog: `<column> <operator> <text>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FindRow {
    /// A column name from [`COLUMNS`] or any metadata field; `All` searches every column.
    pub column: String,
    pub op: FindOp,
    pub text: String,
}

/// A Find / search-bin query. Empty rows (no text) are ignored; a query without rows matches
/// nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FindQuery {
    pub rows: Vec<FindRow>,
    /// Match: All (every row) or Any (one row is enough).
    pub match_all: bool,
    pub case_sensitive: bool,
}

/// A saved query shown as a bin in the Project panel (File ▸ New ▸ Search Bin). Its contents are
/// the items matching the query, computed when shown, so they follow every change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchBin {
    pub id: BinId,
    pub name: String,
    pub query: FindQuery,
}

/// The columns offered by the Find dialog, in its order (metadata fields after the built-ins).
pub const COLUMNS: [&str; 15] = [
    "All",
    "Name",
    "Label",
    "Media Type",
    "Frame Rate",
    "Media Duration",
    "Video Info",
    "Audio Info",
    "File Path",
    "Tape Name",
    "Description",
    "Comment",
    "Log Note",
    "Scene",
    "Shot",
];

/// The text of one column of an item (empty when the item has no such value).
pub fn column_text(p: &Project, it: &ProjectItem, column: &str) -> String {
    let media = match &it.kind {
        ItemKind::Media(m) => Some(m),
        ItemKind::Subclip { parent, .. } => p.item(*parent).and_then(|x| x.as_media()),
        _ => None,
    };
    match column.to_ascii_lowercase().as_str() {
        "name" => it.name.clone(),
        "label" => it.label.name().to_string(),
        "media type" | "type" => it.type_label().to_string(),
        "frame rate" => {
            if it.has_video() {
                format!("{} fps", it.frame_rate().label())
            } else {
                String::new()
            }
        }
        "media duration" | "duration" => {
            let rate = it.frame_rate();
            filmcraft_time::format_timecode_frames(rate.frame_at(it.duration()), rate, false)
        }
        "video info" => media
            .and_then(|m| m.info.video.as_ref())
            .map(|v| format!("{} x {} ({:.4}) {}", v.width, v.height, v.par.0 as f64 / v.par.1.max(1) as f64, v.codec))
            .unwrap_or_default(),
        "audio info" => media.and_then(|m| m.info.audio()).map(|a| format!("{} Hz - {} ch {}", a.sample_rate, a.channels, a.codec)).unwrap_or_default(),
        "file path" | "path" => match media.map(|m| &m.media) {
            Some(MediaRef::File { path }) => path.clone(),
            _ => String::new(),
        },
        _ => it.metadata.iter().find(|(k, _)| k.eq_ignore_ascii_case(column)).map(|(_, v)| v.clone()).unwrap_or_default(),
    }
}

impl FindQuery {
    /// A one-row query.
    pub fn simple(column: &str, op: FindOp, text: &str) -> Self {
        FindQuery { rows: vec![FindRow { column: column.into(), op, text: text.into() }], match_all: true, case_sensitive: false }
    }

    fn active_rows(&self) -> impl Iterator<Item = &FindRow> {
        self.rows.iter().filter(|r| !r.text.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.active_rows().next().is_none()
    }

    /// Does `s` satisfy the row's operator?
    fn row_matches_text(&self, r: &FindRow, s: &str) -> bool {
        if self.case_sensitive { r.op.test(s, &r.text) } else { r.op.test(&s.to_lowercase(), &r.text.to_lowercase()) }
    }

    fn row_matches(&self, p: &Project, it: &ProjectItem, r: &FindRow) -> bool {
        if r.column.is_empty() || r.column.eq_ignore_ascii_case("all") {
            let mut texts: Vec<String> = COLUMNS[1..].iter().map(|c| column_text(p, it, c)).collect();
            texts.extend(it.metadata.values().cloned());
            // "does not contain": no column contains the text; otherwise one column is enough
            return if r.op == FindOp::DoesNotContain {
                texts.iter().all(|t| self.row_matches_text(r, t))
            } else {
                texts.iter().any(|t| self.row_matches_text(r, t))
            };
        }
        self.row_matches_text(r, &column_text(p, it, &r.column))
    }

    /// Does the item match? Graphic sources (not shown in the Project panel) never match.
    pub fn matches(&self, p: &Project, it: &ProjectItem) -> bool {
        if matches!(it.kind, ItemKind::Graphic { .. }) || self.is_empty() {
            return false;
        }
        let mut rows = self.active_rows();
        if self.match_all { rows.all(|r| self.row_matches(p, it, r)) } else { rows.any(|r| self.row_matches(p, it, r)) }
    }

    /// Matching items in Project panel order (bins depth-first, then items not in any bin).
    pub fn find(&self, p: &Project) -> Vec<ItemId> {
        let mut order = Vec::new();
        p.root.all_items(&mut order);
        for id in p.items.keys() {
            if !order.contains(id) {
                order.push(*id);
            }
        }
        order.into_iter().filter(|id| p.item(*id).is_some_and(|it| self.matches(p, it))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Label, Project};

    fn project() -> (Project, ItemId, ItemId) {
        let mut p = Project::new("Find");
        let st = crate::SequenceSettings::default();
        let a = p.new_sequence("Interview A", st.clone(), 1, 1, None);
        let b = p.new_sequence("B-Roll Beach", st, 1, 1, None);
        p.item_mut(a).unwrap().metadata.insert("Scene".into(), "12".into());
        p.item_mut(b).unwrap().metadata.insert("Log Note".into(), "sunset, waves".into());
        p.item_mut(b).unwrap().label = Label::Mango;
        (p, a, b)
    }

    #[test]
    fn operators_columns_and_match_modes() {
        let (p, a, b) = project();
        assert_eq!(FindQuery::simple("Name", FindOp::Contains, "beach").find(&p), vec![b]);
        assert_eq!(FindQuery::simple("Name", FindOp::BeginsWith, "inter").find(&p), vec![a]);
        assert_eq!(FindQuery::simple("Name", FindOp::EndsWith, " a").find(&p), vec![a]);
        assert_eq!(FindQuery::simple("Name", FindOp::Matches, "b-roll beach").find(&p), vec![b]);
        assert!(FindQuery::simple("Name", FindOp::Matches, "b-roll").find(&p).is_empty());
        assert_eq!(FindQuery::simple("Name", FindOp::DoesNotContain, "beach").find(&p), vec![a]);
        assert_eq!(FindQuery::simple("Scene", FindOp::Matches, "12").find(&p), vec![a]);
        assert_eq!(FindQuery::simple("Label", FindOp::Matches, "mango").find(&p), vec![b]);
        assert_eq!(FindQuery::simple("All", FindOp::Contains, "waves").find(&p), vec![b]);
        assert_eq!(FindQuery::simple("Media Type", FindOp::Matches, "sequence").find(&p), vec![a, b]);
        // case sensitivity
        let mut q = FindQuery::simple("Name", FindOp::Contains, "beach");
        q.case_sensitive = true;
        assert!(q.find(&p).is_empty());
        // all vs any
        let mut q = FindQuery {
            rows: vec![
                FindRow { column: "Name".into(), op: FindOp::Contains, text: "a".into() },
                FindRow { column: "Scene".into(), op: FindOp::Matches, text: "12".into() },
            ],
            match_all: true,
            case_sensitive: false,
        };
        assert_eq!(q.find(&p), vec![a]);
        q.match_all = false;
        assert_eq!(q.find(&p), vec![a, b]);
        // empty rows are ignored; no rows match nothing
        q.rows[1].text.clear();
        assert_eq!(q.find(&p), vec![a, b]);
        assert!(FindQuery::default().find(&p).is_empty());
        assert_eq!(FindOp::from_name("Begins With"), Some(FindOp::BeginsWith));
        assert_eq!(FindOp::from_name("doesNotContain"), Some(FindOp::DoesNotContain));
    }
}

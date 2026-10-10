//! Engine side of the Project panel (M12.7): view settings, List view columns and sorting,
//! view presets (all persisted in preferences, `Preferences::project_panel`), Freeform view
//! positions, stacks and saved arrangements (stored in item metadata, so they are undoable and
//! travel with the project without a schema change).
//!
//! | Id | What |
//! |---|---|
//! | `project.view.get` | view settings, the columns shown and every column available |
//! | `project.view.set` | view (List / Icon / Freeform), icon size, font size, Preview Area, thumbnails, Hover Scrub, icon sort |
//! | `project.columns.list` / `project.columns.set` / `project.columns.resize` | Metadata Display: the columns shown, their order and widths |
//! | `project.sort` | sort the List view by a column (again: reverse) |
//! | `project.items` | the rows the panel shows for a bin: sorted, with each shown column's text |
//! | `project.viewPreset.list` / `.save` / `.saveAs` / `.restore` / `.rename` / `.delete` | Save Current View Preset, Save As New View Preset, Restore View Preset ▸ 1–10, Manage Saved View Presets |
//! | `project.freeform.layout` | Freeform positions of a bin's items (auto-placed items included) |
//! | `project.freeform.move` / `.resize` / `.alignToGrid` / `.reset` | place clip cards, Clip Size, Align to Grid, Reset to Grid (undoable) |
//! | `project.freeform.stack` / `.unstack` | group clip cards into a stack (undoable) |
//! | `project.freeform.saveArrangement` / `.restoreArrangement` / `.arrangements` / `.deleteArrangement` | saved arrangements |
//! | `project.freeform.options` | Freeform View Options… (grid, snap, names, durations) |
//! | `project.renameBin` | rename a bin (inline rename, undoable) |
//! | `project.selectAll` / `project.deselectAll` | select every item in the project / clear the Project panel selection (Cmd+A / Cmd+Shift+A with the panel focused) |

use std::collections::BTreeMap;

use filmcraft_project::{Bin, BinEntry, BinId, ItemId, ItemKind, MediaRef, Project, ProjectItem, TrackKind};
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, item_p, str_p, u64_p};
use crate::{Result, Session};

// ------------------------------------------------------------------------------------ settings

/// The Project panel's view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewMode {
    List,
    #[default]
    Icon,
    Freeform,
}

impl ViewMode {
    pub fn from_name(s: &str) -> Option<ViewMode> {
        match s.to_ascii_lowercase().as_str() {
            "list" => Some(ViewMode::List),
            "icon" => Some(ViewMode::Icon),
            "freeform" => Some(ViewMode::Freeform),
            _ => None,
        }
    }
}

/// Panel menu ▸ Font Size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FontSize {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl FontSize {
    pub const ALL: [FontSize; 4] = [FontSize::Small, FontSize::Medium, FontSize::Large, FontSize::ExtraLarge];
    pub fn label(self) -> &'static str {
        match self {
            FontSize::Small => "Small",
            FontSize::Medium => "Medium (default)",
            FontSize::Large => "Large",
            FontSize::ExtraLarge => "Extra Large",
        }
    }
    /// Text size in points.
    pub fn points(self) -> f32 {
        match self {
            FontSize::Small => 10.5,
            FontSize::Medium => 12.0,
            FontSize::Large => 14.0,
            FontSize::ExtraLarge => 16.0,
        }
    }
    pub fn from_name(s: &str) -> Option<FontSize> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        FontSize::ALL.into_iter().find(|f| format!("{f:?}").to_ascii_lowercase() == n || f.label().to_ascii_lowercase().replace(' ', "").starts_with(&n))
    }
}

/// A List view column and its width (points).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub width: f32,
}

/// Sort order: a column name, or (Icon view) `""` for User Order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SortSpec {
    pub column: String,
    pub descending: bool,
}

/// What a view preset saves: the view and how each view shows items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewSettings {
    pub mode: ViewMode,
    /// Icon / Freeform thumbnail width (points).
    pub icon_size: f32,
    pub font_size: FontSize,
    /// List view columns in order (Name first).
    pub columns: Vec<Column>,
    /// List view sort.
    pub sort: SortSpec,
    /// Icon view sort ("" column = User Order).
    pub icon_sort: SortSpec,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            mode: ViewMode::Icon,
            icon_size: 110.0,
            font_size: FontSize::Medium,
            columns: DEFAULT_COLUMNS.iter().map(|c| Column { name: c.to_string(), width: default_width(c) }).collect(),
            sort: SortSpec { column: "Name".into(), descending: false },
            icon_sort: SortSpec::default(),
        }
    }
}

/// A saved view preset (Restore View Preset ▸ Project View Preset N).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPreset {
    pub name: String,
    pub settings: ViewSettings,
}

/// Freeform View Options….
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FreeformOptions {
    /// Grid spacing (points) for Align to Grid and snapping.
    pub grid: f32,
    /// Dropped cards snap to the grid.
    pub snap: bool,
    pub show_names: bool,
    pub show_durations: bool,
    /// Size of cards that have no Clip Size of their own (points).
    pub card_size: f32,
}

impl Default for FreeformOptions {
    fn default() -> Self {
        FreeformOptions { grid: 20.0, snap: false, show_names: true, show_durations: true, card_size: 128.0 }
    }
}

/// Preferences of the Project panel (`Preferences::project_panel`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectPanelPrefs {
    pub view: ViewSettings,
    /// Panel menu ▸ Preview Area (thumbnail + info at the top).
    pub preview_area: bool,
    /// Panel menu ▸ Thumbnails (List view thumbnails).
    pub thumbnails: bool,
    pub thumbnails_show_effects: bool,
    /// Panel menu ▸ Hover Scrub (Shift+H).
    pub hover_scrub: bool,
    pub thumbnail_controls_all_devices: bool,
    /// Ten preset slots (Project View Preset 1–10).
    pub presets: Vec<Option<ViewPreset>>,
    /// The slot last saved or restored (Save Current View Preset overwrites it).
    pub current_preset: Option<usize>,
    pub freeform: FreeformOptions,
}

impl Default for ProjectPanelPrefs {
    fn default() -> Self {
        ProjectPanelPrefs {
            view: ViewSettings::default(),
            preview_area: false,
            thumbnails: false,
            thumbnails_show_effects: true,
            hover_scrub: true,
            thumbnail_controls_all_devices: false,
            presets: vec![None; PRESET_SLOTS],
            current_preset: None,
            freeform: FreeformOptions::default(),
        }
    }
}

pub const PRESET_SLOTS: usize = 10;

// ------------------------------------------------------------------------------------ columns

/// List view columns shown by default (Premiere's default set).
pub const DEFAULT_COLUMNS: [&str; 12] = [
    "Name",
    "Frame Rate",
    "Media Start",
    "Media End",
    "Media Duration",
    "Video In Point",
    "Video Out Point",
    "Video Duration",
    "Video Info",
    "Audio Info",
    "Tape Name",
    "Description",
];

/// Every built-in column (Metadata Display ▸ Project Metadata), in display order.
pub const BUILTIN_COLUMNS: [&str; 36] = [
    "Name",
    "Label",
    "Media Type",
    "Frame Rate",
    "Media Start",
    "Media End",
    "Media Duration",
    "Video In Point",
    "Video Out Point",
    "Video Duration",
    "Audio In Point",
    "Audio Out Point",
    "Audio Duration",
    "Subclip Start",
    "Subclip End",
    "Video Info",
    "Video Codec",
    "Audio Info",
    "Audio Codec",
    "Color Space",
    "Video Usage",
    "Audio Usage",
    "Status",
    "Proxy",
    "File Path",
    "File Size",
    "Date Created",
    "Good",
    "Description",
    "Scene",
    "Shot",
    "Log Note",
    "Comment",
    "Tape Name",
    "Client",
    "Camera Angle",
];

/// Metadata keys FilmCraft uses for its own bookkeeping (not offered as columns).
pub const INTERNAL_KEYS: [&str; 3] = [FREEFORM_POS, FREEFORM_STACK, crate::keyboard::POSTER_FRAME_KEY];

/// Freeform position, stack and arrangement keys (hidden from the Metadata panel).
pub fn is_freeform_key(k: &str) -> bool {
    k == FREEFORM_POS || k == FREEFORM_STACK || k.starts_with(ARRANGEMENT_PREFIX)
}

pub fn is_internal_key(k: &str) -> bool {
    INTERNAL_KEYS.contains(&k) || k.starts_with(ARRANGEMENT_PREFIX)
}

pub fn default_width(c: &str) -> f32 {
    match c {
        "Name" => 220.0,
        "Label" | "Frame Rate" | "Status" | "Good" | "Proxy" => 80.0,
        "Video Info" | "Audio Info" | "File Path" | "Description" | "Log Note" | "Comment" => 170.0,
        _ => 110.0,
    }
}

/// Every column the Metadata Display dialog offers: built-ins, then metadata fields found on the
/// project's items.
pub fn all_columns(p: &Project) -> Vec<String> {
    let mut v: Vec<String> = BUILTIN_COLUMNS.iter().map(|c| c.to_string()).collect();
    let mut extra: Vec<String> = Vec::new();
    for it in p.items.values() {
        for k in it.metadata.keys() {
            if !is_internal_key(k) && !v.iter().any(|c| c.eq_ignore_ascii_case(k)) && !extra.contains(k) {
                extra.push(k.clone());
            }
        }
    }
    extra.sort();
    v.extend(extra);
    v
}

/// A cell's sort key: numbers (times, rates, sizes) sort numerically, text case-insensitively.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub enum SortKey {
    Num(f64),
    Text(String),
}

fn media_of<'a>(p: &'a Project, it: &'a ProjectItem) -> Option<&'a filmcraft_project::MediaClip> {
    match &it.kind {
        ItemKind::Media(m) => Some(m),
        ItemKind::Subclip { parent, .. } => p.item(*parent).and_then(|x| x.as_media()),
        _ => None,
    }
}

/// How often an item is used in the project's sequences: (video uses, audio uses).
pub fn usage(p: &Project, id: ItemId) -> (usize, usize) {
    let (mut v, mut a) = (0, 0);
    for q in p.sequences() {
        for t in q.as_sequence().into_iter().flat_map(|q| q.all_tracks()) {
            let n = t.items.iter().filter(|i| i.item == id).count();
            match t.kind {
                TrackKind::Video => v += n,
                TrackKind::Audio => a += n,
            }
        }
    }
    (v, a)
}

/// The text and sort key of one List view cell.
pub fn cell(p: &Project, it: &ProjectItem, column: &str) -> (String, SortKey) {
    let media = media_of(p, it);
    let rate = match &it.kind {
        ItemKind::Subclip { parent, .. } => p.item(*parent).map(|x| x.frame_rate()).unwrap_or_default(),
        _ => it.frame_rate(),
    };
    let tc = |t: Tick| format_time(t, rate, false, TimeDisplay::Timecode, 48000);
    let time = |t: Option<Tick>| match t {
        Some(t) => (tc(t), SortKey::Num(t.0 as f64)),
        None => (String::new(), SortKey::Num(f64::MIN)),
    };
    let start = media.and_then(|m| m.info.start_timecode).map(|f| rate.tick_of(f)).unwrap_or(Tick::ZERO);
    let (offset, dur) = match &it.kind {
        ItemKind::Subclip { range, .. } => (range.start, range.duration),
        _ => (Tick::ZERO, it.duration()),
    };
    let first = start + offset;
    let fd = rate.frame_duration();
    let marks = match &it.kind {
        ItemKind::Media(m) => (m.mark_in, m.mark_out),
        ItemKind::Sequence(q) => (q.mark_in, q.mark_out),
        _ => (None, None),
    };
    let (mi, mo) = (marks.0.unwrap_or(Tick::ZERO), marks.1.map(|o| o + fd).unwrap_or(dur).min(dur.max(fd)));
    let has_v = it.has_video();
    let has_a = it.has_audio();
    let text = |s: String| {
        let k = SortKey::Text(s.to_lowercase());
        (s, k)
    };
    match column {
        "Name" => text(it.name.clone()),
        "Label" => text(it.label.name().to_string()),
        "Media Type" => text(it.type_label().to_string()),
        "Frame Rate" if has_v => (format!("{} fps", rate.label()), SortKey::Num(rate.num as f64 / rate.den.max(1) as f64)),
        "Frame Rate" => (String::new(), SortKey::Num(0.0)),
        "Media Start" => time(Some(first)),
        "Media End" => time(Some(if dur > Tick::ZERO { first + dur - fd } else { first })),
        "Media Duration" => time(Some(dur)),
        "Video In Point" if has_v => time(Some(first + mi)),
        "Video Out Point" if has_v => time(Some(first + mo - fd)),
        "Video Duration" if has_v => time(Some(mo - mi)),
        "Audio In Point" if has_a => time(Some(first + mi)),
        "Audio Out Point" if has_a => time(Some(first + mo - fd)),
        "Audio Duration" if has_a => time(Some(mo - mi)),
        "Video In Point" | "Video Out Point" | "Video Duration" | "Audio In Point" | "Audio Out Point" | "Audio Duration" => time(None),
        "Subclip Start" | "Subclip End" => match &it.kind {
            ItemKind::Subclip { range, .. } => time(Some(start + if column == "Subclip Start" { range.start } else { range.end() - fd })),
            _ => time(None),
        },
        "Video Info" => match (media.and_then(|m| m.info.video.as_ref()), &it.kind) {
            (Some(v), _) => text(format!("{} x {} ({:.4})", v.width, v.height, v.par.0 as f64 / v.par.1.max(1) as f64)),
            (None, ItemKind::Sequence(q)) => text(format!("{} x {} (1.0)", q.settings.width, q.settings.height)),
            _ => text(String::new()),
        },
        "Video Codec" => text(media.and_then(|m| m.info.video.as_ref()).map(|v| v.codec.clone()).unwrap_or_default()),
        "Audio Info" => text(
            media
                .and_then(|m| m.info.audio())
                .map(|a| {
                    format!(
                        "{} Hz - {}",
                        a.sample_rate,
                        if a.channels == 1 {
                            "Mono".to_string()
                        } else if a.channels == 2 {
                            "Stereo".into()
                        } else {
                            format!("{} ch", a.channels)
                        }
                    )
                })
                .unwrap_or_default(),
        ),
        "Audio Codec" => text(media.and_then(|m| m.info.audio()).map(|a| a.codec.clone()).unwrap_or_default()),
        "Color Space" => text(match (media.and_then(|m| m.info.video.as_ref().map(|v| (m, v))), &it.kind) {
            (Some((m, v)), _) => m.interpret.color_space.unwrap_or_else(|| filmcraft_color::ColorSpace::from_info(&v.color)).label().to_string(),
            (None, ItemKind::Sequence(q)) => q.settings.color.working.label().to_string(),
            _ => String::new(),
        }),
        "Video Usage" | "Audio Usage" => {
            let (v, a) = usage(p, it.id);
            let n = if column == "Video Usage" { v } else { a };
            (if n == 0 { String::new() } else { n.to_string() }, SortKey::Num(n as f64))
        }
        "Status" => text(match media {
            Some(m) if m.offline => "Offline".into(),
            Some(_) => "Online".into(),
            None => String::new(),
        }),
        "Proxy" => text(match media {
            Some(m) if m.proxy.is_some() => "Attached".into(),
            Some(_) => "None".into(),
            None => String::new(),
        }),
        "File Path" => text(match media.map(|m| &m.media) {
            Some(MediaRef::File { path }) => path.clone(),
            _ => String::new(),
        }),
        "File Size" => match media.and_then(|m| m.info.file_size) {
            Some(b) => (format_bytes(b), SortKey::Num(b as f64)),
            None => (String::new(), SortKey::Num(-1.0)),
        },
        "Date Created" => (it.created.to_string(), SortKey::Num(it.created as f64)),
        _ => text(it.metadata.iter().find(|(k, _)| k.eq_ignore_ascii_case(column)).map(|(_, v)| v.clone()).unwrap_or_default()),
    }
}

fn format_bytes(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}

/// Is the item listed in the Project panel? (Graphic clip sources are internal, except source
/// graphics: Upgrade to Source Graphic.)
pub fn listed(p: &Project, it: &ProjectItem) -> bool {
    !matches!(it.kind, ItemKind::Graphic { .. }) || p.source_graphics.contains_key(&it.id)
}

/// Sort item ids by a column (ties keep their order; bins are sorted separately by the caller).
pub fn sort_items(p: &Project, ids: &mut [ItemId], sort: &SortSpec) {
    if sort.column.is_empty() {
        return;
    }
    let mut keyed: Vec<(SortKey, String, ItemId)> = ids
        .iter()
        .filter_map(|i| p.item(*i))
        .map(|it| {
            let (_, k) = cell(p, it, &sort.column);
            (k, it.name.to_lowercase(), it.id)
        })
        .collect();
    keyed.sort_by(|a, b| {
        let o = a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.cmp(&b.1));
        if sort.descending { o.reverse() } else { o }
    });
    for (slot, (_, _, id)) in ids.iter_mut().zip(keyed) {
        *slot = id;
    }
}

/// A bin's direct sub-bins and items (items sorted by `sort`; bins by name, following the
/// direction when the panel sorts by Name).
pub fn bin_children(p: &Project, bin: &Bin, sort: &SortSpec) -> (Vec<BinId>, Vec<ItemId>) {
    let mut bins: Vec<(String, BinId)> = Vec::new();
    let mut items = Vec::new();
    for e in &bin.children {
        match e {
            BinEntry::Bin(b) => bins.push((b.name.to_lowercase(), b.id)),
            BinEntry::Item(i) => {
                if p.item(*i).is_some_and(|it| listed(p, it)) {
                    items.push(*i)
                }
            }
        }
    }
    if !sort.column.is_empty() {
        bins.sort();
        if sort.column == "Name" && sort.descending {
            bins.reverse();
        }
    }
    sort_items(p, &mut items, sort);
    (bins.into_iter().map(|b| b.1).collect(), items)
}

// ------------------------------------------------------------------------------------ freeform

/// Metadata key of a card's Freeform position and size: `"x,y"` or `"x,y,size"` (points).
pub const FREEFORM_POS: &str = "Freeform Position";
/// Metadata key of a card's stack (the id of the stack's first item).
pub const FREEFORM_STACK: &str = "Freeform Stack";
/// Prefix of saved arrangements: `"Freeform Arrangement: <name>"` = `"x,y[,size]"`.
pub const ARRANGEMENT_PREFIX: &str = "Freeform Arrangement: ";

/// One clip card in Freeform view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub item: u64,
    pub x: f32,
    pub y: f32,
    /// Card (thumbnail) width.
    pub size: f32,
    /// Placed by the user (else auto-placed after the placed cards).
    pub placed: bool,
    /// Stack id (the first item of the stack) when stacked.
    pub stack: Option<u64>,
}

pub fn parse_pos(v: &str) -> Option<(f32, f32, Option<f32>)> {
    let mut it = v.split(',').map(|x| x.trim().parse::<f32>().ok());
    let x = it.next()??;
    let y = it.next()??;
    let s = it.next().flatten();
    Some((x, y, s))
}

fn fmt_pos(x: f32, y: f32, size: Option<f32>) -> String {
    match size {
        Some(s) => format!("{},{},{}", x.round(), y.round(), s.round()),
        None => format!("{},{}", x.round(), y.round()),
    }
}

/// Items of a bin as Freeform shows them (direct children, user order).
fn freeform_items(p: &Project, bin: Option<BinId>) -> Vec<ItemId> {
    let b = bin.and_then(|b| p.root.find_bin(b)).unwrap_or(&p.root);
    b.children
        .iter()
        .filter_map(|e| match e {
            BinEntry::Item(i) if p.item(*i).is_some_and(|it| listed(p, it)) => Some(*i),
            _ => None,
        })
        .collect()
}

/// Freeform cards of a bin: placed cards where the user put them, the rest on a grid below them
/// (`width`: the panel width used to wrap the auto-placed cards).
pub fn freeform_layout(p: &Project, bin: Option<BinId>, opts: &FreeformOptions, width: f32) -> Vec<Card> {
    let ids = freeform_items(p, bin);
    let mut cards = Vec::new();
    let mut bottom: f32 = 0.0;
    for id in &ids {
        let Some(it) = p.item(*id) else { continue };
        let stack = it.metadata.get(FREEFORM_STACK).and_then(|v| v.parse::<u64>().ok());
        if let Some((x, y, s)) = it.metadata.get(FREEFORM_POS).and_then(|v| parse_pos(v)) {
            let size = s.unwrap_or(opts.card_size);
            bottom = bottom.max(y + size * 9.0 / 16.0 + 40.0);
            cards.push(Card { item: id.0, x, y, size, placed: true, stack });
        }
    }
    // auto-place the rest on the grid below the placed ones
    let cw = opts.card_size + 24.0;
    let ch = opts.card_size * 9.0 / 16.0 + 44.0;
    let per_row = ((width - 12.0) / cw).floor().max(1.0) as usize;
    let top = if bottom > 0.0 { snap(bottom + 8.0, opts.grid.max(1.0)) } else { 12.0 };
    let mut k = 0usize;
    for id in &ids {
        if cards.iter().any(|c| c.item == id.0) {
            continue;
        }
        let stack = p.item(*id).and_then(|it| it.metadata.get(FREEFORM_STACK)).and_then(|v| v.parse::<u64>().ok());
        let (col, row) = (k % per_row, k / per_row);
        cards.push(Card { item: id.0, x: 12.0 + col as f32 * cw, y: top + row as f32 * ch, size: opts.card_size, placed: false, stack });
        k += 1;
    }
    // stacked cards sit on their stack's first card, slightly offset
    let anchors: BTreeMap<u64, (f32, f32)> = cards.iter().filter(|c| c.stack == Some(c.item)).map(|c| (c.item, (c.x, c.y))).collect();
    let mut depth: BTreeMap<u64, usize> = BTreeMap::new();
    for c in cards.iter_mut() {
        if let Some(s) = c.stack
            && s != c.item
            && let Some((x, y)) = anchors.get(&s)
        {
            let d = depth.entry(s).or_insert(0);
            *d += 1;
            c.x = x + 6.0 * *d as f32;
            c.y = y + 6.0 * *d as f32;
        }
    }
    cards
}

fn snap(v: f32, grid: f32) -> f32 {
    (v / grid).round() * grid
}

fn items_p(s: &Session, p: &Value) -> Vec<ItemId> {
    match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => item_p(p, "item").map(|i| vec![i]).unwrap_or_else(|| s.state.project_selection.clone()),
    }
}

fn bin_p(p: &Value) -> Option<BinId> {
    u64_p(p, "bin").map(BinId)
}

fn layout_json(s: &Session, bin: Option<BinId>, width: f32) -> Value {
    let cards = freeform_layout(&s.project, bin, &s.prefs.project_panel.freeform, width);
    json!({"bin": bin.map(|b| b.0), "cards": cards})
}

/// `project.freeform.move {items?|item?, x, y}` (the first card goes to x, y; the others keep
/// their offsets from it) or `{positions: {"<id>": [x, y]}}`.
fn freeform_move(s: &mut Session, p: &Value) -> Result<Value> {
    let opts = s.prefs.project_panel.freeform.clone();
    let width = f64_p(p, "width").unwrap_or(800.0) as f32;
    let snap_on = bool_p(p, "snap").unwrap_or(opts.snap);
    let mut targets: Vec<(ItemId, f32, f32)> = Vec::new();
    if let Some(m) = p.get("positions").and_then(Value::as_object) {
        for (k, v) in m {
            let id = k.parse::<u64>().map_err(|_| bad("project.freeform.move", format!("bad item id `{k}`")))?;
            let a = v.as_array().ok_or_else(|| bad("project.freeform.move", "positions are [x, y]"))?;
            let (x, y) = (a.first().and_then(Value::as_f64).unwrap_or(0.0) as f32, a.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32);
            targets.push((ItemId(id), x, y));
        }
    } else {
        let items = items_p(s, p);
        if items.is_empty() {
            return Err(bad("project.freeform.move", "need `items` (or a Project panel selection) and `x`, `y`"));
        }
        let (x, y) = (
            f64_p(p, "x").ok_or_else(|| bad("project.freeform.move", "need `x`"))? as f32,
            f64_p(p, "y").ok_or_else(|| bad("project.freeform.move", "need `y`"))? as f32,
        );
        // keep the selection's arrangement: offsets from the first card's current position
        let bin = s.project.root.parent_of(items[0]);
        let cards = freeform_layout(&s.project, bin, &opts, width);
        let at = |id: ItemId| cards.iter().find(|c| c.item == id.0).map(|c| (c.x, c.y));
        let origin = at(items[0]).unwrap_or((x, y));
        for i in &items {
            let (cx, cy) = at(*i).unwrap_or(origin);
            targets.push((*i, x + cx - origin.0, y + cy - origin.1));
        }
    }
    for (i, _, _) in &targets {
        if s.project.item(*i).is_none() {
            return Err(bad("project.freeform.move", format!("no item {}", i.0)));
        }
    }
    let grid = opts.grid.max(1.0);
    s.edit("Move Clip Cards", |pr, _| {
        for (i, x, y) in &targets {
            let (x, y) = if snap_on { (snap(*x, grid), snap(*y, grid)) } else { (*x, *y) };
            let it = pr.item_mut(*i).ok_or_else(|| bad("project.freeform.move", "no such item"))?;
            let size = it.metadata.get(FREEFORM_POS).and_then(|v| parse_pos(v)).and_then(|p| p.2);
            it.metadata.insert(FREEFORM_POS.into(), fmt_pos(x.max(0.0), y.max(0.0), size));
        }
        Ok(())
    })?;
    let bin = targets.first().and_then(|t| s.project.root.parent_of(t.0));
    Ok(layout_json(s, bin, width))
}

/// Pin every card of a bin where it is shown now (so later edits don't move auto-placed cards).
fn pin_all(pr: &mut Project, bin: Option<BinId>, opts: &FreeformOptions, width: f32, f: impl Fn(&Card) -> (f32, f32, Option<f32>)) {
    let cards = freeform_layout(pr, bin, opts, width);
    for c in &cards {
        let (x, y, size) = f(c);
        if let Some(it) = pr.item_mut(ItemId(c.item)) {
            it.metadata.insert(FREEFORM_POS.into(), fmt_pos(x, y, size));
        }
    }
}

fn freeform_resize(s: &mut Session, p: &Value) -> Result<Value> {
    let items = items_p(s, p);
    if items.is_empty() {
        return Err(bad("project.freeform.resize", "need `items` (or a Project panel selection)"));
    }
    let opts = s.prefs.project_panel.freeform.clone();
    let width = f64_p(p, "width").unwrap_or(800.0) as f32;
    let bin = s.project.root.parent_of(items[0]);
    let cards = freeform_layout(&s.project, bin, &opts, width);
    // `size` sets it; `step` (+1 / -1) is Clip Size Next / Previous
    let step = f64_p(p, "step");
    let set = f64_p(p, "size");
    if step.is_none() && set.is_none() {
        return Err(bad("project.freeform.resize", "need `size` or `step`"));
    }
    s.edit("Clip Size", |pr, _| {
        for i in &items {
            let c = cards.iter().find(|c| c.item == i.0).cloned();
            let (x, y, cur) = c.map(|c| (c.x, c.y, c.size)).unwrap_or((12.0, 12.0, opts.card_size));
            let size = match (set, step) {
                (Some(v), _) => v as f32,
                (None, Some(d)) => cur * if d > 0.0 { 1.25 } else { 0.8 },
                _ => cur,
            }
            .clamp(48.0, 480.0);
            let it = pr.item_mut(*i).ok_or_else(|| bad("project.freeform.resize", "no such item"))?;
            it.metadata.insert(FREEFORM_POS.into(), fmt_pos(x, y, Some(size)));
        }
        Ok(())
    })?;
    Ok(layout_json(s, bin, width))
}

fn freeform_align(s: &mut Session, p: &Value) -> Result<Value> {
    let bin = bin_p(p);
    let opts = s.prefs.project_panel.freeform.clone();
    let width = f64_p(p, "width").unwrap_or(800.0) as f32;
    let grid = f64_p(p, "grid").map(|g| g as f32).unwrap_or(opts.grid).max(1.0);
    s.edit("Align to Grid", |pr, _| {
        pin_all(pr, bin, &opts, width, |c| (snap(c.x, grid).max(0.0), snap(c.y, grid).max(0.0), (c.size != opts.card_size).then_some(c.size)));
        Ok(())
    })?;
    Ok(layout_json(s, bin, width))
}

/// Reset to Grid: forget positions and stacks (cards return to the automatic grid).
fn freeform_reset(s: &mut Session, p: &Value) -> Result<Value> {
    let bin = bin_p(p);
    let ids = freeform_items(&s.project, bin);
    s.edit("Reset to Grid", |pr, _| {
        for i in &ids {
            if let Some(it) = pr.item_mut(*i) {
                it.metadata.remove(FREEFORM_POS);
                it.metadata.remove(FREEFORM_STACK);
            }
        }
        Ok(())
    })?;
    Ok(layout_json(s, bin, f64_p(p, "width").unwrap_or(800.0) as f32))
}

fn freeform_stack(s: &mut Session, p: &Value, on: bool) -> Result<Value> {
    let items = items_p(s, p);
    let cmd = if on { "project.freeform.stack" } else { "project.freeform.unstack" };
    if items.is_empty() || (on && items.len() < 2) {
        return Err(bad(cmd, if on { "select two or more items to stack" } else { "need `items` (or a Project panel selection)" }));
    }
    let width = f64_p(p, "width").unwrap_or(800.0) as f32;
    let opts = s.prefs.project_panel.freeform.clone();
    let bin = s.project.root.parent_of(items[0]);
    // unstacking a stack's first card releases the whole stack
    let mut all = items.clone();
    if !on {
        for i in &items {
            for it in s.project.items.values() {
                if it.metadata.get(FREEFORM_STACK).and_then(|v| v.parse::<u64>().ok()) == Some(i.0) && !all.contains(&it.id) {
                    all.push(it.id);
                }
            }
        }
    }
    s.edit(if on { "Stack Clips" } else { "Unstack Clips" }, |pr, _| {
        // keep cards where they are shown before changing the stacking
        pin_all(pr, bin, &opts, width, |c| (c.x, c.y, (c.size != opts.card_size).then_some(c.size)));
        for i in &all {
            let it = pr.item_mut(*i).ok_or_else(|| bad(cmd, "no such item"))?;
            if on {
                it.metadata.insert(FREEFORM_STACK.into(), items[0].0.to_string());
            } else {
                it.metadata.remove(FREEFORM_STACK);
            }
        }
        Ok(())
    })?;
    if !on {
        // spread released cards a little so they don't sit on each other
        let n = all.len();
        if n > 1 {
            let cards = freeform_layout(&s.project, bin, &opts, width);
            let base = cards.iter().find(|c| c.item == all[0].0).map(|c| (c.x, c.y)).unwrap_or((12.0, 12.0));
            let step = opts.card_size + 24.0;
            let positions: serde_json::Map<String, Value> =
                all.iter().enumerate().map(|(k, i)| (i.0.to_string(), json!([base.0 + k as f32 * step, base.1]))).collect();
            // part of the same gesture: fold into the unstack undo step
            let before = s.history.undo.len();
            freeform_move(s, &json!({"positions": positions, "snap": false, "width": width}))?;
            if s.history.undo.len() > before {
                s.history.undo.pop();
            }
        }
    }
    Ok(layout_json(s, bin, width))
}

fn arrangement_names(p: &Project, bin: Option<BinId>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for i in freeform_items(p, bin) {
        if let Some(it) = p.item(i) {
            for k in it.metadata.keys() {
                if let Some(n) = k.strip_prefix(ARRANGEMENT_PREFIX)
                    && !names.iter().any(|x| x == n)
                {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

fn arrangement_save(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("project.freeform.saveArrangement", "need `name`"))?.to_string();
    let bin = bin_p(p);
    let opts = s.prefs.project_panel.freeform.clone();
    let cards = freeform_layout(&s.project, bin, &opts, f64_p(p, "width").unwrap_or(800.0) as f32);
    let key = format!("{ARRANGEMENT_PREFIX}{name}");
    s.edit("Save Arrangement", |pr, _| {
        for c in &cards {
            if let Some(it) = pr.item_mut(ItemId(c.item)) {
                it.metadata.insert(key.clone(), fmt_pos(c.x, c.y, Some(c.size)));
            }
        }
        Ok(())
    })?;
    Ok(json!({"name": name, "arrangements": arrangement_names(&s.project, bin)}))
}

fn arrangement_restore(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").ok_or_else(|| bad("project.freeform.restoreArrangement", "need `name`"))?.to_string();
    let bin = bin_p(p);
    if !arrangement_names(&s.project, bin).contains(&name) {
        return Err(bad("project.freeform.restoreArrangement", format!("no arrangement named `{name}`")));
    }
    let key = format!("{ARRANGEMENT_PREFIX}{name}");
    let ids = freeform_items(&s.project, bin);
    s.edit("Restore Arrangement", |pr, _| {
        for i in &ids {
            if let Some(it) = pr.item_mut(*i) {
                match it.metadata.get(&key).cloned() {
                    Some(v) => {
                        it.metadata.insert(FREEFORM_POS.into(), v);
                    }
                    None => {
                        it.metadata.remove(FREEFORM_POS);
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(layout_json(s, bin, f64_p(p, "width").unwrap_or(800.0) as f32))
}

fn arrangement_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").ok_or_else(|| bad("project.freeform.deleteArrangement", "need `name`"))?.to_string();
    let bin = bin_p(p);
    let key = format!("{ARRANGEMENT_PREFIX}{name}");
    let ids = freeform_items(&s.project, bin);
    s.edit("Delete Arrangement", |pr, _| {
        for i in &ids {
            if let Some(it) = pr.item_mut(*i) {
                it.metadata.remove(&key);
            }
        }
        Ok(())
    })?;
    Ok(json!({"arrangements": arrangement_names(&s.project, bin)}))
}

fn freeform_options(s: &mut Session, p: &Value) -> Result<Value> {
    let mut prefs = s.prefs.clone();
    let o = &mut prefs.project_panel.freeform;
    if let Some(v) = f64_p(p, "grid") {
        o.grid = (v as f32).clamp(4.0, 200.0);
    }
    if let Some(v) = bool_p(p, "snap") {
        o.snap = v;
    }
    if let Some(v) = bool_p(p, "showNames") {
        o.show_names = v;
    }
    if let Some(v) = bool_p(p, "showDurations") {
        o.show_durations = v;
    }
    if let Some(v) = f64_p(p, "cardSize") {
        o.card_size = (v as f32).clamp(48.0, 480.0);
    }
    let out = serde_json::to_value(&prefs.project_panel.freeform).unwrap_or_default();
    save_prefs(s, prefs)?;
    Ok(out)
}

// ------------------------------------------------------------------------------------ view

fn save_prefs(s: &mut Session, prefs: crate::autosave::Preferences) -> Result<()> {
    if prefs != s.prefs {
        s.set_prefs(prefs).map_err(|e| crate::EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(())
}

fn view_json(s: &Session) -> Value {
    let pp = &s.prefs.project_panel;
    json!({
        "view": pp.view,
        "previewArea": pp.preview_area,
        "thumbnails": pp.thumbnails,
        "thumbnailsShowEffects": pp.thumbnails_show_effects,
        "hoverScrub": pp.hover_scrub,
        "thumbnailControlsAllDevices": pp.thumbnail_controls_all_devices,
        "currentPreset": pp.current_preset.map(|i| i + 1),
        "freeform": pp.freeform,
        "available": all_columns(&s.project),
    })
}

fn view_set(s: &mut Session, p: &Value) -> Result<Value> {
    let mut prefs = s.prefs.clone();
    let pp = &mut prefs.project_panel;
    if let Some(v) = str_p(p, "view") {
        pp.view.mode = ViewMode::from_name(v).ok_or_else(|| bad("project.view.set", format!("unknown view `{v}` (list, icon, freeform)")))?;
    }
    if let Some(v) = f64_p(p, "iconSize") {
        pp.view.icon_size = (v as f32).clamp(48.0, 400.0);
    }
    if let Some(v) = str_p(p, "fontSize") {
        pp.view.font_size = FontSize::from_name(v).ok_or_else(|| bad("project.view.set", format!("unknown font size `{v}`")))?;
    }
    for (k, f) in [
        ("previewArea", &mut pp.preview_area as &mut bool),
        ("thumbnails", &mut pp.thumbnails),
        ("thumbnailsShowEffects", &mut pp.thumbnails_show_effects),
        ("hoverScrub", &mut pp.hover_scrub),
        ("thumbnailControlsAllDevices", &mut pp.thumbnail_controls_all_devices),
    ] {
        if let Some(v) = bool_p(p, k) {
            *f = v;
        }
    }
    if let Some(v) = p.get("iconSort") {
        match v {
            Value::String(c) => pp.view.icon_sort.column = canonical_column(&s.project, c).unwrap_or_default(),
            Value::Object(_) => {
                if let Some(c) = str_p(v, "column") {
                    pp.view.icon_sort.column = canonical_column(&s.project, c).unwrap_or_default();
                }
                if let Some(d) = bool_p(v, "descending") {
                    pp.view.icon_sort.descending = d;
                }
            }
            _ => {}
        }
    }
    save_prefs(s, prefs)?;
    Ok(view_json(s))
}

/// The column's canonical spelling, or None for an unknown column ("" / "User Order" = None).
fn canonical_column(p: &Project, c: &str) -> Option<String> {
    all_columns(p).into_iter().find(|x| x.eq_ignore_ascii_case(c.trim()))
}

fn columns_set(s: &mut Session, p: &Value) -> Result<Value> {
    let arr = p.get("columns").and_then(Value::as_array).ok_or_else(|| bad("project.columns.set", "need `columns`: [name | {name, width}]"))?;
    let mut cols: Vec<Column> = Vec::new();
    for v in arr {
        let (name, width) = match v {
            Value::String(n) => (n.clone(), None),
            Value::Object(_) => (str_p(v, "name").unwrap_or_default().to_string(), f64_p(v, "width").map(|w| w as f32)),
            _ => return Err(bad("project.columns.set", "columns are names or {name, width}")),
        };
        // custom fields may be added before any item has them
        let name = canonical_column(&s.project, &name).unwrap_or_else(|| name.trim().to_string());
        if name.is_empty() {
            return Err(bad("project.columns.set", "empty column name"));
        }
        if cols.iter().any(|c| c.name == name) {
            continue;
        }
        let keep = s.prefs.project_panel.view.columns.iter().find(|c| c.name == name).map(|c| c.width);
        cols.push(Column { width: width.or(keep).unwrap_or_else(|| default_width(&name)).clamp(30.0, 800.0), name });
    }
    // Name is always shown, first
    if let Some(i) = cols.iter().position(|c| c.name == "Name") {
        let n = cols.remove(i);
        cols.insert(0, n);
    } else {
        let w = s.prefs.project_panel.view.columns.iter().find(|c| c.name == "Name").map(|c| c.width).unwrap_or(default_width("Name"));
        cols.insert(0, Column { name: "Name".into(), width: w });
    }
    let mut prefs = s.prefs.clone();
    let sort = &mut prefs.project_panel.view.sort;
    if !cols.iter().any(|c| c.name == sort.column) {
        *sort = SortSpec { column: "Name".into(), descending: false };
    }
    prefs.project_panel.view.columns = cols;
    save_prefs(s, prefs)?;
    Ok(json!({"columns": s.prefs.project_panel.view.columns}))
}

fn columns_resize(s: &mut Session, p: &Value) -> Result<Value> {
    let c = str_p(p, "column").ok_or_else(|| bad("project.columns.resize", "need `column`"))?;
    let w = f64_p(p, "width").ok_or_else(|| bad("project.columns.resize", "need `width`"))? as f32;
    let mut prefs = s.prefs.clone();
    let col = prefs
        .project_panel
        .view
        .columns
        .iter_mut()
        .find(|x| x.name.eq_ignore_ascii_case(c))
        .ok_or_else(|| bad("project.columns.resize", format!("`{c}` is not shown")))?;
    col.width = w.clamp(30.0, 800.0);
    save_prefs(s, prefs)?;
    Ok(json!({"columns": s.prefs.project_panel.view.columns}))
}

fn sort(s: &mut Session, p: &Value) -> Result<Value> {
    let c = str_p(p, "column").ok_or_else(|| bad("project.sort", "need `column`"))?;
    let c = canonical_column(&s.project, c).ok_or_else(|| bad("project.sort", format!("unknown column `{c}`")))?;
    let mut prefs = s.prefs.clone();
    let cur = &mut prefs.project_panel.view.sort;
    let desc = bool_p(p, "descending").unwrap_or(cur.column == c && !cur.descending);
    *cur = SortSpec { column: c, descending: desc };
    save_prefs(s, prefs)?;
    Ok(json!({"sort": s.prefs.project_panel.view.sort}))
}

/// `project.items {bin?, recursive?}`: the rows of a bin as the List view shows them.
fn items(s: &mut Session, p: &Value) -> Result<Value> {
    let pr = &s.project;
    let bin = match bin_p(p) {
        Some(b) => pr.root.find_bin(b).ok_or_else(|| bad("project.items", "no such bin"))?,
        None => &pr.root,
    };
    let view = &s.prefs.project_panel.view;
    let recursive = bool_p(p, "recursive").unwrap_or(false);
    let mut rows = Vec::new();
    fn walk(pr: &Project, b: &Bin, view: &ViewSettings, depth: usize, recursive: bool, rows: &mut Vec<Value>) {
        let (bins, items) = bin_children(pr, b, &view.sort);
        for bid in bins {
            let Some(sub) = pr.root.find_bin(bid) else { continue };
            rows.push(json!({"bin": bid.0, "name": sub.name, "depth": depth}));
            if recursive {
                walk(pr, sub, view, depth + 1, recursive, rows);
            }
        }
        for id in items {
            let Some(it) = pr.item(id) else { continue };
            let cells: serde_json::Map<String, Value> = view.columns.iter().map(|c| (c.name.clone(), json!(cell(pr, it, &c.name).0))).collect();
            rows.push(json!({"item": id.0, "name": it.name, "label": it.label.name(), "depth": depth, "cells": cells}));
        }
    }
    walk(pr, bin, view, 0, recursive, &mut rows);
    Ok(json!({"bin": bin.id.0, "columns": view.columns, "sort": view.sort, "rows": rows}))
}

// ------------------------------------------------------------------------------------ presets

fn slot_p(p: &Value, cmd: &str) -> Result<usize> {
    let n = u64_p(p, "slot").ok_or_else(|| bad(cmd, "need `slot` (1–10)"))? as usize;
    if !(1..=PRESET_SLOTS).contains(&n) {
        return Err(bad(cmd, "`slot` is 1–10"));
    }
    Ok(n - 1)
}

fn presets_json(s: &Session) -> Value {
    let pp = &s.prefs.project_panel;
    let slots: Vec<Value> = (0..PRESET_SLOTS)
        .map(|i| match pp.presets.get(i).cloned().flatten() {
            Some(pr) => json!({"slot": i + 1, "name": pr.name, "settings": pr.settings}),
            None => json!({"slot": i + 1, "name": null}),
        })
        .collect();
    json!({"presets": slots, "current": pp.current_preset.map(|i| i + 1)})
}

fn ensure_slots(pp: &mut ProjectPanelPrefs) {
    pp.presets.resize(PRESET_SLOTS, None);
}

fn preset_save(s: &mut Session, p: &Value, as_new: bool) -> Result<Value> {
    let cmd = if as_new { "project.viewPreset.saveAs" } else { "project.viewPreset.save" };
    let mut prefs = s.prefs.clone();
    let pp = &mut prefs.project_panel;
    ensure_slots(pp);
    let slot = if as_new {
        match u64_p(p, "slot") {
            Some(_) => slot_p(p, cmd)?,
            None => pp
                .presets
                .iter()
                .position(Option::is_none)
                .ok_or_else(|| bad(cmd, "all 10 view preset slots are used: delete one in Manage Saved View Presets"))?,
        }
    } else {
        match u64_p(p, "slot") {
            Some(_) => slot_p(p, cmd)?,
            None => pp.current_preset.ok_or_else(|| bad(cmd, "no view preset is current: use Save As New View Preset"))?,
        }
    };
    let name = str_p(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| pp.presets[slot].as_ref().map(|x| x.name.clone()))
        .unwrap_or_else(|| format!("Project View Preset {}", slot + 1));
    pp.presets[slot] = Some(ViewPreset { name, settings: pp.view.clone() });
    pp.current_preset = Some(slot);
    save_prefs(s, prefs)?;
    Ok(presets_json(s))
}

fn preset_restore(s: &mut Session, p: &Value) -> Result<Value> {
    let slot = slot_p(p, "project.viewPreset.restore")?;
    let mut prefs = s.prefs.clone();
    let pp = &mut prefs.project_panel;
    ensure_slots(pp);
    let pr = pp.presets[slot].clone().ok_or_else(|| bad("project.viewPreset.restore", format!("view preset {} is empty", slot + 1)))?;
    pp.view = pr.settings;
    pp.current_preset = Some(slot);
    save_prefs(s, prefs)?;
    Ok(view_json(s))
}

fn preset_rename(s: &mut Session, p: &Value) -> Result<Value> {
    let slot = slot_p(p, "project.viewPreset.rename")?;
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("project.viewPreset.rename", "need `name`"))?;
    let mut prefs = s.prefs.clone();
    ensure_slots(&mut prefs.project_panel);
    let pr = prefs.project_panel.presets[slot].as_mut().ok_or_else(|| bad("project.viewPreset.rename", "that slot is empty"))?;
    pr.name = name.to_string();
    save_prefs(s, prefs)?;
    Ok(presets_json(s))
}

fn preset_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let slot = slot_p(p, "project.viewPreset.delete")?;
    let mut prefs = s.prefs.clone();
    let pp = &mut prefs.project_panel;
    ensure_slots(pp);
    pp.presets[slot] = None;
    if pp.current_preset == Some(slot) {
        pp.current_preset = None;
    }
    save_prefs(s, prefs)?;
    Ok(presets_json(s))
}

fn has_current_preset(s: &Session) -> std::result::Result<(), String> {
    match s.prefs.project_panel.current_preset {
        Some(i) if s.prefs.project_panel.presets.get(i).is_some_and(Option::is_some) => Ok(()),
        _ => Err("no view preset is current".into()),
    }
}

fn has_presets(s: &Session) -> std::result::Result<(), String> {
    if s.prefs.project_panel.presets.iter().any(Option::is_some) { Ok(()) } else { Err("no view presets are saved".into()) }
}

fn rename_bin(s: &mut Session, p: &Value) -> Result<Value> {
    let bin = bin_p(p).ok_or_else(|| bad("project.renameBin", "need `bin`"))?;
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("project.renameBin", "need a non-empty `name`"))?.to_string();
    if bin == s.project.root.id {
        return Err(bad("project.renameBin", "the project's root bin is named after the project"));
    }
    s.edit("Rename Bin", |pr, _| {
        pr.root.find_bin_mut(bin).ok_or_else(|| bad("project.renameBin", "no such bin"))?.name = name.clone();
        Ok(())
    })?;
    Ok(json!({"bin": bin.0, "name": name}))
}

// ------------------------------------------------------------------------------------ registry

pub(crate) fn commands() -> Vec<CommandSpec> {
    let c = |id, label, params, enabled, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal: true };
    let q = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false };
    vec![
        q("project.view.get", "Project Panel View", "{}", |s, _| Ok(view_json(s))),
        c(
            "project.view.set",
            "Set Project Panel View",
            r#"{"view":"list|icon|freeform"?,"iconSize":f32?,"fontSize":"small|medium|large|extraLarge"?,"previewArea":bool?,"thumbnails":bool?,"thumbnailsShowEffects":bool?,"hoverScrub":bool?,"thumbnailControlsAllDevices":bool?,"iconSort":column|{"column":str,"descending":bool}?}"#,
            always,
            view_set,
        ),
        q("project.columns.list", "List Project Columns", "{}", |s, _| {
            let shown = &s.prefs.project_panel.view.columns;
            let all: Vec<Value> = all_columns(&s.project).into_iter().map(|n| json!({"name": n, "shown": shown.iter().any(|c| c.name == n)})).collect();
            Ok(json!({"columns": shown, "available": all}))
        }),
        c("project.columns.set", "Metadata Display", r#"{"columns":[name|{"name":str,"width":f32}]}"#, always, columns_set),
        c("project.columns.resize", "Resize Column", r#"{"column":str,"width":f32}"#, always, columns_resize),
        c("project.sort", "Sort Project Items", r#"{"column":str,"descending":bool?}"#, always, sort),
        q("project.items", "List Project Panel Rows", r#"{"bin":binId?,"recursive":bool?}"#, items),
        q("project.viewPreset.list", "List View Presets", "{}", |s, _| Ok(presets_json(s))),
        c("project.viewPreset.save", "Save Current View Preset", r#"{"slot":1..10?,"name":str?}"#, has_current_preset, |s, p| preset_save(s, p, false)),
        c("project.viewPreset.saveAs", "Save As New View Preset", r#"{"name":str?,"slot":1..10?}"#, always, |s, p| preset_save(s, p, true)),
        c("project.viewPreset.restore", "Restore View Preset", r#"{"slot":1..10}"#, has_presets, preset_restore),
        c("project.viewPreset.rename", "Rename View Preset", r#"{"slot":1..10,"name":str}"#, has_presets, preset_rename),
        c("project.viewPreset.delete", "Delete View Preset", r#"{"slot":1..10}"#, has_presets, preset_delete),
        q("project.freeform.layout", "Freeform Layout", r#"{"bin":binId?,"width":f32?}"#, |s, p| {
            Ok(layout_json(s, bin_p(p), f64_p(p, "width").unwrap_or(800.0) as f32))
        }),
        c("project.freeform.move", "Move Clip Cards", r#"{"items":[id]?,"x":f32,"y":f32,"snap":bool?} | {"positions":{"<id>":[x,y]}}"#, always, freeform_move),
        c("project.freeform.resize", "Clip Size", r#"{"items":[id]?,"size":f32?,"step":1|-1?}"#, always, freeform_resize),
        c("project.freeform.alignToGrid", "Align to Grid", r#"{"bin":binId?,"grid":f32?}"#, always, freeform_align),
        c("project.freeform.reset", "Reset to Grid", r#"{"bin":binId?}"#, always, freeform_reset),
        c("project.freeform.stack", "Stack Clips", r#"{"items":[id]?}"#, always, |s, p| freeform_stack(s, p, true)),
        c("project.freeform.unstack", "Unstack Clips", r#"{"items":[id]?}"#, always, |s, p| freeform_stack(s, p, false)),
        c("project.freeform.saveArrangement", "Save Arrangement", r#"{"name":str,"bin":binId?}"#, always, arrangement_save),
        c("project.freeform.restoreArrangement", "Restore Arrangement", r#"{"name":str,"bin":binId?}"#, always, arrangement_restore),
        c("project.freeform.deleteArrangement", "Delete Arrangement", r#"{"name":str,"bin":binId?}"#, always, arrangement_delete),
        q("project.freeform.arrangements", "List Arrangements", r#"{"bin":binId?}"#, |s, p| {
            Ok(json!({"arrangements": arrangement_names(&s.project, bin_p(p))}))
        }),
        c(
            "project.freeform.options",
            "Freeform View Options…",
            r#"{"grid":f32?,"snap":bool?,"showNames":bool?,"showDurations":bool?,"cardSize":f32?}"#,
            always,
            freeform_options,
        ),
        c("project.renameBin", "Rename Bin", r#"{"bin":binId,"name":str}"#, always, rename_bin),
        c("project.selectAll", "Select All Project Items", "{}", always, |s, _| {
            s.state.project_selection = s.project.items.keys().copied().collect();
            Ok(json!({"selected": s.state.project_selection.len()}))
        }),
        c("project.deselectAll", "Deselect All Project Items", "{}", always, |s, _| {
            s.state.project_selection.clear();
            Ok(Value::Null)
        }),
    ]
}

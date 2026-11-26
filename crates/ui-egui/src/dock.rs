//! FilmCraft's panels and workspaces. craft-ui's docking owns the frame tree, the tab strips,
//! floating windows and gestures (`dock_ui.rs` draws it); this module names the
//! panels, builds the built-in workspaces and draws the Timeline's sequence tabs.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};

use craft_behavior::dock::SplitSize;
use craft_behavior::dock::{Action, Floating, Node};
use craft_behavior::flow::Axis;

use crate::icons::{self, Icon};
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelKind {
    Project,
    MediaBrowser,
    Libraries,
    Info,
    Effects,
    Markers,
    History,
    Source,
    EffectControls,
    AudioClipMixer,
    Metadata,
    Program,
    Timeline,
    Tools,
    AudioMeters,
    AudioTrackMixer,
    LumetriColor,
    LumetriScopes,
    EssentialGraphics,
    EssentialSound,
    TextToSpeech,
    Properties,
    Text,
    Events,
    Progress,
    ReferenceMonitor,
    Timecode,
    ProjectNotes,
}

impl PanelKind {
    pub const ALL: [PanelKind; 28] = [
        PanelKind::Project,
        PanelKind::MediaBrowser,
        PanelKind::Libraries,
        PanelKind::Info,
        PanelKind::Effects,
        PanelKind::Markers,
        PanelKind::History,
        PanelKind::Source,
        PanelKind::EffectControls,
        PanelKind::AudioClipMixer,
        PanelKind::Metadata,
        PanelKind::Program,
        PanelKind::Timeline,
        PanelKind::Tools,
        PanelKind::AudioMeters,
        PanelKind::AudioTrackMixer,
        PanelKind::LumetriColor,
        PanelKind::LumetriScopes,
        PanelKind::EssentialGraphics,
        PanelKind::EssentialSound,
        PanelKind::TextToSpeech,
        PanelKind::Properties,
        PanelKind::Text,
        PanelKind::Events,
        PanelKind::Progress,
        PanelKind::ReferenceMonitor,
        PanelKind::Timecode,
        PanelKind::ProjectNotes,
    ];
    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Project => "Project",
            PanelKind::MediaBrowser => "Media Browser",
            PanelKind::Libraries => "Libraries",
            PanelKind::Info => "Info",
            PanelKind::Effects => "Effects",
            PanelKind::Markers => "Markers",
            PanelKind::History => "History",
            PanelKind::Source => "Source",
            PanelKind::EffectControls => "Effect Controls",
            PanelKind::AudioClipMixer => "Audio Clip Mixer",
            PanelKind::Metadata => "Metadata",
            PanelKind::Program => "Program",
            PanelKind::Timeline => "Timeline",
            PanelKind::Tools => "Tools",
            PanelKind::AudioMeters => "Audio Meters",
            PanelKind::AudioTrackMixer => "Audio Track Mixer",
            PanelKind::LumetriColor => "Color Grading",
            PanelKind::LumetriScopes => "Video Scopes",
            PanelKind::EssentialGraphics => "Essential Graphics",
            PanelKind::EssentialSound => "Essential Sound",
            PanelKind::TextToSpeech => "Text to Speech",
            PanelKind::Properties => "Properties",
            PanelKind::Text => "Text",
            PanelKind::Events => "Events",
            PanelKind::Progress => "Progress",
            PanelKind::ReferenceMonitor => "Reference Monitor",
            PanelKind::Timecode => "Timecode",
            PanelKind::ProjectNotes => "Project Notes",
        }
    }
    pub fn id(self) -> String {
        format!("{self:?}")
    }
    pub fn from_name(s: &str) -> Option<PanelKind> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        if n == "colorgrade" || n == "lumetricolor" {
            return Some(PanelKind::LumetriColor);
        }
        if n == "videoscopes" || n == "lumetriscopes" {
            return Some(PanelKind::LumetriScopes);
        }
        Self::ALL.iter().copied().find(|p| format!("{p:?}").to_ascii_lowercase() == n || p.title().to_ascii_lowercase().replace(' ', "") == n)
    }
    /// Window-menu shortcut (Premiere: Shift+1..9).
    pub fn window_shortcut(self) -> Option<&'static str> {
        match self {
            PanelKind::Project => Some("Shift+1"),
            PanelKind::Source => Some("Shift+2"),
            PanelKind::Timeline => Some("Shift+3"),
            PanelKind::Program => Some("Shift+4"),
            PanelKind::EffectControls => Some("Shift+5"),
            PanelKind::AudioClipMixer => Some("Shift+9"),
            PanelKind::Effects => Some("Shift+7"),
            PanelKind::MediaBrowser => Some("Shift+8"),
            PanelKind::AudioTrackMixer => Some("Shift+6"),
            _ => None,
        }
    }
}

/// A FilmCraft panel arrangement: craft-ui's frame tree and floating windows.
pub type Layout = craft_behavior::dock::Layout<PanelKind>;
/// What a saved workspace stores: its layout, closed panels and lock.
pub type Workspace = craft_behavior::dock::Workspace<PanelKind>;

/// The rect saved as `[x, y, width, height]`.
pub fn rect_of([x, y, width, height]: [f32; 4]) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(width, height))
}

/// `rect` as `[x, y, width, height]`, the form FilmCraft's own files keep.
pub fn array_of(rect: Rect) -> [f32; 4] {
    [rect.min.x, rect.min.y, rect.width(), rect.height()]
}

fn tabs(p: &[PanelKind], active: usize) -> Node<PanelKind> {
    Node::tabs(p.to_vec(), active)
}
fn hsplit(size: SplitSize, a: Node<PanelKind>, b: Node<PanelKind>) -> Node<PanelKind> {
    Node::split(Axis::X, size, a, b)
}
fn vsplit(size: SplitSize, a: Node<PanelKind>, b: Node<PanelKind>) -> Node<PanelKind> {
    Node::split(Axis::Y, size, a, b)
}

pub const WORKSPACES: [&str; 10] =
    ["Editing", "Assembly", "Color", "Effects", "Audio", "Captions and Graphics", "Vertical", "Learning", "Review", "All Panels"];

/// The user's workspaces file, beside `preferences.json` in the data directory.
pub const WORKSPACES_FILE: &str = "workspaces.json";

/// Window ▸ Workspaces: the user's own workspaces and saved changes to the built-in ones, and the
/// workspace in use (reopened at launch).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspacePrefs {
    pub saved: Vec<SavedWorkspace>,
    pub current: String,
    /// The layout in use when the app last closed (`name` is the workspace it belongs to): panel
    /// changes made without Save Changes to this Workspace come back at the next launch, as in
    /// Premiere (#2421). Reset to Saved Layout still returns to the saved layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<SavedWorkspace>,
}

/// A named workspace. Each representation stays as JSON so one unreadable entry does not
/// lose the rest of the preferences. `workspace` is readable by the previous release, with
/// the complete new layout in a nested `filmcraftV4` field the previous preferences writer
/// carries through untouched entries. Older releases still read `layout` and `floating`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedWorkspace {
    pub name: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub workspace: serde_json::Value,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub layout: serde_json::Value,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub floating: serde_json::Value,
}

impl SavedWorkspace {
    pub fn new(name: &str, layout: &Layout) -> Result<Self, String> {
        let current = Workspace::new(layout.clone());
        current.validate().map_err(|e| e.to_string())?;
        let full = serde_json::to_value(&current).map_err(|e| e.to_string())?;
        let workspace = workspace_compat::write(layout, full);
        let (layout, floating) = legacy::write(layout).unwrap_or_default();
        Ok(SavedWorkspace { name: name.to_string(), workspace, layout, floating })
    }
}

impl WorkspacePrefs {
    const MAX_BYTES: u64 = 4 * 1024 * 1024;

    pub fn load(path: &std::path::Path) -> Self {
        use std::io::Read as _;
        // Preferences are untrusted; bound reading before JSON can allocate from it.
        let Some(bytes) = std::fs::File::open(path).ok().and_then(|file| {
            let mut bytes = Vec::new();
            file.take(Self::MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
            (bytes.len() as u64 <= Self::MAX_BYTES).then_some(bytes)
        }) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        // Compatibility copies make the file bigger. Do not replace a readable file
        // with one we would reject at the next launch.
        if bytes.len() as u64 > Self::MAX_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "workspace preferences exceed the size limit"));
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)
    }
}

/// A workspace's command-id key: `Captions and Graphics` → `captionsandgraphics`.
pub fn slug(name: &str) -> String {
    name.to_ascii_lowercase().replace(' ', "")
}

pub fn is_builtin(name: &str) -> bool {
    WORKSPACES.contains(&name)
}

/// Every workspace, built-in and the user's own, in one alphabetical list (Premiere's Window ▸
/// Workspaces).
pub fn names(prefs: &WorkspacePrefs) -> Vec<String> {
    let mut v: Vec<String> = WORKSPACES.iter().map(|s| s.to_string()).collect();
    for s in &prefs.saved {
        if !v.contains(&s.name) {
            v.push(s.name.clone());
        }
    }
    v.sort_by_key(|n| n.to_lowercase());
    v
}

/// The workspace called `query` (any case, spaces optional).
pub fn find(prefs: &WorkspacePrefs, query: &str) -> Option<String> {
    names(prefs).into_iter().find(|n| slug(n) == slug(query))
}

/// A workspace as saved: the user's saved version, else the built-in default. An entry that
/// does not read or validate falls back to the default and stays in the file untouched.
pub fn saved_workspace(prefs: &WorkspacePrefs, name: &str) -> Workspace {
    prefs.saved.iter().find(|saved| saved.name == name).and_then(read_saved).unwrap_or_else(|| Workspace::new(workspace(name)))
}

/// A saved entry's workspace, or `None` when it does not read back or validate (an entry a newer
/// version wrote, or a damaged file): the caller falls back.
pub fn read_saved(saved: &SavedWorkspace) -> Option<Workspace> {
    let workspace = if saved.workspace.is_null() {
        Workspace::new(legacy::layout(&saved.layout, &saved.floating)?)
    } else if let Some(full) = saved.workspace.get("filmcraftV4") {
        serde_json::from_value::<Workspace>(full.clone()).ok()?
    } else {
        workspace_compat::read(saved.workspace.clone())?
    };
    workspace.validate().ok()?;
    Some(workspace)
}

/// The saved layout of a workspace (see [`saved_workspace`]).
pub fn saved_docking(prefs: &WorkspacePrefs, name: &str) -> Layout {
    saved_workspace(prefs, name).layout().clone()
}

/// Wire differences with the craft-ui v2 workspace used by the previous desktop app.
mod workspace_compat {
    use super::{Layout, Workspace, array_of};
    use serde::Deserialize;
    use serde_json::{Value, json};

    /// Make the old root readable: old splits use the original axis names, and the old dock expects
    /// a concrete width. The old and new tab/stack fields agree.
    fn old_root(node: &mut Value) {
        if let Some(split) = node.get_mut("Split") {
            if let Some(axis) = split.get_mut("axis") {
                match axis.as_str() {
                    Some("X") => *axis = json!("Horizontal"),
                    Some("Y") => *axis = json!("Vertical"),
                    _ => {}
                }
            }
            old_root(&mut split["first"]);
            old_root(&mut split["second"]);
        } else if let Some(columns) = node.get_mut("Dock").and_then(|dock| dock.get_mut("columns")).and_then(Value::as_array_mut) {
            for column in columns {
                if column["width"].is_null() {
                    // craft-ui 33be3562 DEFAULT_COLUMN_WIDTH, used for a new dock.
                    column["width"] = json!(240.0);
                }
            }
        }
    }

    pub(super) fn write(layout: &Layout, current: Value) -> Value {
        let mut root = current["layout"]["root"].clone();
        old_root(&mut root);
        let floating: Vec<_> = layout
            .floating
            .iter()
            .map(|window| {
                // The previous reader already accepts groups and reconstructs its own vertical
                // tree. It has no floating column rail, per-group minimized state or edge hint.
                let mut old = json!({"id": window.id, "groups": window.groups(), "rect": array_of(window.rect),
                "minimized": window.column.iconic});
                if let Some(p) = window.native.as_ref() {
                    if let Some([x, y]) = p.position() {
                        let [w, h] = p.normal.size;
                        old["native"] = json!([x, y, w, h]);
                    }
                    // The new monitor key has its dimensions and name but no desktop origin.
                    // Give the previous reader the size fallback, not a fabricated monitor rect.
                    if let Some(monitor) = p.monitor.as_ref() {
                        old["monitor_size"] = json!(monitor.size_px.map(|v| v as f32 / p.scale));
                    }
                }
                old
            })
            .collect();
        json!({"version": 2, "layout": {"root": root, "floating": floating}, "hidden": [], "filmcraftV4": current})
    }

    #[derive(Deserialize)]
    struct OldMonitor {
        name: Option<String>,
        rect: [f32; 4],
        scale: f32,
    }

    /// Carry over the logical content size, and use the old location as a desktop placement
    /// hint where a saved monitor tells us its scale. Old files do not store a native frame
    /// offset or a scale when only `monitor_size` was available.
    fn old_native(window: &Value) -> Option<Value> {
        let [x, y, w, h]: [f32; 4] = serde_json::from_value(window.get("native")?.clone()).ok()?;
        if ![x, y, w, h].iter().all(|v| v.is_finite()) || !(w > 0.0 && h > 0.0) {
            return None;
        }
        let monitor: Option<OldMonitor> = serde_json::from_value(window["monitor"].clone()).ok();
        let monitor = monitor.filter(|m| m.scale.is_finite() && m.scale > 0.0 && m.scale <= 16.0);
        let position = monitor.as_ref().and_then(|m| {
            // Bound before conversion, including hostile values near the signed pixel limit.
            let [px, py] = [x, y].map(|v| f64::from(v) * f64::from(m.scale));
            let fits = |v: f64| (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&v);
            (fits(px) && fits(py)).then(|| [px.round() as i32, py.round() as i32])
        });
        let key = monitor.as_ref().and_then(|m| {
            let [_, _, width, height] = m.rect;
            let size = [width, height].map(|v| f64::from(v) * f64::from(m.scale));
            let name = m.name.as_ref()?;
            size.iter().all(|v| (1.0..=f64::from(u32::MAX)).contains(v)).then(|| json!({"name": name, "size_px": size.map(|v| v.round() as u32)}))
        });
        Some(json!({"normal": {"pos": position.map_or([0.0, 0.0], |_| [x, y]), "size": [w, h]},
            "position_px": position, "scale": monitor.as_ref().map_or(1.0, |m| m.scale), "monitor": key}))
    }

    // A fixed vertical split between two tab groups has a directly representable
    // preferred height on its fixed child. Ratios (and nested split subtrees) have
    // no equivalent to the old divider proportions in next's one-column model.
    fn fixed_groups(root: &Value, minimized: bool) -> Option<Value> {
        let split = root.get("Split")?;
        if !matches!(split["axis"].as_str(), Some("Vertical" | "vertical")) {
            return None;
        }
        let first = split["size"].get("FixedFirst");
        let second = split["size"].get("FixedSecond");
        if first.is_none() && second.is_none() {
            return None;
        }
        let group = |child: &str, height: Option<&Value>| {
            let tab = split[child].get("Tabs")?;
            Some(json!({"panels": tab["panels"], "active": tab["active"], "height": height, "minimized": minimized}))
        };
        Some(json!([group("first", first)?, group("second", second)?]))
    }

    pub(super) fn read(mut old: Value) -> Option<Workspace> {
        if let Some(windows) = old.get_mut("layout").and_then(|layout| layout.get_mut("floating")).and_then(Value::as_array_mut) {
            for window in windows {
                if window["groups"].is_null()
                    && window["panels"].is_null()
                    && let Some(groups) = fixed_groups(&window["root"], window["minimized"].as_bool().unwrap_or_default())
                    && let Some(window) = window.as_object_mut()
                {
                    window.remove("root");
                    window.insert("groups".into(), groups);
                }
                if let Some(native) = old_native(window) {
                    // A side-by-side tree opens as several windows. Let the runtime place
                    // those from their individual layout rects rather than overlapping them
                    // all at an identical native position.
                    if !matches!(window["root"]["Split"]["axis"].as_str(), Some("Horizontal" | "horizontal" | "X")) {
                        window["native"] = native;
                    }
                }
            }
        }
        serde_json::from_value(old).ok()
    }
}

/// The frame tree and floating groups FilmCraft wrote before its workspaces were craft-ui
/// [`Workspace`]s.
mod legacy {
    use super::{Axis, Floating, Layout, Node, PanelKind, SplitSize, array_of, rect_of};
    use serde::{Deserialize, Serialize};

    const MAX_DEPTH: usize = 32;

    #[derive(Deserialize, Serialize)]
    enum Tree {
        /// `vertical`: children stacked top/bottom; else side by side.
        Split {
            vertical: bool,
            size: Size,
            a: Box<Tree>,
            b: Box<Tree>,
        },
        Tabs {
            panels: Vec<PanelKind>,
            active: usize,
        },
    }

    #[derive(Deserialize, Serialize)]
    enum Size {
        Ratio(f32),
        FixedA(f32),
        FixedB(f32),
    }

    #[derive(Deserialize, Serialize)]
    struct Group {
        panels: Vec<PanelKind>,
        active: usize,
        rect: [f32; 4],
    }

    fn node(tree: Tree, depth: usize) -> Option<Node<PanelKind>> {
        if depth > MAX_DEPTH {
            return None;
        }
        Some(match tree {
            Tree::Tabs { panels, active } => Node::tabs(panels, active),
            Tree::Split { vertical, size, a, b } => Node::split(
                if vertical { Axis::Y } else { Axis::X },
                match size {
                    Size::Ratio(value) => SplitSize::Ratio(value),
                    Size::FixedA(value) => SplitSize::FixedFirst(value),
                    Size::FixedB(value) => SplitSize::FixedSecond(value),
                },
                node(*a, depth + 1)?,
                node(*b, depth + 1)?,
            ),
        })
    }

    fn tree(node: &Node<PanelKind>, depth: usize) -> Option<Tree> {
        if depth > MAX_DEPTH {
            return None;
        }
        Some(match node {
            Node::Tabs { panels, active, .. } => Tree::Tabs { panels: panels.clone(), active: *active },
            Node::Split { axis, size, first, second, .. } => Tree::Split {
                vertical: *axis == Axis::Y,
                size: match *size {
                    SplitSize::Ratio(value) => Size::Ratio(value),
                    SplitSize::FixedFirst(value) => Size::FixedA(value),
                    SplitSize::FixedSecond(value) => Size::FixedB(value),
                    _ => return None,
                },
                a: Box::new(tree(first, depth + 1)?),
                b: Box::new(tree(second, depth + 1)?),
            },
            _ => return None,
        })
    }

    /// `layout` as the `layout` and `floating` fields earlier versions read; `None` when a
    /// floating window holds more than one tab group, which they can't represent.
    pub(super) fn write(layout: &Layout) -> Option<(serde_json::Value, serde_json::Value)> {
        let root = match &layout.root {
            Some(root) => tree(root, 0)?,
            None => Tree::Tabs { panels: Vec::new(), active: 0 },
        };
        let floating = layout
            .floating
            .iter()
            .map(|window| match window.groups() {
                [group] => Some(Group { panels: group.panels.clone(), active: group.active, rect: array_of(window.rect) }),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let floating = if floating.is_empty() { serde_json::Value::Null } else { serde_json::to_value(floating).ok()? };
        Some((serde_json::to_value(root).ok()?, floating))
    }

    pub(super) fn layout(root: &serde_json::Value, floating: &serde_json::Value) -> Option<Layout> {
        let tree: Tree = serde_json::from_value(root.clone()).ok()?;
        let floating: Vec<Group> = if floating.is_null() { vec![] } else { serde_json::from_value(floating.clone()).ok()? };
        let mut root = match node(tree, 0)? {
            Node::Tabs { panels, .. } if panels.is_empty() => None,
            root => Some(root),
        };
        if floating.is_empty()
            && let Some(root) = root.as_mut()
        {
            migrate_effects(root);
        }
        let mut layout = Layout::new(root, Vec::new());
        for group in floating {
            let id = layout.fresh_window_id();
            layout.floating.push(Floating::single(id, group.panels, group.active, rect_of(group.rect)));
        }
        Some(layout)
    }

    /// Effects workspaces saved by early versions held the Effect Controls panel twice: selected
    /// beside Source, and an inactive duplicate beside Essential Graphics. Remove only that known
    /// duplicate shape; other malformed saved layouts still fail validation.
    fn migrate_effects(root: &mut Node<PanelKind>) {
        fn count(node: &Node<PanelKind>) -> usize {
            match node {
                Node::Tabs { panels, .. } => panels.iter().filter(|panel| **panel == PanelKind::EffectControls).count(),
                Node::Split { first, second, .. } => count(first) + count(second),
                _ => 0,
            }
        }
        fn selected_source(node: &Node<PanelKind>) -> bool {
            match node {
                Node::Tabs { panels, active, .. } => {
                    panels.contains(&PanelKind::Source)
                        && panels.contains(&PanelKind::AudioClipMixer)
                        && panels.get(*active) == Some(&PanelKind::EffectControls)
                }
                Node::Split { first, second, .. } => selected_source(first) || selected_source(second),
                _ => false,
            }
        }
        fn remove_inactive(node: &mut Node<PanelKind>) {
            match node {
                Node::Tabs { panels, active, .. } if panels.as_slice() == [PanelKind::EffectControls, PanelKind::EssentialGraphics] && *active == 1 => {
                    panels.remove(0);
                    *active = 0;
                }
                Node::Split { first, second, .. } => {
                    remove_inactive(first);
                    remove_inactive(second);
                }
                _ => {}
            }
        }
        if count(root) == 2 && selected_source(root) {
            remove_inactive(root);
        }
    }
}

/// The width of the Tools panel in the built-in workspaces, in points.
const TOOLS_WIDTH: f32 = 64.0;
/// The width of the Audio Meters in the built-in workspaces, in points.
const METERS_WIDTH: f32 = 116.0;

/// The default layout of a named workspace.
pub fn workspace(name: &str) -> Layout {
    Layout::new(Some(workspace_root(name)), Vec::new())
}

fn workspace_root(name: &str) -> Node<PanelKind> {
    use PanelKind::*;
    use SplitSize::Ratio;
    // Tools | Timeline | Audio Meters. Tools and the meters keep their widths as the window
    // resizes; the Timeline takes the rest.
    let bottom_editing = || {
        hsplit(
            SplitSize::FixedFirst(TOOLS_WIDTH),
            tabs(&[Tools], 0),
            hsplit(SplitSize::FixedSecond(METERS_WIDTH), tabs(&[Timeline], 0), tabs(&[AudioMeters], 0)),
        )
    };
    match name {
        "Assembly" => vsplit(
            Ratio(0.52),
            hsplit(
                Ratio(0.46),
                tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer, Metadata], 0), tabs(&[Program], 0)),
            ),
            bottom_editing(),
        ),
        "Color" => hsplit(
            Ratio(0.79),
            vsplit(
                Ratio(0.58),
                hsplit(Ratio(0.42), tabs(&[LumetriScopes, Source, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, Info, Markers, History], 0), bottom_editing()),
            ),
            tabs(&[LumetriColor, Effects], 0),
        ),
        "Effects" => hsplit(
            Ratio(0.8),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer], 1), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Info, Markers, History], 0), bottom_editing()),
            ),
            vsplit(Ratio(0.5), tabs(&[Effects], 0), tabs(&[EssentialGraphics], 0)),
        ),
        "Audio" => hsplit(
            Ratio(0.8),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.5), tabs(&[AudioTrackMixer, Source, AudioClipMixer, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Effects, Markers, History], 0), bottom_editing()),
            ),
            tabs(&[EssentialSound, TextToSpeech], 0),
        ),
        // 0.78, not production's 0.79: next's group frame takes about 13 pt more of the right column,
        // and the Essential Graphics Style row (130 pt of label, six 26 pt buttons) only fits beside
        // the scroll bar at the old column width
        "Captions and Graphics" => hsplit(
            Ratio(0.78),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.4), tabs(&[Text, Source, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Libraries], 0), bottom_editing()),
            ),
            tabs(&[EssentialGraphics, Properties], 0),
        ),
        // Vertical (9:16 editing, #1822): the Program monitor runs the full height on the right, beside
        // the source and project panels and the timeline
        "Vertical" => hsplit(
            Ratio(0.78),
            vsplit(
                Ratio(0.5),
                hsplit(
                    Ratio(0.5),
                    tabs(&[Source, EffectControls, AudioClipMixer, Metadata], 0),
                    tabs(&[Project, MediaBrowser, Effects, Info, Markers, History], 0),
                ),
                bottom_editing(),
            ),
            tabs(&[Program], 0),
        ),
        "All Panels" => vsplit(
            Ratio(0.5),
            hsplit(
                Ratio(0.5),
                tabs(&[Source, EffectControls, AudioClipMixer, Metadata, LumetriScopes, Text], 0),
                hsplit(Ratio(0.6), tabs(&[Program], 0), tabs(&[LumetriColor, EssentialGraphics, EssentialSound, TextToSpeech, Properties, AudioTrackMixer], 0)),
            ),
            hsplit(Ratio(0.26), tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0), bottom_editing()),
        ),
        // Editing (default), Learning, Review — Premiere 26 factory layout:
        // top: Source group | Program | Properties; bottom: Project group | Tools | Timeline | Meters.
        _ => vsplit(
            Ratio(0.57),
            hsplit(
                Ratio(0.72),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer, Metadata], 0), tabs(&[Program], 0)),
                tabs(&[Properties, EssentialGraphics, Text], 0),
            ),
            hsplit(Ratio(0.29), tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0), bottom_editing()),
        ),
    }
}

/// FilmCraft's queries and changes on a [`Layout`] beyond craft-ui's actions.
pub trait LayoutExt {
    /// Whether `panel` is the shown tab of its frame or floating window.
    fn is_visible(&self, panel: PanelKind) -> bool;
    /// Whether `panel` is in the frame tree, not a floating window.
    fn is_docked(&self, panel: PanelKind) -> bool;
    /// The shown panel of each frame in layout order, then of each floating window.
    fn visible_panels(&self) -> Vec<PanelKind>;
    /// Put the Timeline panel back where the workspaces keep it (left of the audio meters) after
    /// it was closed, so opening a sequence always has somewhere to show it.
    fn restore_timeline(&mut self);
    /// Add a panel as a tab next to `near` (else Project, Program or the first docked panel).
    fn open_near(&mut self, panel: PanelKind, near: PanelKind);
}

impl LayoutExt for Layout {
    fn is_visible(&self, panel: PanelKind) -> bool {
        self.panel_state(&panel) == craft_behavior::dock::PanelState::Front
    }

    fn is_docked(&self, panel: PanelKind) -> bool {
        Layout::new(self.root.clone(), Vec::new()).contains(&panel)
    }

    fn visible_panels(&self) -> Vec<PanelKind> {
        // Use the dock's front-panel rule for both docked and floating groups, including rails,
        // minimized groups and accordions, while retaining its layout traversal order.
        self.panels().into_iter().copied().filter(|&panel| self.is_visible(panel)).collect()
    }

    fn restore_timeline(&mut self) {
        if self.contains(&PanelKind::Timeline) {
            return;
        }
        fn beside_meters(n: &mut Node<PanelKind>) -> bool {
            match n {
                Node::Split { first, second, .. } => beside_meters(first) || beside_meters(second),
                Node::Tabs { panels, .. } if panels.as_slice() == [PanelKind::AudioMeters] => {
                    *n = hsplit(SplitSize::FixedSecond(METERS_WIDTH), tabs(&[PanelKind::Timeline], 0), tabs(&[PanelKind::AudioMeters], 0));
                    true
                }
                _ => false,
            }
        }
        if !self.root.as_mut().is_some_and(beside_meters) {
            self.open_near(PanelKind::Timeline, PanelKind::Project);
        }
    }

    fn open_near(&mut self, panel: PanelKind, near: PanelKind) {
        let docked = Layout::new(self.root.clone(), Vec::new());
        let anchor = [near, PanelKind::Project, PanelKind::Program]
            .into_iter()
            .find(|candidate| docked.contains(candidate))
            .or_else(|| docked.panels().first().map(|first| **first));
        let _ = self.apply(Action::open(panel, anchor));
    }
}

/// The sequences open in the Timeline panel, in tab order: `(project item id, name)`.
#[derive(Debug, Clone, Default)]
pub struct SeqTabs {
    pub open: Vec<(u64, String)>,
    pub active: Option<u64>,
}

impl SeqTabs {
    /// The Timeline panel's tab label: its active sequence.
    pub fn label(&self) -> String {
        let name = self.open.iter().find(|(id, _)| self.active == Some(*id)).map(|(_, n)| n.clone());
        name.unwrap_or_else(|| tl!("Timeline: (no sequences)").into())
    }
}

/// What the sequence tabs ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum SeqAction {
    Open(u64),
    Close(u64),
    /// A sequence tab was dragged past a neighbour: its new place among the sequence tabs.
    Move(u64, usize),
    /// A right click: show that sequence and open the Timeline's panel menu there.
    Menu(u64, egui::Pos2),
}

fn sequence_widths(ui: &egui::Ui, seqs: &SeqTabs) -> Vec<f32> {
    seqs.open
        .iter()
        .map(|(id, name)| {
            let label = ui.painter().layout_no_wrap(name.clone(), Tokens::ui(12.0), egui::Color32::WHITE).size().x;
            label + 16.0 + if seqs.active == Some(*id) { 16.0 } else { 0.0 }
        })
        .collect()
}

/// The width [`sequence_tabs`] needs to show every open sequence.
pub fn sequence_tabs_width(ui: &egui::Ui, seqs: &SeqTabs) -> f32 {
    sequence_widths(ui, seqs).iter().map(|w| w + 8.0).sum::<f32>() + 12.0
}

/// Draw the Timeline panel's sequence tabs in `strip`: one tab per open sequence, × on the active
/// one, drag to reorder, middle-click to close, the wheel to step through them, and a » list
/// when they don't fit.
pub fn sequence_tabs(ui: &mut egui::Ui, strip: Rect, seqs: &SeqTabs, t: &Tokens, reg: &mut crate::automation::Registry) -> Vec<SeqAction> {
    let mut actions = Vec::new();
    let painter = ui.painter_at(strip);
    let active_at = seqs.open.iter().position(|(id, _)| seqs.active == Some(*id)).unwrap_or(0);
    let text_y = strip.center().y;
    // Which tabs fit. The active tab is always shown: tabs before it are left out until it
    // fits, then as many after it as there is room for. The » list reaches the rest.
    let widths = sequence_widths(ui, seqs);
    let span = |from: usize, to: usize| widths.get(from..=to).map_or(0.0, |w| w.iter().sum::<f32>() + 8.0 * w.len() as f32);
    let room = strip.width() - 12.0;
    let all_fit = span(0, seqs.open.len().saturating_sub(1)) <= room;
    let room = if all_fit { room } else { room - 24.0 };
    let mut first = 0;
    while first < active_at && span(first, active_at) > room {
        first += 1;
    }
    let mut last = active_at.max(first);
    while last + 1 < seqs.open.len() && span(first, last + 1) <= room {
        last += 1;
    }
    let list_id = egui::Id::unique("timeline-tab-list");
    let more = (!all_fit).then(|| Rect::from_min_size(pos2(strip.max.x - 22.0, strip.center().y - 10.0), vec2(18.0, 20.0)));
    let mut list_opened = false;
    if let Some(r) = more {
        let resp = ui.interact(r, egui::Id::unique("timeline-tab-overflow"), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, resp.enabled(), "more tabs"));
        reg.add_response("timeline.tabs.more", &resp, "more tabs");
        let c = if resp.hovered() { t.tab_text_active } else { t.tab_text };
        icons::paint(&painter, r.shrink(4.0).translate(vec2(-2.0, 0.0)), Icon::ChevronRight, c);
        icons::paint(&painter, r.shrink(4.0).translate(vec2(2.0, 0.0)), Icon::ChevronRight, c);
        paint_widget_focus(&painter, &resp, t);
        if resp.clicked() {
            let open = ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
            ui.ctx().data_mut(|d| d.insert_temp(list_id, !open));
            list_opened = !open;
        }
    }
    // The wheel over the strip steps through the tabs: down = next, up = previous, one tab per
    // notch (trackpad scrolling adds up to a notch first), stopping at the ends.
    let wheel_id = egui::Id::unique("timeline-tab-wheel");
    if ui.rect_contains_pointer(strip) && !seqs.open.is_empty() {
        const NOTCH: f32 = 40.0;
        let total = ui.ctx().data(|d| d.get_temp::<f32>(wheel_id)).unwrap_or(0.0) + crate::panels::timeline::wheel_input(ui.ctx()).delta.y;
        let steps = (total / NOTCH).trunc();
        ui.ctx().data_mut(|d| d.insert_temp(wheel_id, total - steps * NOTCH));
        let to = (active_at as i64).saturating_sub(steps as i64).clamp(0, seqs.open.len() as i64 - 1) as usize;
        if let Some((id, _)) = seqs.open.get(to).filter(|_| to != active_at) {
            actions.push(SeqAction::Open(*id));
        }
    } else {
        ui.ctx().data_mut(|d| d.remove::<f32>(wheel_id));
    }
    let mut x = strip.min.x + 12.0;
    let mut seq_rects: Vec<(u64, Rect)> = Vec::new();
    let mut dragged: Option<u64> = None;
    for (i, (id, name)) in seqs.open.iter().enumerate().take(last + 1).skip(first) {
        let is_active = seqs.active == Some(*id);
        let galley = painter.layout_no_wrap(name.clone(), Tokens::ui(12.0), if is_active { t.tab_text_active } else { t.tab_text });
        let close_w = if is_active { 16.0 } else { 0.0 };
        let w = widths.get(i).copied().unwrap_or(0.0);
        let tab_rect = Rect::from_min_size(pos2(x, strip.min.y), vec2(w, strip.height()));
        // a sequence tab keeps its identity when the tabs are reordered under the pointer
        let resp = ui.interact(tab_rect, egui::Id::unique(("tab-seq", *id)), Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, resp.enabled(), is_active, name));
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.set_role(egui::accesskit::Role::Tab);
            node.set_selected(is_active);
        });
        paint_widget_focus(&painter, &resp, t);
        seq_rects.push((*id, tab_rect));
        if resp.dragged_by(egui::PointerButton::Primary) {
            dragged = Some(*id);
        }
        // Escape puts a dragged tab back where it was (egui ends the drag on Escape, #580)
        let home = egui::Id::unique(("tab-seq-home", *id));
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ui.data_mut(|d| d.insert_temp(home, i));
        }
        if resp.drag_stopped()
            && let Some(at) = ui.data_mut(|d| d.remove_temp::<usize>(home))
            && ui.input(|input| input.key_pressed(egui::Key::Escape))
        {
            actions.push(SeqAction::Move(*id, at));
        }
        reg.add_response(&format!("timeline.tab.{id}"), &resp, name);
        let label_x = tab_rect.min.x + 8.0 + close_w;
        let col = if is_active || resp.hovered() { t.tab_text_active } else { t.tab_text };
        let label_w = galley.size().x;
        painter.galley_with_override_text_color(pos2(label_x, text_y - galley.size().y / 2.0), galley, col);
        let mut closed = false;
        if is_active {
            // × closes the sequence's tab (the sequence stays in the project)
            let cr = Rect::from_center_size(pos2(tab_rect.min.x + 10.0, text_y), vec2(8.0, 8.0));
            let cresp = ui.interact(cr.expand(4.0), egui::Id::unique(("tab-close", *id)), Sense::click());
            cresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, cresp.enabled(), "close sequence"));
            reg.add_response(&format!("timeline.tab.{id}.close"), &cresp, "close sequence");
            let cc = if cresp.hovered() { t.tab_text_active } else { t.tab_text };
            painter.line_segment([cr.left_top(), cr.right_bottom()], Stroke::new(1.2, cc));
            painter.line_segment([cr.right_top(), cr.left_bottom()], Stroke::new(1.2, cc));
            paint_widget_focus(&painter, &cresp, t);
            if cresp.clicked() {
                actions.push(SeqAction::Close(*id));
                closed = true;
            }
            let uy = strip.max.y - 2.0;
            painter.line_segment([pos2(label_x, uy), pos2(label_x + label_w, uy)], Stroke::new(1.0, t.tab_text_active));
        }
        if (resp.clicked() || resp.drag_started_by(egui::PointerButton::Primary)) && !closed && !is_active {
            actions.push(SeqAction::Open(*id));
        }
        if resp.middle_clicked() {
            actions.push(SeqAction::Close(*id));
        }
        if resp.secondary_clicked() {
            // The menu opens on the first whole pixel below the pointer. egui paints an area on
            // whole pixels but tests clicks against the position it was given, so a menu opened at
            // the pointer itself took the clicks on the tab row above its painted edge: a second
            // right-click on a neighbouring tab, at the same height, reached the menu.
            let at = resp.interact_pointer_pos().unwrap_or(tab_rect.left_bottom());
            let ppp = ui.ctx().pixels_per_point().max(1e-3);
            actions.push(SeqAction::Menu(*id, pos2((at.x * ppp).round() / ppp, ((at.y * ppp).floor() + 1.0) / ppp)));
        }
        x += w + 8.0;
    }
    // Dragging a sequence tab sideways: it changes places with a neighbour when the pointer
    // passes that neighbour's middle.
    if let Some(id) = dragged
        && let Some(px) = ui.input(|i| i.pointer.latest_pos()).map(|p| p.x)
        && let Some(at) = seq_rects.iter().position(|(s, _)| *s == id)
        && let Some(index) = seqs.open.iter().position(|(s, _)| *s == id)
    {
        let before = at.checked_sub(1).and_then(|k| seq_rects.get(k));
        let after = seq_rects.get(at + 1);
        if before.is_some_and(|(_, r)| px < r.center().x) {
            actions.push(SeqAction::Move(id, index.saturating_sub(1)));
        } else if after.is_some_and(|(_, r)| px > r.center().x) {
            actions.push(SeqAction::Move(id, index + 1));
        }
    }
    // the » list: every open sequence, the shown one ticked
    let list_open = ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
    if let Some(r) = more.filter(|_| list_open) {
        let mut picked = None;
        let area =
            egui::Area::new(list_id.with("area")).order(egui::Order::Foreground).pivot(Align2::RIGHT_TOP).fixed_pos(r.right_bottom()).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(160.0);
                    // many open sequences: the list scrolls instead of leaving the window
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for (id, name) in &seqs.open {
                            let label = if seqs.active == Some(*id) { format!("✓ {name}") } else { name.clone() };
                            let resp = ui.button(label);
                            reg.add_response(&format!("timeline.tabs.list.{id}"), &resp, name);
                            if resp.clicked() {
                                picked = Some(*id);
                            }
                        }
                    });
                });
            });
        if let Some(id) = picked {
            actions.push(SeqAction::Open(id));
        }
        if picked.is_some() || (!list_opened && area.response.clicked_elsewhere()) {
            ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
        }
    } else if list_open {
        ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
    }
    actions
}

pub(crate) fn paint_widget_focus(painter: &egui::Painter, response: &egui::Response, tokens: &Tokens) {
    if response.has_focus() {
        painter.rect_stroke(response.rect.shrink(2.0), 2.0, Stroke::new(1.5, tokens.focus), StrokeKind::Inside);
    }
}

/// Placeholder body for panels that are not implemented yet.
pub fn placeholder(ui: &mut egui::Ui, rect: Rect, t: &Tokens, text: &str) {
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, Tokens::ui(12.0), t.text_faint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_workspace_gives_program_the_full_height() {
        // #1822: Program alone on the right of the root split, spanning the window's height
        let Some(Node::Split { axis: Axis::X, second, .. }) = workspace("Vertical").root else { panic!("a side-by-side root split") };
        assert!(matches!(*second, Node::Tabs { ref panels, .. } if panels == &[PanelKind::Program]), "{second:?}");
    }

    #[test]
    fn workspaces_contain_core_panels() {
        for w in WORKSPACES {
            let d = workspace(w);
            d.validate().unwrap();
            assert!(d.contains(&PanelKind::Timeline), "{w}");
            assert!(d.contains(&PanelKind::Program), "{w}");
        }
    }

    /// The fixed sizes of a tree's splits, depth first.
    fn fixed_splits(node: &Node<PanelKind>) -> Vec<SplitSize> {
        match node {
            Node::Split { size, first, second, .. } => {
                let mut out = if matches!(size, SplitSize::Ratio(_)) { vec![] } else { vec![*size] };
                out.extend(fixed_splits(first));
                out.extend(fixed_splits(second));
                out
            }
            _ => vec![],
        }
    }

    /// In every built-in workspace Tools and the Audio Meters keep their widths as the window
    /// resizes; every other group resizes with it.
    #[test]
    fn workspaces_fix_only_tools_and_meters() {
        for w in WORKSPACES {
            let layout = workspace(w);
            let fixed = layout.root.as_ref().map(fixed_splits).unwrap_or_default();
            assert_eq!(fixed, vec![SplitSize::FixedFirst(TOOLS_WIDTH), SplitSize::FixedSecond(METERS_WIDTH)], "{w}");
        }
    }

    /// A saved workspace keeps its docked fixed split sizes and ratios; a floating window saved
    /// split top and bottom loads as one window of those groups, top to bottom.
    #[test]
    fn saved_workspaces_keep_fixed_and_ratio_splits() {
        let workspace = serde_json::json!({"layout": {"root": {"Split": {"axis": "Horizontal", "size": {"FixedSecond": 450.0},
            "first": {"Split": {"axis": "Horizontal", "size": {"Ratio": 0.25},
                "first": {"Tabs": {"panels": ["Tools"], "active": 0}}, "second": {"Tabs": {"panels": ["Timeline"], "active": 0}}}},
            "second": {"Tabs": {"panels": ["AudioMeters"], "active": 0}}}},
            "floating": [{"root": {"Split": {"axis": "Vertical", "size": {"FixedFirst": 100.0},
                "first": {"Tabs": {"panels": ["Info"], "active": 0}}, "second": {"Tabs": {"panels": ["Effects"], "active": 0}}}},
                "rect": [10.0, 20.0, 300.0, 304.0]}]}, "hidden": []});
        let prefs = WorkspacePrefs { saved: vec![SavedWorkspace { name: "Mine".into(), workspace, ..Default::default() }], current: "Mine".into(), last: None };
        let loaded = saved_docking(&prefs, "Mine");
        assert!(loaded.is_docked(PanelKind::Tools) && loaded.is_visible(PanelKind::Info) && loaded.is_visible(PanelKind::Effects), "{loaded:?}");
        let root = loaded.root.as_ref().unwrap();
        let Node::Split { size: SplitSize::FixedSecond(outer), first, .. } = root else { panic!("{root:?}") };
        assert_eq!(*outer, 450.0);
        let Node::Split { size: SplitSize::Ratio(inner), .. } = &**first else { panic!("{first:?}") };
        assert_eq!(*inner, 0.25);
        let floating: Vec<Vec<_>> = loaded.floating.iter().map(|window| window.groups().iter().map(|group| group.panels.clone()).collect()).collect();
        assert_eq!(floating, vec![vec![vec![PanelKind::Info], vec![PanelKind::Effects]]], "{:?}", loaded.floating);
    }

    /// #284: with Project and Program closed (or every panel closed) a panel still opens.
    #[test]
    fn open_near_falls_back_to_the_first_group() {
        let mut d = workspace("Editing");
        for p in d.panels().into_iter().copied().collect::<Vec<_>>() {
            d.apply(Action::close(p)).unwrap();
        }
        assert_eq!(d.root, None);
        d.open_near(PanelKind::Program, PanelKind::Project);
        assert!(d.is_visible(PanelKind::Program));
        d.restore_timeline();
        assert!(d.is_visible(PanelKind::Timeline));
        // a tree without Project or Program: the panel joins the first group
        let mut d = Layout::new(Some(hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects], 0), tabs(&[PanelKind::Markers], 0))), Vec::new());
        d.open_near(PanelKind::Info, PanelKind::Source);
        assert!(d.is_visible(PanelKind::Info));
        assert_eq!(d.root, Some(hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects, PanelKind::Info], 1), tabs(&[PanelKind::Markers], 0))));
    }

    /// A workspace file written before FilmCraft saved craft-ui workspaces still opens, and a
    /// saved workspace reads back as written.
    #[test]
    fn saved_workspaces_read_earlier_files_and_round_trip() {
        let legacy = serde_json::json!({"saved": [{"name": "Mine", "layout": {"Split": {"vertical": false, "size": {"FixedA": 300.0},
            "a": {"Tabs": {"panels": ["Project", "Info"], "active": 1}}, "b": {"Tabs": {"panels": ["Timeline"], "active": 0}}}},
            "floating": [{"panels": ["Effects"], "active": 0, "rect": [10.0, 20.0, 300.0, 200.0]}]},
            {"name": "Broken", "layout": {"Tabs": {"panels": ["NoSuchPanel"], "active": 0}}}], "current": "Mine"});
        let prefs: WorkspacePrefs = serde_json::from_value(legacy).unwrap();
        let mine = saved_docking(&prefs, "Mine");
        assert!(mine.is_visible(PanelKind::Info) && mine.is_visible(PanelKind::Timeline) && mine.is_visible(PanelKind::Effects));
        // the Project column keeps its fixed 300 pt
        let Some(Node::Split { size, .. }) = &mine.root else { panic!("{:?}", mine.root) };
        assert_eq!(*size, SplitSize::FixedFirst(300.0));
        assert_eq!(saved_docking(&prefs, "Broken"), workspace("Broken"));
        let saved = SavedWorkspace { name: "Mine".into(), workspace: serde_json::to_value(Workspace::new(mine.clone())).unwrap(), ..Default::default() };
        let prefs = WorkspacePrefs { saved: vec![saved], current: "Mine".into(), last: None };
        let text = serde_json::to_string(&prefs).unwrap();
        assert!(!text.contains("\"layout\":{\"Split\""), "{text}");
        let back: WorkspacePrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(saved_docking(&back, "Mine"), mine);
    }

    #[test]
    fn keyboard_cycle_includes_open_dock_groups_but_not_collapsed_groups() {
        let layout: Layout = serde_json::from_value(serde_json::json!({
            "root": {"Dock": {"side": "End", "columns": [
                {"groups": [{"panels": ["Effects", "Properties"], "active": 1},
                    {"panels": ["Timeline"], "active": 0, "minimized": true}]},
                {"iconic": true, "groups": [{"panels": ["Info"], "active": 0}]}
            ]}},
            "floating": [
                {"rect": [0.0, 0.0, 200.0, 200.0], "panels": ["Program"], "active": 0},
                {"rect": [250.0, 0.0, 200.0, 200.0], "column": {
                    "groups": [{"panels": ["AudioMeters"], "active": 0, "minimized": true}]}}
            ]
        }))
        .unwrap();
        layout.validate().unwrap();
        assert_eq!(layout.visible_panels(), vec![PanelKind::Properties, PanelKind::Program]);
    }

    /// Rollback to the previous production app, not just to the pre-craft-ui reader. Mirror
    /// its v2 version check and persisted types from craft-libs 33be3562 for the exercised
    /// split, dock, floating group, and native placement fields.
    #[test]
    fn workspace_save_opens_in_the_previous_production_app_and_back() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        enum OldAxis {
            Horizontal,
            Vertical,
        }
        #[derive(Serialize, Deserialize)]
        enum OldRoot {
            Split { axis: OldAxis, size: SplitSize, first: Box<OldRoot>, second: Box<OldRoot> },
            Tabs { panels: Vec<PanelKind>, active: usize },
            Dock { side: String, columns: Vec<OldColumn> },
        }
        #[derive(Serialize, Deserialize)]
        struct OldColumn {
            width: f32,
            groups: Vec<OldGroup>,
        }
        #[derive(Serialize, Deserialize)]
        struct OldGroup {
            panels: Vec<PanelKind>,
            active: usize,
            minimized: bool,
            height: Option<f32>,
        }
        #[derive(Serialize, Deserialize)]
        struct OldFloating {
            id: u64,
            rect: [f32; 4],
            native: Option<[f32; 4]>,
            monitor_size: Option<[f32; 2]>,
            groups: Vec<OldGroup>,
        }
        #[derive(Serialize, Deserialize)]
        struct OldLayout {
            root: OldRoot,
            floating: Vec<OldFloating>,
        }
        #[derive(Serialize, Deserialize)]
        struct OldWorkspace {
            version: u32,
            layout: OldLayout,
        }
        #[derive(Serialize, Deserialize)]
        struct OldEntry {
            name: String,
            workspace: OldWorkspace,
        }

        let exact_v2 = serde_json::json!({
            "name": "Mine",
            "workspace": {"version": 2,
                "layout": {"root": {"Split": {
                    "axis": "Horizontal", "size": {"Ratio": 0.7},
                    "first": {"Tabs": {"panels": ["Timeline"], "active": 0}},
                    "second": {"Dock": {"side": "End", "columns": [{"width": 240.0,
                        "groups": [{"panels": ["Source"], "active": 0, "minimized": false, "height": null}]}]}}
                }}, "floating": [{"id": 42, "rect": [10.0, 20.0, 300.0, 400.0],
                    "root": {"Split": {"axis": "Vertical", "size": {"FixedFirst": 150.0},
                        "first": {"Tabs": {"panels": ["Info", "Effects"], "active": 1}},
                        "second": {"Tabs": {"panels": ["Markers"], "active": 0}}}},
                    "native": [-1100.0, 200.0, 300.0, 400.0],
                    "monitor": {"name": "Left", "rect": [-1280.0, 0.0, 1280.0, 800.0], "scale": 2.0}
                }]},
                "hidden": [], "saved": null, "locked": false}
        });
        let from_old = read_saved(&serde_json::from_value(exact_v2).unwrap()).unwrap();
        let layout = from_old.layout();
        assert_eq!(layout.floating[0].groups()[0].height, Some(150.0));
        assert_eq!(layout.floating[0].groups()[1].height, None);
        let native = layout.floating[0].native.as_ref().unwrap();
        assert_eq!(native.position_px, Some([-2200, 400]));
        assert_eq!(native.normal.size, [300.0, 400.0]);
        let monitor = native.monitor.as_ref().unwrap();
        assert_eq!(monitor.name, "Left");
        assert_eq!(monitor.size_px, [2560, 1600]);

        let mut layout = layout.clone();
        let Some(Node::Split { second, .. }) = layout.root.as_mut() else { panic!("expected the docked split") };
        let Node::Dock { columns, .. } = &mut **second else { panic!("expected the dock column") };
        columns[0].width = None;
        layout.floating[0].column.iconic = true;
        layout.floating[0].column.iconic_width = Some(58.0);
        let entry = SavedWorkspace::new("Mine", &layout).unwrap();
        let as_json = serde_json::to_value(&entry).unwrap();
        let mut old: OldEntry = serde_json::from_value(as_json.clone()).unwrap();
        assert_eq!(old.workspace.version, 2);
        let OldRoot::Split { axis, second, .. } = &old.workspace.layout.root else { panic!("expected the docked split") };
        assert_eq!(*axis, OldAxis::Horizontal);
        let OldRoot::Dock { columns, .. } = &**second else { panic!("expected the dock column") };
        assert_eq!(columns[0].width, 240.0);
        let window = &old.workspace.layout.floating[0];
        assert_eq!(window.groups[0].panels, [PanelKind::Info, PanelKind::Effects]);
        assert_eq!(window.groups[0].active, 1);
        assert_eq!(window.groups[0].height, Some(150.0));
        assert_eq!(window.groups[1].panels, [PanelKind::Markers]);
        assert_eq!(window.native, Some([-1100.0, 200.0, 300.0, 400.0]));
        assert_eq!(window.monitor_size, Some([1280.0, 800.0]));
        assert_eq!(read_saved(&serde_json::from_value(as_json).unwrap()).unwrap().layout(), &layout);

        // Old SavedWorkspace holds the entire workspace as a raw Value. Resaving an
        // untouched entry therefore keeps our newer fields even though the v2 dock ignores them.
        #[derive(Serialize, Deserialize)]
        struct OldEnvelope {
            name: String,
            workspace: serde_json::Value,
        }
        let passive: OldEnvelope = serde_json::from_value(serde_json::to_value(&entry).unwrap()).unwrap();
        let reread = serde_json::from_value(serde_json::to_value(passive).unwrap()).unwrap();
        assert_eq!(read_saved(&reread).unwrap().layout(), &layout);

        // Explicitly saving a changed layout in the old app regenerates the v2 workspace.
        // The next app must see the old edit, not resurrect its earlier V4 active tab.
        old.workspace.layout.floating[0].groups[0].active = 0;
        let old_save = serde_json::to_value(old).unwrap();
        assert!(old_save["workspace"].get("filmcraftV4").is_none());
        let back = read_saved(&serde_json::from_value(old_save).unwrap()).unwrap();
        assert_eq!(back.layout().floating[0].groups()[0].active, 0);
    }

    #[test]
    fn damaged_or_future_workspace_is_not_loaded_as_the_downgrade_projection() {
        let layout = workspace("Editing");
        let mut entry = SavedWorkspace::new("Mine", &layout).unwrap();
        entry.workspace["filmcraftV4"]["version"] = serde_json::json!(99);
        assert!(read_saved(&entry).is_none());
        entry.workspace.as_object_mut().unwrap().remove("filmcraftV4");
        assert_eq!(read_saved(&entry).unwrap().layout(), &layout);
        entry.workspace = serde_json::json!(42);
        assert!(read_saved(&entry).is_none());
    }

    /// A saved workspace still opens in versions that read only the earlier `layout` and
    /// `floating` fields, as long as every floating window is one tab group.
    #[test]
    fn saved_workspaces_open_in_earlier_versions() {
        // The shapes earlier versions read the file into.
        #[allow(dead_code)]
        #[derive(Debug, PartialEq, Deserialize)]
        enum OldSize {
            Ratio(f32),
            FixedA(f32),
            FixedB(f32),
        }
        #[allow(dead_code)]
        #[derive(Debug, PartialEq, Deserialize)]
        enum OldNode {
            Split { vertical: bool, size: OldSize, a: Box<OldNode>, b: Box<OldNode> },
            Tabs { panels: Vec<PanelKind>, active: usize },
        }
        #[derive(Debug, PartialEq, Deserialize)]
        struct OldFloating {
            panels: Vec<PanelKind>,
            active: usize,
            rect: [f32; 4],
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        struct OldSaved {
            layout: serde_json::Value,
            floating: serde_json::Value,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        struct OldPrefs {
            saved: Vec<OldSaved>,
        }
        let read = |layout: &Layout| {
            let prefs = WorkspacePrefs { saved: vec![SavedWorkspace::new("Mine", layout).unwrap()], current: "Mine".into(), last: None };
            let text = serde_json::to_string(&prefs).unwrap();
            assert_eq!(saved_docking(&serde_json::from_str(&text).unwrap(), "Mine"), *layout, "{text}");
            let old: OldPrefs = serde_json::from_str(&text).unwrap();
            let entry = old.saved.into_iter().next().unwrap();
            let floating: Vec<OldFloating> = if entry.floating.is_null() { vec![] } else { serde_json::from_value(entry.floating).unwrap() };
            (serde_json::from_value::<OldNode>(entry.layout).ok(), floating)
        };

        fn panels(node: &OldNode, out: &mut Vec<PanelKind>) {
            match node {
                OldNode::Split { a, b, .. } => {
                    panels(a, out);
                    panels(b, out);
                }
                OldNode::Tabs { panels, .. } => out.extend(panels),
            }
        }

        let mut layout = workspace("Editing");
        layout.apply(Action::float(PanelKind::Info, rect_of([10.0, 20.0, 300.0, 200.0]))).unwrap();
        let (root, floating) = read(&layout);
        let mut old = Vec::new();
        panels(&root.unwrap(), &mut old);
        let docked = Layout::new(layout.root.clone(), Vec::new());
        assert_eq!(old, docked.panels().into_iter().copied().collect::<Vec<_>>());
        assert_eq!(floating, vec![OldFloating { panels: vec![PanelKind::Info], active: 0, rect: [10.0, 20.0, 300.0, 200.0] }]);

        // A floating window of two groups has no earlier form: those versions use the default.
        let mut split = layout.clone();
        split.apply(Action::float(PanelKind::Effects, rect_of([400.0, 20.0, 300.0, 200.0]))).unwrap();
        let window = split.floating.iter().position(|w| w.contains(&PanelKind::Effects)).unwrap();
        let effects = split.floating.remove(window);
        split.floating[0].column.groups.extend(effects.column.groups);
        assert_eq!(read(&split), (None, vec![]));
    }
}

#[cfg(test)]
mod naming_tests {
    use super::PanelKind;

    #[test]
    fn grading_panel_names_keep_old_aliases_and_serialization() {
        for name in ["Lumetri Color", "Color Grade", "Color Grading", "LumetriColor"] {
            assert_eq!(PanelKind::from_name(name), Some(PanelKind::LumetriColor));
        }
        for name in ["Lumetri Scopes", "Video Scopes", "LumetriScopes"] {
            assert_eq!(PanelKind::from_name(name), Some(PanelKind::LumetriScopes));
        }
        assert_eq!(PanelKind::LumetriColor.title(), "Color Grading");
        assert_eq!(PanelKind::LumetriScopes.title(), "Video Scopes");
        for panel in [PanelKind::LumetriColor, PanelKind::LumetriScopes] {
            let saved = serde_json::to_value(panel).unwrap();
            let loaded: PanelKind = serde_json::from_value(saved).unwrap();
            assert_eq!(loaded, panel);
        }
    }
}

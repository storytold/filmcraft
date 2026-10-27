//! FilmCraft's panels and workspaces. craft-ui's docking owns the frame tree, the tab strips,
//! floating windows and gestures (`dock_ui.rs` draws it); this module names the
//! panels, builds the built-in workspaces and draws the Timeline's sequence tabs.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};

use craft_ui::docking::{Action, Floating, Node};
use craft_ui::layout::{SplitAxis, SplitSize};

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
            PanelKind::LumetriColor => "Lumetri Color",
            PanelKind::LumetriScopes => "Lumetri Scopes",
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
pub type Layout = craft_ui::docking::Layout<PanelKind>;
/// What a saved workspace stores: its layout, closed panels and lock.
pub type Workspace = craft_ui::docking::Workspace<PanelKind>;

fn tabs(p: &[PanelKind], active: usize) -> Node<PanelKind> {
    Node::Tabs { panels: p.to_vec(), active }
}
fn hsplit(size: SplitSize, a: Node<PanelKind>, b: Node<PanelKind>) -> Node<PanelKind> {
    Node::Split { axis: SplitAxis::Horizontal, size, first: Box::new(a), second: Box::new(b) }
}
fn vsplit(size: SplitSize, a: Node<PanelKind>, b: Node<PanelKind>) -> Node<PanelKind> {
    Node::Split { axis: SplitAxis::Vertical, size, first: Box::new(a), second: Box::new(b) }
}

pub const WORKSPACES: [&str; 9] = ["Editing", "Assembly", "Color", "Effects", "Audio", "Captions and Graphics", "Learning", "Review", "All Panels"];

/// The user's workspaces file, beside `preferences.json` in the data directory.
pub const WORKSPACES_FILE: &str = "workspaces.json";

/// Window ▸ Workspaces: the user's own workspaces and saved changes to the built-in ones, and the
/// workspace in use (reopened at launch).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspacePrefs {
    pub saved: Vec<SavedWorkspace>,
    pub current: String,
}

/// A named workspace. `workspace` is a craft-ui [`Workspace`] kept as JSON, so one unreadable
/// entry (a panel a newer version added) doesn't lose the others. Files written before
/// FilmCraft used craft-ui workspaces hold a frame tree in `layout` and floating groups in
/// `floating` instead; [`saved_workspace`] reads them. [`SavedWorkspace::new`] writes
/// `workspace`, and also `layout` and `floating` while every floating window is one tab
/// group, so that earlier versions still open the workspace.
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
        let workspace = Workspace::new(layout.clone());
        workspace.validate().map_err(|e| e.to_string())?;
        let workspace = serde_json::to_value(&workspace).map_err(|e| e.to_string())?;
        let (layout, floating) = legacy::write(layout).unwrap_or_default();
        Ok(SavedWorkspace { name: name.to_string(), workspace, layout, floating })
    }
}

impl WorkspacePrefs {
    pub fn load(path: &std::path::Path) -> Self {
        use std::io::Read as _;
        // Preferences are untrusted; bound reading before JSON can allocate from it.
        const MAX_BYTES: u64 = 4 * 1024 * 1024;
        let Some(bytes) = std::fs::File::open(path).ok().and_then(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
            (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
        }) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
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
    let saved = prefs.saved.iter().find(|saved| saved.name == name).and_then(|saved| {
        let workspace = if saved.workspace.is_null() {
            Workspace::new(legacy::layout(&saved.layout, &saved.floating)?)
        } else {
            serde_json::from_value::<Workspace>(saved.workspace.clone()).ok()?
        };
        workspace.validate().ok()?;
        Some(workspace)
    });
    saved.unwrap_or_else(|| Workspace::new(workspace(name)))
}

/// The saved layout of a workspace (see [`saved_workspace`]).
pub fn saved_docking(prefs: &WorkspacePrefs, name: &str) -> Layout {
    saved_workspace(prefs, name).layout
}

/// The frame tree and floating groups FilmCraft wrote before its workspaces were craft-ui
/// [`Workspace`]s.
mod legacy {
    use super::{Floating, Layout, Node, PanelKind, SplitAxis, SplitSize};
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
            Tree::Tabs { panels, active } => Node::Tabs { panels, active },
            Tree::Split { vertical, size, a, b } => Node::Split {
                axis: if vertical { SplitAxis::Vertical } else { SplitAxis::Horizontal },
                size: match size {
                    Size::Ratio(value) => SplitSize::Ratio(value),
                    Size::FixedA(value) => SplitSize::FixedFirst(value),
                    Size::FixedB(value) => SplitSize::FixedSecond(value),
                },
                first: Box::new(node(*a, depth + 1)?),
                second: Box::new(node(*b, depth + 1)?),
            },
        })
    }

    fn tree(node: &Node<PanelKind>, depth: usize) -> Option<Tree> {
        if depth > MAX_DEPTH {
            return None;
        }
        Some(match node {
            Node::Tabs { panels, active } => Tree::Tabs { panels: panels.clone(), active: *active },
            Node::Split { axis, size, first, second } => Tree::Split {
                vertical: *axis == SplitAxis::Vertical,
                size: match *size {
                    SplitSize::Ratio(value) => Size::Ratio(value),
                    SplitSize::FixedFirst(value) => Size::FixedA(value),
                    SplitSize::FixedSecond(value) => Size::FixedB(value),
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
            .map(|window| match &window.root {
                Node::Tabs { panels, active } => Some(Group { panels: panels.clone(), active: *active, rect: window.rect }),
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
        Some(Layout { root, floating: floating.into_iter().map(|group| Floating::single(group.panels, group.active, group.rect)).collect() })
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
                Node::Tabs { panels, active } => {
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
                Node::Tabs { panels, active } if panels.as_slice() == [PanelKind::EffectControls, PanelKind::EssentialGraphics] && *active == 1 => {
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
    Layout { root: Some(workspace_root(name)), floating: vec![] }
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
        "Captions and Graphics" => hsplit(
            Ratio(0.79),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.4), tabs(&[Text, Source, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Libraries], 0), bottom_editing()),
            ),
            tabs(&[EssentialGraphics, Properties], 0),
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
        self.panel_state(&panel) == craft_ui::docking::PanelState::Front
    }

    fn is_docked(&self, panel: PanelKind) -> bool {
        Layout { root: self.root.clone(), floating: vec![] }.contains(&panel)
    }

    fn visible_panels(&self) -> Vec<PanelKind> {
        fn walk(node: &Node<PanelKind>, out: &mut Vec<PanelKind>) {
            match node {
                Node::Tabs { panels, active } => out.extend(panels.get(*active).copied()),
                Node::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            walk(root, &mut out);
        }
        for window in &self.floating {
            walk(&window.root, &mut out);
        }
        out
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
        let docked = Layout { root: self.root.clone(), floating: vec![] };
        let anchor = [near, PanelKind::Project, PanelKind::Program]
            .into_iter()
            .find(|candidate| docked.contains(candidate))
            .or_else(|| docked.panels().first().map(|first| **first));
        let _ = self.apply(Action::Open { panel, anchor });
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
        if resp.dragged() {
            dragged = Some(*id);
        }
        // Escape puts a dragged tab back where it was (egui ends the drag on Escape, #580)
        let home = egui::Id::unique(("tab-seq-home", *id));
        if resp.drag_started() {
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
        if (resp.clicked() || resp.drag_started()) && !closed && !is_active {
            actions.push(SeqAction::Open(*id));
        }
        if resp.middle_clicked() {
            actions.push(SeqAction::Close(*id));
        }
        if resp.secondary_clicked() {
            actions.push(SeqAction::Menu(*id, resp.interact_pointer_pos().unwrap_or(tab_rect.left_bottom())));
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

    /// A saved workspace keeps its fixed split sizes, docked and floating, and its ratios.
    #[test]
    fn saved_workspaces_keep_fixed_and_ratio_splits() {
        let workspace = serde_json::json!({"layout": {"root": {"Split": {"axis": "Horizontal", "size": {"FixedSecond": 450.0},
            "first": {"Split": {"axis": "Horizontal", "size": {"Ratio": 0.25},
                "first": {"Tabs": {"panels": ["Tools"], "active": 0}}, "second": {"Tabs": {"panels": ["Timeline"], "active": 0}}}},
            "second": {"Tabs": {"panels": ["AudioMeters"], "active": 0}}}},
            "floating": [{"root": {"Split": {"axis": "Vertical", "size": {"FixedFirst": 100.0},
                "first": {"Tabs": {"panels": ["Info"], "active": 0}}, "second": {"Tabs": {"panels": ["Effects"], "active": 0}}}},
                "rect": [10.0, 20.0, 300.0, 304.0]}]}, "hidden": []});
        let prefs = WorkspacePrefs { saved: vec![SavedWorkspace { name: "Mine".into(), workspace, ..Default::default() }], current: "Mine".into() };
        let loaded = saved_docking(&prefs, "Mine");
        assert!(loaded.is_docked(PanelKind::Tools) && loaded.is_visible(PanelKind::Info) && loaded.is_visible(PanelKind::Effects), "{loaded:?}");
        let root = loaded.root.as_ref().unwrap();
        let Node::Split { size: SplitSize::FixedSecond(outer), first, .. } = root else { panic!("{root:?}") };
        assert_eq!(*outer, 450.0);
        let Node::Split { size: SplitSize::Ratio(inner), .. } = &**first else { panic!("{first:?}") };
        assert_eq!(*inner, 0.25);
        let floating: Vec<_> = loaded.floating.iter().flat_map(|window| fixed_splits(&window.root)).collect();
        assert_eq!(floating, vec![SplitSize::FixedFirst(100.0)], "{:?}", loaded.floating);
    }

    /// #284: with Project and Program closed (or every panel closed) a panel still opens.
    #[test]
    fn open_near_falls_back_to_the_first_group() {
        let mut d = workspace("Editing");
        for p in d.panels().into_iter().copied().collect::<Vec<_>>() {
            d.apply(Action::Close { panel: p }).unwrap();
        }
        assert_eq!(d.root, None);
        d.open_near(PanelKind::Program, PanelKind::Project);
        assert!(d.is_visible(PanelKind::Program));
        d.restore_timeline();
        assert!(d.is_visible(PanelKind::Timeline));
        // a tree without Project or Program: the panel joins the first group
        let mut d = Layout { root: Some(hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects], 0), tabs(&[PanelKind::Markers], 0))), floating: vec![] };
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
        let prefs = WorkspacePrefs { saved: vec![saved], current: "Mine".into() };
        let text = serde_json::to_string(&prefs).unwrap();
        assert!(!text.contains("\"layout\":{\"Split\""), "{text}");
        let back: WorkspacePrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(saved_docking(&back, "Mine"), mine);
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
            let prefs = WorkspacePrefs { saved: vec![SavedWorkspace::new("Mine", layout).unwrap()], current: "Mine".into() };
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
        layout.apply(Action::Float { panel: PanelKind::Info, rect: [10.0, 20.0, 300.0, 200.0] }).unwrap();
        let (root, floating) = read(&layout);
        let mut old = Vec::new();
        panels(&root.unwrap(), &mut old);
        let docked = Layout { root: layout.root.clone(), floating: vec![] };
        assert_eq!(old, docked.panels().into_iter().copied().collect::<Vec<_>>());
        assert_eq!(floating, vec![OldFloating { panels: vec![PanelKind::Info], active: 0, rect: [10.0, 20.0, 300.0, 200.0] }]);

        // A floating window split in two has no earlier form: those versions use the default.
        let mut split = layout.clone();
        split.apply(Action::Float { panel: PanelKind::Effects, rect: [400.0, 20.0, 300.0, 200.0] }).unwrap();
        let window = split.floating.iter().position(|w| w.contains(&PanelKind::Effects)).unwrap();
        let effects = split.floating.remove(window);
        split.floating[0].root = Node::Split {
            axis: SplitAxis::Vertical,
            size: SplitSize::Ratio(0.5),
            first: Box::new(split.floating[0].root.clone()),
            second: Box::new(effects.root),
        };
        assert_eq!(read(&split), (None, vec![]));
    }
}

//! Premiere-style docking: a tree of splits and tab groups. Panels are rounded frames separated by
//! thin gutters; each group has a tab strip (active tab bright, with a panel menu "≡"); the focused
//! panel gets a blue outline. Gutters drag to resize; workspaces are serialized trees.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};

use craft_ui::layout::{ResizeHandle, ResizeHandleStyle, SplitAxis, SplitSize as SharedSplitSize, split_rect};

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
    /// Narrow chrome-less panels (no tab strip; a slim grip instead).
    pub fn compact(self) -> bool {
        matches!(self, PanelKind::Tools | PanelKind::AudioMeters)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SplitSize {
    Ratio(f32),
    /// First child has a fixed size in points.
    FixedA(f32),
    /// Second child has a fixed size in points.
    FixedB(f32),
}

impl SplitSize {
    fn shared(self) -> SharedSplitSize {
        match self {
            Self::Ratio(value) => SharedSplitSize::Ratio(value),
            Self::FixedA(value) => SharedSplitSize::FixedFirst(value),
            Self::FixedB(value) => SharedSplitSize::FixedSecond(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DockNode {
    /// `vertical`: children stacked top/bottom; else side by side.
    Split {
        vertical: bool,
        size: SplitSize,
        a: Box<DockNode>,
        b: Box<DockNode>,
    },
    Tabs {
        panels: Vec<PanelKind>,
        active: usize,
    },
}

fn tabs(p: &[PanelKind], active: usize) -> DockNode {
    DockNode::Tabs { panels: p.to_vec(), active }
}
fn hsplit(size: SplitSize, a: DockNode, b: DockNode) -> DockNode {
    DockNode::Split { vertical: false, size, a: Box::new(a), b: Box::new(b) }
}
fn vsplit(size: SplitSize, a: DockNode, b: DockNode) -> DockNode {
    DockNode::Split { vertical: true, size, a: Box::new(a), b: Box::new(b) }
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

/// A named layout. `layout` is a [`DockNode`] kept as JSON, so one unreadable entry (a panel a
/// newer version added) doesn't lose the others.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedWorkspace {
    pub name: String,
    pub layout: serde_json::Value,
    /// Kept raw for the same per-workspace failure isolation as the dock tree.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub floating: serde_json::Value,
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

/// The saved layout of a workspace: the user's saved version, else the built-in default.
pub fn saved_layout(prefs: &WorkspacePrefs, name: &str) -> DockNode {
    saved_docking(prefs, name).root
}

pub fn saved_docking(prefs: &WorkspacePrefs, name: &str) -> Layout {
    let saved = prefs.saved.iter().find(|saved| saved.name == name).and_then(|saved| {
        let mut layout = Layout {
            root: serde_json::from_value(saved.layout.clone()).ok()?,
            floating: if saved.floating.is_null() { vec![] } else { serde_json::from_value(saved.floating.clone()).ok()? },
        };
        if layout.floating.is_empty() {
            migrate_legacy_effects(&mut layout.root);
        }
        layout.validate().ok().map(|()| layout)
    });
    saved.unwrap_or_else(|| Layout { root: workspace(name), floating: vec![] })
}

/// Older Effects workspaces stored the same Effect Controls panel twice: selected beside
/// Source, and an inactive duplicate beside Essential Graphics. Remove only that known
/// duplicate shape; unrelated malformed or ambiguous saved layouts still fail validation.
fn migrate_legacy_effects(root: &mut DockNode) {
    let Ok(shared) = crate::dock_shared::encode(root, &[]) else { return };
    if shared.panels().iter().filter(|panel| ***panel == PanelKind::EffectControls).count() != 2 {
        return;
    }
    fn selected_source(node: &DockNode) -> bool {
        match node {
            DockNode::Tabs { panels, active } => {
                panels.contains(&PanelKind::Source) && panels.contains(&PanelKind::AudioClipMixer) && panels.get(*active) == Some(&PanelKind::EffectControls)
            }
            DockNode::Split { a, b, .. } => selected_source(a) || selected_source(b),
        }
    }
    fn remove_inactive(node: &mut DockNode) {
        match node {
            DockNode::Tabs { panels, active } if panels.as_slice() == [PanelKind::EffectControls, PanelKind::EssentialGraphics] && *active == 1 => {
                panels.remove(0);
                *active = 0;
            }
            DockNode::Split { a, b, .. } => {
                remove_inactive(a);
                remove_inactive(b);
            }
            DockNode::Tabs { .. } => {}
        }
    }
    if selected_source(root) {
        remove_inactive(root);
    }
}

/// The default layout of a named workspace.
pub fn workspace(name: &str) -> DockNode {
    use PanelKind::*;
    use SplitSize::*;
    let bottom_editing = || hsplit(FixedA(64.0), tabs(&[Tools], 0), hsplit(FixedB(116.0), tabs(&[Timeline], 0), tabs(&[AudioMeters], 0)));
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
            FixedB(340.0),
            vsplit(
                Ratio(0.58),
                hsplit(Ratio(0.42), tabs(&[LumetriScopes, Source, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, Info, Markers, History], 0), bottom_editing()),
            ),
            tabs(&[LumetriColor, Effects], 0),
        ),
        "Effects" => hsplit(
            FixedB(320.0),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer], 1), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Info, Markers, History], 0), bottom_editing()),
            ),
            vsplit(Ratio(0.5), tabs(&[Effects], 0), tabs(&[EssentialGraphics], 0)),
        ),
        "Audio" => hsplit(
            FixedB(320.0),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.5), tabs(&[AudioTrackMixer, Source, AudioClipMixer, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Effects, Markers, History], 0), bottom_editing()),
            ),
            tabs(&[EssentialSound, TextToSpeech], 0),
        ),
        "Captions and Graphics" => hsplit(
            FixedB(330.0),
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
                FixedB(450.0),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer, Metadata], 0), tabs(&[Program], 0)),
                tabs(&[Properties, EssentialGraphics, Text], 0),
            ),
            hsplit(
                Ratio(0.29),
                tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0),
                hsplit(FixedA(64.0), tabs(&[Tools], 0), hsplit(FixedB(116.0), tabs(&[Timeline], 0), tabs(&[AudioMeters], 0))),
            ),
        ),
    }
}

impl DockNode {
    /// Every panel in the tree.
    pub fn panels(&self, out: &mut Vec<PanelKind>) {
        match self {
            DockNode::Split { a, b, .. } => {
                a.panels(out);
                b.panels(out);
            }
            DockNode::Tabs { panels, .. } => out.extend(panels.iter().copied()),
        }
    }
    pub fn contains(&self, p: PanelKind) -> bool {
        let mut v = Vec::new();
        self.panels(&mut v);
        v.contains(&p)
    }
    /// Make `p` the active tab of its group. Returns false if not present.
    pub fn activate(&mut self, p: PanelKind) -> bool {
        crate::dock_shared::apply_tree(self, craft_ui::docking::Action::Activate { panel: p })
    }
    pub fn is_visible(&self, p: PanelKind) -> bool {
        match self {
            DockNode::Split { a, b, .. } => a.is_visible(p) || b.is_visible(p),
            DockNode::Tabs { panels, active } => panels.get(*active) == Some(&p),
        }
    }
    /// Close a panel (remove its tab; empty groups collapse their split).
    pub fn close(&mut self, p: PanelKind) {
        crate::dock_shared::apply_tree(self, craft_ui::docking::Action::Close { panel: p });
    }
    /// Put the Timeline panel back where the workspaces keep it (left of the audio meters) after
    /// it was closed, so opening a sequence always has somewhere to show it.
    pub fn restore_timeline(&mut self) {
        if self.contains(PanelKind::Timeline) {
            return;
        }
        fn beside_meters(n: &mut DockNode) -> bool {
            match n {
                DockNode::Split { a, b, .. } => beside_meters(a) || beside_meters(b),
                DockNode::Tabs { panels, .. } if panels.as_slice() == [PanelKind::AudioMeters] => {
                    *n = hsplit(SplitSize::FixedB(116.0), tabs(&[PanelKind::Timeline], 0), tabs(&[PanelKind::AudioMeters], 0));
                    true
                }
                DockNode::Tabs { .. } => false,
            }
        }
        if !beside_meters(self) {
            self.open_near(PanelKind::Timeline, PanelKind::Project);
        }
    }
    /// Add a panel as a tab next to `near` (or into the first group).
    pub fn open_near(&mut self, p: PanelKind, near: PanelKind) {
        if crate::dock_shared::encode(self, &[]).is_err() {
            return;
        }
        let anchor = [near, PanelKind::Project, PanelKind::Program].into_iter().find(|panel| self.contains(*panel)).or_else(|| {
            let mut panels = Vec::new();
            self.panels(&mut panels);
            panels.first().copied()
        });
        crate::dock_shared::apply_tree(self, craft_ui::docking::Action::Open { panel: p, anchor });
    }
}

/// A floating group retains the same panel identities as docked tabs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Floating {
    pub panels: Vec<PanelKind>,
    pub active: usize,
    /// [x, y, width, height] in logical points.
    pub rect: [f32; 4],
}

/// App persistence shape; all mutations pass through the shared docking model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub root: DockNode,
    #[serde(default)]
    pub floating: Vec<Floating>,
}

impl Layout {
    pub fn apply(&mut self, action: craft_ui::docking::Action<PanelKind>) -> Result<(), String> {
        crate::dock_shared::apply(self, action)
    }
    pub fn validate(&self) -> Result<(), String> {
        crate::dock_shared::encode(&self.root, &self.floating)?.validate().map_err(|error| error.to_string())
    }
    pub fn contains(&self, panel: PanelKind) -> bool {
        self.root.contains(panel) || self.floating.iter().any(|group| group.panels.contains(&panel))
    }
}

/// One laid-out tab group.
pub struct Group {
    pub path: String,
    pub rect: Rect,
    pub content: Rect,
    pub panels: Vec<PanelKind>,
    pub active: usize,
}

/// Actions produced by interacting with the dock chrome.
#[derive(Debug, Clone, PartialEq)]
pub enum DockAction {
    Activate(PanelKind),
    BeginDrag(PanelKind),
    Focus(PanelKind),
    Close(PanelKind),
    PanelMenu(PanelKind, egui::Pos2),
    /// A sequence tab of the Timeline panel was clicked / closed (project item id).
    OpenSequence(u64),
    CloseSequence(u64),
    /// A sequence tab was dragged past a neighbour: its new place among the sequence tabs.
    MoveSequence(u64, usize),
}

/// Lay out the tree into group rects (with `gap` gutters) and handle gutter dragging.
pub fn layout(ui: &mut egui::Ui, node: &mut DockNode, rect: Rect, t: &Tokens, path: &str, out: &mut Vec<Group>, reg: &mut crate::automation::Registry) {
    let mut actions = Vec::new();
    layout_with_actions(ui, node, rect, t, path, out, reg, &mut actions);
    for action in actions {
        crate::dock_shared::apply_tree(node, action);
    }
}

/// Lay out chrome while returning mutations for the application's UI command route.
pub fn layout_with_actions(
    ui: &mut egui::Ui,
    node: &mut DockNode,
    rect: Rect,
    t: &Tokens,
    path: &str,
    out: &mut Vec<Group>,
    reg: &mut crate::automation::Registry,
    actions: &mut Vec<craft_ui::docking::Action<PanelKind>>,
) {
    layout_inner(ui, node, rect, t, path, &[], out, reg, actions);
}

/// A child that is a strip: one compact panel (Tools, Audio Meters).
fn is_strip(node: &DockNode) -> bool {
    matches!(node, DockNode::Tabs { panels, .. } if panels.len() == 1 && panels[0].compact())
}

/// The narrowest a panel gets before a drag pushes past it.
const PANEL_MIN: f32 = 40.0;

fn split_axis(vertical: bool) -> SplitAxis {
    if vertical { SplitAxis::Vertical } else { SplitAxis::Horizontal }
}

/// The smallest extent of `node` laid out in `rect`, along the height when `vertical`, else the width.
/// Tools needs the width of as many columns as its height takes.
fn min_extent(node: &DockNode, rect: Rect, vertical: bool, t: &Tokens) -> f32 {
    match node {
        DockNode::Tabs { panels, active } => {
            let tab_h = if matches!(panels.as_slice(), [panel] if panel.compact()) { 8.0 } else { t.tab_h };
            match panels.get(*active) {
                Some(PanelKind::Tools) if vertical => tab_h + crate::panels::tools::min_height(),
                Some(PanelKind::Tools) => crate::panels::tools::width_for(rect.height() - tab_h),
                Some(PanelKind::Timeline) if !vertical => crate::panels::timeline::min_width(),
                _ => PANEL_MIN,
            }
        }
        DockNode::Split { vertical: v, size, a, b } => {
            let Some(split) = split_rect(rect, split_axis(*v), size.shared(), t.gap, 20.0) else { return PANEL_MIN };
            let (a, b) = (min_extent(a, split.first, vertical, t), min_extent(b, split.second, vertical, t));
            if *v == vertical { a + b + t.gap } else { a.max(b) }
        }
    }
}

/// One split of a run: its first child is the run's items `lo..mid`, its second `mid..hi`.
#[derive(Clone, Debug, PartialEq)]
struct RunSplit {
    path: String,
    tree_path: Vec<bool>,
    size: SplitSize,
    lo: usize,
    mid: usize,
    hi: usize,
}

/// A run item: a group, or a split the other way, with where the tree puts it.
struct RunItem<'n> {
    node: &'n mut DockNode,
    rect: Rect,
    path: String,
    tree_path: Vec<bool>,
}

/// A row of panels side by side (a column when `vertical`): the splits the same way as the one at
/// the top, flattened, so a drag treats the row as a whole however the tree nests it.
fn collect_run<'n>(
    node: &'n mut DockNode,
    vertical: bool,
    rect: Rect,
    path: String,
    tree_path: Vec<bool>,
    gap: f32,
    items: &mut Vec<RunItem<'n>>,
    splits: &mut Vec<RunSplit>,
) {
    if !matches!(&*node, DockNode::Split { vertical: v, .. } if *v == vertical) {
        items.push(RunItem { node, rect, path, tree_path });
        return;
    }
    let DockNode::Split { size, a, b, .. } = node else { return };
    let Some(split) = split_rect(rect, split_axis(vertical), size.shared(), gap, 20.0) else { return };
    let index = splits.len();
    let lo = items.len();
    splits.push(RunSplit { path: path.clone(), tree_path: tree_path.clone(), size: *size, lo, mid: lo, hi: lo });
    let child = |second: bool| {
        let mut p = tree_path.clone();
        p.push(second);
        p
    };
    collect_run(a, vertical, split.first, format!("{path}a"), child(false), gap, items, splits);
    let mid = items.len();
    collect_run(b, vertical, split.second, format!("{path}b"), child(true), gap, items, splits);
    let hi = items.len();
    if let Some(split) = splits.get_mut(index) {
        (split.mid, split.hi) = (mid, hi);
    }
}

/// The extent of items `lo..hi` and the gaps between them.
fn span(sizes: &[f32], lo: usize, hi: usize, gap: f32) -> f32 {
    sizes.get(lo..hi).unwrap_or_default().iter().sum::<f32>() + hi.saturating_sub(lo + 1) as f32 * gap
}

/// Grow every item below its minimum (Tools made shorter needs another column) from the others'
/// room above theirs: panels before strips, the nearest first, the next one along before the one
/// behind.
fn fit_minimums(sizes: &[f32], mins: &[f32], strips: &[bool]) -> Vec<f32> {
    let mut s = sizes.to_vec();
    let min = |j: usize| mins.get(j).copied().unwrap_or(0.0);
    for i in 0..s.len() {
        let need = min(i) - s.get(i).copied().unwrap_or(0.0);
        if need.is_nan() || need <= 0.0 {
            continue;
        }
        let mut order: Vec<usize> = (0..s.len()).filter(|&j| j != i).collect();
        order.sort_by_key(|&j| (strips.get(j).copied().unwrap_or(false), j.abs_diff(i), j < i));
        let mut got = 0.0;
        for j in order {
            if let Some(size) = s.get_mut(j) {
                let take = (*size - min(j)).max(0.0).min(need - got);
                *size -= take;
                got += take;
            }
        }
        if let Some(size) = s.get_mut(i) {
            *size += got;
        }
    }
    s
}

/// Move the divider before item `k` by `delta` (towards the end when positive): shrink-then-push.
/// The items it moves into shrink one after another, the nearest first, each down to its minimum
/// before the next gives way; the item on the other side grows. It stops where everything it moves
/// into is at its minimum.
fn push_divider(sizes: &[f32], mins: &[f32], k: usize, delta: f32) -> Option<Vec<f32>> {
    if !delta.is_finite() || k == 0 || k >= sizes.len() {
        return None;
    }
    let mut s = sizes.to_vec();
    let mut left = delta.abs();
    let (into, grows): (Vec<usize>, usize) = if delta > 0.0 { ((k..s.len()).collect(), k - 1) } else { ((0..k).rev().collect(), k) };
    for j in into {
        let (Some(size), Some(min)) = (s.get_mut(j), mins.get(j)) else { return None };
        let take = (*size - min).max(0.0).min(left);
        *size -= take;
        left -= take;
    }
    *s.get_mut(grows)? += delta.abs() - left;
    Some(s)
}

/// The sizes to store for the splits of a run whose items went from `shown` to `sizes`. A split
/// whose stored side didn't change keeps its value, so a strip only widened to fit (Tools showing
/// another column) keeps the width it was given and narrows again once it can.
fn write_back(splits: &[RunSplit], shown: &[f32], sizes: &[f32], gap: f32) -> Vec<(Vec<bool>, SharedSplitSize)> {
    let mut out = vec![];
    for split in splits {
        let (f0, s0) = (span(shown, split.lo, split.mid, gap), span(shown, split.mid, split.hi, gap));
        let (f1, s1) = (span(sizes, split.lo, split.mid, gap), span(sizes, split.mid, split.hi, gap));
        let size = match split.size {
            SplitSize::Ratio(_) if f0 + s0 > 0.0 && f1 + s1 > 0.0 => {
                let ratio = f1 / (f1 + s1);
                ((ratio - f0 / (f0 + s0)).abs() > 1e-5).then_some(SharedSplitSize::Ratio(ratio))
            }
            SplitSize::FixedA(_) => ((f1 - f0).abs() > 1e-2).then_some(SharedSplitSize::FixedFirst(f1)),
            SplitSize::FixedB(_) => ((s1 - s0).abs() > 1e-2).then_some(SharedSplitSize::FixedSecond(s1)),
            SplitSize::Ratio(_) => None,
        };
        out.extend(size.map(|size| (split.tree_path.clone(), size)));
    }
    out
}

/// A divider drag in progress: the run's sizes when it began and how far it has gone.
#[derive(Clone, Debug)]
struct DragStart {
    sizes: Vec<f32>,
    offset: f32,
}

fn layout_inner(
    ui: &mut egui::Ui,
    node: &mut DockNode,
    rect: Rect,
    t: &Tokens,
    path: &str,
    tree_path: &[bool],
    out: &mut Vec<Group>,
    reg: &mut crate::automation::Registry,
    actions: &mut Vec<craft_ui::docking::Action<PanelKind>>,
) {
    let vertical = match node {
        DockNode::Tabs { panels, active } => {
            let tab_h = if panels.len() == 1 && panels[0].compact() { 8.0 } else { t.tab_h };
            let content = Rect::from_min_max(pos2(rect.min.x, rect.min.y + tab_h), rect.max);
            out.push(Group { path: path.to_string(), rect, content, panels: panels.clone(), active: *active });
            return;
        }
        DockNode::Split { vertical, .. } => *vertical,
    };
    let mut items = vec![];
    let mut splits = vec![];
    collect_run(node, vertical, rect, path.to_string(), tree_path.to_vec(), t.gap, &mut items, &mut splits);
    if items.is_empty() {
        return;
    }
    let along = |r: Rect| if vertical { r.height() } else { r.width() };
    let tree: Vec<f32> = items.iter().map(|item| along(item.rect)).collect();
    let mins: Vec<f32> = items.iter().map(|item| min_extent(&*item.node, item.rect, vertical, t)).collect();
    let strips: Vec<bool> = items.iter().map(|item| is_strip(&*item.node)).collect();
    let shown = fit_minimums(&tree, &mins, &strips);
    let mut at = if vertical { rect.min.y } else { rect.min.x };
    let rects: Vec<Rect> = shown
        .iter()
        .map(|size| {
            let r = if vertical {
                Rect::from_min_max(pos2(rect.min.x, at), pos2(rect.max.x, at + size))
            } else {
                Rect::from_min_max(pos2(at, rect.min.y), pos2(at + size, rect.max.y))
            };
            at += size + t.gap;
            r
        })
        .collect();
    let handle_style = ResizeHandleStyle { hovered: t.focus.gamma_multiply(0.4), dragged: t.focus.gamma_multiply(0.9), focused: t.focus };
    let label = if vertical { tl!("Height:") } else { tl!("Width:") };
    for split in &splits {
        let (Some(&before), Some(&after)) = (split.mid.checked_sub(1).and_then(|i| rects.get(i)), rects.get(split.mid)) else { continue };
        let divider = if vertical {
            Rect::from_min_max(pos2(rect.min.x, before.max.y), pos2(rect.max.x, after.min.y))
        } else {
            Rect::from_min_max(pos2(before.max.x, rect.min.y), pos2(after.min.x, rect.max.y))
        };
        let first = span(&shown, split.lo, split.mid, t.gap);
        let available = first + span(&shown, split.mid, split.hi, t.gap);
        let id = egui::Id::new(("dock-gutter", split.path.clone()));
        let response = ResizeHandle::new(id, split_axis(vertical), divider, label, handle_style).value(first, 0.0..=available).show(ui);
        reg.add_response(&format!("dock.gutter.{}", split.path), &response.response, "gutter");
        let start_id = id.with("start");
        if response.response.drag_started() {
            ui.data_mut(|d| d.insert_temp(start_id, DragStart { sizes: shown.clone(), offset: 0.0 }));
        }
        if response.delta != 0.0 {
            // a pointer drag works from where it began, so dragging back undoes a push
            let start = response.response.dragged().then(|| ui.data(|d| d.get_temp::<DragStart>(start_id))).flatten().filter(|s| s.sizes.len() == shown.len());
            let (from, offset) = match start {
                Some(DragStart { sizes, offset }) => {
                    let offset = offset + response.delta;
                    ui.data_mut(|d| d.insert_temp(start_id, DragStart { sizes: sizes.clone(), offset }));
                    (sizes, offset)
                }
                None => (shown.clone(), response.delta),
            };
            if let Some(sizes) = push_divider(&from, &mins, split.mid, offset) {
                for (path, size) in write_back(&splits, &shown, &sizes, t.gap) {
                    actions.push(craft_ui::docking::Action::ResizeSplit { path, size });
                }
            }
        }
        if response.response.drag_stopped() {
            ui.data_mut(|d| d.remove::<DragStart>(start_id));
        }
    }
    for (item, r) in items.into_iter().zip(rects) {
        layout_inner(ui, item.node, r, t, &item.path, &item.tree_path, out, reg, actions);
    }
}

/// Draw a group's frame + tab strip. Returns actions (tab clicks, panel menu, focus).
/// The sequences open in the Timeline panel, in tab order: `(project item id, name)`.
#[derive(Debug, Clone, Default)]
pub struct SeqTabs {
    pub open: Vec<(u64, String)>,
    pub active: Option<u64>,
}

pub fn draw_group_chrome(
    ui: &mut egui::Ui,
    g: &Group,
    focused: PanelKind,
    seqs: &SeqTabs,
    t: &Tokens,
    reg: &mut crate::automation::Registry,
) -> Vec<DockAction> {
    let chrome = draw_group_chrome_details(ui, g, seqs, t, reg);
    paint_group_focus(ui, g, focused, t);
    chrome.actions
}

#[derive(Default)]
pub struct Chrome {
    pub actions: Vec<DockAction>,
    pub tabs: Vec<(PanelKind, Rect)>,
}

pub fn draw_group_chrome_details(ui: &mut egui::Ui, g: &Group, seqs: &SeqTabs, t: &Tokens, reg: &mut crate::automation::Registry) -> Chrome {
    let mut actions = Vec::new();
    let mut tab_rects: Vec<(PanelKind, Rect)> = Vec::new();
    let painter = ui.painter().clone();
    painter.rect_filled(g.rect, t.radius, t.panel_bg);
    let active_panel = g.panels.get(g.active).copied();
    let compact = g.panels.len() == 1 && g.panels[0].compact();
    // A narrow header grip moves the entire panel, including Timeline panels whose tabs
    // separately reorder open sequences.
    if let Some(panel) = active_panel {
        let grip = Rect::from_min_size(g.rect.min, vec2(if compact { g.rect.width() } else { 10.0 }, if compact { 8.0 } else { t.tab_h }));
        let response = ui.interact(grip, egui::Id::new(("panel-drag", g.path.clone())), Sense::drag());
        reg.add_response(&format!("panel.drag.{}", panel.id()), &response, "Move panel");
        if response.drag_started() {
            actions.push(DockAction::BeginDrag(panel));
        }
    }
    if compact {
        // grip dots
        let c = pos2(g.rect.center().x, g.rect.min.y + 4.0);
        for dx in [-4.0, 0.0, 4.0] {
            painter.circle_filled(c + vec2(dx, 0.0), 1.0, t.text_faint);
        }
    } else {
        let strip = Rect::from_min_size(g.rect.min, vec2(g.rect.width(), t.tab_h));
        // the Timeline panel shows one tab per open sequence (its own tab names the sequence)
        struct Tab {
            panel: PanelKind,
            seq: Option<u64>,
            title: String,
            active: bool,
        }
        let mut tabs: Vec<Tab> = Vec::new();
        for (i, p) in g.panels.iter().enumerate() {
            let active = i == g.active;
            if *p == PanelKind::Timeline && active && !seqs.open.is_empty() {
                tabs.extend(seqs.open.iter().map(|(id, name)| Tab { panel: *p, seq: Some(*id), title: name.clone(), active: seqs.active == Some(*id) }));
            } else if *p == PanelKind::Timeline {
                let name = seqs.open.iter().find(|(id, _)| seqs.active == Some(*id)).map(|(_, n)| n.clone());
                tabs.push(Tab { panel: *p, seq: None, title: name.unwrap_or_else(|| tl!("Timeline: (no sequences)").into()), active });
            } else {
                tabs.push(Tab { panel: *p, seq: None, title: crate::i18n::t(p.title()).to_string(), active });
            }
        }
        let activate = |tab: &Tab, actions: &mut Vec<DockAction>| match tab.seq {
            Some(id) => actions.push(DockAction::OpenSequence(id)),
            None => actions.push(DockAction::Activate(tab.panel)),
        };
        let active_at = tabs.iter().position(|tab| tab.active).unwrap_or(0);
        let text_y = strip.min.y + 16.0;
        // Which tabs fit. The active tab is always shown: tabs before it are left out until it
        // fits, then as many after it as there is room for. Tabs that are left out are reached
        // through the » list at the right end of the strip.
        let widths: Vec<f32> = tabs
            .iter()
            .map(|tab| {
                let label = painter.layout_no_wrap(tab.title.clone(), Tokens::ui(12.0), t.tab_text).size().x;
                label + 16.0 + if tab.active { 20.0 } else { 0.0 } + if tab.active && tab.seq.is_some() { 16.0 } else { 0.0 }
            })
            .collect();
        let span = |from: usize, to: usize| widths.get(from..=to).map_or(0.0, |w| w.iter().sum::<f32>() + 8.0 * w.len() as f32);
        let room = strip.width() - 12.0;
        let all_fit = span(0, tabs.len().saturating_sub(1)) <= room;
        let room = if all_fit { room } else { room - 24.0 };
        let mut first = 0;
        while first < active_at && span(first, active_at) > room {
            first += 1;
        }
        let mut last = active_at.max(first);
        while last + 1 < tabs.len() && span(first, last + 1) <= room {
            last += 1;
        }
        let list_id = egui::Id::new(("tab-list", g.path.clone()));
        let more = (!all_fit).then(|| Rect::from_min_size(pos2(strip.max.x - 22.0, strip.min.y + 6.0), vec2(18.0, 20.0)));
        let mut list_opened = false;
        if let Some(r) = more {
            let resp = ui.interact(r, egui::Id::new(("tab-overflow", g.path.clone())), Sense::click());
            let name = if g.panels.contains(&PanelKind::Timeline) { "timeline.tabs.more".to_string() } else { format!("panel.tabs.more.{}", g.path) };
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, resp.enabled(), "more tabs"));
            reg.add_response(&name, &resp, "more tabs");
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
        // The wheel over the strip steps through the tabs: down = next, up = previous, one tab
        // per wheel notch (trackpad scrolling adds up to a notch first), stopping at the ends.
        let wheel_id = egui::Id::new(("tab-wheel", g.path.clone()));
        if ui.rect_contains_pointer(strip) && !tabs.is_empty() {
            const NOTCH: f32 = 40.0;
            let total = ui.ctx().data(|d| d.get_temp::<f32>(wheel_id)).unwrap_or(0.0) + crate::panels::timeline::wheel_input(ui.ctx()).delta.y;
            let steps = (total / NOTCH).trunc();
            ui.ctx().data_mut(|d| d.insert_temp(wheel_id, total - steps * NOTCH));
            let to = (active_at as i64).saturating_sub(steps as i64).clamp(0, tabs.len() as i64 - 1) as usize;
            if let Some(tab) = tabs.get(to).filter(|_| to != active_at) {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(tab.panel));
            }
        } else {
            ui.ctx().data_mut(|d| d.remove::<f32>(wheel_id));
        }
        let mut x = strip.min.x + 12.0;
        // sequence tabs as drawn (for dragging one past its neighbours), and the one being dragged
        let mut seq_rects: Vec<(u64, Rect)> = Vec::new();
        let mut dragged: Option<u64> = None;
        for (i, tab) in tabs.iter().enumerate().take(last + 1).skip(first) {
            let p = &tab.panel;
            let is_active = tab.active;
            let galley = painter.layout_no_wrap(tab.title.clone(), Tokens::ui(12.0), if is_active { t.tab_text_active } else { t.tab_text });
            let close_w = if is_active && tab.seq.is_some() { 16.0 } else { 0.0 };
            let w = widths.get(i).copied().unwrap_or(0.0);
            let tab_rect = Rect::from_min_size(pos2(x, strip.min.y), vec2(w, t.tab_h));
            // a sequence tab keeps its identity when the tabs are reordered under the pointer
            let resp = match tab.seq {
                Some(id) => ui.interact(tab_rect, egui::Id::new(("tab-seq", g.path.clone(), id)), Sense::click_and_drag()),
                None => ui.interact(tab_rect, egui::Id::new(("tab", g.path.clone(), i)), Sense::click_and_drag()),
            };
            if tab.seq.is_none() && resp.drag_started() {
                actions.push(DockAction::BeginDrag(*p));
            }
            if let Some((_, rect)) = tab_rects.iter_mut().find(|(panel, _)| panel == p) {
                *rect = rect.union(tab_rect);
            } else {
                tab_rects.push((*p, tab_rect));
            }
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, resp.enabled(), is_active, &tab.title));
            ui.ctx().accesskit_node_builder(resp.id, |node| {
                node.set_role(egui::accesskit::Role::Tab);
                node.set_selected(is_active);
            });
            paint_widget_focus(&painter, &resp, t);
            if let Some(id) = tab.seq {
                seq_rects.push((id, tab_rect));
                if resp.dragged() {
                    dragged = Some(id);
                }
                // Escape puts a dragged tab back where it was (egui ends the drag on Escape, #580)
                let home = egui::Id::new(("tab-seq-home", id));
                if resp.drag_started()
                    && let Some(at) = seqs.open.iter().position(|(s, _)| *s == id)
                {
                    ui.data_mut(|d| d.insert_temp(home, at));
                }
                if resp.drag_stopped()
                    && let Some(at) = ui.data_mut(|d| d.remove_temp::<usize>(home))
                    && ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    actions.push(DockAction::MoveSequence(id, at));
                }
            }
            if let Some(id) = tab.seq {
                reg.add_response(&format!("timeline.tab.{id}"), &resp, &tab.title);
            }
            if tab.seq.is_none() || is_active {
                reg.add_response(&format!("panel.tab.{}", p.id()), &resp, p.title());
            }
            let label_x = tab_rect.min.x + 8.0 + close_w;
            let label_w = galley.size().x;
            let col = if is_active || resp.hovered() { t.tab_text_active } else { t.tab_text };
            painter.galley_with_override_text_color(pos2(label_x, text_y - galley.size().y / 2.0), galley, col);
            let mut closed = false;
            if let Some(id) = tab.seq.filter(|_| is_active) {
                // × closes the sequence's tab (the sequence stays in the project)
                let cr = Rect::from_center_size(pos2(tab_rect.min.x + 10.0, text_y), vec2(8.0, 8.0));
                let cresp = ui.interact(cr.expand(4.0), egui::Id::new(("tab-close", g.path.clone(), id)), Sense::click());
                cresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, cresp.enabled(), "close sequence"));
                reg.add_response(&format!("timeline.tab.{id}.close"), &cresp, "close sequence");
                let cc = if cresp.hovered() { t.tab_text_active } else { t.tab_text };
                painter.line_segment([cr.left_top(), cr.right_bottom()], Stroke::new(1.2, cc));
                painter.line_segment([cr.right_top(), cr.left_bottom()], Stroke::new(1.2, cc));
                paint_widget_focus(&painter, &cresp, t);
                if cresp.clicked() {
                    actions.push(DockAction::CloseSequence(id));
                    closed = true;
                }
            }
            if is_active {
                let mr = Rect::from_center_size(pos2(label_x + label_w + 12.0, text_y), vec2(12.0, 10.0));
                let mresp = ui.interact(mr.expand(3.0), egui::Id::new(("tab-menu", g.path.clone())), Sense::click());
                mresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, mresp.enabled(), "panel menu"));
                reg.add_response(&format!("panel.menu.{}", p.id()), &mresp, "panel menu");
                let mc = if mresp.hovered() { t.tab_text_active } else { t.tab_text };
                icons::paint(&painter, Rect::from_center_size(mr.center(), vec2(16.0, 16.0)), Icon::Hamburger, mc);
                // 1 pt underline spanning label + ≡, 23 pt below the frame top
                let uy = strip.min.y + 23.0;
                painter.line_segment([pos2(label_x, uy), pos2(mr.max.x, uy)], Stroke::new(1.0, t.tab_text_active));
                paint_widget_focus(&painter, &mresp, t);
                if mresp.clicked() {
                    actions.push(DockAction::PanelMenu(*p, mr.left_bottom()));
                }
            }
            if (resp.clicked() || resp.drag_started()) && !closed {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(*p));
            }
            // a right-click shows the tab and opens its panel menu (as in Premiere)
            if resp.secondary_clicked() {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(*p));
                let at = resp.interact_pointer_pos().unwrap_or(tab_rect.left_bottom());
                actions.push(DockAction::PanelMenu(*p, at));
            }
            if resp.middle_clicked() {
                // a sequence tab closes its sequence; the Timeline panel itself stays
                match tab.seq.or(seqs.active.filter(|_| *p == PanelKind::Timeline)) {
                    Some(id) => actions.push(DockAction::CloseSequence(id)),
                    None => actions.push(DockAction::Close(*p)),
                }
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
                actions.push(DockAction::MoveSequence(id, index.saturating_sub(1)));
            } else if after.is_some_and(|(_, r)| px > r.center().x) {
                actions.push(DockAction::MoveSequence(id, index + 1));
            }
        }
        // the » list: every tab of the group, the shown one ticked
        let list_open = ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
        if let Some(r) = more.filter(|_| list_open) {
            let mut picked = None;
            let area = egui::Area::new(list_id.with("area")).order(egui::Order::Foreground).pivot(Align2::RIGHT_TOP).fixed_pos(r.right_bottom()).show(
                ui.ctx(),
                |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(160.0);
                        // many open sequences: the list scrolls instead of leaving the window
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            for tab in &tabs {
                                let label = if tab.active { format!("✓ {}", tab.title) } else { tab.title.clone() };
                                let resp = ui.button(label);
                                match tab.seq {
                                    Some(id) => reg.add_response(&format!("timeline.tabs.list.{id}"), &resp, &tab.title),
                                    None => reg.add_response(&format!("panel.tabs.list.{}", tab.panel.id()), &resp, &tab.title),
                                }
                                if resp.clicked() {
                                    picked = Some(tab);
                                }
                            }
                        });
                    });
                },
            );
            if let Some(tab) = picked {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(tab.panel));
            }
            if picked.is_some() || (!list_opened && area.response.clicked_elsewhere()) {
                ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
            }
        } else if list_open {
            ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
        }
    }
    // Activating presses focus their tab immediately, rather than the group's old active panel.
    // Otherwise entering an inspector can briefly reactivate a stale Project/Timeline selection.
    // Middle-click closes a tab, so it keeps the active panel as the focus target.
    if let Some(p) = active_panel
        && ui.rect_contains_pointer(g.rect)
        && ui.input(|i| i.pointer.any_pressed())
    {
        let panel = ui
            .input(|i| i.pointer.interact_pos().filter(|_| i.pointer.primary_pressed() || i.pointer.secondary_pressed()))
            .and_then(|pos| tab_rects.iter().find(|(_, rect)| rect.contains(pos)).map(|(panel, _)| *panel))
            .unwrap_or(p);
        actions.push(DockAction::Focus(panel));
    }
    Chrome { actions, tabs: tab_rects }
}

/// Paint once after the group's contents so child backgrounds cannot hide its focus (#1075).
pub(crate) fn paint_group_focus(ui: &egui::Ui, group: &Group, focused: PanelKind, tokens: &Tokens) {
    if group.panels.get(group.active).copied() == Some(focused) {
        ui.painter().rect_stroke(group.rect, 0.0, Stroke::new(1.0, tokens.focus), StrokeKind::Inside);
    }
}

fn paint_widget_focus(painter: &egui::Painter, response: &egui::Response, tokens: &Tokens) {
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
    fn split_first_never_panics_on_tiny_areas() {
        let split_first = |size: SplitSize, avail: f32| {
            split_rect(Rect::from_min_size(egui::Pos2::ZERO, vec2(avail, 100.0)), SplitAxis::Horizontal, size.shared(), 0.0, 20.0).unwrap().first_size()
        };
        for avail in [0.0, 1.0, 15.0, 20.0, 35.0, 39.9, 40.0, 100.0, 2000.0] {
            for size in [
                SplitSize::Ratio(0.0),
                SplitSize::Ratio(0.5),
                SplitSize::Ratio(1.0),
                SplitSize::FixedA(300.0),
                SplitSize::FixedB(300.0),
                SplitSize::FixedA(0.0),
            ] {
                let f = split_first(size, avail);
                assert!((0.0..=avail).contains(&f), "{size:?} in {avail}: {f}");
            }
        }
        assert_eq!(split_first(SplitSize::FixedA(300.0), 1000.0), 300.0);
        assert_eq!(split_first(SplitSize::FixedB(300.0), 1000.0), 700.0);
        assert_eq!(split_first(SplitSize::Ratio(0.0), 1000.0), 20.0);
    }

    #[test]
    fn divider_shrinks_then_pushes() {
        // Project | Tools | Timeline | Meters
        let sizes = [300.0, 64.0, 600.0, 116.0];
        let mins = [PANEL_MIN; 4];
        let push = |k, delta| push_divider(&sizes, &mins, k, delta).unwrap();
        // Project|Tools right: Tools narrows, then Timeline, then the Meters
        assert_eq!(push(1, 10.0), [310.0, 54.0, 600.0, 116.0]);
        assert_eq!(push(1, 100.0), [400.0, 40.0, 524.0, 116.0]);
        assert_eq!(push(1, 5000.0), [960.0, 40.0, 40.0, 40.0]);
        // Tools|Timeline left: Tools narrows, then Project
        assert_eq!(push(2, -10.0), [300.0, 54.0, 610.0, 116.0]);
        assert_eq!(push(2, -100.0), [224.0, 40.0, 700.0, 116.0]);
        // pulled outwards, the strip widens
        assert_eq!(push(2, 30.0), [300.0, 94.0, 570.0, 116.0]);
        assert_eq!(push(1, -30.0), [270.0, 94.0, 600.0, 116.0]);
        // hostile input moves nothing
        assert_eq!(push_divider(&sizes, &mins, 1, f32::NAN), None);
        assert_eq!(push_divider(&sizes, &mins, 0, 10.0), None);
        assert_eq!(push_divider(&sizes, &mins, 4, 10.0), None);
    }

    #[test]
    fn strip_below_min_takes_room_from_nearest() {
        let strips = [false, true, false, true];
        // Tools needs a second column: the Timeline after it gives the room, not the Meters
        assert_eq!(fit_minimums(&[300.0, 64.0, 600.0, 116.0], &[40.0, 100.0, 40.0, 40.0], &strips), [300.0, 100.0, 564.0, 116.0]);
        // Project | Tools | Meters | Timeline: Project is the nearest panel
        assert_eq!(fit_minimums(&[300.0, 64.0, 116.0, 600.0], &[40.0, 100.0, 40.0, 40.0], &[false, true, true, false]), [264.0, 100.0, 116.0, 600.0]);
        // nothing to spare: it stays as it is
        assert_eq!(fit_minimums(&[40.0, 64.0], &[40.0, 100.0], &[false, true]), [40.0, 64.0]);
    }

    #[test]
    fn tools_strip_widens_adds_column() {
        let t = Tokens::for_kind(crate::theme::ThemeKind::Dark);
        let row = |h: f32| {
            let mut node = hsplit(SplitSize::FixedA(64.0), tabs(&[PanelKind::Tools], 0), tabs(&[PanelKind::Timeline], 0));
            let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, h));
            let (mut items, mut splits) = (vec![], vec![]);
            collect_run(&mut node, false, rect, String::new(), vec![], t.gap, &mut items, &mut splits);
            let tree: Vec<f32> = items.iter().map(|i| i.rect.width()).collect();
            let mins: Vec<f32> = items.iter().map(|i| min_extent(&*i.node, i.rect, false, &t)).collect();
            fit_minimums(&tree, &mins, &[true, false])[0]
        };
        assert_eq!(row(600.0), 64.0);
        assert_eq!(row(200.0), crate::panels::tools::width_for(192.0));
        assert!(row(200.0) > 64.0 && row(100.0) > row(200.0));
    }

    #[test]
    fn timeline_min_keeps_track_headers() {
        let t = Tokens::for_kind(crate::theme::ThemeKind::Dark);
        let timeline = tabs(&[PanelKind::Timeline], 0);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(500.0, 300.0));
        // its track headers and scrollbar stay in view; its height has the usual minimum
        assert_eq!(min_extent(&timeline, rect, false, &t), 204.0 + 17.0);
        assert_eq!(min_extent(&timeline, rect, true, &t), PANEL_MIN);
    }

    /// Every way of nesting `panels` into splits side by side; strips get their fixed size.
    fn nestings(panels: &[PanelKind]) -> Vec<DockNode> {
        if let [panel] = panels {
            return vec![tabs(&[*panel], 0)];
        }
        let mut out = vec![];
        for cut in 1..panels.len() {
            for a in nestings(&panels[..cut]) {
                for b in nestings(&panels[cut..]) {
                    let size = match (is_strip(&a), is_strip(&b)) {
                        (true, _) => SplitSize::FixedA(if cut == 1 && panels[0] == PanelKind::Tools { 64.0 } else { 116.0 }),
                        (_, true) => SplitSize::FixedB(if panels[cut] == PanelKind::Tools { 64.0 } else { 116.0 }),
                        _ => SplitSize::Ratio(0.5),
                    };
                    out.push(hsplit(size, a.clone(), b));
                }
            }
        }
        out
    }

    fn permutations(panels: &[PanelKind]) -> Vec<Vec<PanelKind>> {
        if panels.len() <= 1 {
            return vec![panels.to_vec()];
        }
        let mut out = vec![];
        for i in 0..panels.len() {
            let mut rest = panels.to_vec();
            let first = rest.remove(i);
            for mut p in permutations(&rest) {
                p.insert(0, first);
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn every_arrangement_shrinks_then_pushes() {
        use PanelKind::*;
        let t = Tokens::for_kind(crate::theme::ThemeKind::Dark);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(1600.0, 600.0));
        let measure = |node: &mut DockNode| {
            let (mut items, mut splits) = (vec![], vec![]);
            collect_run(node, false, rect, String::new(), vec![], t.gap, &mut items, &mut splits);
            let sizes: Vec<f32> = items.iter().map(|i| i.rect.width()).collect();
            let mins: Vec<f32> = items.iter().map(|i| min_extent(&*i.node, i.rect, false, &t)).collect();
            (sizes, mins, splits)
        };
        let mut checked = 0;
        for order in permutations(&[Project, MediaBrowser, Tools, AudioMeters, Timeline]) {
            for tree in nestings(&order) {
                let (sizes, mins, splits) = measure(&mut tree.clone());
                for split in &splits {
                    for delta in [15.0, 90.0, 3000.0, -15.0, -90.0, -3000.0] {
                        let want = push_divider(&sizes, &mins, split.mid, delta).unwrap();
                        let k = split.mid;
                        // the item it moves into shrinks first; the one behind only once that is at its minimum
                        let (near, far) = if delta > 0.0 { (k, k + 1) } else { (k - 1, k.wrapping_sub(2)) };
                        if let Some(&next) = want.get(far)
                            && next < sizes[far]
                        {
                            assert_eq!(want[near], mins[near].min(sizes[near]), "{order:?} {tree:?} divider {k} by {delta}");
                        }
                        let mut moved = tree.clone();
                        for (path, size) in write_back(&splits, &sizes, &want, t.gap) {
                            assert!(crate::dock_shared::apply_tree(&mut moved, craft_ui::docking::Action::ResizeSplit { path, size }), "{order:?} {tree:?}");
                        }
                        let (got, _, _) = measure(&mut moved);
                        for (g, w) in got.iter().zip(&want) {
                            assert!((g - w).abs() < 0.05, "{order:?} {tree:?} divider {k} by {delta}: {got:?} != {want:?}");
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 10_000, "{checked}");
    }

    #[test]
    fn workspaces_contain_core_panels() {
        for w in WORKSPACES {
            let d = workspace(w);
            assert!(d.contains(PanelKind::Timeline), "{w}");
            assert!(d.contains(PanelKind::Program), "{w}");
        }
    }

    #[test]
    fn close_and_open() {
        let mut d = workspace("Editing");
        d.close(PanelKind::Metadata);
        assert!(!d.contains(PanelKind::Metadata));
        d.open_near(PanelKind::LumetriColor, PanelKind::Program);
        assert!(d.is_visible(PanelKind::LumetriColor));
        let s = serde_json::to_string(&d).unwrap();
        let back: DockNode = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
    }

    /// #284: with Project and Program closed (or every panel closed) a panel still opens.
    #[test]
    fn open_near_falls_back_to_the_first_group() {
        let mut d = workspace("Editing");
        let mut all = Vec::new();
        d.panels(&mut all);
        for p in all {
            d.close(p);
        }
        assert_eq!(d, DockNode::Tabs { panels: vec![], active: 0 });
        d.open_near(PanelKind::Program, PanelKind::Project);
        assert!(d.is_visible(PanelKind::Program));
        d.restore_timeline();
        assert!(d.is_visible(PanelKind::Timeline));
        // a tree without Project or Program: the panel joins the first group
        let mut d = hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects], 0), tabs(&[PanelKind::Markers], 0));
        d.open_near(PanelKind::Info, PanelKind::Source);
        assert!(d.is_visible(PanelKind::Info));
        assert_eq!(d, hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects, PanelKind::Info], 1), tabs(&[PanelKind::Markers], 0)));
    }
}

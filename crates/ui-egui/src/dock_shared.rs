//! Convert the persisted workspace schema to shared docking transactions.

use craft_ui::docking::{Action, Floating as SharedFloating, Layout as SharedLayout, Location, Node, Placement, Zone};
use craft_ui::layout::{SplitAxis, SplitSize as SharedSplitSize};

use crate::dock::{DockNode, Floating, Layout, PanelKind, SplitSize};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4096;
const MAX_PANELS: usize = 128;

fn encode_node(node: &DockNode, depth: usize, count: &mut usize) -> Result<Node<PanelKind>, String> {
    *count = count.saturating_add(1);
    if depth > MAX_DEPTH || *count > MAX_NODES {
        return Err("workspace dock tree exceeds supported limits".into());
    }
    Ok(match node {
        DockNode::Tabs { panels, active } => {
            if panels.len() > MAX_PANELS {
                return Err("too many workspace tabs".into());
            }
            Node::Tabs { panels: panels.clone(), active: *active }
        }
        DockNode::Split { vertical, size, a, b } => Node::Split {
            axis: if *vertical { SplitAxis::Vertical } else { SplitAxis::Horizontal },
            size: match *size {
                SplitSize::Ratio(value) => SharedSplitSize::Ratio(value),
                SplitSize::FixedA(value) => SharedSplitSize::FixedFirst(value),
                SplitSize::FixedB(value) => SharedSplitSize::FixedSecond(value),
            },
            first: Box::new(encode_node(a, depth.saturating_add(1), count)?),
            second: Box::new(encode_node(b, depth.saturating_add(1), count)?),
        },
    })
}

fn decode_node(node: Node<PanelKind>) -> Result<DockNode, String> {
    Ok(match node {
        Node::Tabs { panels, active } => DockNode::Tabs { panels, active },
        Node::Stack { .. } => return Err("FilmCraft does not use stacked panels".into()),
        Node::Split { axis, size, first, second } => DockNode::Split {
            vertical: axis == SplitAxis::Vertical,
            size: match size {
                SharedSplitSize::Ratio(value) => SplitSize::Ratio(value),
                SharedSplitSize::FixedFirst(value) => SplitSize::FixedA(value),
                SharedSplitSize::FixedSecond(value) => SplitSize::FixedB(value),
            },
            a: Box::new(decode_node(*first)?),
            b: Box::new(decode_node(*second)?),
        },
    })
}

pub(crate) fn encode(root: &DockNode, floating: &[Floating]) -> Result<SharedLayout<PanelKind>, String> {
    if floating.len() > MAX_PANELS || floating.iter().any(|group| group.panels.len() > MAX_PANELS) {
        return Err("too many floating workspace panels".into());
    }
    let root = if matches!(root, DockNode::Tabs { panels, active: 0 } if panels.is_empty()) { None } else { Some(encode_node(root, 0, &mut 0)?) };
    Ok(SharedLayout {
        root,
        floating: floating.iter().map(|group| SharedFloating { panels: group.panels.clone(), active: group.active, rect: group.rect }).collect(),
    })
}

pub(crate) fn decode(layout: SharedLayout<PanelKind>) -> Result<Layout, String> {
    Ok(Layout {
        root: match layout.root {
            Some(root) => decode_node(root)?,
            None => DockNode::Tabs { panels: vec![], active: 0 },
        },
        floating: layout.floating.into_iter().map(|group| Floating { panels: group.panels, active: group.active, rect: group.rect }).collect(),
    })
}

pub(crate) fn apply(layout: &mut Layout, action: Action<PanelKind>) -> Result<(), String> {
    let mut shared = encode(&layout.root, &layout.floating)?;
    shared.validate().map_err(|error| error.to_string())?;
    check_last_docked(&shared, &action)?;
    shared.apply(action).map_err(|error| error.to_string())?;
    let proposed = decode(shared)?;
    encode(&proposed.root, &proposed.floating)?;
    *layout = proposed;
    Ok(())
}

pub(crate) fn apply_tree(root: &mut DockNode, action: Action<PanelKind>) -> bool {
    let Ok(mut shared) = encode(root, &[]) else { return false };
    if shared.apply(action).is_err() {
        return false;
    }
    let Ok(layout) = decode(shared) else { return false };
    if encode(&layout.root, &[]).is_err() {
        return false;
    }
    *root = layout.root;
    true
}

/// Dock a panel along one edge of the whole workspace. The shared model restores a panel beside a
/// complete sibling subtree, so naming every remaining docked panel targets the root itself.
pub(crate) fn dock_at_edge(layout: &mut SharedLayout<PanelKind>, panel: PanelKind, zone: Zone) -> Result<(), String> {
    let size = match zone {
        Zone::Left | Zone::Top => SharedSplitSize::Ratio(0.25),
        Zone::Right | Zone::Bottom => SharedSplitSize::Ratio(0.75),
        Zone::Center => return Err("a workspace edge needs a left, right, top or bottom zone".into()),
    };
    let mut proposed = layout.clone();
    proposed.apply(Action::Close { panel }).map_err(|error| error.to_string())?;
    let sibling: Vec<PanelKind> = SharedLayout { root: proposed.root.clone(), floating: vec![] }.panels().into_iter().copied().collect();
    let anchor = *sibling.first().ok_or("another docked panel is required to dock beside")?;
    let location = Location { floating: None, anchor: Some(anchor), placement: Placement::Split(zone), size: Some(size), stack: None, sibling };
    proposed.restore(panel, &location).map_err(|error| error.to_string())?;
    *layout = proposed;
    Ok(())
}

/// Keep one docked destination available for dragging floating panels back into the workspace.
pub(crate) fn check_last_docked(layout: &SharedLayout<PanelKind>, action: &Action<PanelKind>) -> Result<(), String> {
    let panel = match action {
        Action::Float { panel, .. } => Some(panel),
        Action::Move { panel, anchor, .. } if layout.floating.iter().any(|group| group.panels.contains(anchor)) => Some(panel),
        _ => None,
    };
    if let Some(panel) = panel {
        let docked = SharedLayout { root: layout.root.clone(), floating: vec![] };
        if docked.contains(panel) && docked.panels().len() <= 1 {
            return Err("the last docked panel must remain in the workspace".into());
        }
    }
    Ok(())
}

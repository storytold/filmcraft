use filmcraft_time::Tick;
use roxmltree::Node;
use std::collections::HashSet;

use super::effects;
use super::graph::{Graph, PRESETS, at, decode, error, key, parse, text_at};
use super::{ImportedPreset, PresetTiming};
use crate::xml::{child_text, elements};
use crate::{Report, Result};

/// Import user-authored native `.prfpset` files. Names and component order come from TreeItem
/// references, not from object numbering or translated effect display names.
pub fn import_presets(bytes: &[u8]) -> Result<(Vec<ImportedPreset>, Report)> {
    let text = decode(bytes, PRESETS)?;
    let doc = parse(&text, PRESETS)?;
    let graph = Graph::new(doc.root_element(), PRESETS)?;
    let mut report = Report::default();
    let mut presets = Vec::new();
    for (item, name) in preset_entries(&graph)? {
        let Some(data) = at(item, &["TreeItemBase", "Data"]) else { continue };
        let preset_item = graph.resolve(data)?;
        if !preset_item.has_tag_name("FilterPresetItem") {
            continue;
        }
        let mut preset = None;
        for filter in graph.references(preset_item, &["FilterPresets"])? {
            if !filter.has_tag_name("FilterPreset") {
                return Err(error(PRESETS, "FilterPresets references a different object type"));
            }
            let start = graph.required_integer(filter, &["AnchorInPoint"])?;
            let end = graph.required_integer(filter, &["AnchorOutPoint"])?;
            let duration = end.checked_sub(start).filter(|d| *d >= 0).ok_or_else(|| error(PRESETS, "invalid preset anchor range"))?;
            if duration > super::MAX_TIME_TICKS {
                return Err(error(PRESETS, "preset duration is out of range"));
            }
            let timing = match graph.integer(filter, &["Type"], 0)? {
                0 => PresetTiming::Scale,
                1 => PresetTiming::AnchorToIn,
                2 => PresetTiming::AnchorToOut,
                other => return Err(error(PRESETS, format!("unknown preset timing type {other}"))),
            };
            let speed = child_text(filter, "Speed").unwrap_or("1").parse::<f64>().map_err(|e| error(PRESETS, e.to_string()))?;
            if !speed.is_finite() {
                return Err(error(PRESETS, "preset speed is not finite"));
            }
            if speed != 1.0 {
                report.warn(format!("Premiere preset \"{name}\" has a speed adjustment that is not supported"));
            }
            let component = graph.reference(filter, &["Component"])?;
            let transition_duration = graph.integer(filter, &["TransitionDuration"], 0)?;
            if !(0..=super::MAX_TIME_TICKS).contains(&transition_duration) {
                return Err(error(PRESETS, "transition preset duration is out of range"));
            }
            let transition = transition_duration > 0 && child_text(component, "MatchName") == Some("AE.ADBE Cross Dissolve New");
            let effect = if transition {
                if graph.boolean(component, &["Component", "Bypass"], false)? {
                    return Err(error(PRESETS, "disabled transition presets are not supported"));
                }
                filmcraft_project::find_effect("cross_dissolve").map(|d| d.instance())
            } else {
                effects::component(&graph, component, Tick(start), (1920, 1080), (1920, 1080), &mut report)?
            };
            let Some(effect) = effect else { continue };
            let p = preset.get_or_insert_with(|| ImportedPreset {
                name: name.clone(),
                description: child_text(filter, "Description").unwrap_or("").to_string(),
                timing,
                source_duration: Tick(duration),
                source_size: (1920, 1080),
                effects: Vec::new(),
                transition_duration: transition.then_some(Tick(transition_duration)),
            });
            if p.source_duration != Tick(duration) || p.timing != timing {
                return Err(error(PRESETS, format!("preset \"{name}\" combines incompatible timing modes or anchor ranges")));
            }
            if p.transition_duration.is_some() != transition || (transition && !p.effects.is_empty()) {
                return Err(error(PRESETS, "mixed transition and clip-effect presets are not supported"));
            }
            if duration == 0 && effect.is_animated() {
                return Err(error(PRESETS, "animated preset has zero source duration"));
            }
            p.effects.push(effect);
        }
        if let Some(p) = preset {
            presets.push(p);
        } else {
            report.warn(format!("Premiere preset \"{name}\" was not imported because it contains no supported clip effects"));
        }
    }
    if presets.is_empty() && report.is_empty() {
        return Err(error(PRESETS, "document contains no effect presets"));
    }
    if !presets.is_empty() {
        report.info("Premiere preset normalised point parameters use a 1920x1080 reference size and scale to the target frame size");
    }
    Ok((presets, report))
}

fn preset_entries<'a, 'i>(graph: &Graph<'a, 'i>) -> Result<Vec<(Node<'a, 'i>, String)>> {
    let tree = elements(graph.root)
        .find(|n| n.has_tag_name("Tree") && n.attribute("ObjectID").is_some())
        .ok_or_else(|| error(PRESETS, "document contains no preset tree"))?;
    let root = graph.reference(tree, &["RootBin"])?;
    let mut entries = Vec::new();
    walk_tree(graph, root, "", true, &mut HashSet::new(), &mut entries, 0)?;
    Ok(entries)
}

fn walk_tree<'a, 'i>(
    graph: &Graph<'a, 'i>,
    node: Node<'a, 'i>,
    prefix: &str,
    root: bool,
    active: &mut HashSet<String>,
    entries: &mut Vec<(Node<'a, 'i>, String)>,
    depth: usize,
) -> Result<()> {
    graph.charge()?;
    if depth >= 64 || !active.insert(key(node)) {
        return Err(error(PRESETS, "cyclic or excessively deep preset bins"));
    }
    let name = text_at(node, &["TreeItemBase", "Name"]).filter(|n| !n.is_empty()).unwrap_or("Imported preset");
    if node.has_tag_name("BinTreeItem") {
        let defaults = text_at(node, &["TreeItemBase", "Node", "Properties", "HandlerEffects.EffectItemTree.PresetsBin"]) == Some("1");
        let prefix = if root || defaults {
            prefix.to_string()
        } else if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if prefix.len() > 4096 {
            return Err(error(PRESETS, "expanded preset folder name exceeds 4096 bytes"));
        }
        for child in graph.references(node, &["Items"])? {
            walk_tree(graph, child, &prefix, false, active, entries, depth + 1)?
        }
    } else if node.has_tag_name("TreeItem") {
        let name = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
        if name.len() > 4096 {
            return Err(error(PRESETS, "expanded preset name exceeds 4096 bytes"));
        }
        entries.push((node, name));
    }
    active.remove(&key(node));
    Ok(())
}
